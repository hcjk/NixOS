#![no_std]

pub const ABI_VERSION: u32 = 1;
pub const MAX_SYSCALL_ERROR: u64 = 4095;

#[repr(u64)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Syscall {
    Exit = 0,
    Spawn = 1,
    Wait = 2,
    Open = 16,
    Close = 17,
    Read = 18,
    Write = 19,
    Seek = 20,
    Stat = 21,
    ReadDir = 22,
    CreateDir = 23,
    Remove = 24,
    Rename = 25,
    MapMemory = 32,
    UnmapMemory = 33,
    ClockGet = 48,
    Sleep = 49,
    DeviceControl = 64,
    SystemInfo = 65,
    Reboot = 66,
}

impl TryFrom<u64> for Syscall {
    type Error = ();

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Exit,
            1 => Self::Spawn,
            2 => Self::Wait,
            16 => Self::Open,
            17 => Self::Close,
            18 => Self::Read,
            19 => Self::Write,
            20 => Self::Seek,
            21 => Self::Stat,
            22 => Self::ReadDir,
            23 => Self::CreateDir,
            24 => Self::Remove,
            25 => Self::Rename,
            32 => Self::MapMemory,
            33 => Self::UnmapMemory,
            48 => Self::ClockGet,
            49 => Self::Sleep,
            64 => Self::DeviceControl,
            65 => Self::SystemInfo,
            66 => Self::Reboot,
            _ => return Err(()),
        })
    }
}

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    PermissionDenied = 1,
    NotFound = 2,
    Interrupted = 4,
    Io = 5,
    InvalidArgument = 22,
    NoSpace = 28,
    ReadOnly = 30,
    NotSupported = 95,
    TimedOut = 110,
}

impl Error {
    #[must_use]
    pub const fn as_syscall_result(self) -> i64 {
        -(self as i32 as i64)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FramebufferInfo {
    pub address: u64,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bits_per_pixel: u16,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryKind {
    Usable = 1,
    Reserved = 2,
    AcpiReclaimable = 3,
    AcpiNvs = 4,
    BadMemory = 5,
    BootloaderReclaimable = 6,
    KernelAndModules = 7,
    Framebuffer = 8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    pub base: u64,
    pub length: u64,
    pub kind: MemoryKind,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BootInfo {
    pub abi_version: u32,
    pub reserved: u32,
    pub framebuffer: FramebufferInfo,
    pub rsdp_address: u64,
    pub kernel_physical_base: u64,
    pub kernel_virtual_base: u64,
    pub memory_regions: u64,
    pub memory_region_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syscall_numbers_are_stable() {
        assert_eq!(Syscall::try_from(18), Ok(Syscall::Read));
        assert_eq!(Syscall::try_from(999), Err(()));
        assert_eq!(Error::NoSpace.as_syscall_result(), -28);
    }

    #[test]
    fn boot_info_has_c_layout_friendly_alignment() {
        assert_eq!(core::mem::align_of::<BootInfo>(), 8);
        assert_eq!(core::mem::size_of::<MemoryRegion>(), 24);
    }
}
