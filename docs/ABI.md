# NexOS ABI v1

The shared definitions live in `crates/nexos-abi` and use fixed-width,
`#[repr(C)]` layouts.

## Syscalls

On x86-64, userspace places the syscall number in `rax` and arguments in `rdi`,
`rsi`, `rdx`, `r10`, `r8`, and `r9`. The return value is in `rax`. Values from
-1 through -4095 are errors.

The initial namespace reserves process, file, memory, time, device-control, and
system-information calls. Implementing the ring transition is a later kernel
milestone; changing an assigned number requires an ABI-major-version change.

## Boot information

Limine-specific response objects are translated into `BootInfo` before being
passed deeper into the kernel. No Limine type is permitted in portable kernel
subsystems.

