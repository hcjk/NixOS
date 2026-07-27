# NexOS

NexOS is an original, Unix-inspired x86-64 operating system written in Rust
and assembly. It is not based on Linux and does not provide Linux binary
compatibility.

The repository currently implements the Milestone 11 hardware-installer
preview:

- a versioned kernel/userspace ABI;
- host-testable block-device, MBR, and GPT validation code;
- the NexFS v1 formatter, fixed-size inodes, nested directories, direct and
  indirect file blocks, allocation bitmaps, dirty-mount protocol, and offline
  consistency checker;
- a safe disk-image utility (`nexosctl`);
- a Limine-aware, higher-half `no_std` x86-64 kernel;
- a physical page-frame allocator built from the Limine memory map;
- a 16 MiB global kernel allocator backed by contiguous physical frames;
- GDT/TSS and IDT exception handling with a dedicated double-fault stack;
- legacy PIC interrupt routing, a 100 Hz PIT clock, and timer ticks;
- live four-level page-table inspection through Limine's higher-half map;
- a readable 24/32-bit framebuffer terminal with scrolling and colors;
- x86-64 feature detection and COM1 diagnostic logging;
- an interrupt-driven PS/2 keyboard with an interactive kernel monitor;
- ACPI platform discovery, APIC/I/O APIC interrupt routing, PCI enumeration,
  and PS/2 mouse packets;
- polling SATA AHCI DMA and legacy IDE PIO block drivers;
- bounded partition devices, MBR/GPT validation, and a write-back block cache;
- a native `no_std` FAT32 reader/writer supporting nested 8.3 directories and
  files;
- validated x86-64 ELF64 load segments, a fixed-capacity process/handle table,
  and timer-driven round-robin scheduling;
- a mount-aware VFS core with canonical path traversal; and
- a real ring-3 `IRETQ` transition and versioned `SYSCALL`/`SYSRET` entry path;
- a freestanding userspace syscall library and bounded shell parser with
  quoting, environment expansion, history, pipelines, and redirection; and
- a 45-command Unix-inspired registry covering file, system, storage, and
  utility commands; and
- a polling xHCI controller with firmware handoff, DMA command/event rings,
  root-port reset, USB device addressing/configuration, descriptor-based class
  discovery, and tested HID, hub, and mass-storage protocol primitives.
- safe image-only `diskutil` and `nex-install` executables with GPT/MBR
  editing, FAT32/NexFS formatting, guided and manual installs, exact-target
  confirmation, transactional replacement, and post-install verification; and
- in-kernel `diskutil` inspection/planning plus a destructive, exactly
  confirmed `nex-install` path for 512-byte-sector AHCI/IDE disks on UEFI
  x86-64 hardware.

Live USB class endpoint I/O, filesystem-backed ring-3 command execution, and
legacy-BIOS installation from inside NexOS remain tracked work. See
[docs/ROADMAP.md](docs/ROADMAP.md).

## Developer setup (Windows)

Open PowerShell in the repository:

```powershell
.\scripts\doctor.ps1
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cargo test
cargo run -p nexosctl -- create-image build\nexfs.img 64
cargo run -p nexosctl -- fs-info build\nexfs.img
cargo run -p nexosctl -- fs-mkdir build\nexfs.img /docs
cargo run -p nexosctl -- fs-put build\nexfs.img README.md /docs/readme.md
cargo run -p nexosctl -- fs-ls build\nexfs.img /docs
cargo run -p nexosctl -- fs-check build\nexfs.img
```

The Windows/Linux host tools only operate on ordinary image files and refuse
raw-device paths. The installer embedded in the release ISO can write a
physical AHCI or IDE disk after an exact `ERASE-diskN` confirmation.
The full NexFS v1 layout and metadata ordering rules are documented in
[docs/NEXFS.md](docs/NEXFS.md).

## Disk utility and installer

Create, inspect, format, and check a combined BIOS/UEFI disk image:

```powershell
cargo run -p nexos-installer --bin diskutil -- guided build\disk.img combined 128 --yes --confirm build\disk.img
cargo run -p nexos-installer --bin diskutil -- inspect build\disk.img
cargo run -p nexos-installer --bin diskutil -- format build\disk.img 3 nexfs --yes --confirm build\disk.img
cargo run -p nexos-installer --bin diskutil -- check build\disk.img 3
```

Install and verify NexOS on an image:

```powershell
cargo run -p nexos-installer --bin nex-install -- --target build\installed.img --kernel target\x86_64-nexos\debug\nexos-kernel --limine vendor\limine\limine-binary --mode combined --yes --confirm build\installed.img
cargo run -p nexos-installer --bin nex-install -- verify build\installed.img
```

Run either executable without arguments for its interactive workflow. See
[docs/INSTALLER.md](docs/INSTALLER.md) for guided/manual modes and safety
behavior.

From the NexOS release ISO, the equivalent real-disk UEFI workflow is:

```text
nexos> lsblk
nexos> diskutil inspect disk0
nexos> diskutil plan disk0
nexos> nex-install disk0 ERASE-disk0
nexos> diskutil verify disk0
```

Replace `disk0` with the exact target shown by `lsblk`. The install command
erases the whole selected disk. It refuses non-512-byte-sector disks, disks
smaller than 128 MiB, the detected live boot disk, missing installer payloads,
and incomplete confirmation tokens. This release installs a UEFI boot path;
Secure Boot must be disabled.

## Kernel build

```powershell
.\scripts\build-kernel.ps1
```

This produces the freestanding kernel ELF. Creating BIOS/UEFI media additionally
uses the pinned Limine binary release:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\fetch-limine.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\build-image.ps1
```

The result is `build\nexos.img`, a verified GPT image containing a BIOS boot
partition, FAT32 EFI System Partition, NexFS root, Limine BIOS stage, and the
standard `EFI\BOOT\BOOTX64.EFI` fallback.

To create a BIOS/UEFI ISO for VMware or real x86-64 hardware, install `xorriso`
and make sure `xorriso.exe` is on `PATH`, then run:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-iso.ps1
```

This creates `build\nexos.iso`. Attach it to a VM as a virtual CD/DVD, burn it
to optical media, or write it to a USB drive with a raw-image tool. Disable
Secure Boot on the target machine. The ISO contains both Limine's legacy BIOS
El Torito image and its x86-64 UEFI image.

## Publishing a development prerelease

After updating the workspace version and adding matching release notes, run:

```powershell
.\scripts\publish-prerelease.ps1
```

The command validates the worktree, runs the host and kernel checks, creates
and pushes the version tag, and then GitHub Actions publishes the ISO, zipped
disk image, and SHA-256 checksum file as a prerelease. A browser and GitHub CLI
are not required.

Boot it in QEMU with a graphical framebuffer and serial output:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\run-qemu.ps1
```

At the `nexos>` prompt, try `help`, `uname`, `meminfo`, `heapinfo`, `heaptest`,
`cpuinfo`, `bootinfo`, `acpi`, `lspci`, `lsusb`, `usbinfo`, `usbtest`, `lsblk`,
`disktest 0`, `uptime`,
`virtinfo`, `ps`, `schedinfo`, `syscalls`, `usertest`, `commands`, `shelltest`,
`vfspath /home/../bin`, `int3`, `clear`, `echo hello`, `reboot`, or `halt`.
`usertest` executes a small ring-3 program that queries ABI v1 with `SYSCALL`
and exits back to the monitor. `disktest` is read-only: it reads LBA 0, reports
its CRC32, and never writes the disk. The Unix-like shell frontend is present,
but the filesystem-backed ring-3 shell is not yet the default prompt.
The monitor accepts input from PS/2/i8042 keyboards and COM1.

For an automated serial-only BIOS smoke test, add `-Headless`. To exercise the
UEFI path, add `-Firmware uefi`.

## License

MIT
