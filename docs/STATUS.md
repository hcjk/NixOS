# NexOS status

Last verified: 2026-07-24

## Working

- Freestanding, higher-half x86-64 Rust kernel.
- Limine memory-map, HHDM, framebuffer, and ACPI RSDP requests.
- COM1 serial diagnostics and a color framebuffer terminal.
- Physical frame allocation with the first 1 MiB reserved.
- Four-level x86-64 page translation plus 4 KiB map/unmap primitives.
- Uncached MMIO mappings for the local APIC and I/O APIC.
- A 512 KiB physical-frame-backed heap with aligned variable-size allocation,
  deallocation, adjacent-free-block coalescing, and reuse.
- GDT, 64-bit TSS, IDT exception handlers, and a dedicated double-fault stack.
- ACPI 1.0 RSDT and ACPI 2.0+ XSDT parsing with checksum validation.
- MADT processor, local-APIC, I/O-APIC, and interrupt-override discovery.
- HPET and PCI MCFG table discovery.
- Local APIC and I/O APIC routing for PIT, PS/2 keyboard, and PS/2 mouse IRQs.
- Automatic legacy PIC fallback when APIC initialization is unavailable.
- 100 Hz PIT monotonic ticks and uptime reporting.
- Interrupt-driven i8042 keyboard and three-byte PS/2 mouse packets.
- PCI configuration mechanism 1 enumeration across all buses and functions.
- SATA AHCI discovery with DMA IDENTIFY, LBA28/LBA48 reads and writes, cache
  flush, bounded polling, BIOS handoff, and task-file error reporting.
- Legacy PCI IDE primary/secondary and master/slave discovery with PIO
  IDENTIFY, reads, writes, cache flush, and bounded polling.
- Bounded partition child devices and a generic write-back LRU block cache.
- Validated primary MBR and GPT headers, entry arrays, CRCs, disk bounds, and
  partition overlap checks.
- Native `no_std` FAT32 mount, root/nested directory listing, 8.3 path
  traversal, file reads, file create/replace, directory creation, mirrored FAT
  updates, allocation, and flush.
- Interactive `acpi`, `lspci`, `irqinfo`, `mouseinfo`, `heapinfo`,
  `heapstats`, `heaptest`, `virtinfo`, `maptest`, `lsblk`, and read-only
  `disktest` diagnostics.
- Combined MBR/FAT32 disk image with BIOS and UEFI Limine boot paths.
- Host-testable syscall ABI, block-device interfaces, MBR/GPT validation, and
  NexFS v1 superblock formatting/checking.
- Safe image-only `nexosctl`; raw physical disks remain intentionally refused.

## Emulator verification

The milestone-5 kernel booted under QEMU 11.0.0 through Q35 legacy BIOS and
UEFI. Both paths discovered a 128 MiB QEMU disk through the ICH9 AHCI
controller, completed ATA IDENTIFY, and reached the monitor. The BIOS path
then ran `lsblk` and a read-only `disktest 0`:

```text
NexOS 0.5.0-dev x86-64
storage: 1 disks (AHCI=1, IDE=0)
disk0: 262144 sectors x 512 bytes (sata-ahci)
nexos> lsblk
disk0: sata-ahci, AHCI 0x00010000 port 0, 128 MiB,
       512-byte sectors, QEMU HARDDISK
  MBR: 1 primary partitions
    p1: type=0xef, first=2048, sectors=260096, boot=yes
nexos> disktest 0
disk0 read-only test passed: LBA0 CRC32=0x557a5a41, signature=0xaa55
```

Legacy `pc`/PIIX emulation discovered the same boot image as one IDE PIO disk
and passed the same LBA 0 CRC32 test. UEFI reported ACPI revision 2 with six
XSDT entries and mapped its AHCI ABAR at `0x81084000`.

## Not implemented yet

Page-table frame reclamation, a Rust `GlobalAlloc` adapter, HPET clock use, PCI
ECAM access, power-off through the FADT, SMP, full NexFS file operations,
processes, syscalls, VFS mounts, userspace, shell, USB, NVMe, FAT32
long-file-name creation, and physical-disk installation remain later
milestones. The current prompt is a kernel monitor, not yet the planned
Unix-like userspace shell.
