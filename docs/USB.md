# NexOS USB architecture

Milestone 9 introduces an original `no_std` USB stack and an xHCI hardware
driver. It does not use Linux drivers or a Linux ABI.

## Controller bring-up

The kernel discovers PCI class `0c:03:30`, maps BAR 0 uncached, requests
firmware ownership through the xHCI extended-capability chain, halts and resets
the controller, and validates 4 KiB page support. It then allocates and
programs:

- the Device Context Base Address Array (DCBAA);
- a 256-entry command ring with a cycle-toggling Link TRB;
- a 256-entry event ring and one-entry Event Ring Segment Table;
- controller scratchpads when requested by `HCSPARAMS2`; and
- per-device input/output contexts, endpoint-zero transfer rings, and control
  buffers.

NexOS polls the event ring without blocking the monitor. HID and hub interrupt
TRBs remain queued while control and bulk commands use bounded synchronous
waits. Interrupt-driven delivery can later replace polling without changing
the class interfaces.

## Enumeration

For every connected root port, the kernel powers the port when required,
resets USB 2 ports, issues Enable Slot and Address Device commands, and performs
standard endpoint-zero control transfers. It reads the device descriptor and
complete configuration descriptor, validates every descriptor boundary, then
issues `SET_CONFIGURATION`.

The `lsusb` command reports the assigned address, slot, root port, speed,
VID/PID, USB version, device class, interface and endpoint counts, and detected
boot-HID, hub, or Bulk-Only mass-storage interfaces. `usbinfo` exposes
controller capabilities and enumeration counters. `usbtest` submits another
No-Op command and requires a successful Command Completion Event.

## Live class I/O

The shared `nexos-usb` crate contains allocation-free implementations for:

- USB device, configuration, interface, and endpoint descriptor parsing;
- deterministic enumeration states and actions;
- xHCI TRB construction, producer-ring cycle toggling, and event consumption;
- boot-protocol keyboard and mouse report decoding;
- hub descriptor, downstream-port iteration, and port-status decoding; and
- USB mass-storage Bulk-Only Transport CBW/CSW validation plus SCSI INQUIRY,
  TEST UNIT READY, REQUEST SENSE, READ CAPACITY(10), READ(10), WRITE(10), and
  SYNCHRONIZE CACHE(10) commands.

The kernel binds the first compatible endpoint set for each interface:

- Boot keyboards and mice use persistent nonblocking interrupt-IN transfers.
- Hubs are powered, reset, and enumerated to one downstream level, including
  route strings and transaction-translator parent information.
- Bulk-Only devices use CBW/data/CSW transactions with SCSI INQUIRY,
  READ CAPACITY(10), READ(10), WRITE(10), and SYNCHRONIZE CACHE(10).
- USB disks implement the common block-device interface, appear as
  `/dev/usbN`, and can be inspected or selected by `diskutil` and
  `nex-install`.
- Root and downstream disconnects retire their slots and make later block I/O
  fail cleanly instead of touching removed hardware.

## Current boundary

Milestone 12 intentionally supports xHCI, boot-protocol HID, one external hub
level, BOT/SCSI LUN zero, READ/WRITE(10), and media whose logical blocks fit in
the 4 KiB transfer buffer. Reconnecting or attaching new devices after boot,
multiple LUNs, UAS, USB audio, and EHCI/OHCI/UHCI remain future work.
