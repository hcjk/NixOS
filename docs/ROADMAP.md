# NexOS implementation status

Legend: **done**, *foundation*, planned.

1. **Workspace, ABI, higher-half kernel ELF, BIOS/UEFI boot, serial output**
2. **Framebuffer terminal, physical frame allocator, CPUID, PS/2 keyboard
   polling, interactive kernel monitor, block-device API**
3. Interrupts, GDT/TSS/IDT, virtual memory manager, kernel heap
4. ACPI, APIC, timers, PCI/PCIe, interrupt-driven PS/2 keyboard and mouse
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

The complete v1 described in the product plan is a long-running systems project.
Every milestone must retain host tests and QEMU smoke tests before real disks or
hardware are enabled.
