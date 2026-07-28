#!/usr/bin/env python3
"""Boot NexOS with xHCI devices behind a hub and exercise live class I/O."""

from __future__ import annotations

import argparse
import pathlib
import socket
import subprocess
import tempfile
import time


def reserve_tcp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


class QemuSession:
    def __init__(self, command: list[str], serial_port: int, monitor_port: int) -> None:
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
        )
        self.socket = self._connect(serial_port)
        self.monitor = self._connect(monitor_port)
        self.monitor.settimeout(5)
        self._read_monitor_prompt()
        self.transcript = bytearray()

    def _connect(self, port: int) -> socket.socket:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                error = self.process.stderr.read().decode(errors="replace")
                raise RuntimeError(f"QEMU exited before opening serial:\n{error}")
            try:
                connection = socket.create_connection(("127.0.0.1", port), timeout=1)
                connection.settimeout(0.25)
                return connection
            except OSError:
                time.sleep(0.05)
        raise TimeoutError("QEMU did not open its serial console")

    def wait_for(self, marker: bytes, timeout: float) -> None:
        self._wait_for_since(marker, 0, timeout)

    def _wait_for_since(self, marker: bytes, start: int, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while marker not in self.transcript[start:]:
            if self.process.poll() is not None:
                error = self.process.stderr.read().decode(errors="replace")
                raise RuntimeError(
                    f"QEMU exited while waiting for {marker!r}:\n{error}\n{self.tail()}"
                )
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    f"Timed out waiting for {marker!r}\nSerial tail:\n{self.tail()}"
                )
            try:
                chunk = self.socket.recv(4096)
                if chunk:
                    self.transcript.extend(chunk)
                elif self.process.poll() is not None:
                    error = self.process.stderr.read().decode(errors="replace")
                    raise RuntimeError(
                        f"QEMU closed serial while waiting for {marker!r}:\n"
                        f"{error}\n{self.tail()}"
                    )
            except TimeoutError:
                continue
            except OSError as error:
                qemu_error = ""
                if self.process.poll() is not None:
                    qemu_error = self.process.stderr.read().decode(errors="replace")
                raise RuntimeError(
                    f"Serial connection failed while waiting for {marker!r}: {error}\n"
                    f"{qemu_error}\n{self.tail()}"
                ) from error

    def command(self, command: str, marker: bytes, timeout: float) -> None:
        start = len(self.transcript)
        for byte in f"{command}\r".encode("ascii"):
            self.socket.sendall(bytes([byte]))
            time.sleep(0.02)
        self._wait_for_since(marker, start, timeout)

    def hmp(self, command: str) -> None:
        self.monitor.sendall(f"{command}\n".encode("ascii"))
        self._read_monitor_prompt()

    def _read_monitor_prompt(self) -> None:
        response = bytearray()
        while b"(qemu)" not in response:
            chunk = self.monitor.recv(4096)
            if not chunk:
                raise RuntimeError("QEMU monitor closed unexpectedly")
            response.extend(chunk)

    def tail(self) -> str:
        return bytes(self.transcript[-8192:]).decode("utf-8", errors="replace")

    def close(self) -> None:
        try:
            self.socket.close()
            self.monitor.close()
        finally:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=5)

    def __enter__(self) -> QemuSession:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iso", required=True, type=pathlib.Path)
    parser.add_argument(
        "--qemu",
        type=pathlib.Path,
        default=pathlib.Path("qemu-system-x86_64"),
    )
    parser.add_argument(
        "--direct",
        action="store_true",
        help="attach class devices directly to root ports instead of through a hub",
    )
    parser.add_argument("--timeout", type=float, default=240)
    arguments = parser.parse_args()

    iso = arguments.iso.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="nexos-usb-smoke-") as temporary:
        disk = pathlib.Path(temporary) / "usb-storage.img"
        with disk.open("wb") as image:
            image.truncate(256 * 1024 * 1024)

        serial_port = reserve_tcp_port()
        monitor_port = reserve_tcp_port()
        if arguments.direct:
            usb_devices = [
                "-device",
                "usb-kbd,bus=xhci.0,port=1",
                "-device",
                "usb-mouse,bus=xhci.0,port=2",
            ]
            storage_port = "3"
            expected_classes = b"classes: keyboards=1, mice=1, hubs=0, USB disks=1"
        else:
            usb_devices = [
                "-device",
                "usb-hub,id=hub,bus=xhci.0,port=1,ports=8",
                "-device",
                "usb-kbd,bus=xhci.0,port=1.1",
                "-device",
                "usb-mouse,bus=xhci.0,port=1.2",
            ]
            storage_port = "1.3"
            expected_classes = b"classes: keyboards=1, mice=1, hubs=1, USB disks=1"

        command = [
            str(arguments.qemu),
            "-machine",
            "q35,i8042=off",
            "-accel",
            "tcg",
            "-m",
            "512M",
            "-drive",
            f"file={iso},media=cdrom,format=raw,readonly=on",
            "-device",
            "qemu-xhci,id=xhci",
            *usb_devices,
            "-drive",
            f"if=none,id=usbdisk,file={disk},format=raw",
            "-device",
            f"usb-storage,id=usbstorage,bus=xhci.0,port={storage_port},drive=usbdisk",
            "-boot",
            "order=d,strict=on",
            "-serial",
            f"tcp:127.0.0.1:{serial_port},server=on,wait=off",
            "-monitor",
            f"tcp:127.0.0.1:{monitor_port},server=on,wait=off",
            "-display",
            "none",
            "-no-reboot",
        ]
        with QemuSession(command, serial_port, monitor_port) as session:
            session.wait_for(b"nexos>", arguments.timeout)
            start = len(session.transcript)
            for key in "uname":
                session.hmp(f"sendkey {key}")
                time.sleep(0.05)
            session.hmp("sendkey ret")
            session._wait_for_since(b"(independent kernel)", start, arguments.timeout)

            session.hmp("mouse_move 12 7")
            time.sleep(0.25)
            mouse_start = len(session.transcript)
            session.command("mouseinfo", b"USB mouse events=", arguments.timeout)
            mouse_output = bytes(session.transcript[mouse_start:])
            if b"USB mouse events=0" in mouse_output:
                raise RuntimeError(f"USB mouse did not produce an event\n{session.tail()}")

            session.command(
                "usbinfo",
                expected_classes,
                arguments.timeout,
            )
            session.command("lsblk", b"/dev/usb0", arguments.timeout)
            session.command("disktest 0", b"read-only test passed", arguments.timeout)
            session.command(
                "nex-install disk0 ERASE-disk0",
                b"NexOS installation verified",
                arguments.timeout,
            )
            session.command("usbtest", b"passed=true", arguments.timeout)
            session.hmp("device_del usbstorage")
            time.sleep(0.5)
            session.command("usbinfo", b"disconnects=1", arguments.timeout)

    print("xHCI hub, HID, and mass-storage smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
