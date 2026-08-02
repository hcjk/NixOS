# NexOS status

Last verified: 2026-08-02

## Working

- Freestanding, higher-half x86-64 Rust kernel.
- Limine memory-map, HHDM, framebuffer, and ACPI RSDP requests.
- Bidirectional COM1 monitor/diagnostics and a color framebuffer terminal.
- Physical frame allocation with the first 1 MiB reserved.
- Four-level x86-64 page translation plus 4 KiB map/unmap primitives.
- Uncached MMIO mappings for the local APIC and I/O APIC.
- A 16 MiB physical-frame-backed `GlobalAlloc` heap with aligned variable-size
  allocation, deallocation, adjacent-free-block coalescing, and reuse.
- GDT, 64-bit TSS, IDT exception handlers, and a dedicated double-fault stack.
- ACPI 1.0 RSDT and ACPI 2.0+ XSDT parsing with checksum validation.
- MADT processor, local-APIC, I/O-APIC, and interrupt-override discovery.
- HPET main-counter timekeeping with period validation and PIT fallback.
- PCI MCFG ECAM enumeration for segment zero with mechanism 1 fallback.
- FADT reset/PM1 register discovery, AML `_S5_` parsing, and working ACPI
  shutdown/reset paths from the monitor and ring-3 shell.
- Limine-assisted application-processor startup for up to 64 CPUs, shared
  kernel GDT/IDT loading, per-CPU local-APIC enablement, and bounded online,
  scheduler-tick, and idle-halt accounting.
- Local APIC and I/O APIC routing for PIT, PS/2 keyboard, and PS/2 mouse IRQs.
- Automatic legacy PIC fallback when APIC initialization is unavailable.
- 100 Hz PIT scheduler ticks and HPET/PIT-selected uptime reporting.
- Interrupt-driven i8042 keyboard and three-byte PS/2 mouse packets.
- PCI ECAM or configuration mechanism 1 enumeration across advertised buses
  and functions.
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
- Host-testable syscall ABI and block-device interfaces.
- NexFS v1 fixed-size inodes and directory entries with checksums, nested
  directories, direct and indirect file blocks, random reads/writes,
  zero-filled gaps, rename, truncate, and removal.
- NexFS allocation bitmaps, ordered dirty-mount/clean-unmount protocol, and an
  offline checker for duplicate or leaked blocks, invalid references,
  duplicate names, and orphaned inodes.
- `nexosctl` NexFS image commands for information, checking, listing, file
  import/output, directory creation, rename, truncate, and removal.
- Safe image-only `nexosctl`; raw physical disks remain intentionally refused.
- Fixed-capacity process and descriptor tables with parent/child exit and reap
  semantics.
- Timer-driven five-tick round-robin scheduler state machine with sleeping,
  blocking, wakeups, FIFO wait queues, and task exit.
- Strict little-endian x86-64 ELF64 validation and a validated load-segment
  target interface.
- Mount-aware VFS path normalization and traversal with regular-file,
  directory, block-device, and character-device node types.
- User GDT segments, TSS ring-0 stack, user page mappings, and a dedicated
  syscall stack.
- Versioned x86-64 `SYSCALL`/`SYSRET` entry with negative ABI errors and a real
  ring-3 `usertest`.
- Interactive `ps`, `schedinfo`, `syscalls`, `usertest`, and `vfspath`
  diagnostics.
- Freestanding userspace syscall wrapper with host-test stubs and shared ABI
  error decoding.
- Bounded shell parser with quotes, escapes, environment expansion, eight-stage
  pipelines, input/output/append redirection, environment storage, and command
  history.
- A classified registry of 45 Unix-inspired commands plus `commands`,
  `shellparse`, and `shelltest` kernel diagnostics.
- NexFS root-partition discovery by GPT type, mount metadata, and bounded
  file/directory handles exposed through the versioned syscall ABI.
- A separately linked, optimized 64-bit `/bin/nexsh` ELF installed into both
  NexFS and the boot filesystem.
- Validated ELF segment mapping, zero-filled BSS, a 64 KiB user stack with
  SysV alignment, and synchronous ring-3 process launch.
- Validated user-memory ranges for stdin/stdout, open, close, read, stat, and
  directory-entry syscalls. Blocking input temporarily enables IRQ delivery
  around `HLT` and restores the masked syscall invariant afterward.
- The default installed-system prompt is `nexsh>` with working `help`,
  `uname`, `smpinfo`, `uptime`, `pwd`, `echo`, `ls`, `cat`, `stat`, `clear`,
  `shutdown`, `reboot`, and `exit` commands.
- PCI xHCI discovery, firmware ownership handoff, controller halt/reset/start,
  4 KiB DMA command and event rings, ERST/DCBAA setup, scratchpad allocation,
  and bounded polling.
- USB 2 root-port reset plus USB 2/3 port speed, power, link, connection, and
  enabled-state reporting.
- Enable Slot and Address Device commands followed by endpoint-zero
  `GET_DESCRIPTOR` and `SET_CONFIGURATION` control transfers.
- Descriptor-based VID/PID, USB version, interface, endpoint, boot HID, hub,
  and Bulk-Only mass-storage classification through `lsusb`.
- `usbinfo` controller/capability diagnostics and a repeatable xHCI No-Op
  command-ring `usbtest`.
- A reusable `no_std` USB library covering descriptor validation,
  enumeration states, xHCI TRB rings, HID boot keyboard/mouse reports,
  one-level hub descriptors/status, and mass-storage BOT/SCSI commands.
- Descriptor-driven xHCI endpoint contexts and DMA rings for nonblocking HID
  and hub interrupt-IN transfers plus synchronous bulk transfers.
- Live USB boot keyboard and mouse input, including modifiers, Caps Lock,
  wheel movement, and button events.
- One-level hub port power, reset, status, route-string, and downstream-device
  enumeration.
- Writable USB BOT/SCSI block devices with INQUIRY, READ CAPACITY(10),
  READ(10), WRITE(10), cache synchronization, `/dev/usbN` naming, and
  disconnect retirement.
- Image-only `diskutil` workflows for GPT/MBR layout creation and inspection,
  partition creation/deletion, FAT32/NexFS formatting, and filesystem checks.
- Guided combined BIOS/UEFI, UEFI-only, and BIOS-only `nex-install` modes,
  plus manual ESP/root selection that preserves unrelated partitions.
- Exact-target destructive confirmation, raw-device refusal, sibling-image
  preparation, backup/restore commit, installed-file checks, NexFS checking,
  and UUID verification.
- A combined installed image with a GPT BIOS boot partition, FAT32 EFI System
  Partition, NexFS root, Limine BIOS/UEFI files, kernel, fstab, release data,
  and install manifest.
- Native FAT32 creation inside `no_std` NexOS, including the long
  `limine.conf` boot filename used by the installer.
- Limine installer payload discovery for the running kernel and
  `BOOTX64.EFI`, including boot-source metadata used to refuse the live disk.
- In-kernel `diskutil inspect`, `diskutil plan`, and `diskutil verify`
  commands for detected storage devices.
- A guided in-kernel `nex-install diskN ERASE-diskN` workflow that creates a
  protective MBR plus primary/backup GPT, a BIOS-reserved partition, a 64 MiB
  FAT32 ESP, and a NexFS root, then verifies GPT CRCs, boot files, kernel CRC,
  configuration, and root UUID.
- Limine legacy-BIOS stage-1 and stage-2 installation, `limine-bios.sys`
  placement, and byte-for-byte BIOS-stage verification alongside the UEFI
  fallback path.
- A nine-step framebuffer and COM1 progress bar for destructive installs.
- A release-blocking QEMU regression harness that performs the in-OS install,
  removes the ISO, and verifies both SeaBIOS and UEFI installed-disk boots.

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

The milestone-6 release kernel and its versioned raw image and hybrid ISO were
then smoke-tested under QEMU 11.0.0 in four configurations: raw-image BIOS,
raw-image UEFI, ISO BIOS, and ISO UEFI. Every configuration reached the
`NexOS 0.6.0-dev x86-64` kernel and initialized the 1280x800x32 framebuffer.
The host suite passes 23 unit tests, strict Clippy with warnings denied, and a
custom-target `no_std` NexFS build using `core`, `alloc`, and
`compiler_builtins`.

The milestone-7 Q35 BIOS test entered a mapped ring-3 program with `IRETQ`.
That program queried ABI version 1 through `SYSCALL`, issued Exit, returned to
the saved ring-0 continuation, and continued handling keyboard interrupts.
The same boot reported the process table, timer-driven scheduler decisions,
two dispatched syscalls, and canonicalized `/home/../bin` to `/bin`. The host
suite passes 36 unit tests.

The milestone-8 host suite passes 43 tests. BIOS and UEFI QEMU boots execute
`shelltest`, producing two pipeline stages, expanding `$HOME` to `/home/root`,
and preserving overwrite redirection to `/tmp/count`.

The milestone-9 host suite passes 61 tests. A QEMU xHCI controller with an
emulated USB keyboard and USB mass-storage disk reports two connected, enabled,
addressed, and configured devices. An isolated mass-storage run identifies
VID/PID `46f4:0001`, USB 3.0, one mass-storage interface, and two bulk
endpoints; repeated No-Op commands complete successfully.

The milestone-10 installer suite adds seven focused safety, layout, formatting,
preservation, and verification tests. A real guided install using the release
kernel and Limine completes post-commit verification, then boots from its GPT
disk through both QEMU Q35 legacy BIOS and UEFI. Both paths discover the
installed 128 MiB disk through AHCI and reach the interactive kernel monitor.

The milestone-11 storage suite passes 22 focused NexFS and storage tests,
including native FAT32 formatting/LFN creation and guided GPT write/verify. A
UEFI release ISO carrying the kernel and `BOOTX64.EFI` installed a disposable
256 MiB AHCI disk using the exact in-OS command
`nex-install disk0 ERASE-disk0`. `diskutil verify disk0` independently
validated the result. After removing the ISO, QEMU firmware loaded
`EFI/BOOT/BOOTX64.EFI` from that disk and NexOS 0.11.0-dev returned to the
monitor.

The v0.11.1 regression test first reproduced the previous legacy boot hang
immediately after SeaBIOS printed `Booting from Hard Disk...`. A fresh install
then displayed all nine progress steps, verified the new MBR/HDD stages and
FAT32 boot files, and booted the same 256 MiB disk without the ISO under both
QEMU SeaBIOS and UEFI.

The v0.11.2 release workflow turns that manual regression into an automated
publication gate. It creates a temporary disk, drives `nex-install` over COM1,
waits for the verified 100% step, then starts two fresh QEMU processes without
the ISO and requires each firmware path to reach `nexos>`.

The milestone-12 QEMU test disables i8042, attaches a keyboard, mouse, and
256 MiB mass-storage device through an eight-port xHCI hub, then types `uname`
through USB, injects mouse movement, detects `/dev/usb0`, reads the disk,
installs NexOS onto it through BOT/SCSI, verifies the result, and removes the
device. Hub, HID, block writes, cache flush, and disconnect accounting all
complete before release publication.

The milestone-13 release gate performs a fresh in-OS installation, removes
the ISO, and boots the installed disk under both SeaBIOS and UEFI. Each boot
must mount disk0p3 as `/`, validate and load `/bin/nexsh`, reach `nexsh>`, run
`uname`, read `/etc/nexos-release`, and enumerate the NexFS root directory.
The existing xHCI gate also installs the expanded root through BOT/SCSI.

The milestone-14 platform gate boots four virtual CPUs under both SeaBIOS and
UEFI, requires all APs online, exercises `smpinfo`, confirms HPET uptime and
MCFG ECAM enumeration, inspects FADT power registers, and requires ACPI S5 to
terminate QEMU. Installed-disk boots repeat four-CPU startup and invoke S5 from
the ring-3 shell syscall.

## Not implemented yet

Page-table frame reclamation, separate per-process page tables, asynchronous
process scheduling, `Spawn`/`Wait`, multiprocess pipelines and redirection,
cross-CPU task migration, per-CPU TSS/user syscall stacks, USB hotplug
re-enumeration, multiple USB LUNs, UAS, EHCI/OHCI/UHCI, NVMe, general FAT32
long-file-name creation, and NexFS journaling remain later work.

The in-OS installer is a deliberately narrow real-hardware preview: BIOS or
UEFI x86-64, Secure Boot disabled, 512-byte logical sectors, AHCI, legacy IDE,
or xHCI BOT/SCSI storage, whole-disk guided layout, and PS/2, USB boot-HID, or
COM1 input. It does not preserve partitions, resize filesystems, detect
software RAID, or support NVMe/4Kn targets. The host-side tools still refuse
raw disks.
