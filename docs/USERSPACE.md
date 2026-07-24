# NexOS userspace and shell frontend

The `nexos-userspace` crate is a freestanding, `no_std` library shared by
future ring-3 programs. It is specific to the versioned NexOS ABI and does not
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
The kernel monitor exposes `commands`, `shelltest`, and `shellparse` so this
frontend can be tested before the root filesystem launches the final ring-3
shell.

Filesystem-backed execution, process pipelines, and interactive ring-3 line
editing depend on the NexFS VFS adapter and executable packaging. Those are
the next integration steps; the syntax and ABI-facing userspace core are
already host- and kernel-build tested.
