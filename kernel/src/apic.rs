use core::arch::asm;
use core::fmt;

use crate::acpi::PlatformInfo;
use crate::memory::FrameAllocator;
use crate::paging::{PageMapError, PagingInfo};

pub const LOCAL_APIC_VIRTUAL: u64 = 0xffff_ff80_0000_0000;
pub const IO_APIC_VIRTUAL: u64 = 0xffff_ff80_0000_1000;

const IA32_APIC_BASE: u32 = 0x1b;
const APIC_GLOBAL_ENABLE: u64 = 1 << 11;
const LOCAL_APIC_ID: u64 = 0x20;
const LOCAL_APIC_VERSION: u64 = 0x30;
const LOCAL_APIC_EOI: u64 = 0xb0;
const LOCAL_APIC_SPURIOUS: u64 = 0xf0;
const IO_APIC_REGISTER_SELECT: u64 = 0;
const IO_APIC_WINDOW: u64 = 0x10;

#[derive(Clone, Copy)]
pub struct ApicInfo {
    pub local_id: u8,
    pub local_version: u8,
    pub io_id: u8,
    pub io_version: u8,
    pub redirection_entries: u8,
}

pub fn initialize(
    platform: &PlatformInfo,
    paging: &mut PagingInfo,
    allocator: &mut FrameAllocator,
) -> Result<ApicInfo, ApicError> {
    let local_physical = platform
        .local_apic_address
        .ok_or(ApicError::MissingLocalApic)?;
    let io = platform.io_apic.ok_or(ApicError::MissingIoApic)?;
    paging.map_mmio_page(LOCAL_APIC_VIRTUAL, local_physical, allocator)?;
    paging.map_mmio_page(IO_APIC_VIRTUAL, io.address, allocator)?;
    enable_local_apic();

    let local_id = local_read(LOCAL_APIC_ID).to_le_bytes()[3];
    let local_version = local_read(LOCAL_APIC_VERSION).to_le_bytes()[0];
    let spurious = local_read(LOCAL_APIC_SPURIOUS);
    local_write(LOCAL_APIC_SPURIOUS, (spurious & !0xff) | 0x1ff);

    let io_identity = io_read(0);
    let io_version_register = io_read(1);
    let io_id = io_identity.to_le_bytes()[3];
    let io_version_bytes = io_version_register.to_le_bytes();
    let io_version = io_version_bytes[0];
    let maximum_redirection = io_version_bytes[2];
    for index in 0..=maximum_redirection {
        let register = 0x10 + u32::from(index) * 2;
        io_write(register, io_read(register) | (1 << 16));
    }

    configure_irq(
        platform,
        io.global_interrupt_base,
        maximum_redirection,
        0,
        32,
        local_id,
    )?;
    configure_irq(
        platform,
        io.global_interrupt_base,
        maximum_redirection,
        1,
        33,
        local_id,
    )?;
    configure_irq(
        platform,
        io.global_interrupt_base,
        maximum_redirection,
        12,
        44,
        local_id,
    )?;

    Ok(ApicInfo {
        local_id,
        local_version,
        io_id,
        io_version,
        redirection_entries: maximum_redirection.saturating_add(1),
    })
}

pub fn end_of_interrupt() {
    local_write(LOCAL_APIC_EOI, 0);
}

fn configure_irq(
    platform: &PlatformInfo,
    global_base: u32,
    maximum_redirection: u8,
    source_irq: u8,
    vector: u8,
    destination: u8,
) -> Result<(), ApicError> {
    let (global_interrupt, flags) = platform.global_interrupt_for(source_irq);
    let redirection_index = global_interrupt
        .checked_sub(global_base)
        .ok_or(ApicError::InvalidGlobalInterrupt)?;
    if redirection_index > u32::from(maximum_redirection) {
        return Err(ApicError::InvalidGlobalInterrupt);
    }
    let mut low = u32::from(vector);
    if flags & 0b11 == 0b11 {
        low |= 1 << 13;
    }
    if (flags >> 2) & 0b11 == 0b11 {
        low |= 1 << 15;
    }
    let high = u32::from(destination) << 24;
    let register = 0x10 + redirection_index * 2;
    io_write(register + 1, high);
    io_write(register, low);
    Ok(())
}

fn enable_local_apic() {
    let value = read_msr(IA32_APIC_BASE) | APIC_GLOBAL_ENABLE;
    write_msr(IA32_APIC_BASE, value);
}

fn local_read(offset: u64) -> u32 {
    // SAFETY: The APIC initialization mapped this MMIO page uncached.
    unsafe { core::ptr::read_volatile((LOCAL_APIC_VIRTUAL + offset) as *const u32) }
}

fn local_write(offset: u64, value: u32) {
    // SAFETY: The APIC initialization mapped this MMIO page uncached.
    unsafe { core::ptr::write_volatile((LOCAL_APIC_VIRTUAL + offset) as *mut u32, value) };
}

fn io_read(register: u32) -> u32 {
    // SAFETY: IO_APIC_VIRTUAL maps the discovered I/O APIC MMIO page.
    unsafe {
        core::ptr::write_volatile(
            (IO_APIC_VIRTUAL + IO_APIC_REGISTER_SELECT) as *mut u32,
            register,
        );
        core::ptr::read_volatile((IO_APIC_VIRTUAL + IO_APIC_WINDOW) as *const u32)
    }
}

fn io_write(register: u32, value: u32) {
    // SAFETY: IO_APIC_VIRTUAL maps the discovered I/O APIC MMIO page.
    unsafe {
        core::ptr::write_volatile(
            (IO_APIC_VIRTUAL + IO_APIC_REGISTER_SELECT) as *mut u32,
            register,
        );
        core::ptr::write_volatile((IO_APIC_VIRTUAL + IO_APIC_WINDOW) as *mut u32, value);
    }
}

fn read_msr(register: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: IA32_APIC_BASE is an architectural MSR on APIC-capable x86-64.
    unsafe {
        asm!(
            "rdmsr",
            in("ecx") register,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack)
        );
    }
    (u64::from(high) << 32) | u64::from(low)
}

fn write_msr(register: u32, value: u64) {
    let bytes = value.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let high = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    // SAFETY: The caller supplies a valid architectural MSR value.
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") register,
            in("eax") low,
            in("edx") high,
            options(nomem, nostack)
        );
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ApicError {
    MissingLocalApic,
    MissingIoApic,
    InvalidGlobalInterrupt,
    Paging(PageMapError),
}

impl From<PageMapError> for ApicError {
    fn from(error: PageMapError) -> Self {
        Self::Paging(error)
    }
}

impl fmt::Display for ApicError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Paging(error) => write!(formatter, "Paging({error})"),
            _ => write!(formatter, "{self:?}"),
        }
    }
}
