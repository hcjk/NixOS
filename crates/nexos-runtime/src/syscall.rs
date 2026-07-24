use nexos_abi::{Error, Syscall};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SyscallArguments {
    pub values: [u64; 6],
}

pub trait KernelServices {
    fn invoke(&mut self, syscall: Syscall, arguments: SyscallArguments) -> Result<u64, Error>;
}

pub fn dispatch<S: KernelServices>(
    services: &mut S,
    number: u64,
    arguments: SyscallArguments,
) -> i64 {
    let Ok(syscall) = Syscall::try_from(number) else {
        return Error::NotSupported.as_syscall_result();
    };
    match services.invoke(syscall, arguments) {
        Ok(value) => i64::try_from(value).unwrap_or(i64::MAX),
        Err(error) => error.as_syscall_result(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Services {
        calls: u32,
    }

    impl KernelServices for Services {
        fn invoke(&mut self, syscall: Syscall, arguments: SyscallArguments) -> Result<u64, Error> {
            self.calls += 1;
            if syscall == Syscall::SystemInfo {
                Ok(arguments.values[0] + 7)
            } else {
                Err(Error::PermissionDenied)
            }
        }
    }

    #[test]
    fn validates_numbers_and_encodes_errors() {
        let mut services = Services { calls: 0 };
        let arguments = SyscallArguments {
            values: [5, 0, 0, 0, 0, 0],
        };
        assert_eq!(
            dispatch(&mut services, Syscall::SystemInfo as u64, arguments),
            12
        );
        assert_eq!(dispatch(&mut services, 999, arguments), -95);
        assert_eq!(services.calls, 1);
    }
}
