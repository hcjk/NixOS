# NexOS implementation status

Legend: **done**, *foundation*, planned.

1. **Workspace, ABI, higher-half kernel ELF, BIOS/UEFI boot, serial output**
2. **Framebuffer terminal, physical frame allocator, CPUID, PS/2 keyboard
   polling, interactive kernel monitor, block-device API**
3. **GDT/TSS/IDT, exception handlers, legacy PIC/PIT interrupts,
   interrupt-driven PS/2 keyboard, early kernel heap, page-table inspection**
4. **Virtual mapping/unmapping, reclaiming heap, ACPI RSDT/XSDT/MADT/
   HPET/MCFG discovery, APIC/I/O APIC routing, PCI mechanism 1, PS/2 mouse**
5. *MBR/GPT validation*, AHCI, IDE, block cache, FAT32
6. *NexFS format and superblock checking*, full inode/directory implementation
7. Processes, ELF loader, syscalls, VFS, scheduler
8. Userspace runtime, shell, Unix-like commands
9. xHCI, USB enumeration, HID, hubs, mass storage
10. *Safe image tooling*, interactive disk utility and NexOS installer
11. UEFI/BIOS installation tests, SMP, real-hardware stabilization

## Completed foundation acceptance

- `cargo test` passes on the host.
- The custom x86-64 kernel target links without an operating-system runtime.
- Invalid block ranges, malformed MBR/GPT data, corrupt NexFS metadata, and
  raw Windows disk targets are rejected.
- A valid NexFS disk image can be created and inspected.
- When packaged with Limine, the kernel writes diagnostics to COM1 and displays
  a readable framebuffer terminal.
- BIOS and UEFI builds accept PS/2 keyboard input and execute kernel-monitor
  commands.
- The physical frame allocator reserves low memory and returns page-aligned
  frames from usable memory-map entries.
- A 100 Hz PIT interrupt advances monotonic ticks and IRQ1 feeds the keyboard
  queue under both BIOS and UEFI.
- Breakpoint and page-fault exceptions enter documented IDT handlers; double
  faults have a dedicated TSS interrupt stack.
- The kernel can allocate and verify memory from a physical-frame-backed heap
  and inspect active four-level page-table mappings.
- ACPI checksums and table layouts are validated under BIOS RSDT and UEFI XSDT
  boot paths; MADT interrupt topology, HPET, and MCFG data are discovered.
- The local APIC and I/O APIC route PIT, keyboard, and mouse interrupts, with a
  legacy PIC fallback when platform discovery or APIC setup is unavailable.
- The heap supports aligned variable-size allocations, deallocation,
  coalescing, reuse, and fragmentation statistics.
- A scratch virtual page can be mapped, translated, written, unmapped, and
  invalidated; PCI configuration mechanism 1 discovers device functions.
- IRQ12 mouse packets are decoded into signed motion and button events.

The complete v1 described in the product plan is a long-running systems project.
Every milestone must retain host tests and QEMU smoke tests before real disks or
hardware are enabled.
