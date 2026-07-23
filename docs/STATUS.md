# NexOS status

Last verified: 2026-07-24

## Working

- Freestanding, higher-half x86-64 Rust kernel.
- Limine memory-map, HHDM, framebuffer, and ACPI RSDP requests.
- COM1 serial diagnostics and a color framebuffer terminal.
- Physical frame allocation with the first 1 MiB reserved.
- Four-level x86-64 page translation plus 4 KiB map/unmap primitives.
- Uncached MMIO mappings for the local APIC and I/O APIC.
- A 512 KiB physical-frame-backed heap with aligned variable-size allocation,
  deallocation, adjacent-free-block coalescing, and reuse.
- GDT, 64-bit TSS, IDT exception handlers, and a dedicated double-fault stack.
- ACPI 1.0 RSDT and ACPI 2.0+ XSDT parsing with checksum validation.
- MADT processor, local-APIC, I/O-APIC, and interrupt-override discovery.
- HPET and PCI MCFG table discovery.
- Local APIC and I/O APIC routing for PIT, PS/2 keyboard, and PS/2 mouse IRQs.
- Automatic legacy PIC fallback when APIC initialization is unavailable.
- 100 Hz PIT monotonic ticks and uptime reporting.
- Interrupt-driven i8042 keyboard and three-byte PS/2 mouse packets.
- PCI configuration mechanism 1 enumeration across all buses and functions.
- Interactive `acpi`, `lspci`, `irqinfo`, `mouseinfo`, `heapinfo`,
  `heapstats`, `heaptest`, `virtinfo`, and `maptest` diagnostics.
- Combined MBR/FAT32 disk image with BIOS and UEFI Limine boot paths.
- Host-testable syscall ABI, block-device interfaces, MBR/GPT validation, and
  NexFS v1 superblock formatting/checking.
- Safe image-only `nexosctl`; raw physical disks remain intentionally refused.

## Emulator verification

The same milestone-4 kernel booted under QEMU 11.0.0 through legacy BIOS and
UEFI. BIOS used the ACPI RSDT path; UEFI validated the extended RSDP and XSDT.
Both selected APIC/I/O APIC interrupt routing and accepted IRQ-driven keyboard
input while the PIT advanced.

```text
NexOS 0.4.0-dev x86-64
ACPI: rev 0, 5 tables, root=RSDT, CPUs=1, IOAPIC=true, HPET=true, MCFG=true
PCI: 6 functions discovered
interrupts: GDT/TSS/IDT online, APIC/I/O APIC, PIT 100 Hz,
            PS/2 keyboard=true, mouse=true
APIC: local id=0 v0x14, IOAPIC id=0 v0x20, redirections=24
nexos> heaptest
heap ok: checksum=6112, released=true, reused=true
nexos> maptest
maptest: phys=0x183000, translated=0x183000, unmapped=true, passed=true
nexos> mouseinfo
mouse events=1, last dx=24, dy=-15, buttons=0b000
```

UEFI reported ACPI revision 2 with six XSDT entries and also passed `maptest`
while PIT uptime and IRQ keyboard input remained active.

## Not implemented yet

Page-table frame reclamation, a Rust `GlobalAlloc` adapter, HPET clock use, PCI
ECAM access, power-off through the FADT, SMP, AHCI, IDE, FAT32, full NexFS file
operations, processes, syscalls, VFS, userspace, shell, USB, and physical-disk
installation remain later milestones. The current prompt is a kernel monitor,
not yet the planned Unix-like userspace shell.
