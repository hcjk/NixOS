# NexOS milestone-5 storage

Milestone 5 establishes one block-device contract from host tests through the
x86-64 kernel. It deliberately keeps physical-disk installation disabled.

## Kernel drivers

- AHCI supports PCI class `01:06:01`, BIOS/OS ownership handoff, ABAR mapping,
  SATA port detection, ATA IDENTIFY, LBA28/LBA48 DMA read/write, FLUSH CACHE,
  one polling command slot per port, and bounded timeouts.
- IDE supports PCI class `01:01`, legacy and native channel BARs,
  primary/secondary channels, master/slave devices, ATA IDENTIFY, LBA28/LBA48
  PIO read/write, FLUSH CACHE, and bounded timeouts.
- The kernel monitor names discovered devices `disk0` through `disk7`.
  `lsblk` reports capacity and MBR/GPT metadata. `disktest N` is read-only and
  reports the CRC32 and signature of LBA 0.

The current drivers are synchronous and single-core. Interrupt-driven command
queues, Native Command Queuing, hot-plug, ATAPI, NVMe, and recovery after a
controller reset are later work.

## Shared storage crate

`nexos-storage` has an allocation-free feature set used by the kernel for the
block trait and fixed-buffer MBR/GPT header parsing. Its default host/tooling
feature adds:

- memory-backed block devices for corruption and bounds tests;
- bounded partition views;
- full primary-MBR and GPT entry-array validation;
- an LRU write-back sector cache; and
- a native `no_std` FAT32 implementation.

FAT32 validates its BIOS Parameter Block and mirrored FAT geometry, traverses
nested directories, reads files, creates/replaces files, creates directories,
allocates and frees cluster chains, and flushes metadata. Milestone 5 accepts
8.3 names; long-file-name creation and timestamp updates are deferred.

## Safety boundary

Host-side `nexosctl`, `diskutil`, and `nex-install` continue to reject raw
device paths and operate only on image files. The Milestone 11 release ISO
contains a separate in-kernel guided installer for detected AHCI and IDE
devices. It requires the exact target name and an `ERASE-diskN` token, refuses
the detected boot disk, writes a new GPT/FAT32/NexFS layout, flushes it, and
reads the installed payload back before reporting success.

The in-kernel path is whole-disk and UEFI-only. It supports 512-byte logical
sectors and does not resize or preserve existing partitions. NVMe, USB
mass-storage transfers, hotplug, RAID, 4Kn media, and legacy-BIOS stage
installation from inside NexOS remain outside this release.
