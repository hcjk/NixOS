# NexOS ABI v1

The shared definitions live in `crates/nexos-abi` and use fixed-width,
`#[repr(C)]` layouts.

## Syscalls

On x86-64, userspace places the syscall number in `rax` and arguments in `rdi`,
`rsi`, `rdx`, `r10`, `r8`, and `r9`. The return value is in `rax`. Values from
-1 through -4095 are errors.

The initial namespace reserves process, file, memory, time, device-control, and
system-information calls. Changing an assigned number requires an
ABI-major-version change.

NexOS v0.7 configures `IA32_STAR`, `IA32_LSTAR`, `IA32_FMASK`, and
`IA32_EFER.SCE`. A syscall masks interrupts, direction flag, and trap flag,
switches to a dedicated kernel stack, preserves the user register frame, and
dispatches only assigned ABI numbers. Normal returns use `SYSRETQ`; process
exit restores the saved kernel continuation. Kernel/user GDT selectors and a
TSS privilege stack support ring transitions and user-originated exceptions.

The v1 shared structures include `FileStat`, `DirectoryEntry`, open flags,
seek origins, file types, system-information selectors, path/name limits, and
negative error codes. Unknown syscalls return `-NotSupported`.

## Boot information

Limine-specific response objects are translated into `BootInfo` before being
passed deeper into the kernel. No Limine type is permitted in portable kernel
subsystems.
