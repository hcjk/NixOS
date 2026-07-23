use core::arch::asm;

const MAX_PCI_DEVICES: usize = 128;

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
}

pub struct PciInventory {
    devices: [PciDevice; MAX_PCI_DEVICES],
    count: usize,
    truncated: bool,
}

impl PciInventory {
    #[must_use]
    pub fn scan() -> Self {
        let mut inventory = Self {
            devices: [PciDevice::EMPTY; MAX_PCI_DEVICES],
            count: 0,
            truncated: false,
        };
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
                    inventory.push(read_device(bus, device, function));
                }
            }
        }
        inventory
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

    fn push(&mut self, device: PciDevice) {
        if self.count == MAX_PCI_DEVICES {
            self.truncated = true;
            return;
        }
        self.devices[self.count] = device;
        self.count += 1;
    }
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
