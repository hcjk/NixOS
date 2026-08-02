# NexOS x86-64 platform services

Milestone 14 activates the ACPI platform information that earlier releases
only displayed.

## Multiprocessing

NexOS requests the x86-64 processor list from Limine and supports up to 64
registered CPUs. The bootstrap processor initializes shared paging, GDT, IDT,
interrupt controllers, and runtime state before releasing each application
processor. An AP configures SSE, loads the shared ring-0 GDT and IDT, enables
its local APIC, publishes `Online`, and enters an interruptible idle loop.

`smpinfo` reports processor and LAPIC IDs, online state, scheduler ticks, and
idle halts. The current shell and device work remain on the BSP. AP task
migration is intentionally deferred until per-CPU TSS, syscall stacks, and
full context-switch interrupt return are available.

## Timekeeping

The HPET table must describe system-memory registers in one page. NexOS maps
that page uncached, validates a 64-bit main counter and its femtosecond period,
resets and enables it, and uses it for monotonic milliseconds. The PIT remains at
100 Hz for scheduler ticks and is the uptime fallback when HPET is absent.

## PCI configuration

For ACPI MCFG segment zero, NexOS computes each ECAM function page, maps one
uncached scratch page, reads the function header, and immediately unmaps it.
This avoids permanently mapping an entire 256 MiB ECAM aperture. Legacy PCI
configuration mechanism 1 remains the fallback and handles device command/BAR
updates after discovery.

## Power control

The FADT parser selects extended or legacy PM1 control blocks, validates the
DSDT, extracts `_S5_` sleep types, and records the reset register when
supported. `shutdown` writes `SLP_TYP` and `SLP_EN`; `reboot` tries the FADT
reset register before falling back to the i8042 reset pulse. Both commands are
available in the recovery monitor and through the ring-3 syscall used by
`nexsh`.

System-memory FADT registers use the bootloader HHDM. System-I/O registers use
the declared 8/16/32-bit port width. Unsupported address spaces and malformed
widths fail safely instead of issuing an arbitrary hardware access.
