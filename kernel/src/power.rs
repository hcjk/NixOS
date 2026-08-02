use core::arch::asm;

use crate::acpi::{GenericAddress, PowerInfo};

const SYSTEM_MEMORY: u8 = 0;
const SYSTEM_IO: u8 = 1;
const SLEEP_ENABLE: u64 = 1 << 13;
const SLEEP_TYPE_SHIFT: u32 = 10;

#[derive(Clone, Copy, Debug)]
pub enum PowerError {
    AcpiUnavailable,
    UnsupportedAddressSpace,
    UnsupportedWidth,
    InvalidAddress,
    HardwareDidNotRespond,
}

pub fn power_off(info: Option<PowerInfo>, hhdm_offset: u64) -> Result<(), PowerError> {
    let info = info.ok_or(PowerError::AcpiUnavailable)?;
    if !info.sleep_supported {
        return Err(PowerError::AcpiUnavailable);
    }
    let primary = (read_register(info.pm1a_control, hhdm_offset)? & !(7 << SLEEP_TYPE_SHIFT))
        | (u64::from(info.sleep_type_a) << SLEEP_TYPE_SHIFT)
        | SLEEP_ENABLE;
    write_register(info.pm1a_control, primary, hhdm_offset)?;
    if info.pm1b_control.is_present() {
        let secondary = (read_register(info.pm1b_control, hhdm_offset)? & !(7 << SLEEP_TYPE_SHIFT))
            | (u64::from(info.sleep_type_b) << SLEEP_TYPE_SHIFT)
            | SLEEP_ENABLE;
        write_register(info.pm1b_control, secondary, hhdm_offset)?;
    }
    delay();
    Err(PowerError::HardwareDidNotRespond)
}

pub fn reboot(info: Option<PowerInfo>, hhdm_offset: u64) -> ! {
    if let Some(info) = info
        && info.reset_register.is_present()
    {
        let _ = write_register(
            info.reset_register,
            u64::from(info.reset_value),
            hhdm_offset,
        );
        delay();
    }
    crate::ps2::reboot()
}

fn read_register(register: GenericAddress, hhdm_offset: u64) -> Result<u64, PowerError> {
    let width = register_width(register)?;
    match register.address_space {
        SYSTEM_IO => {
            let port = u16::try_from(register.address).map_err(|_| PowerError::InvalidAddress)?;
            // SAFETY: ACPI declares the register's I/O address and width.
            Ok(unsafe {
                match width {
                    1 => u64::from(inb(port)),
                    2 => u64::from(inw(port)),
                    4 => u64::from(inl(port)),
                    _ => return Err(PowerError::UnsupportedWidth),
                }
            })
        }
        SYSTEM_MEMORY => {
            let address = hhdm_offset
                .checked_add(register.address)
                .ok_or(PowerError::InvalidAddress)?;
            // SAFETY: ACPI supplies a system-memory register and the HHDM
            // exposes its physical address for volatile access.
            Ok(unsafe {
                match width {
                    1 => u64::from(core::ptr::read_volatile(address as *const u8)),
                    2 => u64::from(core::ptr::read_volatile(address as *const u16)),
                    4 => u64::from(core::ptr::read_volatile(address as *const u32)),
                    8 => core::ptr::read_volatile(address as *const u64),
                    _ => return Err(PowerError::UnsupportedWidth),
                }
            })
        }
        _ => Err(PowerError::UnsupportedAddressSpace),
    }
}

fn write_register(
    register: GenericAddress,
    value: u64,
    hhdm_offset: u64,
) -> Result<(), PowerError> {
    let width = register_width(register)?;
    let bytes = value.to_le_bytes();
    let low_word = u16::from_le_bytes([bytes[0], bytes[1]]);
    let low_double_word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    match register.address_space {
        SYSTEM_IO => {
            let port = u16::try_from(register.address).map_err(|_| PowerError::InvalidAddress)?;
            // SAFETY: ACPI declares the register's I/O address and width.
            unsafe {
                match width {
                    1 => outb(port, bytes[0]),
                    2 => outw(port, low_word),
                    4 => outl(port, low_double_word),
                    _ => return Err(PowerError::UnsupportedWidth),
                }
            }
            Ok(())
        }
        SYSTEM_MEMORY => {
            let address = hhdm_offset
                .checked_add(register.address)
                .ok_or(PowerError::InvalidAddress)?;
            // SAFETY: ACPI supplies a system-memory register and the HHDM
            // exposes its physical address for volatile access.
            unsafe {
                match width {
                    1 => core::ptr::write_volatile(address as *mut u8, bytes[0]),
                    2 => core::ptr::write_volatile(address as *mut u16, low_word),
                    4 => core::ptr::write_volatile(address as *mut u32, low_double_word),
                    8 => core::ptr::write_volatile(address as *mut u64, value),
                    _ => return Err(PowerError::UnsupportedWidth),
                }
            }
            Ok(())
        }
        _ => Err(PowerError::UnsupportedAddressSpace),
    }
}

fn register_width(register: GenericAddress) -> Result<u8, PowerError> {
    if register.bit_offset != 0 {
        return Err(PowerError::UnsupportedWidth);
    }
    let width = match register.access_size {
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        _ => register.bit_width.div_ceil(8),
    };
    matches!(width, 1 | 2 | 4 | 8)
        .then_some(width)
        .ok_or(PowerError::UnsupportedWidth)
}

fn delay() {
    for _ in 0..1_000_000 {
        core::hint::spin_loop();
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe { asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack)) };
    value
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe { asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack)) };
    value
}

unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe { asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack)) };
    value
}

unsafe fn outb(port: u16, value: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack)) };
}

unsafe fn outw(port: u16, value: u16) {
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack)) };
}

unsafe fn outl(port: u16, value: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack)) };
}
