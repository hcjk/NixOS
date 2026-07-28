use alloc::boxed::Box;
use core::fmt::Write;
use core::hint::spin_loop;
use core::sync::atomic::{Ordering, compiler_fence};

use nexos_storage::{BlockDevice, StorageError};
use nexos_usb::UsbSpeed;
use nexos_usb::descriptor::{
    ClassBindings, ConfigurationDescriptor, ConfigurationSummary, DeviceDescriptor, Direction,
    EndpointDescriptor, TransferType, classify_configuration, summarize_configuration,
};
use nexos_usb::hid::{BootKeyboard, KeyboardEvent, MouseReport};
use nexos_usb::hub::{HubDescriptor, HubPortIterator, HubPortStatus};
use nexos_usb::mass_storage::{
    CBW_BYTES, CSW_BYTES, Capacity10, CommandBlockWrapper, CommandStatus, CommandStatusWrapper,
    DataDirection, ScsiCommand,
};
use nexos_usb::xhci::{PortStatus, Trb, TrbType};

use crate::memory::FrameAllocator;
use crate::paging::{PageMapError, PagingInfo};
use crate::pci::{PciDevice, PciInventory};
use crate::serial::SerialPort;

const PAGE_SIZE: u64 = 4096;
const PAGE_BYTES: usize = 4096;
const MMIO_BYTES: u64 = 0x1_0000;
const MMIO_BASE: u64 = 0xffff_ff30_0000_0000;
const MMIO_STRIDE: u64 = 0x20_0000;
const MAX_CONTROLLERS: usize = 2;
const MAX_ROOT_PORTS: usize = 32;
const MAX_USB_DEVICES: usize = 16;
const MAX_CLASS_ENDPOINTS: usize = 4;
pub const MAX_USB_STORAGE_DEVICES: usize = 8;
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
    EndpointLimit,
    InvalidEndpoint,
    BotProtocol,
    UnsupportedCapacity,
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
    pub hid_keyboards: u16,
    pub hid_mice: u16,
    pub hubs: u16,
    pub mass_storage_devices: u16,
    pub input_events: u64,
    pub storage_commands: u64,
    pub disconnects: u16,
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
    pub connected: bool,
    pub keyboard_online: bool,
    pub mouse_online: bool,
    pub hub_ports: u8,
    pub mass_storage_online: bool,
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
        connected: false,
        keyboard_online: false,
        mouse_online: false,
        hub_ports: 0,
        mass_storage_online: false,
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

#[derive(Clone, Copy)]
struct ControlPipe {
    ring_physical: u64,
    ring_virtual: u64,
    enqueue: usize,
    cycle: bool,
    buffer_physical: u64,
    buffer_virtual: u64,
}

impl ControlPipe {
    const EMPTY: Self = Self {
        ring_physical: 0,
        ring_virtual: 0,
        enqueue: 0,
        cycle: true,
        buffer_physical: 0,
        buffer_virtual: 0,
    };
}

#[derive(Clone, Copy)]
struct EndpointPipe {
    dci: u8,
    ring_physical: u64,
    ring_virtual: u64,
    enqueue: usize,
    cycle: bool,
    buffer_physical: u64,
    buffer_virtual: u64,
    max_packet_size: u16,
    pending: bool,
    pending_trb: u64,
    pending_length: u16,
    completed: bool,
    completion_code: u8,
    residual: u32,
}

impl EndpointPipe {
    const EMPTY: Self = Self {
        dci: 0,
        ring_physical: 0,
        ring_virtual: 0,
        enqueue: 0,
        cycle: true,
        buffer_physical: 0,
        buffer_virtual: 0,
        max_packet_size: 0,
        pending: false,
        pending_trb: 0,
        pending_length: 0,
        completed: false,
        completion_code: 0,
        residual: 0,
    };
}

#[derive(Clone, Copy)]
struct MassStorageState {
    interface_number: u8,
    bulk_in: u8,
    bulk_out: u8,
    logical_unit: u8,
    next_tag: u32,
    sector_size: u32,
    sector_count: u64,
    model: [u8; 40],
    online: bool,
}

impl MassStorageState {
    const EMPTY: Self = Self {
        interface_number: 0,
        bulk_in: 0,
        bulk_out: 0,
        logical_unit: 0,
        next_tag: 1,
        sector_size: 0,
        sector_count: 0,
        model: [b' '; 40],
        online: false,
    };
}

#[derive(Clone, Copy)]
struct DeviceRuntime {
    active: bool,
    slot_id: u8,
    root_port: u8,
    parent_hub_slot: u8,
    parent_port: u8,
    input_context_physical: u64,
    input_context_virtual: u64,
    control: ControlPipe,
    endpoints: [EndpointPipe; MAX_CLASS_ENDPOINTS],
    endpoint_count: usize,
    keyboard_endpoint: Option<u8>,
    keyboard: BootKeyboard,
    mouse_endpoint: Option<u8>,
    mouse_previous: MouseReport,
    hub_endpoint: Option<u8>,
    hub_ports: u8,
    mass_storage: Option<MassStorageState>,
}

impl DeviceRuntime {
    const EMPTY: Self = Self {
        active: false,
        slot_id: 0,
        root_port: 0,
        parent_hub_slot: 0,
        parent_port: 0,
        input_context_physical: 0,
        input_context_virtual: 0,
        control: ControlPipe::EMPTY,
        endpoints: [EndpointPipe::EMPTY; MAX_CLASS_ENDPOINTS],
        endpoint_count: 0,
        keyboard_endpoint: None,
        keyboard: BootKeyboard::new(),
        mouse_endpoint: None,
        mouse_previous: MouseReport {
            buttons: 0,
            delta_x: 0,
            delta_y: 0,
            wheel: 0,
        },
        hub_endpoint: None,
        hub_ports: 0,
        mass_storage: None,
    };
}

#[derive(Clone, Copy, Debug)]
pub enum UsbInputEvent {
    Keyboard(KeyboardEvent),
    Mouse(MouseReport),
}

#[derive(Clone, Copy)]
struct DeviceTopology {
    root_port: u8,
    route_string: u32,
    parent_hub_slot: u8,
    parent_port: u8,
}

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
    devices: Box<[UsbDeviceInfo]>,
    runtimes: Box<[DeviceRuntime]>,
    device_count: usize,
    enumeration_attempts: u8,
    enumeration_failures: u8,
    last_enumeration_error: Option<XhciError>,
    input_events: u64,
    storage_commands: u64,
    disconnects: u16,
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
            devices: alloc::vec![UsbDeviceInfo::EMPTY; MAX_USB_DEVICES].into_boxed_slice(),
            runtimes: alloc::vec![DeviceRuntime::EMPTY; MAX_USB_DEVICES].into_boxed_slice(),
            device_count: 0,
            enumeration_attempts: 0,
            enumeration_failures: 0,
            last_enumeration_error: None,
            input_events: 0,
            storage_commands: 0,
            disconnects: 0,
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
        let mut disabled_slots = [0_u8; MAX_USB_DEVICES];
        let mut disabled_count = 0;
        for index in 0..self.device_count {
            let runtime = &mut self.runtimes[index];
            if !runtime.active || runtime.parent_hub_slot != 0 {
                continue;
            }
            let connected = self
                .ports
                .get(usize::from(runtime.root_port.saturating_sub(1)))
                .is_some_and(|port| port.connected);
            if connected {
                continue;
            }
            runtime.active = false;
            self.devices[index].connected = false;
            self.devices[index].mass_storage_online = false;
            disabled_slots[disabled_count] = runtime.slot_id;
            disabled_count += 1;
            self.disconnects = self.disconnects.saturating_add(1);
        }
        for slot_id in disabled_slots.into_iter().take(disabled_count) {
            let _ = self.submit_command(
                Trb::command(TrbType::DisableSlotCommand)
                    .with_control_bits(u32::from(slot_id) << 24),
            );
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
            usb_log(format_args!(
                "xHCI: enumerate root port {} ({:?})",
                port.number, port.speed
            ));
            let topology = DeviceTopology {
                root_port: port.number,
                route_string: 0,
                parent_hub_slot: 0,
                parent_port: 0,
            };
            match self.enumerate_device(port, topology, allocator, hhdm_offset) {
                Ok((device, runtime)) => {
                    usb_log(format_args!(
                        "xHCI: slot {} address {} configured (kbd={}, mouse={}, hub-ports={}, storage={})",
                        device.slot_id,
                        device.address,
                        device.keyboard_online,
                        device.mouse_online,
                        device.hub_ports,
                        device.mass_storage_online
                    ));
                    let device_index = self.device_count;
                    self.devices[device_index] = device;
                    self.runtimes[device_index] = runtime;
                    self.device_count += 1;
                    if runtime.hub_ports != 0 {
                        self.enumerate_hub_children(device_index, allocator, hhdm_offset);
                    }
                }
                Err(error) => {
                    usb_log(format_args!(
                        "xHCI: root port {} enumeration failed: {error:?}",
                        port.number
                    ));
                    self.enumeration_failures = self.enumeration_failures.saturating_add(1);
                    self.last_enumeration_error = Some(error);
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn enumerate_device(
        &mut self,
        port: RootPortInfo,
        topology: DeviceTopology,
        allocator: &mut FrameAllocator,
        hhdm_offset: u64,
    ) -> Result<(UsbDeviceInfo, DeviceRuntime), XhciError> {
        let enable_slot = self.submit_command(Trb::command(TrbType::EnableSlotCommand))?;
        let slot_id = enable_slot.slot_id();
        if slot_id == 0 {
            return Err(XhciError::CommandFailed(enable_slot.completion_code()));
        }
        usb_log(format_args!(
            "xHCI: slot {slot_id} enabled (route={:#x}, root={}, parent={}:{})",
            topology.route_string,
            topology.root_port,
            topology.parent_hub_slot,
            topology.parent_port
        ));

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
            (topology.route_string & 0x000f_ffff)
                | (u32::from(speed_id(port.speed)) << 20)
                | (1 << 27),
        );
        write_memory32(slot_context + 4, u32::from(topology.root_port) << 16);
        if topology.parent_hub_slot != 0 {
            write_memory32(
                slot_context + 8,
                u32::from(topology.parent_hub_slot) | (u32::from(topology.parent_port) << 8),
            );
        }
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

        usb_log(format_args!("xHCI: address slot {slot_id}"));
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
        usb_log(format_args!(
            "xHCI: slot {slot_id} assigned address {address}"
        ));

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
        usb_log(format_args!(
            "xHCI: slot {slot_id} descriptor {:04x}:{:04x}",
            descriptor.vendor_id, descriptor.product_id
        ));

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
        let bindings = classify_configuration(configuration_bytes)
            .map_err(|_| XhciError::InvalidDescriptor)?;
        self.control_no_data(
            slot_id,
            &mut pipe,
            0,
            9,
            u16::from(configuration.configuration_value),
            0,
        )?;

        let hub_ports = if bindings.hub.is_some() {
            let descriptor_type: u8 = if matches!(port.speed, UsbSpeed::Super | UsbSpeed::SuperPlus)
            {
                0x2a
            } else {
                0x29
            };
            self.control_in(
                slot_id,
                &mut pipe,
                0xa0,
                6,
                u16::from(descriptor_type) << 8,
                0,
                12,
            )?;
            // SAFETY: The bounded class request completed into the pipe's
            // exclusive DMA page.
            let hub_bytes =
                unsafe { core::slice::from_raw_parts(pipe.buffer_virtual as *const u8, 12) };
            HubDescriptor::parse(hub_bytes)
                .map_err(|_| XhciError::InvalidDescriptor)?
                .ports
        } else {
            0
        };

        let mut runtime = DeviceRuntime {
            active: true,
            slot_id,
            root_port: topology.root_port,
            parent_hub_slot: topology.parent_hub_slot,
            parent_port: topology.parent_port,
            input_context_physical,
            input_context_virtual,
            control: pipe,
            ..DeviceRuntime::EMPTY
        };
        self.configure_class_endpoints(
            &mut runtime,
            bindings,
            port.speed,
            hub_ports,
            allocator,
            hhdm_offset,
        )?;
        usb_log(format_args!(
            "xHCI: slot {slot_id} configured {} class endpoint(s)",
            runtime.endpoint_count
        ));

        if let Some(keyboard) = bindings.keyboard {
            self.control_no_data(
                slot_id,
                &mut runtime.control,
                0x21,
                0x0b,
                0,
                u16::from(keyboard.interface_number),
            )?;
        }
        if let Some(mouse) = bindings.mouse {
            self.control_no_data(
                slot_id,
                &mut runtime.control,
                0x21,
                0x0b,
                0,
                u16::from(mouse.interface_number),
            )?;
        }

        let mut mass_storage_online = false;
        if runtime.mass_storage.is_some() {
            match self.initialize_mass_storage(&mut runtime) {
                Ok(()) => mass_storage_online = true,
                Err(error) => {
                    runtime.mass_storage = None;
                    self.last_enumeration_error = Some(error);
                }
            }
        }

        Ok((
            UsbDeviceInfo {
                slot_id,
                address,
                root_port: topology.root_port,
                speed: port.speed,
                vendor_id: descriptor.vendor_id,
                product_id: descriptor.product_id,
                usb_version: descriptor.usb_version,
                device_class: descriptor.device_class,
                device_subclass: descriptor.device_subclass,
                device_protocol: descriptor.device_protocol,
                configuration_value: configuration.configuration_value,
                summary,
                connected: true,
                keyboard_online: runtime.keyboard_endpoint.is_some(),
                mouse_online: runtime.mouse_endpoint.is_some(),
                hub_ports,
                mass_storage_online,
            },
            runtime,
        ))
    }

    #[allow(clippy::too_many_lines)]
    fn configure_class_endpoints(
        &mut self,
        runtime: &mut DeviceRuntime,
        bindings: ClassBindings,
        speed: UsbSpeed,
        hub_ports: u8,
        allocator: &mut FrameAllocator,
        hhdm_offset: u64,
    ) -> Result<(), XhciError> {
        if let Some(binding) = bindings.keyboard {
            let pipe =
                self.add_endpoint(runtime, binding.descriptor, speed, allocator, hhdm_offset)?;
            runtime.keyboard_endpoint = Some(pipe);
        }
        if let Some(binding) = bindings.mouse {
            runtime.mouse_endpoint = Some(self.add_endpoint(
                runtime,
                binding.descriptor,
                speed,
                allocator,
                hhdm_offset,
            )?);
        }
        if let Some(binding) = bindings.hub {
            runtime.hub_endpoint = Some(self.add_endpoint(
                runtime,
                binding.descriptor,
                speed,
                allocator,
                hhdm_offset,
            )?);
            runtime.hub_ports = hub_ports;
        }
        if let Some(binding) = bindings.mass_storage {
            let bulk_in = binding.bulk_in.ok_or(XhciError::InvalidEndpoint)?;
            let bulk_out = binding.bulk_out.ok_or(XhciError::InvalidEndpoint)?;
            let bulk_in = self.add_endpoint(runtime, bulk_in, speed, allocator, hhdm_offset)?;
            let bulk_out = self.add_endpoint(runtime, bulk_out, speed, allocator, hhdm_offset)?;
            runtime.mass_storage = Some(MassStorageState {
                interface_number: binding.interface_number,
                bulk_in,
                bulk_out,
                ..MassStorageState::EMPTY
            });
        }
        if runtime.endpoint_count == 0 {
            return Ok(());
        }

        let context_stride = u64::from(self.context_bytes);
        let slot_context = runtime.input_context_virtual + context_stride;
        let mut slot_dword0 = read_memory32(slot_context);
        let highest_dci = runtime.endpoints[..runtime.endpoint_count]
            .iter()
            .map(|pipe| pipe.dci)
            .max()
            .ok_or(XhciError::InvalidEndpoint)?;
        slot_dword0 = (slot_dword0 & !(0x1f << 27)) | (u32::from(highest_dci) << 27);
        if hub_ports != 0 {
            slot_dword0 |= 1 << 26;
            let dword1 = read_memory32(slot_context + 4);
            write_memory32(
                slot_context + 4,
                (dword1 & !(0xff << 24)) | (u32::from(hub_ports) << 24),
            );
        }
        write_memory32(slot_context, slot_dword0);

        let mut add_flags = 1_u32;
        for pipe in &runtime.endpoints[..runtime.endpoint_count] {
            add_flags |= 1_u32 << pipe.dci;
        }
        write_memory32(runtime.input_context_virtual, 0);
        write_memory32(runtime.input_context_virtual + 4, add_flags);
        let event = self.submit_command(
            Trb::command(TrbType::ConfigureEndpointCommand)
                .with_parameter(runtime.input_context_physical)
                .with_control_bits(u32::from(runtime.slot_id) << 24),
        )?;
        if event.slot_id() != runtime.slot_id {
            return Err(XhciError::CommandFailed(event.completion_code()));
        }
        Ok(())
    }

    fn add_endpoint(
        &self,
        runtime: &mut DeviceRuntime,
        descriptor: EndpointDescriptor,
        speed: UsbSpeed,
        allocator: &mut FrameAllocator,
        hhdm_offset: u64,
    ) -> Result<u8, XhciError> {
        let dci = endpoint_dci(descriptor)?;
        if let Some((index, _)) = runtime.endpoints[..runtime.endpoint_count]
            .iter()
            .enumerate()
            .find(|(_, pipe)| pipe.dci == dci)
        {
            return u8::try_from(index).map_err(|_| XhciError::EndpointLimit);
        }
        if runtime.endpoint_count == MAX_CLASS_ENDPOINTS {
            return Err(XhciError::EndpointLimit);
        }
        let ring_physical = allocate_zeroed_frame(allocator, hhdm_offset)?;
        let buffer_physical = allocate_zeroed_frame(allocator, hhdm_offset)?;
        if !self.supports_64_bit
            && (ring_physical > u64::from(u32::MAX) || buffer_physical > u64::from(u32::MAX))
        {
            return Err(XhciError::AddressAboveFourGib);
        }
        let ring_virtual = hhdm_offset
            .checked_add(ring_physical)
            .ok_or(XhciError::OutOfFrames)?;
        let buffer_virtual = hhdm_offset
            .checked_add(buffer_physical)
            .ok_or(XhciError::OutOfFrames)?;
        write_trb(
            ring_virtual + ((TRBS_PER_PAGE - 1) * 16) as u64,
            Trb::command(TrbType::Link)
                .with_parameter(ring_physical)
                .with_control_bits(1 | (1 << 1)),
        );

        let pipe = EndpointPipe {
            dci,
            ring_physical,
            ring_virtual,
            enqueue: 0,
            cycle: true,
            buffer_physical,
            buffer_virtual,
            max_packet_size: descriptor.max_packet_size,
            pending: false,
            pending_trb: 0,
            pending_length: 0,
            completed: false,
            completion_code: 0,
            residual: 0,
        };
        let endpoint_context =
            runtime.input_context_virtual + u64::from(dci + 1) * u64::from(self.context_bytes);
        write_memory32(
            endpoint_context,
            u32::from(endpoint_interval(speed, descriptor.interval)) << 16,
        );
        write_memory32(
            endpoint_context + 4,
            (3 << 1)
                | (u32::from(endpoint_type(descriptor)?) << 3)
                | (u32::from(descriptor.max_packet_size) << 16),
        );
        write_memory64(endpoint_context + 8, ring_physical | 1);
        write_memory32(endpoint_context + 16, u32::from(descriptor.max_packet_size));

        let index = runtime.endpoint_count;
        runtime.endpoints[index] = pipe;
        runtime.endpoint_count += 1;
        u8::try_from(index).map_err(|_| XhciError::EndpointLimit)
    }

    fn enumerate_hub_children(
        &mut self,
        hub_index: usize,
        allocator: &mut FrameAllocator,
        hhdm_offset: u64,
    ) {
        let mut hub = self.runtimes[hub_index];
        usb_log(format_args!(
            "xHCI: scan {} downstream ports on hub slot {}",
            hub.hub_ports, hub.slot_id
        ));
        for downstream_port in HubPortIterator::new(hub.hub_ports) {
            if downstream_port > 15 || self.device_count == MAX_USB_DEVICES {
                break;
            }
            let _ = self.control_no_data(
                hub.slot_id,
                &mut hub.control,
                0x23,
                3,
                8,
                u16::from(downstream_port),
            );
        }
        for _ in 0..200_000 {
            spin_loop();
        }
        for downstream_port in HubPortIterator::new(hub.hub_ports) {
            if downstream_port > 15 || self.device_count == MAX_USB_DEVICES {
                break;
            }
            if self
                .control_in(
                    hub.slot_id,
                    &mut hub.control,
                    0xa3,
                    0,
                    0,
                    u16::from(downstream_port),
                    4,
                )
                .is_err()
            {
                continue;
            }
            // SAFETY: GET_STATUS completed into the hub's exclusive buffer.
            let bytes =
                unsafe { core::slice::from_raw_parts(hub.control.buffer_virtual as *const u8, 4) };
            let Ok(mut status) = HubPortStatus::parse(bytes) else {
                continue;
            };
            if !status.connected {
                continue;
            }
            let _ = self.control_no_data(
                hub.slot_id,
                &mut hub.control,
                0x23,
                3,
                4,
                u16::from(downstream_port),
            );
            for _ in 0..64 {
                if self
                    .control_in(
                        hub.slot_id,
                        &mut hub.control,
                        0xa3,
                        0,
                        0,
                        u16::from(downstream_port),
                        4,
                    )
                    .is_err()
                {
                    break;
                }
                // SAFETY: GET_STATUS completed into the hub buffer.
                let bytes = unsafe {
                    core::slice::from_raw_parts(hub.control.buffer_virtual as *const u8, 4)
                };
                let Ok(updated) = HubPortStatus::parse(bytes) else {
                    break;
                };
                status = updated;
                if status.enabled && !status.resetting {
                    break;
                }
                spin_loop();
            }
            if !status.enabled {
                continue;
            }
            let speed = if status.low_speed {
                UsbSpeed::Low
            } else if status.high_speed {
                UsbSpeed::High
            } else {
                UsbSpeed::Full
            };
            let port = RootPortInfo {
                number: hub.root_port,
                connected: true,
                enabled: true,
                powered: status.powered,
                link_state: 0,
                speed,
            };
            let topology = DeviceTopology {
                root_port: hub.root_port,
                route_string: u32::from(downstream_port),
                parent_hub_slot: hub.slot_id,
                parent_port: downstream_port,
            };
            self.enumeration_attempts = self.enumeration_attempts.saturating_add(1);
            usb_log(format_args!(
                "xHCI: enumerate hub slot {} port {} ({speed:?})",
                hub.slot_id, downstream_port
            ));
            match self.enumerate_device(port, topology, allocator, hhdm_offset) {
                Ok((device, runtime)) => {
                    usb_log(format_args!(
                        "xHCI: downstream slot {} address {} configured",
                        device.slot_id, device.address
                    ));
                    self.devices[self.device_count] = device;
                    self.runtimes[self.device_count] = runtime;
                    self.device_count += 1;
                }
                Err(error) => {
                    usb_log(format_args!(
                        "xHCI: hub port {} enumeration failed: {error:?}",
                        downstream_port
                    ));
                    self.enumeration_failures = self.enumeration_failures.saturating_add(1);
                    self.last_enumeration_error = Some(error);
                }
            }
        }
        self.runtimes[hub_index] = hub;
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

    fn initialize_mass_storage(&mut self, runtime: &mut DeviceRuntime) -> Result<(), XhciError> {
        let interface_number = runtime
            .mass_storage
            .ok_or(XhciError::InvalidEndpoint)?
            .interface_number;
        if self
            .control_in(
                runtime.slot_id,
                &mut runtime.control,
                0xa1,
                0xfe,
                0,
                u16::from(interface_number),
                1,
            )
            .is_ok()
        {
            // NexOS v1 intentionally addresses LUN zero. Reading GET_MAX_LUN
            // still validates the BOT class control path.
            let _maximum_lun = read8(runtime.control.buffer_virtual);
        }
        let mut inquiry = [0_u8; 36];
        self.bot_command(
            runtime,
            ScsiCommand::inquiry(inquiry.len() as u8),
            Some(&mut inquiry),
            None,
        )?;
        let mut capacity_bytes = [0_u8; 8];
        self.bot_command(
            runtime,
            ScsiCommand::read_capacity_10(),
            Some(&mut capacity_bytes),
            None,
        )?;
        let capacity =
            Capacity10::parse(&capacity_bytes).map_err(|_| XhciError::UnsupportedCapacity)?;
        if capacity.block_size as usize > PAGE_BYTES || capacity.block_size < 512 {
            return Err(XhciError::UnsupportedCapacity);
        }
        let mass_storage = runtime
            .mass_storage
            .as_mut()
            .ok_or(XhciError::InvalidEndpoint)?;
        mass_storage.sector_size = capacity.block_size;
        mass_storage.sector_count = capacity.block_count();
        mass_storage.model.fill(b' ');
        mass_storage.model[..8].copy_from_slice(&inquiry[8..16]);
        mass_storage.model[9..25].copy_from_slice(&inquiry[16..32]);
        mass_storage.model[26..30].copy_from_slice(&inquiry[32..36]);
        mass_storage.online = true;
        Ok(())
    }

    fn bot_command(
        &mut self,
        runtime: &mut DeviceRuntime,
        command: ScsiCommand,
        mut data_in: Option<&mut [u8]>,
        data_out: Option<&[u8]>,
    ) -> Result<(), XhciError> {
        let mut state = runtime.mass_storage.ok_or(XhciError::InvalidEndpoint)?;
        let tag = state.next_tag;
        state.next_tag = state.next_tag.wrapping_add(1).max(1);
        runtime.mass_storage = Some(state);

        let wrapper = CommandBlockWrapper::from_scsi(tag, state.logical_unit, command);
        let mut cbw = [0_u8; CBW_BYTES];
        wrapper
            .encode(&mut cbw)
            .map_err(|_| XhciError::BotProtocol)?;
        self.endpoint_out(runtime.slot_id, runtime, state.bulk_out, &cbw)?;
        match command.direction() {
            DataDirection::In => {
                let output = data_in.as_deref_mut().ok_or(XhciError::BotProtocol)?;
                if output.len() != command.transfer_bytes() as usize {
                    return Err(XhciError::BotProtocol);
                }
                self.endpoint_in(runtime.slot_id, runtime, state.bulk_in, output)?;
            }
            DataDirection::Out => {
                let input = data_out.ok_or(XhciError::BotProtocol)?;
                if input.len() != command.transfer_bytes() as usize {
                    return Err(XhciError::BotProtocol);
                }
                self.endpoint_out(runtime.slot_id, runtime, state.bulk_out, input)?;
            }
            DataDirection::None => {}
        }
        let mut csw = [0_u8; CSW_BYTES];
        self.endpoint_in(runtime.slot_id, runtime, state.bulk_in, &mut csw)?;
        let status = CommandStatusWrapper::parse(&csw, tag).map_err(|_| XhciError::BotProtocol)?;
        self.storage_commands = self.storage_commands.saturating_add(1);
        if status.status == CommandStatus::Passed && status.residue == 0 {
            Ok(())
        } else {
            Err(XhciError::BotProtocol)
        }
    }

    fn endpoint_in(
        &mut self,
        slot_id: u8,
        runtime: &mut DeviceRuntime,
        endpoint_index: u8,
        output: &mut [u8],
    ) -> Result<(), XhciError> {
        if output.len() > PAGE_BYTES {
            return Err(XhciError::TransferTooLarge);
        }
        let index = usize::from(endpoint_index);
        let mut pipe = *runtime
            .endpoints
            .get(index)
            .filter(|pipe| pipe.dci != 0)
            .ok_or(XhciError::InvalidEndpoint)?;
        if pipe.max_packet_size == 0 {
            return Err(XhciError::InvalidEndpoint);
        }
        // SAFETY: The endpoint pipe owns this DMA buffer page.
        unsafe { core::ptr::write_bytes(pipe.buffer_virtual as *mut u8, 0, output.len()) };
        let buffer_physical = pipe.buffer_physical;
        let transfer = Self::push_endpoint(
            &mut pipe,
            Trb::command(TrbType::Normal)
                .with_parameter(buffer_physical)
                .with_status(u32::try_from(output.len()).map_err(|_| XhciError::TransferTooLarge)?)
                .with_control_bits((1 << 5) | (1 << 2)),
        );
        self.ring_endpoint_doorbell(slot_id, pipe.dci);
        let event = self.wait_transfer_event(slot_id, transfer)?;
        let residual = usize::try_from(event.status & 0x00ff_ffff).unwrap_or(usize::MAX);
        let transferred = output.len().saturating_sub(residual.min(output.len()));
        // SAFETY: The completed transfer initialized `transferred` bytes.
        let bytes =
            unsafe { core::slice::from_raw_parts(pipe.buffer_virtual as *const u8, transferred) };
        output[..transferred].copy_from_slice(bytes);
        output[transferred..].fill(0);
        runtime.endpoints[index] = pipe;
        Ok(())
    }

    fn endpoint_out(
        &mut self,
        slot_id: u8,
        runtime: &mut DeviceRuntime,
        endpoint_index: u8,
        input: &[u8],
    ) -> Result<(), XhciError> {
        if input.len() > PAGE_BYTES {
            return Err(XhciError::TransferTooLarge);
        }
        let index = usize::from(endpoint_index);
        let mut pipe = *runtime
            .endpoints
            .get(index)
            .filter(|pipe| pipe.dci != 0)
            .ok_or(XhciError::InvalidEndpoint)?;
        if pipe.max_packet_size == 0 {
            return Err(XhciError::InvalidEndpoint);
        }
        // SAFETY: The endpoint pipe owns this DMA buffer page.
        unsafe {
            core::ptr::copy_nonoverlapping(
                input.as_ptr(),
                pipe.buffer_virtual as *mut u8,
                input.len(),
            );
        }
        let buffer_physical = pipe.buffer_physical;
        let transfer = Self::push_endpoint(
            &mut pipe,
            Trb::command(TrbType::Normal)
                .with_parameter(buffer_physical)
                .with_status(u32::try_from(input.len()).map_err(|_| XhciError::TransferTooLarge)?)
                .with_control_bits(1 << 5),
        );
        self.ring_endpoint_doorbell(slot_id, pipe.dci);
        self.wait_transfer(slot_id, transfer)?;
        runtime.endpoints[index] = pipe;
        Ok(())
    }

    fn push_endpoint(pipe: &mut EndpointPipe, mut trb: Trb) -> u64 {
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

    fn ring_endpoint_doorbell(&self, slot_id: u8, dci: u8) {
        compiler_fence(Ordering::SeqCst);
        write32(self.doorbells + u64::from(slot_id) * 4, u32::from(dci));
    }

    fn wait_transfer(&mut self, slot_id: u8, expected_trb: u64) -> Result<(), XhciError> {
        self.wait_transfer_event(slot_id, expected_trb).map(drop)
    }

    fn wait_transfer_event(&mut self, slot_id: u8, expected_trb: u64) -> Result<Trb, XhciError> {
        for _ in 0..POLL_LIMIT {
            if let Some(event) = self.next_event() {
                if event.trb_type() == Some(TrbType::TransferEvent)
                    && event.slot_id() == slot_id
                    && event.parameter() & !0xf == expected_trb
                {
                    let completion = event.completion_code();
                    return if completion == 1 || completion == 13 {
                        Ok(event)
                    } else {
                        Err(XhciError::TransferFailed(completion))
                    };
                }
                self.record_endpoint_completion(event);
            }
            spin_loop();
        }
        Err(XhciError::TransferTimeout)
    }

    pub fn poll_input(&mut self) -> Option<UsbInputEvent> {
        while let Some(event) = self.next_event() {
            self.record_endpoint_completion(event);
        }
        for device_index in 0..self.device_count {
            let mut runtime = self.runtimes[device_index];
            if !runtime.active {
                continue;
            }
            if let Some(endpoint) = runtime.keyboard_endpoint {
                let mut report = [0_u8; 8];
                if self
                    .poll_interrupt_report(&mut runtime, endpoint, &mut report)
                    .is_some()
                    && let Ok(Some(event)) = runtime.keyboard.update(&report)
                {
                    self.runtimes[device_index] = runtime;
                    self.input_events = self.input_events.saturating_add(1);
                    return Some(UsbInputEvent::Keyboard(event));
                }
            }
            if let Some(endpoint) = runtime.mouse_endpoint {
                let mut report = [0_u8; 4];
                if self
                    .poll_interrupt_report(&mut runtime, endpoint, &mut report)
                    .is_some()
                    && let Ok(event) = MouseReport::parse(&report)
                    && (event.delta_x != 0
                        || event.delta_y != 0
                        || event.wheel != 0
                        || event.buttons != runtime.mouse_previous.buttons)
                {
                    runtime.mouse_previous = event;
                    self.runtimes[device_index] = runtime;
                    self.input_events = self.input_events.saturating_add(1);
                    return Some(UsbInputEvent::Mouse(event));
                }
            }
            if let Some(endpoint) = runtime.hub_endpoint {
                let mut changes = [0_u8; 8];
                let bytes = (usize::from(runtime.hub_ports) + 2).div_ceil(8);
                let length = bytes.min(changes.len());
                if self
                    .poll_interrupt_report(&mut runtime, endpoint, &mut changes[..length])
                    .is_some()
                    && changes.iter().any(|byte| *byte != 0)
                {
                    self.refresh_hub_child_connections(&mut runtime);
                }
            }
            self.runtimes[device_index] = runtime;
        }
        None
    }

    fn poll_interrupt_report(
        &self,
        runtime: &mut DeviceRuntime,
        endpoint_index: u8,
        output: &mut [u8],
    ) -> Option<usize> {
        let index = usize::from(endpoint_index);
        let mut pipe = *runtime.endpoints.get(index).filter(|pipe| pipe.dci != 0)?;
        let mut transferred = None;
        if pipe.completed {
            let length = usize::from(pipe.pending_length);
            if matches!(pipe.completion_code, 1 | 13) {
                let bytes = length.saturating_sub(
                    usize::try_from(pipe.residual)
                        .unwrap_or(usize::MAX)
                        .min(length),
                );
                let bytes = bytes.min(output.len());
                // SAFETY: The completed transfer initialized `bytes` bytes in
                // this endpoint's exclusive DMA buffer.
                let source =
                    unsafe { core::slice::from_raw_parts(pipe.buffer_virtual as *const u8, bytes) };
                output[..bytes].copy_from_slice(source);
                output[bytes..].fill(0);
                transferred = Some(bytes);
            }
            pipe.completed = false;
        }
        if !pipe.pending {
            let length = output.len().min(PAGE_BYTES);
            // SAFETY: The endpoint owns this DMA buffer page.
            unsafe { core::ptr::write_bytes(pipe.buffer_virtual as *mut u8, 0, length) };
            let buffer_physical = pipe.buffer_physical;
            let transfer = Self::push_endpoint(
                &mut pipe,
                Trb::command(TrbType::Normal)
                    .with_parameter(buffer_physical)
                    .with_status(u32::try_from(length).ok()?)
                    .with_control_bits((1 << 5) | (1 << 2)),
            );
            pipe.pending = true;
            pipe.pending_trb = transfer;
            pipe.pending_length = u16::try_from(length).ok()?;
            self.ring_endpoint_doorbell(runtime.slot_id, pipe.dci);
        }
        runtime.endpoints[index] = pipe;
        transferred
    }

    fn record_endpoint_completion(&mut self, event: Trb) {
        if event.trb_type() != Some(TrbType::TransferEvent) {
            return;
        }
        let pointer = event.parameter() & !0xf;
        for runtime in self.runtimes[..self.device_count].iter_mut() {
            if !runtime.active || runtime.slot_id != event.slot_id() {
                continue;
            }
            for pipe in &mut runtime.endpoints[..runtime.endpoint_count] {
                if pipe.pending && pipe.pending_trb == pointer {
                    pipe.pending = false;
                    pipe.completed = true;
                    pipe.completion_code = event.completion_code();
                    pipe.residual = event.status & 0x00ff_ffff;
                    return;
                }
            }
        }
    }

    fn refresh_hub_child_connections(&mut self, hub: &mut DeviceRuntime) {
        for downstream_port in HubPortIterator::new(hub.hub_ports) {
            if self
                .control_in(
                    hub.slot_id,
                    &mut hub.control,
                    0xa3,
                    0,
                    0,
                    u16::from(downstream_port),
                    4,
                )
                .is_err()
            {
                continue;
            }
            // SAFETY: GET_STATUS completed into the hub control buffer.
            let bytes =
                unsafe { core::slice::from_raw_parts(hub.control.buffer_virtual as *const u8, 4) };
            let Ok(status) = HubPortStatus::parse(bytes) else {
                continue;
            };
            if status.connected {
                continue;
            }
            for child_index in 0..self.device_count {
                let child = &mut self.runtimes[child_index];
                if !child.active
                    || child.parent_hub_slot != hub.slot_id
                    || child.parent_port != downstream_port
                {
                    continue;
                }
                child.active = false;
                self.devices[child_index].connected = false;
                self.devices[child_index].mass_storage_online = false;
                self.disconnects = self.disconnects.saturating_add(1);
                let slot_id = child.slot_id;
                let _ = self.submit_command(
                    Trb::command(TrbType::DisableSlotCommand)
                        .with_control_bits(u32::from(slot_id) << 24),
                );
                break;
            }
        }
    }

    fn mass_storage_snapshot(&self, device_index: usize) -> Option<MassStorageState> {
        let runtime = self.runtimes.get(device_index)?;
        if !runtime.active {
            return None;
        }
        runtime.mass_storage.filter(|state| state.online)
    }

    fn mass_read(
        &mut self,
        device_index: usize,
        lba: u64,
        output: &mut [u8],
    ) -> Result<(), StorageError> {
        let mut runtime = *self
            .runtimes
            .get(device_index)
            .filter(|runtime| runtime.active)
            .ok_or(StorageError::Device)?;
        let state = runtime
            .mass_storage
            .filter(|state| state.online)
            .ok_or(StorageError::Device)?;
        let result = (|| {
            validate_mass_range(state, lba, output.len())?;
            let sector_size = state.sector_size as usize;
            let maximum_blocks = (PAGE_BYTES / sector_size).min(usize::from(u16::MAX));
            let mut completed = 0;
            while completed < output.len() {
                let bytes = (output.len() - completed).min(maximum_blocks * sector_size);
                let blocks =
                    u16::try_from(bytes / sector_size).map_err(|_| StorageError::InvalidBuffer)?;
                let command_lba = lba
                    .checked_add((completed / sector_size) as u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(StorageError::OutOfBounds)?;
                let command = ScsiCommand::read_10(command_lba, blocks, state.sector_size)
                    .ok_or(StorageError::InvalidBuffer)?;
                self.bot_command(
                    &mut runtime,
                    command,
                    Some(&mut output[completed..completed + bytes]),
                    None,
                )
                .map_err(storage_error)?;
                completed += bytes;
            }
            Ok(())
        })();
        self.runtimes[device_index] = runtime;
        result
    }

    fn mass_write(
        &mut self,
        device_index: usize,
        lba: u64,
        input: &[u8],
    ) -> Result<(), StorageError> {
        let mut runtime = *self
            .runtimes
            .get(device_index)
            .filter(|runtime| runtime.active)
            .ok_or(StorageError::Device)?;
        let state = runtime
            .mass_storage
            .filter(|state| state.online)
            .ok_or(StorageError::Device)?;
        let result = (|| {
            validate_mass_range(state, lba, input.len())?;
            let sector_size = state.sector_size as usize;
            let maximum_blocks = (PAGE_BYTES / sector_size).min(usize::from(u16::MAX));
            let mut completed = 0;
            while completed < input.len() {
                let bytes = (input.len() - completed).min(maximum_blocks * sector_size);
                let blocks =
                    u16::try_from(bytes / sector_size).map_err(|_| StorageError::InvalidBuffer)?;
                let command_lba = lba
                    .checked_add((completed / sector_size) as u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(StorageError::OutOfBounds)?;
                let command = ScsiCommand::write_10(command_lba, blocks, state.sector_size)
                    .ok_or(StorageError::InvalidBuffer)?;
                self.bot_command(
                    &mut runtime,
                    command,
                    None,
                    Some(&input[completed..completed + bytes]),
                )
                .map_err(storage_error)?;
                completed += bytes;
            }
            Ok(())
        })();
        self.runtimes[device_index] = runtime;
        result
    }

    fn mass_flush(&mut self, device_index: usize) -> Result<(), StorageError> {
        let mut runtime = *self
            .runtimes
            .get(device_index)
            .filter(|runtime| runtime.active)
            .ok_or(StorageError::Device)?;
        if runtime.mass_storage.is_none_or(|state| !state.online) {
            return Err(StorageError::Device);
        }
        let result = self
            .bot_command(
                &mut runtime,
                ScsiCommand::synchronize_cache_10(),
                None,
                None,
            )
            .map_err(storage_error);
        self.runtimes[device_index] = runtime;
        result
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
            if let Some(event) = self.next_event() {
                if event.trb_type() == Some(TrbType::CommandCompletionEvent)
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
                self.record_endpoint_completion(event);
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

#[derive(Clone, Copy)]
pub struct UsbMassStorageDevice {
    controller: *mut XhciController,
    device_index: usize,
    controller_index: u8,
    slot_id: u8,
    root_port: u8,
    sector_size: u32,
    sector_count: u64,
    model: [u8; 40],
}

impl UsbMassStorageDevice {
    #[must_use]
    pub const fn model_bytes(&self) -> &[u8] {
        &self.model
    }

    #[must_use]
    pub const fn controller_index(self) -> u8 {
        self.controller_index
    }

    #[must_use]
    pub const fn slot_id(self) -> u8 {
        self.slot_id
    }

    #[must_use]
    pub const fn root_port(self) -> u8 {
        self.root_port
    }
}

impl BlockDevice for UsbMassStorageDevice {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn sector_count(&self) -> u64 {
        self.sector_count
    }

    fn read_sectors(&mut self, lba: u64, output: &mut [u8]) -> Result<(), StorageError> {
        // SAFETY: XhciManager stores controllers in Boxes for their entire
        // lifetime. The kernel is single-threaded and serializes monitor I/O.
        unsafe { (&mut *self.controller).mass_read(self.device_index, lba, output) }
    }

    fn write_sectors(&mut self, lba: u64, input: &[u8]) -> Result<(), StorageError> {
        // SAFETY: See `read_sectors`; all access is serialized by the monitor.
        unsafe { (&mut *self.controller).mass_write(self.device_index, lba, input) }
    }

    fn flush(&mut self) -> Result<(), StorageError> {
        // SAFETY: See `read_sectors`; the boxed controller address is stable.
        unsafe { (&mut *self.controller).mass_flush(self.device_index) }
    }
}

pub struct XhciManager {
    controllers: [Option<Box<XhciController>>; MAX_CONTROLLERS],
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
            controllers: [const { None }; MAX_CONTROLLERS],
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
                    manager.stats.hid_keyboards =
                        manager
                            .stats
                            .hid_keyboards
                            .saturating_add(count_devices(&controller, |device| {
                                device.keyboard_online
                            }));
                    manager.stats.hid_mice = manager
                        .stats
                        .hid_mice
                        .saturating_add(count_devices(&controller, |device| device.mouse_online));
                    manager.stats.hubs = manager
                        .stats
                        .hubs
                        .saturating_add(count_devices(&controller, |device| device.hub_ports != 0));
                    manager.stats.mass_storage_devices = manager
                        .stats
                        .mass_storage_devices
                        .saturating_add(count_devices(&controller, |device| {
                            device.mass_storage_online
                        }));
                    manager.controllers[manager.count] = Some(Box::new(controller));
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
        self.refresh_dynamic_stats();
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

    pub fn poll_input(&mut self) -> Option<UsbInputEvent> {
        for controller in self.controllers.iter_mut().flatten() {
            if let Some(event) = controller.poll_input() {
                self.refresh_dynamic_stats();
                return Some(event);
            }
        }
        self.refresh_dynamic_stats();
        None
    }

    pub fn mass_storage_devices(
        &mut self,
        output: &mut [Option<UsbMassStorageDevice>; MAX_USB_STORAGE_DEVICES],
    ) -> usize {
        output.fill(None);
        let mut count = 0;
        for (controller_index, controller) in self.controllers.iter_mut().flatten().enumerate() {
            let controller_pointer = controller.as_mut() as *mut XhciController;
            for device_index in 0..controller.device_count {
                if count == output.len() {
                    return count;
                }
                let Some(storage) = controller.mass_storage_snapshot(device_index) else {
                    continue;
                };
                let runtime = controller.runtimes[device_index];
                output[count] = Some(UsbMassStorageDevice {
                    controller: controller_pointer,
                    device_index,
                    controller_index: u8::try_from(controller_index).unwrap_or(u8::MAX),
                    slot_id: runtime.slot_id,
                    root_port: runtime.root_port,
                    sector_size: storage.sector_size,
                    sector_count: storage.sector_count,
                    model: storage.model,
                });
                count += 1;
            }
        }
        count
    }

    fn refresh_dynamic_stats(&mut self) {
        self.stats.hid_keyboards = 0;
        self.stats.hid_mice = 0;
        self.stats.hubs = 0;
        self.stats.mass_storage_devices = 0;
        self.stats.input_events = 0;
        self.stats.storage_commands = 0;
        self.stats.disconnects = 0;
        for controller in self.controllers.iter().flatten() {
            self.stats.hid_keyboards = self
                .stats
                .hid_keyboards
                .saturating_add(count_devices(controller, |device| device.keyboard_online));
            self.stats.hid_mice = self
                .stats
                .hid_mice
                .saturating_add(count_devices(controller, |device| device.mouse_online));
            self.stats.hubs = self
                .stats
                .hubs
                .saturating_add(count_devices(controller, |device| device.hub_ports != 0));
            self.stats.mass_storage_devices =
                self.stats
                    .mass_storage_devices
                    .saturating_add(count_devices(controller, |device| {
                        device.mass_storage_online
                    }));
            self.stats.input_events = self
                .stats
                .input_events
                .saturating_add(controller.input_events);
            self.stats.storage_commands = self
                .stats
                .storage_commands
                .saturating_add(controller.storage_commands);
            self.stats.disconnects = self
                .stats
                .disconnects
                .saturating_add(controller.disconnects);
        }
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
        self.controllers.get(index)?.as_deref()
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

fn usb_log(arguments: core::fmt::Arguments<'_>) {
    let mut serial = SerialPort::new(0x3f8);
    let _ = writeln!(serial, "{arguments}");
}

fn count_devices(controller: &XhciController, predicate: impl Fn(&UsbDeviceInfo) -> bool) -> u16 {
    u16::try_from(
        controller
            .devices()
            .iter()
            .filter(|device| device.connected && predicate(device))
            .count(),
    )
    .unwrap_or(u16::MAX)
}

fn validate_mass_range(
    state: MassStorageState,
    lba: u64,
    byte_length: usize,
) -> Result<(), StorageError> {
    let sector_size =
        usize::try_from(state.sector_size).map_err(|_| StorageError::InvalidBuffer)?;
    if byte_length == 0 || !byte_length.is_multiple_of(sector_size) {
        return Err(StorageError::InvalidBuffer);
    }
    let blocks = u64::try_from(byte_length / sector_size).map_err(|_| StorageError::OutOfBounds)?;
    if lba
        .checked_add(blocks)
        .is_none_or(|end| end > state.sector_count)
    {
        return Err(StorageError::OutOfBounds);
    }
    Ok(())
}

const fn storage_error(error: XhciError) -> StorageError {
    match error {
        XhciError::CommandTimeout
        | XhciError::TransferTimeout
        | XhciError::FirmwareOwnershipTimeout
        | XhciError::HaltTimeout
        | XhciError::ResetTimeout
        | XhciError::StartTimeout => StorageError::Timeout,
        XhciError::TransferTooLarge | XhciError::UnsupportedCapacity => {
            StorageError::UnsupportedSectorSize
        }
        _ => StorageError::Device,
    }
}

fn endpoint_dci(descriptor: EndpointDescriptor) -> Result<u8, XhciError> {
    if descriptor.number == 0 || descriptor.max_packet_size == 0 {
        return Err(XhciError::InvalidEndpoint);
    }
    let direction = u8::from(descriptor.direction == Direction::In);
    descriptor
        .number
        .checked_mul(2)
        .and_then(|value| value.checked_add(direction))
        .filter(|value| *value < 32)
        .ok_or(XhciError::InvalidEndpoint)
}

const fn endpoint_type(descriptor: EndpointDescriptor) -> Result<u8, XhciError> {
    match (descriptor.transfer_type, descriptor.direction) {
        (TransferType::Bulk, Direction::Out) => Ok(2),
        (TransferType::Interrupt, Direction::Out) => Ok(3),
        (TransferType::Bulk, Direction::In) => Ok(6),
        (TransferType::Interrupt, Direction::In) => Ok(7),
        (TransferType::Control | TransferType::Isochronous, _) => Err(XhciError::InvalidEndpoint),
    }
}

fn endpoint_interval(speed: UsbSpeed, interval: u8) -> u8 {
    let interval = interval.max(1);
    match speed {
        UsbSpeed::High | UsbSpeed::Super | UsbSpeed::SuperPlus => {
            interval.saturating_sub(1).min(15)
        }
        UsbSpeed::Low | UsbSpeed::Full | UsbSpeed::Unknown => {
            ceil_log2(interval).saturating_add(3).min(15)
        }
    }
}

fn ceil_log2(value: u8) -> u8 {
    let mut exponent = 0;
    let mut rounded = 1_u16;
    while rounded < value as u16 {
        rounded <<= 1;
        exponent += 1;
    }
    exponent
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
