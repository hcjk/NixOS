# NexOS process, scheduler, ELF, and VFS core

NexOS v0.7 introduces the kernel/userspace execution boundary without adopting
the Linux ABI or Linux process model.

## Processes and handles

Each process has a monotonic process ID, optional parent, lifecycle state,
address-space root, entry point, user stack, absolute current directory, exit
status, and a bounded descriptor table. Descriptor allocation returns the
lowest available number. Exiting closes every descriptor, leaves a waitable
status, and allows the parent to reap the child.

The development kernel uses fixed capacities of 16 processes, 16 handles per
process, and 32 tasks. Capacity exhaustion returns explicit ABI errors instead
of allocating memory in an interrupt path.

## Scheduling

The round-robin scheduler supports ready, running, sleeping, blocked, and
exited task states. A five-tick quantum is driven by the 100 Hz timer. Sleep
deadlines, FIFO wait queues, explicit wakeups, exit, and reap are part of the
portable `no_std` runtime core. Milestone 14 starts application processors and
publishes per-CPU online, scheduler-tick, and idle state. User tasks remain on
the boot processor until each AP has a private TSS/syscall stack and the
scheduler gains cross-CPU task migration.

## ELF64 loading

The loader accepts little-endian x86-64 `ET_EXEC` and `ET_DYN` images. It
validates header sizes, program-header bounds, user address limits, file versus
memory size, alignment, overflow, entry-point execute permission, and rejects
writable-executable load segments. Only validated `PT_LOAD` descriptions are
passed to an address-space target.

## Ring 3 and syscalls

The GDT contains kernel and user code/data segments, while the TSS supplies
privilege and double-fault stacks. `IRETQ` enters ring 3. `SYSCALL` switches to
a dedicated 16 KiB kernel stack and dispatches the versioned shared ABI.
`SYSRETQ` handles normal returns; Exit restores the saved kernel continuation.

The `usertest` monitor command maps supervisor-protected user code and stack
pages, enters ring 3, queries ABI version 1, exits, and verifies the returned
status. This is an executable hardware-path test, not a simulated parser test.

Installed systems extend that path by reading `/bin/nexsh` from NexFS,
validating its ELF64 program headers, mapping each segment with user/write/NX
permissions, zeroing BSS, and entering with a 64 KiB SysV-aligned stack.
Read, Write, Open, Close, Stat, and ReadDir validate every user page before
calling console or root-filesystem services. Exit returns synchronously to the
kernel recovery monitor.

## VFS

The VFS core defines filesystem operations for lookup, read, write, create,
and remove, plus metadata for regular files, directories, block devices, and
character devices. It canonicalizes absolute or current-directory-relative
paths, resolves repeated separators, `.` and `..`, enforces length limits, and
routes lookups to the longest matching mount prefix.

Milestone 8 supplies the syscall-facing userspace library, shell syntax,
pipeline/redirection model, environment, history, and command registry.
Milestone 13 mounts the GPT NexFS root and launches a real separately linked
shell from `/bin`. The first execution model intentionally runs one foreground
process in the boot address space; separate page tables, Spawn/Wait scheduling,
pipelines, and executable redirection remain later work.
