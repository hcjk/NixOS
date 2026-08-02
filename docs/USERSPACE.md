# NexOS userspace and shell frontend

The `nexos-userspace` crate is a freestanding, `no_std` library shared by
ring-3 programs. It is specific to the versioned NexOS ABI and does not
implement the Linux syscall ABI.

## Syscall runtime

On x86-64 NexOS, the runtime places the syscall number in `rax`, arguments in
`rdi`, `rsi`, `rdx`, `r10`, `r8`, and `r9`, and executes `SYSCALL`. It treats
non-negative `rax` values as success and maps assigned negative values back to
the shared `Error` enum. Host builds use a non-executing stub so parser and
command tests run safely on Windows and CI.

## Shell syntax

The bounded parser accepts:

- up to eight pipeline stages and sixteen arguments per stage;
- single quotes, double quotes, and backslash escapes;
- `$NAME` environment-variable expansion outside single quotes;
- `<`, `>`, and `>>` redirection with or without surrounding spaces; and
- adjacent quoted/unquoted fragments as one word.

It reports empty pipeline stages, missing redirect targets, duplicate
redirections, trailing escapes, unterminated quotes, excessive arguments, and
expanded-line overflow. Parsing writes into a fixed 512-byte arena, so no heap
allocation is required.

Environment storage is limited to 16 entries with validated names. History
stores the 16 most recent non-empty commands, drops adjacent duplicates, and
truncates entries safely.

## Command surface

The command registry contains the planned 45 commands in five classes:
built-ins; file commands; system commands; storage commands; and utilities.
Installed systems discover a GPT NexFS root, validate `/bin/nexsh` as an
x86-64 ELF, map its read/execute and read/write segments with user permissions,
zero BSS, create a 64 KiB user stack, and enter it at ring 3. The optimized
shell is separately linked at `0x40000000` and installed into the root and boot
filesystems.

The live shell uses validated Read, Write, Open, Close, Stat, and ReadDir
syscalls. It provides `help`, `uname`, `pwd`, `echo`, `ls`, `cat`, `stat`,
`clear`, and `exit`. ISO boots without a NexFS root use the kernel recovery
monitor; installed disks use `nexsh>` by default.

The richer parser and 45-command registry remain the model for later process
spawning, pipelines, redirection, environment mutation, and standalone
programs. Milestone 13 deliberately runs one synchronous foreground userspace
process in the boot address space; separate process page tables and
asynchronous scheduling are subsequent work.
