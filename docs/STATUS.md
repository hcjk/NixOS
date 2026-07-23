# NexOS status

Last verified: 2026-07-23

## Working

- Freestanding, higher-half x86-64 kernel ELF.
- Limine base-revision, memory-map, framebuffer, and ACPI RSDP requests.
- COM1 serial initialization and panic output.
- 24/32-bit RGB/BGR framebuffer terminal with bitmap text, color, scrolling,
  cursor handling, and backspace.
- Physical page-frame allocation from usable boot-memory ranges, with the first
  1 MiB reserved.
- x86-64 CPUID detection for APIC, NX, and SSE2.
- Polling i8042 PS/2 keyboard input with Shift and Caps Lock handling.
- Interactive kernel monitor with `help`, `clear`, `uname`, `meminfo`,
  `cpuinfo`, `bootinfo`, `reboot`, `halt`, and `echo`.
- Combined MBR/FAT32 disk image with BIOS and UEFI Limine boot paths.
- Host-testable syscall ABI, in-memory block device, MBR/GPT validation, and
  NexFS v1 superblock formatting/checking.
- Safe image-only `nexosctl`; raw physical disks are intentionally refused.

## Emulator verification

Both firmware paths booted the same `build/nexos.img` under QEMU 11.0.0, read
injected PS/2 keystrokes, and executed monitor commands:

```text
NexOS 0.2.0-dev x86-64
original Rust kernel; Linux ABI is not used
memory: 18 regions, 251 MiB usable, bootstrap frame 0x100000
ACPI RSDP: 0xffff8000000f52e0
framebuffer: 1280x800x32 pitch 5120
cpu: APIC=true NX=true SSE2=true
milestone 2 ready; entering kernel monitor
nexos> cpuinfo
x86_64 features: APIC=yes NX=yes SSE2=yes
```

UEFI reported 34 memory regions, 200 MiB usable after the low-memory reserve,
allocated its bootstrap frame at `0x100000`, and executed `meminfo`.

## Not implemented yet

Interrupt handlers, virtual memory management, a kernel heap, ACPI table
parsing, PCI, PS/2 mouse support, AHCI, IDE, full NexFS file operations,
processes, syscalls, userspace, shell, USB, and physical disk installation
remain later milestones. The current prompt is a kernel monitor, not yet a
general-purpose operating system or the planned Unix-like userspace shell.
