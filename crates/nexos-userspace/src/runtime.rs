use nexos_abi::{DirectoryEntry, Error, FileStat, Syscall, SystemInfoSelector};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyscallResult(pub i64);

impl SyscallResult {
    pub fn into_result(self) -> Result<u64, Error> {
        if self.0 >= 0 {
            return Ok(self.0.unsigned_abs());
        }
        Err(error_from_code(self.0).unwrap_or(Error::Io))
    }
}

#[cfg(target_os = "none")]
#[must_use]
/// Issues one raw `NexOS` syscall.
///
/// # Safety
///
/// Pointer arguments must identify user-accessible memory valid for the
/// selected syscall and remain live until the kernel returns.
pub unsafe fn syscall(number: Syscall, arguments: [u64; 6]) -> SyscallResult {
    let mut result = number as u64;
    // SAFETY: The NexOS ABI assigns these registers to syscall number,
    // arguments, and result. RCX and R11 are architectural clobbers.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") result,
            in("rdi") arguments[0],
            in("rsi") arguments[1],
            in("rdx") arguments[2],
            in("r10") arguments[3],
            in("r8") arguments[4],
            in("r9") arguments[5],
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
    }
    SyscallResult(result.cast_signed())
}

#[cfg(not(target_os = "none"))]
#[must_use]
/// Host-test stub for the raw `NexOS` syscall entry.
///
/// # Safety
///
/// This host implementation does not dereference any arguments and always
/// returns `NotSupported`.
pub unsafe fn syscall(_number: Syscall, _arguments: [u64; 6]) -> SyscallResult {
    SyscallResult(Error::NotSupported.as_syscall_result())
}

pub fn abi_version() -> Result<u32, Error> {
    // SAFETY: SystemInfo with selector zero has no pointer arguments.
    let result = unsafe { syscall(Syscall::SystemInfo, [0, 0, 0, 0, 0, 0]) }.into_result()?;
    u32::try_from(result).map_err(|_| Error::Io)
}

pub fn uptime_milliseconds() -> Result<u64, Error> {
    // SAFETY: ClockGet has no pointer arguments.
    unsafe { syscall(Syscall::ClockGet, [0; 6]) }.into_result()
}

pub fn processor_count() -> Result<u64, Error> {
    system_info(SystemInfoSelector::ProcessorCount)
}

pub fn online_processor_count() -> Result<u64, Error> {
    system_info(SystemInfoSelector::OnlineProcessorCount)
}

fn system_info(selector: SystemInfoSelector) -> Result<u64, Error> {
    // SAFETY: SystemInfo consumes only a scalar selector.
    unsafe { syscall(Syscall::SystemInfo, [selector as u64, 0, 0, 0, 0, 0]) }.into_result()
}

pub fn open(path: &[u8], flags: u32) -> Result<u32, Error> {
    // SAFETY: The path slice remains valid for the duration of Open.
    let result = unsafe {
        syscall(
            Syscall::Open,
            [
                path.as_ptr() as u64,
                path.len() as u64,
                u64::from(flags),
                0,
                0,
                0,
            ],
        )
    }
    .into_result()?;
    u32::try_from(result).map_err(|_| Error::BadHandle)
}

pub fn close(handle: u32) -> Result<(), Error> {
    // SAFETY: Close consumes only a scalar descriptor.
    unsafe { syscall(Syscall::Close, [u64::from(handle), 0, 0, 0, 0, 0]) }.into_result()?;
    Ok(())
}

pub fn read(handle: u32, output: &mut [u8]) -> Result<usize, Error> {
    // SAFETY: The mutable output remains valid until Read returns.
    let result = unsafe {
        syscall(
            Syscall::Read,
            [
                u64::from(handle),
                output.as_mut_ptr() as u64,
                output.len() as u64,
                0,
                0,
                0,
            ],
        )
    }
    .into_result()?;
    usize::try_from(result).map_err(|_| Error::Io)
}

pub fn write(handle: u32, input: &[u8]) -> Result<usize, Error> {
    // SAFETY: The input slice remains valid until Write returns.
    let result = unsafe {
        syscall(
            Syscall::Write,
            [
                u64::from(handle),
                input.as_ptr() as u64,
                input.len() as u64,
                0,
                0,
                0,
            ],
        )
    }
    .into_result()?;
    usize::try_from(result).map_err(|_| Error::Io)
}

pub fn stat(path: &[u8], output: &mut FileStat) -> Result<(), Error> {
    // SAFETY: Both the path and output remain valid until Stat returns.
    unsafe {
        syscall(
            Syscall::Stat,
            [
                path.as_ptr() as u64,
                path.len() as u64,
                core::ptr::from_mut(output) as u64,
                0,
                0,
                0,
            ],
        )
    }
    .into_result()?;
    Ok(())
}

pub fn read_dir(handle: u32, output: &mut DirectoryEntry) -> Result<bool, Error> {
    // SAFETY: The output entry remains valid until ReadDir returns.
    let result = unsafe {
        syscall(
            Syscall::ReadDir,
            [
                u64::from(handle),
                core::ptr::from_mut(output) as u64,
                0,
                0,
                0,
                0,
            ],
        )
    }
    .into_result()?;
    Ok(result != 0)
}

pub fn reboot() -> Result<(), Error> {
    power_action(0)
}

pub fn shutdown() -> Result<(), Error> {
    power_action(1)
}

fn power_action(action: u64) -> Result<(), Error> {
    // SAFETY: Reboot consumes only a scalar action selector.
    unsafe { syscall(Syscall::Reboot, [action, 0, 0, 0, 0, 0]) }.into_result()?;
    Ok(())
}

pub fn exit(status: i32) -> ! {
    // SAFETY: Exit consumes only the scalar status argument.
    let encoded_status = u64::from_ne_bytes(i64::from(status).to_ne_bytes());
    let _ = unsafe { syscall(Syscall::Exit, [encoded_status, 0, 0, 0, 0, 0]) };
    loop {
        core::hint::spin_loop();
    }
}

fn error_from_code(value: i64) -> Option<Error> {
    Some(match -value {
        1 => Error::PermissionDenied,
        2 => Error::NotFound,
        4 => Error::Interrupted,
        5 => Error::Io,
        9 => Error::BadHandle,
        10 => Error::NoChild,
        11 => Error::WouldBlock,
        12 => Error::OutOfMemory,
        17 => Error::AlreadyExists,
        20 => Error::NotDirectory,
        21 => Error::IsDirectory,
        22 => Error::InvalidArgument,
        24 => Error::HandleLimit,
        27 => Error::FileTooLarge,
        28 => Error::NoSpace,
        30 => Error::ReadOnly,
        36 => Error::NameTooLong,
        67 => Error::ProcessLimit,
        69 => Error::MountLimit,
        95 => Error::NotSupported,
        110 => Error::TimedOut,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_stub_returns_not_supported() {
        assert_eq!(abi_version(), Err(Error::NotSupported));
    }

    #[test]
    fn decodes_shared_error_numbers() {
        assert_eq!(SyscallResult(-36).into_result(), Err(Error::NameTooLong));
        assert_eq!(SyscallResult(42).into_result(), Ok(42));
    }

    #[test]
    fn host_file_wrappers_report_not_supported() {
        let mut buffer = [0_u8; 8];
        let mut metadata = FileStat::default();
        assert_eq!(open(b"/", 0), Err(Error::NotSupported));
        assert_eq!(read(0, &mut buffer), Err(Error::NotSupported));
        assert_eq!(write(1, b"hello"), Err(Error::NotSupported));
        assert_eq!(stat(b"/", &mut metadata), Err(Error::NotSupported));
        assert_eq!(reboot(), Err(Error::NotSupported));
        assert_eq!(shutdown(), Err(Error::NotSupported));
        assert_eq!(uptime_milliseconds(), Err(Error::NotSupported));
        assert_eq!(processor_count(), Err(Error::NotSupported));
    }
}
