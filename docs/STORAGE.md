# NexOS storage subsystem

Milestones 5 through 15 establish one block-device contract from host tests
through the x86-64 kernel and the guarded in-OS installer.

## Kernel drivers

- AHCI supports PCI class `01:06:01`, BIOS/OS ownership handoff, ABAR mapping,
  SATA port detection, ATA IDENTIFY, LBA28/LBA48 DMA read/write, FLUSH CACHE,
  one polling command slot per port, and bounded timeouts.
- IDE supports PCI class `01:01`, legacy and native channel BARs,
  primary/secondary channels, master/slave devices, ATA IDENTIFY, LBA28/LBA48
  PIO read/write, FLUSH CACHE, and bounded timeouts.
- NVMe supports PCI class `01:08`, BAR mapping, controller disable/enable,
  Identify Controller and namespace 1, physically contiguous admin and I/O
  queues, page-sized PRP transfers, flush, bounded completion polling, and
  queue recreation after controller reset. Active namespace formats with
  512-byte through 4096-byte logical blocks and no metadata are accepted.
- The kernel monitor names discovered devices `disk0` through `disk15`.
  `lsblk` reports capacity and MBR/GPT metadata. `disktest N` is read-only and
  reports the CRC32 and signature of LBA 0. `diskstress N R` performs up to
  4096 read-only boundary rounds, and `diskutil recover diskN` resets an NVMe
  controller and rebuilds its queues.

The current drivers are synchronous and submit storage from the bootstrap
processor. NVMe interrupt queues, multiple namespaces, AHCI Native Command
Queuing, live NVMe/AHCI hot-plug, and ATAPI remain later work. USB disconnect
retirement is implemented by the xHCI stack.

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

The in-kernel path is whole-disk. On 512-byte media it installs both Limine
HDD/MBR stages and a UEFI fallback loader. On 4Kn media it creates a larger
FAT32 ESP and installs UEFI only, because Limine's legacy stage offsets are
defined in 512-byte units. It does not resize or preserve partitions, and it
does not support RAID.
