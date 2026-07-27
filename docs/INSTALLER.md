# NexOS disk utility and installer

Milestone 10 provides two host-side Rust executables for safe development and
virtual-machine disk images:

- `diskutil` creates, inspects, partitions, formats, and checks images.
- `nex-install` installs a bootable NexOS system into an image.

They do not yet run inside the ring-3 NexOS shell and they deliberately reject
raw devices such as `\\.\PhysicalDrive0`, `\\.\GLOBALROOT\...`, `/dev/sda`, and
`/dev/nvme0n1`. Physical-disk installation remains disabled until the kernel
has filesystem-backed userspace and stronger mounted-device/active-I/O checks.

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
.\scripts\build-kernel.ps1
.\scripts\fetch-limine.ps1
```

A guided combined install:

```powershell
cargo run -p nexos-installer --bin nex-install -- `
  --target build\installed.img `
  --kernel target\x86_64-nexos\debug\nexos-kernel `
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
- `/limine.conf`

The NexFS root contains `/etc/nexos-release`, `/etc/fstab`,
`/system/kernel-location`, and `/system/install-manifest`, along with the
initial `/bin`, `/var`, and `/home` directories. The manifest records the
partition indexes, kernel size, and kernel CRC32. The kernel is stored only on
the boot filesystem because the current NexFS v1 single-indirect layout limits
individual files to roughly 2 MiB.

The installer checks all eight metadata/boot files, runs the NexFS consistency
checker, verifies the root UUID, flushes writes, and reports success only after
the final committed image passes validation.
