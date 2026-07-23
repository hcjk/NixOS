# NexOS status

Last verified: 2026-07-23

## Working

- Freestanding, higher-half x86-64 kernel ELF.
- Limine memory-map, HHDM, framebuffer, and ACPI RSDP requests.
- COM1 serial diagnostics and panic output.
- GDT with kernel code/data descriptors and a 64-bit TSS.
- IDT exception handlers and a dedicated double-fault interrupt stack.
- Legacy 8259 PIC routing with local-APIC fallback selection.
- 100 Hz PIT timer with monotonic tick and uptime reporting.
- Interrupt-driven i8042 PS/2 keyboard input with a lock-free scancode queue.
- 24/32-bit RGB/BGR framebuffer terminal with bitmap text, color, scrolling,
  cursor handling, and backspace.
- Physical page-frame allocation from usable memory, reserving the first 1 MiB.
- A 256 KiB early bump heap backed by 64 contiguous physical frames.
- Active four-level page-table inspection and virtual-to-physical translation.
- x86-64 CPUID detection for APIC, NX, and SSE2.
- Interactive kernel monitor with boot, CPU, memory, heap, paging, interrupt,
  uptime, reboot, and halt diagnostics.
- Combined MBR/FAT32 disk image with BIOS and UEFI Limine boot paths.
- Host-testable syscall ABI, in-memory block device, MBR/GPT validation, and
  NexFS v1 superblock formatting/checking.
- Safe image-only `nexosctl`; raw physical disks are intentionally refused.

## Emulator verification

The same `build/nexos.img` booted through BIOS and UEFI under QEMU 11.0.0.
Both paths received PS/2 IRQ1 input and advanced PIT ticks. The BIOS diagnostic
run also verified heap writes, live page-table translation, and a recoverable
breakpoint exception:

```text
NexOS 0.3.0-dev x86-64
memory: 21 regions, 250 MiB usable, bootstrap frame 0x100000
paging: CR3 0xff91000, HHDM 0xffff800000000000,
        heap 0xffff800000101000 (256 KiB)
interrupts: GDT/TSS/IDT online, legacy PIC, PIT 100 Hz, PS/2 IRQ1
nexos> heaptest
heap allocation ok: address=0xffff800000101000, size=64, checksum=6112
nexos> virtinfo
kernel virt 0xffffffff80001fa0 -> phys 0xfb58fa0, page=4 KiB
nexos> int3
interrupt: breakpoint handled
Breakpoint handler returned successfully.
```

UEFI reported 34 memory regions, 200 MiB usable, allocated above the 1 MiB
reserve, accepted IRQ-driven keyboard input, and reported advancing uptime.

## Not implemented yet

Virtual page mapping/unmapping, a reclaiming general-purpose heap, ACPI table
parsing, APIC/I/O APIC routing, HPET, PCI, PS/2 mouse support, AHCI, IDE, full
NexFS file operations, processes, syscalls, userspace, shell, USB, and physical
disk installation remain later milestones. The current prompt is a kernel
monitor, not yet the planned Unix-like userspace shell.
