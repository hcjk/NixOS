# NexOS disk utility and installer

## Install from the NexOS release ISO

Milestone 11 adds a guided installer directly to the NexOS kernel monitor. It
targets x86-64 BIOS or UEFI machines with Secure Boot disabled, an AHCI or
legacy IDE target disk, and 512-byte logical sectors. The disk must be at least
128 MiB.

Boot the release ISO, then inspect every detected disk:

```text
nexos> lsblk
nexos> diskutil inspect disk0
nexos> diskutil plan disk0
```

The plan prints the exact target, capacity, and partitions that will be
created. To accept complete destruction of that disk, enter the exact command
it prints:

```text
nexos> nex-install disk0 ERASE-disk0
```

Do not copy that example without checking `lsblk`; the correct target may be
`disk1` or another number. A successful install ends with
`NexOS installation verified`. An optional second read-back is:

```text
nexos> diskutil verify disk0
```

Remove the ISO and reboot from the installed disk. The installer writes
Limine's MBR/HDD stages into the reserved BIOS partition and also installs the
UEFI fallback loader at `EFI/BOOT/BOOTX64.EFI`.

The in-OS safety checks refuse a partial/mismatched confirmation, the detected
live boot disk, missing installer payloads, unsupported sector sizes,
undersized devices, malformed writes, and failed read-back verification. The
release ISO carries its kernel and UEFI loader as read-only Limine modules;
ordinary installed boots intentionally omit the loader module and cannot start
another destructive install.

The in-OS installer is whole-disk. It does not preserve or resize partitions,
support NVMe/USB mass-storage/RAID/4Kn targets, or provide rollback after
writes begin.

During installation, NexOS displays a nine-step progress bar covering target
validation, GPT creation, FAT32 formatting, file copying, BIOS-stage
installation, NexFS formatting, cache flushing, byte verification, and
completion.

## Host image tools

Milestone 10 also provides two host-side Rust executables for safe development
and virtual-machine disk images:

- `diskutil` creates, inspects, partitions, formats, and checks images.
- `nex-install` installs a bootable NexOS system into an image.

These host executables deliberately reject raw devices such as
`\\.\PhysicalDrive0`, `\\.\GLOBALROOT\...`, `/dev/sda`, and `/dev/nvme0n1`.
Physical-disk installation is available only through the guarded in-OS UEFI
workflow above.

## Safety model

Every destructive command requires both:

1. `--yes`
2. `--confirm <complete-target-name>`

The confirmation must exactly match the target argument. New contents are
written to a sibling `.nexos-new` image and verified before commit. When an
existing target is replaced, it is first renamed to `.nexos-backup`; a failed
post-commit partition-table check restores the original.

The tools refuse symlink targets, non-regular existing targets, unaligned
images, stale temporary/backup files, invalid or overlapping partition
layouts, undersized partitions, and unsupported filesystems.

## `diskutil`

Run without arguments for the interactive menu, or use explicit subcommands:

```text
diskutil inspect <image>
diskutil create <image> <size-MiB> --yes --confirm <image>
diskutil guided <image> <combined|uefi|bios> <size-MiB> --yes --confirm <image>
diskutil table <image> <gpt|mbr> --yes --confirm <image>
diskutil partition-create <image> <bios|esp|nexfs|data> <first-LBA> <sectors> <name> --yes --confirm <image>
diskutil partition-delete <image> <index> --yes --confirm <image>
diskutil format <image> <index> <fat32|nexfs> --yes --confirm <image>
diskutil check <image> <index>
```

Guided layouts use 1 MiB alignment:

- `combined`: GPT with a 1 MiB BIOS boot partition, 32 MiB FAT32 ESP, and
  remaining space for NexFS.
- `uefi`: GPT with a 32 MiB FAT32 ESP and remaining space for NexFS.
- `bios`: MBR with a bootable 32 MiB FAT32 boot partition and remaining space
  for NexFS.

Manual partition editing does not resize or move partitions.

## `nex-install`

Build the kernel and fetch Limine first:

```powershell
.\scripts\build-userspace.ps1
.\scripts\build-kernel.ps1
.\scripts\fetch-limine.ps1
```

A guided combined install:

```powershell
cargo run -p nexos-installer --bin nex-install -- `
  --target build\installed.img `
  --kernel target\x86_64-nexos\debug\nexos-kernel `
  --shell target\x86_64-nexos-user\release\nexsh `
  --limine vendor\limine\limine-binary `
  --size-mib 128 `
  --mode combined `
  --yes `
  --confirm build\installed.img
```

Manual mode formats only the selected ESP and root partitions:

```powershell
cargo run -p nexos-installer --bin nex-install -- `
  --manual `
  --target build\manual.img `
  --esp 2 `
  --root 3 `
  --bios `
  --kernel target\x86_64-nexos\debug\nexos-kernel `
  --shell target\x86_64-nexos-user\release\nexsh `
  --limine vendor\limine\limine-binary `
  --yes `
  --confirm build\manual.img
```

Use `--bios` in manual mode when the existing image has a Limine-compatible
BIOS boot layout. Omitting it performs a UEFI-only manual install.

Verify an installation without modifying it:

```powershell
cargo run -p nexos-installer --bin nex-install -- verify build\installed.img
```

## Installed layout

The FAT32 boot filesystem contains:

- `/EFI/BOOT/BOOTX64.EFI`
- `/boot/limine-bios.sys`
- `/boot/nexos-kernel`
- `/boot/nexsh`
- `/limine.conf`

The NexFS root contains `/etc/nexos-release`, `/etc/fstab`,
`/system/kernel-location`, `/system/install-manifest`, and the optimized
`/bin/nexsh` ELF, along with the initial `/var` and `/home` directories. The
manifest records the partition indexes plus kernel and shell sizes and CRC32s.
The larger debug kernel remains only on the boot filesystem because NexFS v1's
single-indirect layout limits individual files to roughly 2 MiB.

The installer checks all ten metadata/boot files, runs the NexFS consistency
checker, verifies the root UUID, flushes writes, and reports success only after
the final committed image passes validation.
