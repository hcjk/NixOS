# NexOS implementation status

Legend: **done**, *foundation*, planned.

1. **Workspace, ABI, higher-half kernel ELF, BIOS/UEFI boot, serial output**
2. **Framebuffer terminal, physical frame allocator, CPUID, PS/2 keyboard
   polling, interactive kernel monitor, block-device API**
3. **GDT/TSS/IDT, exception handlers, legacy PIC/PIT interrupts,
   interrupt-driven PS/2 keyboard, early kernel heap, page-table inspection**
4. **Virtual mapping/unmapping, reclaiming heap, ACPI RSDT/XSDT/MADT/
   HPET/MCFG discovery, APIC/I/O APIC routing, PCI mechanism 1, PS/2 mouse**
5. **MBR/GPT validation, AHCI, IDE, block cache, FAT32**
6. **NexFS fixed inodes/directories, direct and indirect file blocks,
   allocation, mutation operations, dirty-mount protocol, offline checker**
7. **Process table, ELF64 loader, SYSCALL/SYSRET, VFS core, timer-driven
   round-robin scheduler**
8. **`no_std` userspace syscall runtime, shell syntax/environment/history,
   pipeline/redirection model, and initial Unix-like command registry**
9. *xHCI controller/rings, root-port reset, device enumeration, configuration,
   descriptor parsing, and HID/hub/mass-storage protocol cores*
10. **Safe image tooling, interactive disk utility, guided/manual NexOS
    installer, and BIOS/UEFI installed-image verification**
11. *UEFI physical-disk installation, global allocator, serial-console input,
    native FAT32 formatting/LFN boot configuration, and hardware safeguards*

## Completed foundation acceptance

- `cargo test` passes on the host.
- The custom x86-64 kernel target links without an operating-system runtime.
- Invalid block ranges, malformed MBR/GPT data, corrupt NexFS metadata, and
  raw Windows disk targets are rejected.
- A valid NexFS disk image can be created and inspected.
- When packaged with Limine, the kernel writes diagnostics to COM1 and displays
  a readable framebuffer terminal.
- BIOS and UEFI builds accept PS/2 keyboard input and execute kernel-monitor
  commands.
- The physical frame allocator reserves low memory and returns page-aligned
  frames from usable memory-map entries.
- A 100 Hz PIT interrupt advances monotonic ticks and IRQ1 feeds the keyboard
  queue under both BIOS and UEFI.
- Breakpoint and page-fault exceptions enter documented IDT handlers; double
  faults have a dedicated TSS interrupt stack.
- The kernel can allocate and verify memory from a physical-frame-backed heap
  and inspect active four-level page-table mappings.
- ACPI checksums and table layouts are validated under BIOS RSDT and UEFI XSDT
  boot paths; MADT interrupt topology, HPET, and MCFG data are discovered.
- The local APIC and I/O APIC route PIT, keyboard, and mouse interrupts, with a
  legacy PIC fallback when platform discovery or APIC setup is unavailable.
- The heap supports aligned variable-size allocations, deallocation,
  coalescing, reuse, and fragmentation statistics.
- A scratch virtual page can be mapped, translated, written, unmapped, and
  invalidated; PCI configuration mechanism 1 discovers device functions.
- IRQ12 mouse packets are decoded into signed motion and button events.
- PCI AHCI controllers use polling DMA commands for IDENTIFY, LBA28/LBA48
  reads, writes, and cache flushes, with bounded timeouts and task-file error
  detection.
- Legacy PCI IDE controllers use master/slave discovery and polling PIO
  IDENTIFY, reads, writes, and cache flushes.
- The shared storage crate provides bounded partition devices, a write-back
  LRU sector cache, complete primary-MBR/GPT validation, and a native `no_std`
  FAT32 reader/writer with nested 8.3 directories.
- NexFS v1 persists fixed-size inodes and directory entries, supports nested
  directories plus create/read/write/rename/truncate/remove operations, and
  addresses files through twelve direct blocks plus one indirect block.
- NexFS writable mounts mark the volume dirty before changes and only restore
  the clean flag after ordered flushes; the offline checker rejects dirty
  volumes, duplicate block ownership, leaked blocks, corrupt entries, and
  orphaned inodes.
- `nexosctl` can create and inspect NexFS images and exercise directory, file
  import, listing, rename, truncate, removal, and consistency-check operations
  without accepting raw physical-disk paths.
- QEMU BIOS and UEFI tests discover the boot disk through AHCI; legacy `pc`
  emulation discovers the same image through IDE. Both pass a read-only LBA 0
  checksum test and identify the expected MBR partition.
- Ring-3 test code enters through `IRETQ`, obtains the ABI version through the
  x86-64 `SYSCALL` entry, exits through the process syscall, and returns to the
  kernel monitor with interrupt state restored.
- The fixed-capacity process table tracks parents, current directories,
  handles, address spaces, exit status, and process limits.
- The timer-driven scheduler implements round-robin quanta, sleeping, blocking,
  wait queues, wakeups, exit/reap, and observable scheduling decisions.
- The ELF64 loader rejects unsupported architectures, invalid bounds,
  non-user addresses, invalid alignment, and writable-executable segments
  before presenting validated load segments to an address-space target.
- The VFS core normalizes absolute and relative paths, resolves `.` and `..`,
  selects the longest mount prefix, traverses filesystem nodes, and defines
  file, directory, block-device, and character-device interfaces.
- The userspace crate issues the versioned x86-64 syscall convention without a
  standard library and translates negative kernel results into shared errors.
- The shell frontend supports single/double quotes, escapes, environment
  expansion, bounded history, pipelines, input redirection, overwrite/append
  output redirection, and rejects malformed syntax without partial execution.
- The initial registry documents 45 built-in, file, system, storage, and
  utility commands, including `diskutil` and `nex-install`.
- QEMU xHCI controllers complete reset, run, No-Op, Enable Slot, Address
  Device, and endpoint-zero control transfers. Connected USB 2 and USB 3
  devices are addressed, configured, and classified from their descriptors.
- `lsusb`, `usbinfo`, and `usbtest` expose controller capabilities, root-port
  state, VID/PID, device/interface classes, endpoint counts, and command-ring
  health.
- The reusable `no_std` USB crate validates descriptor chains, enumeration
  transitions, HID boot reports, one-level hub status, xHCI TRB cycle rules,
  and mass-storage BOT/SCSI packets.
- `diskutil` creates and inspects GPT/MBR image layouts, adds and removes
  partitions, formats FAT32 and NexFS, and checks supported filesystems.
- `nex-install` supports guided combined, UEFI, and BIOS layouts plus manual
  partition selection. Destructive commands require `--yes` and an exact
  target-name confirmation.
- Installer writes are prepared and verified in a sibling image before an
  atomic replacement. Existing manual-mode partitions not selected for
  formatting remain byte-for-byte intact.
- The installed GPT image contains a BIOS boot partition, FAT32 ESP, and
  NexFS root; Limine, the kernel, release metadata, fstab, UUID, checksums, and
  an install manifest are verified after commit.
- The Milestone 10 installed image reaches the kernel monitor under QEMU Q35
  through both legacy BIOS and UEFI firmware.
- The Milestone 11 release ISO embeds its running kernel and UEFI loader as
  read-only Limine payloads. `diskutil` previews a three-partition GPT layout,
  and `nex-install diskN ERASE-diskN` writes FAT32/NexFS filesystems, installs
  the UEFI fallback loader and kernel, flushes every layer, and verifies the
  installed bytes before success.
- A disposable 256 MiB AHCI disk was installed from the UEFI ISO, verified
  again with `diskutil verify`, detached from the ISO, and booted back to the
  NexOS monitor through its installed `EFI/BOOT/BOOTX64.EFI`.
- Installed systems omit the UEFI installer module, preventing an ordinary
  installed boot from becoming a self-erasing installation environment.
- COM1 is now an input as well as diagnostic console, and the kernel uses a
  16 MiB coalescing global allocator for filesystem and installer operations.

The complete v1 described in the product plan is a long-running systems
project. Physical hardware support in Milestone 11 is deliberately limited to
UEFI, AHCI/legacy IDE, 512-byte logical sectors, framebuffer output, and
PS/2/i8042 or COM1 input. SMP, live USB HID/mass-storage transfers, NVMe,
legacy-BIOS installation from NexOS, and filesystem-backed ring-3 userspace
remain later stabilization work.
