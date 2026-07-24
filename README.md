# NexOS

NexOS is an original, Unix-inspired x86-64 operating system written in Rust
and assembly. It is not based on Linux and does not provide Linux binary
compatibility.

The repository currently implements the first six engineering milestones:

- a versioned kernel/userspace ABI;
- host-testable block-device, MBR, and GPT validation code;
- the NexFS v1 formatter, fixed-size inodes, nested directories, direct and
  indirect file blocks, allocation bitmaps, dirty-mount protocol, and offline
  consistency checker;
- a safe disk-image utility (`nexosctl`);
- a Limine-aware, higher-half `no_std` x86-64 kernel;
- a physical page-frame allocator built from the Limine memory map;
- a 256 KiB early kernel heap backed by contiguous physical frames;
- GDT/TSS and IDT exception handling with a dedicated double-fault stack;
- legacy PIC interrupt routing, a 100 Hz PIT clock, and timer ticks;
- live four-level page-table inspection through Limine's higher-half map;
- a readable 24/32-bit framebuffer terminal with scrolling and colors;
- x86-64 feature detection and COM1 diagnostic logging; and
- an interrupt-driven PS/2 keyboard with an interactive kernel monitor.
- ACPI platform discovery, APIC/I/O APIC interrupt routing, PCI enumeration,
  and PS/2 mouse packets;
- polling SATA AHCI DMA and legacy IDE PIO block drivers;
- bounded partition devices, MBR/GPT validation, and a write-back block cache;
  and
- a native `no_std` FAT32 reader/writer supporting nested 8.3 directories and
  files.

Processes, the userspace shell, USB, and installation onto physical disks
remain tracked milestones. See [docs/ROADMAP.md](docs/ROADMAP.md).

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

`nexosctl` only operates on ordinary image files in this milestone. It refuses
Windows raw-device paths so an unfinished installer cannot erase a real disk.
The full NexFS v1 layout and metadata ordering rules are documented in
[docs/NEXFS.md](docs/NEXFS.md).

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

The result is `build\nexos.img`, an MBR-partitioned FAT32 disk image containing
both the Limine BIOS stage and the standard `EFI\BOOT\BOOTX64.EFI` fallback.

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
`cpuinfo`, `bootinfo`, `acpi`, `lspci`, `lsblk`, `disktest 0`, `uptime`,
`virtinfo`, `int3`, `clear`, `echo hello`, `reboot`, or `halt`. `disktest` is
read-only: it reads LBA 0, reports its CRC32, and never writes the disk. This is
an early kernel monitor; the Unix-like userspace shell is a later milestone.

For an automated serial-only BIOS smoke test, add `-Headless`. To exercise the
UEFI path, add `-Firmware uefi`.

## License

MIT
