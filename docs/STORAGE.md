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

The released monitor exposes only disk inspection and read-only diagnostics.
Raw Windows device paths remain rejected by `nexosctl`. Write operations are
available to kernel/filesystem code but are not exposed as destructive monitor
commands or an installer until the transactional safeguards planned for
Milestone 10 exist.
