use core::hint::spin_loop;
use core::sync::atomic::{Ordering, compiler_fence};

use nexos_usb::UsbSpeed;
use nexos_usb::descriptor::{
    ConfigurationDescriptor, ConfigurationSummary, DeviceDescriptor, summarize_configuration,
};
use nexos_usb::xhci::{PortStatus, Trb, TrbType};

use crate::memory::FrameAllocator;
use crate::paging::{PageMapError, PagingInfo};
use crate::pci::{PciDevice, PciInventory};

const PAGE_SIZE: u64 = 4096;
const PAGE_BYTES: usize = 4096;
const MMIO_BYTES: u64 = 0x1_0000;
const MMIO_BASE: u64 = 0xffff_ff30_0000_0000;
const MMIO_STRIDE: u64 = 0x20_0000;
const MAX_CONTROLLERS: usize = 2;
const MAX_ROOT_PORTS: usize = 32;
const MAX_USB_DEVICES: usize = 16;
const TRBS_PER_PAGE: usize = PAGE_BYTES / 16;
const TRBS_PER_PAGE_U32: u32 = 256;
const POLL_LIMIT: usize = 8_000_000;

const CAP_HCSPARAMS1: u64 = 0x04;
const CAP_HCSPARAMS2: u64 = 0x08;
const CAP_HCCPARAMS1: u64 = 0x10;
const CAP_DBOFF: u64 = 0x14;
const CAP_RTSOFF: u64 = 0x18;

const OP_USBCMD: u64 = 0x00;
const OP_USBSTS: u64 = 0x04;
const OP_PAGESIZE: u64 = 0x08;
const OP_CRCR: u64 = 0x18;
const OP_DCBAAP: u64 = 0x30;
const OP_CONFIG: u64 = 0x38;
const OP_PORT_BASE: u64 = 0x400;
const OP_PORT_STRIDE: u64 = 0x10;

const USBCMD_RUN_STOP: u32 = 1;
const USBCMD_HOST_CONTROLLER_RESET: u32 = 1 << 1;
const USBSTS_HALTED: u32 = 1;
const USBSTS_CONTROLLER_NOT_READY: u32 = 1 << 11;

const INTERRUPTER_ZERO: u64 = 0x20;
const IMAN: u64 = 0x00;
const IMOD: u64 = 0x04;
const ERSTSZ: u64 = 0x08;
const ERSTBA: u64 = 0x10;
const ERDP: u64 = 0x18;

const PORTSC_POWER: u32 = 1 << 9;
const PORTSC_CHANGE_BITS: u32 = 0x7f << 17;

#[derive(Clone, Copy, Debug)]
pub enum XhciError {
    MissingBar,
    MmioMap(PageMapError),
    InvalidCapability,
    FirmwareOwnershipTimeout,
    HaltTimeout,
    ResetTimeout,
    UnsupportedPageSize,
    OutOfFrames,
    AddressAboveFourGib,
    ScratchpadCountTooLarge,
    StartTimeout,
    CommandTimeout,
    CommandFailed(u8),
    TransferTimeout,
    TransferFailed(u8),
    InvalidDescriptor,
    TransferTooLarge,
}

impl XhciError {
    #[must_use]
    pub const fn detail_code(self) -> u32 {
        match self {
            Self::MmioMap(error) => match error {
                PageMapError::Unaligned => 1,
                PageMapError::InvalidIndex => 2,
                PageMapError::AddressOverflow => 3,
                PageMapError::OutOfFrames => 4,
                PageMapError::HugePageConflict => 5,
                PageMapError::AlreadyMapped => 6,
                PageMapError::NotMapped => 7,
            },
            Self::CommandFailed(code) | Self::TransferFailed(code) => code as u32,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProbeStats {
    pub pci_controllers: u8,
    pub mapped_controllers: u8,
    pub initialized_controllers: u8,
    pub total_ports: u16,
    pub connected_ports: u16,
    pub enabled_ports: u16,
    pub command_completions: u16,
    pub enumeration_attempts: u16,
    pub enumerated_devices: u16,
    pub enumeration_failures: u16,
    pub last_error: Option<XhciError>,
}

#[derive(Clone, Copy, Debug)]
pub struct RootPortInfo {
    pub number: u8,
    pub connected: bool,
    pub enabled: bool,
    pub powered: bool,
    pub link_state: u8,
    pub speed: UsbSpeed,
}

#[derive(Clone, Copy, Debug)]
pub struct UsbDeviceInfo {
    pub slot_id: u8,
    pub address: u8,
    pub root_port: u8,
    pub speed: UsbSpeed,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usb_version: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub configuration_value: u8,
    pub summary: ConfigurationSummary,
}

impl UsbDeviceInfo {
    const EMPTY: Self = Self {
        slot_id: 0,
        address: 0,
        root_port: 0,
        speed: UsbSpeed::Unknown,
        vendor_id: 0,
        product_id: 0,
        usb_version: 0,
        device_class: 0,
        device_subclass: 0,
        device_protocol: 0,
        configuration_value: 0,
        summary: ConfigurationSummary {
            descriptors: 0,
            interfaces: 0,
            endpoints: 0,
            hid_keyboards: 0,
            hid_mice: 0,
            hubs: 0,
            mass_storage: 0,
        },
    };
}

impl RootPortInfo {
    const EMPTY: Self = Self {
        number: 0,
        connected: false,
        enabled: false,
        powered: false,
        link_state: 0,
        speed: UsbSpeed::Unknown,
    };
}

struct ControlPipe {
    ring_physical: u64,
    ring_virtual: u64,
    enqueue: usize,
    cycle: bool,
    buffer_physical: u64,
    buffer_virtual: u64,
}

#[derive(Clone, Copy)]
pub struct XhciController {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub version: u16,
    pub max_slots: u8,
    pub max_interrupters: u16,
    pub context_bytes: u8,
    pub supports_64_bit: bool,
    pub scratchpads: u16,
    pub no_op_completion: u8,
    operational: u64,
    doorbells: u64,
    runtime: u64,
    command_ring_physical: u64,
    command_ring_virtual: u64,
    command_enqueue: usize,
    command_cycle: bool,
    completed_commands: u16,
    event_ring_physical: u64,
    event_ring_virtual: u64,
    event_dequeue: usize,
    event_cycle: bool,
    dcbaa_virtual: u64,
    ports: [RootPortInfo; MAX_ROOT_PORTS],
    port_count: usize,
    devices: [UsbDeviceInfo; MAX_USB_DEVICES],
    device_count: usize,
    enumeration_attempts: u8,
    enumeration_failures: u8,
    last_enumeration_error: Option<XhciError>,
}

impl XhciController {
    #[allow(clippy::similar_names, clippy::too_many_lines)]
    fn initialize(
        pci: &PciDevice,
        controller_index: u64,
        paging: &mut PagingInfo,
        allocator: &mut FrameAllocator,
    ) -> Result<Self, XhciError> {
        let bar = pci
            .bar(0)
            .filter(|bar| !bar.is_io)
            .ok_or(XhciError::MissingBar)?;
        let virtual_base = MMIO_BASE
            .checked_add(controller_index.saturating_mul(MMIO_STRIDE))
            .ok_or(XhciError::InvalidCapability)?;
        let capability = map_mmio(paging, allocator, bar.address, MMIO_BYTES, virtual_base)
            .map_err(XhciError::MmioMap)?;
        let capability_length = u64::from(read8(capability));
        if !(0x20..=0x80).contains(&capability_length) {
            return Err(XhciError::InvalidCapability);
        }
        let capability_header = read32(capability).to_le_bytes();
        let version = u16::from_le_bytes([capability_header[2], capability_header[3]]);
        let hcsparams1 = read32(capability + CAP_HCSPARAMS1);
        let hcsparams2 = read32(capability + CAP_HCSPARAMS2);
        let hccparams1 = read32(capability + CAP_HCCPARAMS1);
        let max_slots = (hcsparams1 & 0xff) as u8;
        let max_interrupters = ((hcsparams1 >> 8) & 0x7ff) as u16;
        let max_ports = ((hcsparams1 >> 24) & 0xff) as u8;
        if max_slots == 0 || max_ports == 0 {
            return Err(XhciError::InvalidCapability);
        }
        let scratchpads = (((hcsparams2 >> 21) & 0x1f) << 5) | ((hcsparams2 >> 27) & 0x1f);
        let scratchpads =
            u16::try_from(scratchpads).map_err(|_| XhciError::ScratchpadCountTooLarge)?;
        let supports_64_bit = hccparams1 & 1 != 0;
        let context_bytes = if hccparams1 & (1 << 2) == 0 { 32 } else { 64 };
        let doorbell_offset = u64::from(read32(capability + CAP_DBOFF) & !3);
        let runtime_offset = u64::from(read32(capability + CAP_RTSOFF) & !0x1f);
        let operational = capability + capability_length;
        let doorbells = capability + doorbell_offset;
        let runtime = capability + runtime_offset;

        pci.enable_memory_bus_mastering();
        take_firmware_ownership(capability, hccparams1)?;
        halt_and_reset(operational)?;
        if read32(operational + OP_PAGESIZE) & 1 == 0 {
            return Err(XhciError::UnsupportedPageSize);
        }

        let dcbaa_physical = allocate_zeroed_frame(allocator, paging.hhdm_offset())?;
        let command_ring_physical = allocate_zeroed_frame(allocator, paging.hhdm_offset())?;
        let event_ring_physical = allocate_zeroed_frame(allocator, paging.hhdm_offset())?;
        let erst_physical = allocate_zeroed_frame(allocator, paging.hhdm_offset())?;
        if !supports_64_bit
            && [
                dcbaa_physical,
                command_ring_physical,
                event_ring_physical,
                erst_physical,
            ]
            .into_iter()
            .any(|address| address > u64::from(u32::MAX))
        {
            return Err(XhciError::AddressAboveFourGib);
        }
        let command_ring_virtual = paging
            .hhdm_offset()
            .checked_add(command_ring_physical)
            .ok_or(XhciError::OutOfFrames)?;
        let event_ring_virtual = paging
            .hhdm_offset()
            .checked_add(event_ring_physical)
            .ok_or(XhciError::OutOfFrames)?;
        let dcbaa_virtual = paging
            .hhdm_offset()
            .checked_add(dcbaa_physical)
            .ok_or(XhciError::OutOfFrames)?;
        let erst_virtual = paging
            .hhdm_offset()
            .checked_add(erst_physical)
            .ok_or(XhciError::OutOfFrames)?;

        if scratchpads != 0 {
            initialize_scratchpads(
                dcbaa_virtual,
                scratchpads,
                supports_64_bit,
                allocator,
                paging.hhdm_offset(),
            )?;
        }

        write_trb(
            command_ring_virtual + ((TRBS_PER_PAGE - 1) * 16) as u64,
            Trb::command(TrbType::Link)
                .with_parameter(command_ring_physical)
                .with_control_bits(1 | (1 << 1)),
        );
        write_memory64(erst_virtual, event_ring_physical);
        write_memory32(erst_virtual + 8, TRBS_PER_PAGE_U32);
        write_memory32(erst_virtual + 12, 0);

        write64(operational + OP_DCBAAP, dcbaa_physical);
        write64(operational + OP_CRCR, command_ring_physical | 1);
        write32(operational + OP_CONFIG, u32::from(max_slots.min(32)) & 0xff);
        let interrupter = runtime + INTERRUPTER_ZERO;
        write32(interrupter + IMAN, 1);
        write32(interrupter + IMOD, 0);
        write32(interrupter + ERSTSZ, 1);
        write64(interrupter + ERSTBA, erst_physical);
        write64(interrupter + ERDP, event_ring_physical);

        let mut controller = Self {
            bus: pci.bus,
            device: pci.device,
            function: pci.function,
            vendor_id: pci.vendor_id,
            device_id: pci.device_id,
            version,
            max_slots,
            max_interrupters,
            context_bytes,
            supports_64_bit,
            scratchpads,
            no_op_completion: 0,
            operational,
            doorbells,
            runtime,
            command_ring_physical,
            command_ring_virtual,
            command_enqueue: 0,
            command_cycle: true,
            completed_commands: 0,
            event_ring_physical,
            event_ring_virtual,
            event_dequeue: 0,
            event_cycle: true,
            dcbaa_virtual,
            ports: [RootPortInfo::EMPTY; MAX_ROOT_PORTS],
            port_count: usize::from(max_ports).min(MAX_ROOT_PORTS),
            devices: [UsbDeviceInfo::EMPTY; MAX_USB_DEVICES],
            device_count: 0,
            enumeration_attempts: 0,
            enumeration_failures: 0,
            last_enumeration_error: None,
        };
        controller.power_root_ports(hcsparams1 & (1 << 3) != 0);
        write32(
            operational + OP_USBCMD,
            read32(operational + OP_USBCMD) | USBCMD_RUN_STOP,
        );
        for _ in 0..POLL_LIMIT {
            if read32(operational + OP_USBSTS) & USBSTS_HALTED == 0 {
                controller.reset_usb2_ports();
                controller.refresh_ports();
                controller.no_op_completion = controller
                    .submit_command(Trb::command(TrbType::NoOpCommand))?
                    .completion_code();
                controller.enumerate_root_devices(allocator, paging.hhdm_offset());
                return Ok(controller);
            }
            spin_loop();
        }
        Err(XhciError::StartTimeout)
    }

    fn power_root_ports(&self, power_control: bool) {
        if !power_control {
            return;
        }
        for index in 0..self.port_count {
            let register = self.port_register(index);
            let value = read32(register);
            if value & PORTSC_POWER == 0 {
                write32(register, (value & !PORTSC_CHANGE_BITS) | PORTSC_POWER);
            }
        }
    }

    pub fn refresh_ports(&mut self) {
        for index in 0..self.port_count {
            let status = PortStatus::from_portsc(read32(self.port_register(index)));
            self.ports[index] = RootPortInfo {
                number: u8::try_from(index + 1).unwrap_or(u8::MAX),
                connected: status.connected,
                enabled: status.enabled,
                powered: status.powered,
                link_state: status.link_state,
                speed: UsbSpeed::from_xhci_port_speed(status.speed_id),
            };
        }
    }

    fn reset_usb2_ports(&self) {
        for index in 0..self.port_count {
            let register = self.port_register(index);
            let value = read32(register);
            let status = PortStatus::from_portsc(value);
            if !status.connected || status.enabled || status.speed_id >= 4 {
                continue;
            }
            write32(
                register,
                (value & !PORTSC_CHANGE_BITS) | (1 << 4) | PORTSC_POWER,
            );
            for _ in 0..POLL_LIMIT {
                let updated = read32(register);
                if updated & (1 << 4) == 0 {
                    break;
                }
                spin_loop();
            }
        }
    }

    fn enumerate_root_devices(&mut self, allocator: &mut FrameAllocator, hhdm_offset: u64) {
        for port_index in 0..self.port_count {
            if self.device_count == MAX_USB_DEVICES {
                break;
            }
            let port = self.ports[port_index];
            if !port.connected || !port.enabled {
                continue;
            }
            self.enumeration_attempts = self.enumeration_attempts.saturating_add(1);
            match self.enumerate_root_device(port, allocator, hhdm_offset) {
                Ok(device) => {
                    self.devices[self.device_count] = device;
                    self.device_count += 1;
                }
                Err(error) => {
                    self.enumeration_failures = self.enumeration_failures.saturating_add(1);
                    self.last_enumeration_error = Some(error);
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn enumerate_root_device(
        &mut self,
        port: RootPortInfo,
        allocator: &mut FrameAllocator,
        hhdm_offset: u64,
    ) -> Result<UsbDeviceInfo, XhciError> {
        let enable_slot = self.submit_command(Trb::command(TrbType::EnableSlotCommand))?;
        let slot_id = enable_slot.slot_id();
        if slot_id == 0 {
            return Err(XhciError::CommandFailed(enable_slot.completion_code()));
        }

        let device_context_physical = allocate_zeroed_frame(allocator, hhdm_offset)?;
        let input_context_physical = allocate_zeroed_frame(allocator, hhdm_offset)?;
        let transfer_ring_physical = allocate_zeroed_frame(allocator, hhdm_offset)?;
        let buffer_physical = allocate_zeroed_frame(allocator, hhdm_offset)?;
        if !self.supports_64_bit
            && [
                device_context_physical,
                input_context_physical,
                transfer_ring_physical,
                buffer_physical,
            ]
            .into_iter()
            .any(|address| address > u64::from(u32::MAX))
        {
            return Err(XhciError::AddressAboveFourGib);
        }
        let device_context_virtual = hhdm_offset
            .checked_add(device_context_physical)
            .ok_or(XhciError::OutOfFrames)?;
        let input_context_virtual = hhdm_offset
            .checked_add(input_context_physical)
            .ok_or(XhciError::OutOfFrames)?;
        let transfer_ring_virtual = hhdm_offset
            .checked_add(transfer_ring_physical)
            .ok_or(XhciError::OutOfFrames)?;
        let buffer_virtual = hhdm_offset
            .checked_add(buffer_physical)
            .ok_or(XhciError::OutOfFrames)?;
        write_trb(
            transfer_ring_virtual + ((TRBS_PER_PAGE - 1) * 16) as u64,
            Trb::command(TrbType::Link)
                .with_parameter(transfer_ring_physical)
                .with_control_bits(1 | (1 << 1)),
        );

        let context_stride = u64::from(self.context_bytes);
        let slot_context = input_context_virtual + context_stride;
        let endpoint_zero_context = input_context_virtual + context_stride * 2;
        write_memory32(input_context_virtual + 4, 3);
        write_memory32(
            slot_context,
            (u32::from(speed_id(port.speed)) << 20) | (1 << 27),
        );
        write_memory32(slot_context + 4, u32::from(port.number) << 16);
        let max_packet_size = endpoint_zero_packet_size(port.speed);
        write_memory32(
            endpoint_zero_context + 4,
            (3 << 1) | (4 << 3) | (u32::from(max_packet_size) << 16),
        );
        write_memory64(endpoint_zero_context + 8, transfer_ring_physical | 1);
        write_memory32(endpoint_zero_context + 16, 8);
        write_memory64(
            self.dcbaa_virtual + u64::from(slot_id) * 8,
            device_context_physical,
        );

        let address_event = self.submit_command(
            Trb::command(TrbType::AddressDeviceCommand)
                .with_parameter(input_context_physical)
                .with_control_bits(u32::from(slot_id) << 24),
        )?;
        if address_event.slot_id() != slot_id {
            return Err(XhciError::CommandFailed(address_event.completion_code()));
        }
        let address = read_memory32(device_context_virtual + 12).to_le_bytes()[0];
        if address == 0 {
            return Err(XhciError::InvalidDescriptor);
        }

        let mut pipe = ControlPipe {
            ring_physical: transfer_ring_physical,
            ring_virtual: transfer_ring_virtual,
            enqueue: 0,
            cycle: true,
            buffer_physical,
            buffer_virtual,
        };
        self.control_in(slot_id, &mut pipe, 0x80, 6, 0x0100, 0, 18)?;
        // SAFETY: The completed control transfer filled the exclusive DMA
        // buffer with exactly one standard device descriptor.
        let device_bytes = unsafe { core::slice::from_raw_parts(buffer_virtual as *const u8, 18) };
        let descriptor =
            DeviceDescriptor::parse(device_bytes).map_err(|_| XhciError::InvalidDescriptor)?;

        self.control_in(slot_id, &mut pipe, 0x80, 6, 0x0200, 0, 9)?;
        // SAFETY: The completed transfer filled the first nine bytes.
        let configuration_header =
            unsafe { core::slice::from_raw_parts(buffer_virtual as *const u8, 9) };
        let configuration = ConfigurationDescriptor::parse(configuration_header)
            .map_err(|_| XhciError::InvalidDescriptor)?;
        let total_length = usize::from(configuration.total_length);
        if total_length > PAGE_BYTES {
            return Err(XhciError::TransferTooLarge);
        }
        self.control_in(
            slot_id,
            &mut pipe,
            0x80,
            6,
            0x0200,
            0,
            configuration.total_length,
        )?;
        // SAFETY: The request length was bounded to the exclusive DMA page.
        let configuration_bytes =
            unsafe { core::slice::from_raw_parts(buffer_virtual as *const u8, total_length) };
        let summary = summarize_configuration(configuration_bytes)
            .map_err(|_| XhciError::InvalidDescriptor)?;
        self.control_no_data(
            slot_id,
            &mut pipe,
            0,
            9,
            u16::from(configuration.configuration_value),
            0,
        )?;

        Ok(UsbDeviceInfo {
            slot_id,
            address,
            root_port: port.number,
            speed: port.speed,
            vendor_id: descriptor.vendor_id,
            product_id: descriptor.product_id,
            usb_version: descriptor.usb_version,
            device_class: descriptor.device_class,
            device_subclass: descriptor.device_subclass,
            device_protocol: descriptor.device_protocol,
            configuration_value: configuration.configuration_value,
            summary,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn control_in(
        &mut self,
        slot_id: u8,
        pipe: &mut ControlPipe,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        length: u16,
    ) -> Result<(), XhciError> {
        if usize::from(length) > PAGE_BYTES {
            return Err(XhciError::TransferTooLarge);
        }
        // SAFETY: The control pipe owns one DMA buffer page.
        unsafe {
            core::ptr::write_bytes(pipe.buffer_virtual as *mut u8, 0, usize::from(length));
        }
        let setup = setup_packet(request_type, request, value, index, length);
        Self::push_transfer(
            pipe,
            Trb::command(TrbType::SetupStage)
                .with_parameter(setup)
                .with_status(8)
                .with_control_bits((3 << 16) | (1 << 6) | (1 << 4)),
        );
        Self::push_transfer(
            pipe,
            Trb::command(TrbType::DataStage)
                .with_parameter(pipe.buffer_physical)
                .with_status(u32::from(length))
                .with_control_bits((1 << 16) | (1 << 4)),
        );
        let status_physical = Self::push_transfer(
            pipe,
            Trb::command(TrbType::StatusStage).with_control_bits(1 << 5),
        );
        self.ring_control_doorbell(slot_id);
        self.wait_transfer(slot_id, status_physical)
    }

    #[allow(clippy::too_many_arguments)]
    fn control_no_data(
        &mut self,
        slot_id: u8,
        pipe: &mut ControlPipe,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
    ) -> Result<(), XhciError> {
        let setup = setup_packet(request_type, request, value, index, 0);
        Self::push_transfer(
            pipe,
            Trb::command(TrbType::SetupStage)
                .with_parameter(setup)
                .with_status(8)
                .with_control_bits((1 << 6) | (1 << 4)),
        );
        let status_physical = Self::push_transfer(
            pipe,
            Trb::command(TrbType::StatusStage).with_control_bits((1 << 16) | (1 << 5)),
        );
        self.ring_control_doorbell(slot_id);
        self.wait_transfer(slot_id, status_physical)
    }

    fn push_transfer(pipe: &mut ControlPipe, mut trb: Trb) -> u64 {
        if pipe.enqueue == TRBS_PER_PAGE - 1 {
            write_trb(
                pipe.ring_virtual + (pipe.enqueue * 16) as u64,
                Trb::command(TrbType::Link)
                    .with_parameter(pipe.ring_physical)
                    .with_control_bits(u32::from(pipe.cycle) | (1 << 1)),
            );
            pipe.enqueue = 0;
            pipe.cycle = !pipe.cycle;
        }
        trb.control = (trb.control & !1) | u32::from(pipe.cycle);
        let physical = pipe.ring_physical + (pipe.enqueue * 16) as u64;
        write_trb(pipe.ring_virtual + (pipe.enqueue * 16) as u64, trb);
        pipe.enqueue += 1;
        physical
    }

    fn ring_control_doorbell(&self, slot_id: u8) {
        compiler_fence(Ordering::SeqCst);
        write32(self.doorbells + u64::from(slot_id) * 4, 1);
    }

    fn wait_transfer(&mut self, slot_id: u8, expected_trb: u64) -> Result<(), XhciError> {
        for _ in 0..POLL_LIMIT {
            if let Some(event) = self.next_event() {
                if event.trb_type() != Some(TrbType::TransferEvent)
                    || event.slot_id() != slot_id
                    || event.parameter() & !0xf != expected_trb
                {
                    continue;
                }
                let completion = event.completion_code();
                return if completion == 1 || completion == 13 {
                    Ok(())
                } else {
                    Err(XhciError::TransferFailed(completion))
                };
            }
            spin_loop();
        }
        Err(XhciError::TransferTimeout)
    }

    #[must_use]
    pub fn ports(&self) -> &[RootPortInfo] {
        &self.ports[..self.port_count]
    }

    #[must_use]
    pub fn devices(&self) -> &[UsbDeviceInfo] {
        &self.devices[..self.device_count]
    }

    pub fn self_test(&mut self) -> Result<u8, XhciError> {
        let completion = self
            .submit_command(Trb::command(TrbType::NoOpCommand))?
            .completion_code();
        self.no_op_completion = completion;
        self.refresh_ports();
        Ok(completion)
    }

    fn submit_command(&mut self, mut trb: Trb) -> Result<Trb, XhciError> {
        if self.command_enqueue == TRBS_PER_PAGE - 1 {
            write_trb(
                self.command_ring_virtual + (self.command_enqueue * 16) as u64,
                Trb::command(TrbType::Link)
                    .with_parameter(self.command_ring_physical)
                    .with_control_bits(u32::from(self.command_cycle) | (1 << 1)),
            );
            self.command_enqueue = 0;
            self.command_cycle = !self.command_cycle;
        }
        trb.control = (trb.control & !1) | u32::from(self.command_cycle);
        let command_physical = self.command_ring_physical + (self.command_enqueue * 16) as u64;
        let command_virtual = self.command_ring_virtual + (self.command_enqueue * 16) as u64;
        write_trb(command_virtual, trb);
        self.command_enqueue += 1;
        compiler_fence(Ordering::SeqCst);
        write32(self.doorbells, 0);

        for _ in 0..POLL_LIMIT {
            if let Some(event) = self.next_event()
                && event.trb_type() == Some(TrbType::CommandCompletionEvent)
                && event.parameter() & !0xf == command_physical
            {
                let completion = event.completion_code();
                return if completion == 1 {
                    self.completed_commands = self.completed_commands.saturating_add(1);
                    Ok(event)
                } else {
                    Err(XhciError::CommandFailed(completion))
                };
            }
            spin_loop();
        }
        Err(XhciError::CommandTimeout)
    }

    fn next_event(&mut self) -> Option<Trb> {
        let address = self.event_ring_virtual + (self.event_dequeue * 16) as u64;
        let event = read_trb(address);
        if event.cycle() != self.event_cycle {
            return None;
        }
        self.event_dequeue += 1;
        if self.event_dequeue == TRBS_PER_PAGE {
            self.event_dequeue = 0;
            self.event_cycle = !self.event_cycle;
        }
        let dequeue_physical = self.event_ring_physical + (self.event_dequeue * 16) as u64;
        write64(
            self.runtime + INTERRUPTER_ZERO + ERDP,
            dequeue_physical | (1 << 3),
        );
        Some(event)
    }

    const fn port_register(&self, index: usize) -> u64 {
        self.operational + OP_PORT_BASE + index as u64 * OP_PORT_STRIDE
    }
}

pub struct XhciManager {
    controllers: [Option<XhciController>; MAX_CONTROLLERS],
    count: usize,
    stats: ProbeStats,
}

impl XhciManager {
    #[must_use]
    pub fn discover(
        inventory: &PciInventory,
        paging: &mut PagingInfo,
        allocator: &mut FrameAllocator,
    ) -> Self {
        let mut manager = Self {
            controllers: [None; MAX_CONTROLLERS],
            count: 0,
            stats: ProbeStats::default(),
        };
        let mut controller_index = 0_u64;
        for device in inventory.devices() {
            if device.class != 0x0c
                || device.subclass != 0x03
                || device.programming_interface != 0x30
            {
                continue;
            }
            manager.stats.pci_controllers = manager.stats.pci_controllers.saturating_add(1);
            if manager.count == MAX_CONTROLLERS {
                continue;
            }
            match XhciController::initialize(device, controller_index, paging, allocator) {
                Ok(controller) => {
                    manager.stats.mapped_controllers =
                        manager.stats.mapped_controllers.saturating_add(1);
                    manager.stats.initialized_controllers =
                        manager.stats.initialized_controllers.saturating_add(1);
                    manager.stats.command_completions = manager
                        .stats
                        .command_completions
                        .saturating_add(controller.completed_commands);
                    manager.stats.enumeration_attempts = manager
                        .stats
                        .enumeration_attempts
                        .saturating_add(u16::from(controller.enumeration_attempts));
                    manager.stats.enumerated_devices = manager
                        .stats
                        .enumerated_devices
                        .saturating_add(u16::try_from(controller.device_count).unwrap_or(u16::MAX));
                    manager.stats.enumeration_failures = manager
                        .stats
                        .enumeration_failures
                        .saturating_add(u16::from(controller.enumeration_failures));
                    if let Some(error) = controller.last_enumeration_error {
                        manager.stats.last_error = Some(error);
                    }
                    manager.controllers[manager.count] = Some(controller);
                    manager.count += 1;
                }
                Err(error) => manager.stats.last_error = Some(error),
            }
            controller_index += 1;
        }
        manager.refresh();
        manager
    }

    pub fn refresh(&mut self) {
        self.stats.total_ports = 0;
        self.stats.connected_ports = 0;
        self.stats.enabled_ports = 0;
        for controller in self.controllers.iter_mut().flatten() {
            controller.refresh_ports();
            self.stats.total_ports = self
                .stats
                .total_ports
                .saturating_add(u16::try_from(controller.ports().len()).unwrap_or(u16::MAX));
            for port in controller.ports() {
                self.stats.connected_ports = self
                    .stats
                    .connected_ports
                    .saturating_add(u16::from(port.connected));
                self.stats.enabled_ports = self
                    .stats
                    .enabled_ports
                    .saturating_add(u16::from(port.enabled));
            }
        }
    }

    pub fn self_test(&mut self) -> bool {
        let mut passed = self.count != 0;
        for controller in self.controllers.iter_mut().flatten() {
            match controller.self_test() {
                Ok(1) => {
                    self.stats.command_completions =
                        self.stats.command_completions.saturating_add(1);
                }
                Ok(_) => passed = false,
                Err(error) => {
                    self.stats.last_error = Some(error);
                    passed = false;
                }
            }
        }
        self.refresh();
        passed
    }

    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    #[must_use]
    pub const fn stats(&self) -> ProbeStats {
        self.stats
    }

    #[must_use]
    pub fn controller(&self, index: usize) -> Option<&XhciController> {
        self.controllers.get(index)?.as_ref()
    }
}

fn initialize_scratchpads(
    dcbaa_virtual: u64,
    scratchpad_count: u16,
    supports_64_bit: bool,
    allocator: &mut FrameAllocator,
    hhdm_offset: u64,
) -> Result<(), XhciError> {
    if usize::from(scratchpad_count) > PAGE_BYTES / 8 {
        return Err(XhciError::ScratchpadCountTooLarge);
    }
    let pointer_array = allocate_zeroed_frame(allocator, hhdm_offset)?;
    let buffers = allocator
        .allocate_contiguous(u64::from(scratchpad_count))
        .ok_or(XhciError::OutOfFrames)?;
    if !supports_64_bit
        && (pointer_array > u64::from(u32::MAX)
            || buffers
                .checked_add(u64::from(scratchpad_count) * PAGE_SIZE)
                .is_none_or(|end| end > u64::from(u32::MAX)))
    {
        return Err(XhciError::AddressAboveFourGib);
    }
    let pointer_array_virtual = hhdm_offset
        .checked_add(pointer_array)
        .ok_or(XhciError::OutOfFrames)?;
    let buffers_virtual = hhdm_offset
        .checked_add(buffers)
        .ok_or(XhciError::OutOfFrames)?;
    // SAFETY: The allocator granted exclusive ownership of these scratchpad
    // backing pages and the HHDM maps them writable.
    unsafe {
        core::ptr::write_bytes(
            buffers_virtual as *mut u8,
            0,
            usize::from(scratchpad_count) * PAGE_BYTES,
        );
    }
    for index in 0..usize::from(scratchpad_count) {
        write_memory64(
            pointer_array_virtual + (index * 8) as u64,
            buffers + index as u64 * PAGE_SIZE,
        );
    }
    write_memory64(dcbaa_virtual, pointer_array);
    Ok(())
}

fn take_firmware_ownership(capability: u64, hccparams1: u32) -> Result<(), XhciError> {
    let mut offset = u64::from(hccparams1 >> 16) * 4;
    for _ in 0..64 {
        if offset == 0 || offset >= MMIO_BYTES - 8 {
            return Ok(());
        }
        let register = capability + offset;
        let header = read32(register);
        let capability_id = (header & 0xff) as u8;
        if capability_id == 1 {
            write32(register, header | (1 << 24));
            for _ in 0..POLL_LIMIT {
                if read32(register) & (1 << 16) == 0 {
                    write32(register + 4, 0);
                    return Ok(());
                }
                spin_loop();
            }
            return Err(XhciError::FirmwareOwnershipTimeout);
        }
        let next = u64::from((header >> 8) & 0xff) * 4;
        if next == 0 {
            return Ok(());
        }
        offset = offset
            .checked_add(next)
            .ok_or(XhciError::InvalidCapability)?;
    }
    Err(XhciError::InvalidCapability)
}

fn halt_and_reset(operational: u64) -> Result<(), XhciError> {
    write32(
        operational + OP_USBCMD,
        read32(operational + OP_USBCMD) & !USBCMD_RUN_STOP,
    );
    let mut halted = false;
    for _ in 0..POLL_LIMIT {
        if read32(operational + OP_USBSTS) & USBSTS_HALTED != 0 {
            halted = true;
            break;
        }
        spin_loop();
    }
    if !halted {
        return Err(XhciError::HaltTimeout);
    }
    write32(
        operational + OP_USBCMD,
        read32(operational + OP_USBCMD) | USBCMD_HOST_CONTROLLER_RESET,
    );
    for _ in 0..POLL_LIMIT {
        if read32(operational + OP_USBCMD) & USBCMD_HOST_CONTROLLER_RESET == 0
            && read32(operational + OP_USBSTS) & USBSTS_CONTROLLER_NOT_READY == 0
        {
            return Ok(());
        }
        spin_loop();
    }
    Err(XhciError::ResetTimeout)
}

fn allocate_zeroed_frame(
    allocator: &mut FrameAllocator,
    hhdm_offset: u64,
) -> Result<u64, XhciError> {
    let physical = allocator.allocate().ok_or(XhciError::OutOfFrames)?;
    let virtual_address = hhdm_offset
        .checked_add(physical)
        .ok_or(XhciError::OutOfFrames)?;
    // SAFETY: The frame was just allocated exclusively and the HHDM maps it.
    unsafe { core::ptr::write_bytes(virtual_address as *mut u8, 0, PAGE_BYTES) };
    Ok(physical)
}

const fn speed_id(speed: UsbSpeed) -> u8 {
    match speed {
        UsbSpeed::Full => 1,
        UsbSpeed::Low => 2,
        UsbSpeed::High => 3,
        UsbSpeed::Super => 4,
        UsbSpeed::SuperPlus => 5,
        UsbSpeed::Unknown => 0,
    }
}

const fn endpoint_zero_packet_size(speed: UsbSpeed) -> u16 {
    match speed {
        UsbSpeed::Low | UsbSpeed::Full | UsbSpeed::Unknown => 8,
        UsbSpeed::High => 64,
        UsbSpeed::Super | UsbSpeed::SuperPlus => 512,
    }
}

const fn setup_packet(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> u64 {
    request_type as u64
        | ((request as u64) << 8)
        | ((value as u64) << 16)
        | ((index as u64) << 32)
        | ((length as u64) << 48)
}

fn map_mmio(
    paging: &mut PagingInfo,
    allocator: &mut FrameAllocator,
    physical: u64,
    length: u64,
    virtual_base: u64,
) -> Result<u64, PageMapError> {
    let physical_page = physical & !(PAGE_SIZE - 1);
    let offset = physical - physical_page;
    let bytes = offset
        .checked_add(length)
        .ok_or(PageMapError::AddressOverflow)?;
    for page in 0..bytes.div_ceil(PAGE_SIZE) {
        paging.map_mmio_page(
            virtual_base + page * PAGE_SIZE,
            physical_page + page * PAGE_SIZE,
            allocator,
        )?;
    }
    virtual_base
        .checked_add(offset)
        .ok_or(PageMapError::AddressOverflow)
}

fn read8(address: u64) -> u8 {
    // SAFETY: The address is within the mapped xHCI MMIO aperture.
    unsafe { core::ptr::read_volatile(address as *const u8) }
}

fn read32(address: u64) -> u32 {
    // SAFETY: The address is an aligned xHCI MMIO register.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn read_memory32(address: u64) -> u32 {
    // SAFETY: The address is within an exclusive aligned DMA allocation.
    unsafe { core::ptr::read_volatile(address as *const u32) }
}

fn write32(address: u64, value: u32) {
    // SAFETY: The address is an aligned writable xHCI MMIO register.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

fn write64(address: u64, value: u64) {
    // SAFETY: The address is an aligned writable xHCI MMIO register.
    unsafe { core::ptr::write_volatile(address as *mut u64, value) };
}

fn write_memory32(address: u64, value: u32) {
    // SAFETY: The address is within an exclusive aligned DMA allocation.
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
}

fn write_memory64(address: u64, value: u64) {
    // SAFETY: The address is within an exclusive aligned DMA allocation.
    unsafe { core::ptr::write_volatile(address as *mut u64, value) };
}

fn write_trb(address: u64, trb: Trb) {
    write_memory32(address, trb.parameter_low);
    write_memory32(address + 4, trb.parameter_high);
    write_memory32(address + 8, trb.status);
    compiler_fence(Ordering::Release);
    write_memory32(address + 12, trb.control);
}

fn read_trb(address: u64) -> Trb {
    let control = read32(address + 12);
    compiler_fence(Ordering::Acquire);
    Trb {
        parameter_low: read32(address),
        parameter_high: read32(address + 4),
        status: read32(address + 8),
        control,
    }
}
