# NexOS

NexOS is an original, Unix-inspired x86-64 operating system written in Rust
and assembly. It is not based on Linux and does not provide Linux binary
compatibility.

The repository currently implements the first two engineering milestones:

- a versioned kernel/userspace ABI;
- host-testable block-device, MBR, and GPT validation code;
- the NexFS v1 superblock, formatter, and checker;
- a safe disk-image utility (`nexosctl`);
- a Limine-aware, higher-half `no_std` x86-64 kernel;
- a physical page-frame allocator built from the Limine memory map;
- a readable 24/32-bit framebuffer terminal with scrolling and colors;
- x86-64 feature detection and COM1 diagnostic logging; and
- a polling PS/2 keyboard driver with an interactive kernel monitor.

Interrupts, storage drivers, processes, the userspace shell, USB, and
installation onto physical disks remain tracked milestones. See
[docs/ROADMAP.md](docs/ROADMAP.md).

## Developer setup (Windows)

Open PowerShell in the repository:

```powershell
.\scripts\doctor.ps1
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cargo test
cargo run -p nexosctl -- create-image build\nexfs.img 64
cargo run -p nexosctl -- fs-info build\nexfs.img
```

`nexosctl` only operates on ordinary image files in this milestone. It refuses
Windows raw-device paths so an unfinished installer cannot erase a real disk.

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

Boot it in QEMU with a graphical framebuffer and serial output:

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\run-qemu.ps1
```

At the `nexos>` prompt, try `help`, `uname`, `meminfo`, `cpuinfo`, `bootinfo`,
`clear`, `echo hello`, `reboot`, or `halt`. This is an early kernel monitor;
the Unix-like userspace shell is a later milestone.

For an automated serial-only BIOS smoke test, add `-Headless`. To exercise the
UEFI path, add `-Firmware uefi`.

## License

MIT
