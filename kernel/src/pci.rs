use core::arch::asm;

use crate::acpi::{McfgInfo, PlatformInfo};
use crate::memory::FrameAllocator;
use crate::paging::{PageMapError, PagingInfo};

const MAX_PCI_DEVICES: usize = 128;
const ECAM_SCRATCH_VIRTUAL: u64 = 0xffff_ff80_0000_3000;

#[derive(Clone, Copy)]
pub enum PciAccess {
    Ecam,
    Mechanism1,
}

impl PciAccess {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ecam => "ACPI ECAM",
            Self::Mechanism1 => "mechanism 1",
        }
    }
}

#[derive(Clone, Copy)]
pub struct PciBar {
    pub address: u64,
    pub is_io: bool,
}

#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub programming_interface: u8,
    pub revision: u8,
}

impl PciDevice {
    const EMPTY: Self = Self {
        bus: 0,
        device: 0,
        function: 0,
        vendor_id: 0,
        device_id: 0,
        class: 0,
        subclass: 0,
        programming_interface: 0,
        revision: 0,
    };

    #[must_use]
    pub const fn class_name(&self) -> &'static str {
        match self.class {
            0x01 => match self.subclass {
                0x01 => "IDE controller",
                0x06 => "SATA controller",
                0x08 => "NVMe controller",
                _ => "mass storage",
            },
            0x02 => "network controller",
            0x03 => "display controller",
            0x04 => "multimedia controller",
            0x06 => match self.subclass {
                0x00 => "host bridge",
                0x01 => "ISA bridge",
                0x04 => "PCI bridge",
                _ => "bridge",
            },
            0x0c => match self.subclass {
                0x03 => "USB controller",
                _ => "serial bus",
            },
            _ => "unknown device",
        }
    }

    #[must_use]
    pub fn bar(&self, index: u8) -> Option<PciBar> {
        if index >= 6 {
            return None;
        }
        let offset = 0x10_u8.checked_add(index.checked_mul(4)?)?;
        let low = read_config_u32(self.bus, self.device, self.function, offset);
        if low == 0 || low == u32::MAX {
            return None;
        }
        if low & 1 != 0 {
            return Some(PciBar {
                address: u64::from(low & !3),
                is_io: true,
            });
        }
        let memory_type = (low >> 1) & 3;
        let is_64_bit = memory_type == 2;
        let high = if is_64_bit {
            if index == 5 {
                return None;
            }
            read_config_u32(self.bus, self.device, self.function, offset.checked_add(4)?)
        } else {
            0
        };
        Some(PciBar {
            address: (u64::from(high) << 32) | u64::from(low & !0xf),
            is_io: false,
        })
    }

    pub fn enable_memory_bus_mastering(&self) {
        let command = read_config_u16(self.bus, self.device, self.function, 4);
        write_config_u16(
            self.bus,
            self.device,
            self.function,
            4,
            command | (1 << 1) | (1 << 2),
        );
    }

    pub fn enable_io_space(&self) {
        let command = read_config_u16(self.bus, self.device, self.function, 4);
        write_config_u16(self.bus, self.device, self.function, 4, command | 1);
    }
}

pub struct PciInventory {
    devices: [PciDevice; MAX_PCI_DEVICES],
    count: usize,
    truncated: bool,
    access: PciAccess,
}

impl PciInventory {
    #[must_use]
    pub fn scan(
        platform: Option<&PlatformInfo>,
        paging: &mut PagingInfo,
        allocator: &mut FrameAllocator,
    ) -> Self {
        let mut inventory = Self {
            devices: [PciDevice::EMPTY; MAX_PCI_DEVICES],
            count: 0,
            truncated: false,
            access: PciAccess::Mechanism1,
        };
        if let Some(mcfg) = platform.and_then(|info| info.mcfg)
            && mcfg.segment_group == 0
            && mcfg.start_bus <= mcfg.end_bus
            && inventory.scan_ecam(mcfg, paging, allocator).is_ok()
        {
            inventory.access = PciAccess::Ecam;
            return inventory;
        }
        inventory.count = 0;
        inventory.truncated = false;
        inventory.scan_mechanism1();
        inventory
    }

    fn scan_mechanism1(&mut self) {
        for bus_value in 0_u16..=255 {
            let bus = u8::try_from(bus_value).unwrap_or(0);
            for device in 0_u8..32 {
                let vendor = read_config_u16(bus, device, 0, 0);
                if vendor == 0xffff {
                    continue;
                }
                let header_type = read_config_u8(bus, device, 0, 0x0e);
                let function_count = if header_type & 0x80 != 0 { 8 } else { 1 };
                for function in 0_u8..function_count {
                    if read_config_u16(bus, device, function, 0) == 0xffff {
                        continue;
                    }
                    self.push(read_device(bus, device, function));
                }
            }
        }
    }

    fn scan_ecam(
        &mut self,
        mcfg: McfgInfo,
        paging: &mut PagingInfo,
        allocator: &mut FrameAllocator,
    ) -> Result<(), PageMapError> {
        for bus_value in u16::from(mcfg.start_bus)..=u16::from(mcfg.end_bus) {
            let bus = u8::try_from(bus_value).unwrap_or(mcfg.end_bus);
            for device in 0_u8..32 {
                let Some((function_zero, header_type)) =
                    read_ecam_device(mcfg, bus, device, 0, paging, allocator)?
                else {
                    continue;
                };
                self.push(function_zero);
                if header_type & 0x80 == 0 {
                    continue;
                }
                for function in 1_u8..8 {
                    if let Some((entry, _)) =
                        read_ecam_device(mcfg, bus, device, function, paging, allocator)?
                    {
                        self.push(entry);
                    }
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn devices(&self) -> &[PciDevice] {
        &self.devices[..self.count]
    }

    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    #[must_use]
    pub const fn access(&self) -> PciAccess {
        self.access
    }

    fn push(&mut self, device: PciDevice) {
        if self.count == MAX_PCI_DEVICES {
            self.truncated = true;
            return;
        }
        self.devices[self.count] = device;
        self.count += 1;
    }
}

fn read_ecam_device(
    mcfg: McfgInfo,
    bus: u8,
    device: u8,
    function: u8,
    paging: &mut PagingInfo,
    allocator: &mut FrameAllocator,
) -> Result<Option<(PciDevice, u8)>, PageMapError> {
    let physical = nexos_runtime::platform::ecam_function_address(
        mcfg.base_address,
        mcfg.start_bus,
        bus,
        device,
        function,
    )
    .ok_or(PageMapError::Unaligned)?;
    paging.map_mmio_page(ECAM_SCRATCH_VIRTUAL, physical, allocator)?;
    // SAFETY: The scratch page maps this function's 4 KiB ECAM configuration
    // space uncached for the duration of these volatile reads.
    let identity = unsafe { core::ptr::read_volatile(ECAM_SCRATCH_VIRTUAL as *const u32) };
    let result = if identity & 0xffff == 0xffff {
        None
    } else {
        // SAFETY: Offsets 0x08 and 0x0c are within the same mapped function.
        let class_data =
            unsafe { core::ptr::read_volatile((ECAM_SCRATCH_VIRTUAL + 0x08) as *const u32) };
        let header =
            unsafe { core::ptr::read_volatile((ECAM_SCRATCH_VIRTUAL + 0x0c) as *const u32) };
        let identity_bytes = identity.to_le_bytes();
        let class_bytes = class_data.to_le_bytes();
        Some((
            PciDevice {
                bus,
                device,
                function,
                vendor_id: u16::from_le_bytes([identity_bytes[0], identity_bytes[1]]),
                device_id: u16::from_le_bytes([identity_bytes[2], identity_bytes[3]]),
                class: class_bytes[3],
                subclass: class_bytes[2],
                programming_interface: class_bytes[1],
                revision: class_bytes[0],
            },
            header.to_le_bytes()[2],
        ))
    };
    paging.unmap_page(ECAM_SCRATCH_VIRTUAL)?;
    Ok(result)
}

fn read_device(bus: u8, device: u8, function: u8) -> PciDevice {
    let identity = read_config_u32(bus, device, function, 0);
    let class_data = read_config_u32(bus, device, function, 8);
    let identity_bytes = identity.to_le_bytes();
    let class_bytes = class_data.to_le_bytes();
    PciDevice {
        bus,
        device,
        function,
        vendor_id: u16::from_le_bytes([identity_bytes[0], identity_bytes[1]]),
        device_id: u16::from_le_bytes([identity_bytes[2], identity_bytes[3]]),
        class: class_bytes[3],
        subclass: class_bytes[2],
        programming_interface: class_bytes[1],
        revision: class_bytes[0],
    }
}

fn read_config_u8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    let register = read_config_u32(bus, device, function, offset);
    register.to_le_bytes()[usize::from(offset & 3)]
}

fn read_config_u16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let register = read_config_u32(bus, device, function, offset);
    let bytes = register.to_le_bytes();
    let index = usize::from(offset & 2);
    u16::from_le_bytes([bytes[index], bytes[index + 1]])
}

fn read_config_u32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = 0x8000_0000
        | (u32::from(bus) << 16)
        | (u32::from(device) << 11)
        | (u32::from(function) << 8)
        | u32::from(offset & 0xfc);
    // SAFETY: PCI configuration mechanism 1 serializes accesses through CF8
    // and CFC. NexOS is single-core during discovery.
    unsafe {
        outl(0xcf8, address);
        inl(0xcfc)
    }
}

fn write_config_u16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let aligned = offset & 0xfc;
    let current = read_config_u32(bus, device, function, aligned);
    let shift = u32::from(offset & 2) * 8;
    let mask = 0xffff_u32 << shift;
    let updated = (current & !mask) | (u32::from(value) << shift);
    write_config_u32(bus, device, function, aligned, updated);
}

fn write_config_u32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    let address = 0x8000_0000
        | (u32::from(bus) << 16)
        | (u32::from(device) << 11)
        | (u32::from(function) << 8)
        | u32::from(offset & 0xfc);
    // SAFETY: PCI configuration mechanism 1 serializes accesses through CF8
    // and CFC. NexOS is single-core during discovery.
    unsafe {
        outl(0xcf8, address);
        outl(0xcfc, value);
    }
}

unsafe fn outl(port: u16, value: u32) {
    // SAFETY: Caller guarantees ownership and validity of the I/O port.
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
    }
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    // SAFETY: Caller guarantees ownership and validity of the I/O port.
    unsafe {
        asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack));
    }
    value
}
