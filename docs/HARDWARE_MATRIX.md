# NexOS x86-64 compatibility matrix

Last updated: 2026-08-08 for v0.15.0-dev.

## Release-blocking emulation

| Platform | Firmware | Storage | Input | Status |
| --- | --- | --- | --- | --- |
| QEMU Q35 | SeaBIOS | AHCI, 512-byte | PS/2 and COM1 | Automated install and no-ISO boot |
| QEMU Q35 | OVMF UEFI | AHCI, 512-byte | PS/2 and COM1 | Automated install and no-ISO boot |
| QEMU Q35 | SeaBIOS/UEFI | xHCI BOT/SCSI | USB HID only | Automated hub, install, I/O, disconnect |
| QEMU Q35 | SeaBIOS | NVMe, 512-byte | COM1 | Automated discovery, I/O, reset, stress |
| QEMU Q35 | OVMF UEFI | NVMe, 4Kn | COM1 | Automated install and no-ISO boot |
| QEMU `pc` | SeaBIOS | IDE PIO, 512-byte | PS/2 and COM1 | Boot/read regression |

## Physical-hardware reporting

Physical machines vary substantially in firmware, ACPI, PCI layout, USB
controllers, and storage behavior. A configuration is not marked verified
until its exact result is reported. Use this table when submitting a result:

| Field | Value |
| --- | --- |
| Computer/mainboard | Vendor and model |
| CPU | Model and logical CPU count |
| Firmware | BIOS/UEFI vendor and version; Secure Boot state |
| Storage controller | PCI vendor/device ID and mode |
| Disk | Model, transport, capacity, logical sector size |
| USB controller/input | PCI ID and tested devices |
| Display mode | Resolution, pitch, RGB/BGR, bits per pixel |
| NexOS result | ISO boot, install, no-ISO boot, commands tested |
| Serial log | Attach the complete COM1 transcript |

The expected v0.15 hardware envelope is x86-64 with NX/SSE2, legacy BIOS or
UEFI with Secure Boot disabled, an ACPI-described platform, framebuffer or
COM1 output, PS/2 or xHCI boot-HID input, and NVMe namespace 1, AHCI SATA,
legacy IDE, or xHCI BOT/SCSI storage. NVMe metadata/protection information,
multiple namespaces, UAS, RAID, Secure Boot, and live controller hot-plug are
outside this release.
