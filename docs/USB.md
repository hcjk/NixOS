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

NexOS uses bounded polling for the first USB release. Interrupt-driven event
delivery can replace it without changing the higher-level descriptor and class
interfaces.

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

## Class protocol cores

The shared `nexos-usb` crate contains allocation-free implementations for:

- USB device, configuration, interface, and endpoint descriptor parsing;
- deterministic enumeration states and actions;
- xHCI TRB construction, producer-ring cycle toggling, and event consumption;
- boot-protocol keyboard and mouse report decoding;
- hub descriptor, downstream-port iteration, and port-status decoding; and
- USB mass-storage Bulk-Only Transport CBW/CSW validation plus SCSI INQUIRY,
  TEST UNIT READY, REQUEST SENSE, READ CAPACITY(10), READ(10), WRITE(10), and
  SYNCHRONIZE CACHE(10) commands.

## Current boundary

The v0.9.0-dev kernel addresses and configures root-port devices and identifies
their class endpoints. Live interrupt polling for HID, downstream hub device
enumeration, bulk transfer rings for mass storage, hotplug/disconnect recovery,
and exposing USB disks through the storage manager remain follow-up work. Until
those paths are complete, the PS/2 keyboard remains the interactive monitor
input and USB mass-storage disks are identified but not mounted.
