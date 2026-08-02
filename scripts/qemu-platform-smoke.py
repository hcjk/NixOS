#!/usr/bin/env python3
"""Verify SMP, HPET, PCI ECAM, and ACPI S5 under BIOS and UEFI."""

from __future__ import annotations

import argparse
import pathlib
import socket
import subprocess
import time


def reserve_tcp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


class QemuSession:
    def __init__(self, command: list[str], port: int) -> None:
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.socket = self._connect(port)
        self.transcript = bytearray()

    def _connect(self, port: int) -> socket.socket:
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError("QEMU exited before opening its serial console")
            try:
                connection = socket.create_connection(("127.0.0.1", port), timeout=1)
                connection.settimeout(0.25)
                return connection
            except OSError:
                time.sleep(0.05)
        raise TimeoutError("QEMU did not open its serial console")

    def wait_for(self, marker: bytes, timeout: float, start: int = 0) -> None:
        deadline = time.monotonic() + timeout
        while marker not in self.transcript[start:]:
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"QEMU exited while waiting for {marker!r}: {self.process.returncode}\n"
                    f"{self.tail()}"
                )
            if time.monotonic() >= deadline:
                raise TimeoutError(f"Timed out waiting for {marker!r}\n{self.tail()}")
            try:
                chunk = self.socket.recv(4096)
                if chunk:
                    self.transcript.extend(chunk)
            except TimeoutError:
                continue

    def command(self, command: str, marker: bytes, timeout: float) -> None:
        start = len(self.transcript)
        for byte in command.encode("ascii"):
            echo_start = len(self.transcript)
            self.socket.sendall(bytes([byte]))
            self.wait_for(bytes([byte]), 10, echo_start)
            time.sleep(0.04)
        self.socket.sendall(b"\r")
        self.wait_for(marker, timeout, start)

    def require_exit_after(self, command: str, marker: bytes, timeout: float) -> None:
        self.command(command, marker, timeout)
        deadline = time.monotonic() + 20
        while self.process.poll() is None and time.monotonic() < deadline:
            try:
                chunk = self.socket.recv(4096)
                if chunk:
                    self.transcript.extend(chunk)
            except (TimeoutError, OSError):
                pass
        if self.process.poll() is None:
            raise TimeoutError(f"{command} did not stop QEMU\n{self.tail()}")
        if self.process.returncode != 0:
            raise RuntimeError(f"QEMU exited with {self.process.returncode}\n{self.tail()}")

    def tail(self) -> str:
        return bytes(self.transcript[-4096:]).decode("utf-8", errors="replace")

    def close(self) -> None:
        try:
            self.socket.close()
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


def verify_boot(
    qemu: pathlib.Path,
    iso: pathlib.Path,
    firmware: pathlib.Path | None,
    label: str,
    timeout: float,
) -> None:
    port = reserve_tcp_port()
    command = qemu_command(qemu, iso, firmware, port)
    with QemuSession(command, port) as session:
        session.wait_for(b"nexos>", timeout)
        for marker in (
            b"SMP: requested=4, registered=4, online=4",
            b"HPET: active",
            b"functions discovered via ACPI ECAM",
            b"FADT power=true",
        ):
            if marker not in session.transcript:
                raise RuntimeError(f"{label} missing {marker!r}\n{session.tail()}")
        session.command("smpinfo", b"cpu3:", timeout)
        session.command("acpi", b"S5=yes", timeout)
        session.command("lspci", b"access=ACPI ECAM", timeout)
        session.command("uptime", b"source=HPET", timeout)
        session.require_exit_after("shutdown", b"Powering off through ACPI S5", timeout)
    print(f"{label} SMP/HPET/ECAM/ACPI shutdown passed")


def verify_reset(
    qemu: pathlib.Path,
    iso: pathlib.Path,
    firmware: pathlib.Path | None,
    label: str,
    timeout: float,
) -> None:
    port = reserve_tcp_port()
    with QemuSession(qemu_command(qemu, iso, firmware, port), port) as session:
        session.wait_for(b"nexos>", timeout)
        session.require_exit_after(
            "reboot", b"Rebooting through ACPI reset with i8042 fallback", timeout
        )
    print(f"{label} ACPI reset passed")


def qemu_command(
    qemu: pathlib.Path,
    iso: pathlib.Path,
    firmware: pathlib.Path | None,
    port: int,
) -> list[str]:
    command = [
        str(qemu),
        "-machine",
        "q35",
        "-accel",
        "tcg",
        "-smp",
        "4",
        "-m",
        "512M",
    ]
    if firmware is not None:
        command += ["-drive", f"if=pflash,format=raw,readonly=on,file={firmware}"]
    command += [
        "-drive",
        f"file={iso},media=cdrom,format=raw,readonly=on",
        "-boot",
        "order=d,strict=on",
        "-serial",
        f"tcp:127.0.0.1:{port},server=on,wait=off",
        "-monitor",
        "none",
        "-display",
        "none",
        "-no-reboot",
    ]
    return command


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iso", required=True, type=pathlib.Path)
    parser.add_argument("--uefi-firmware", required=True, type=pathlib.Path)
    parser.add_argument(
        "--qemu", type=pathlib.Path, default=pathlib.Path("qemu-system-x86_64")
    )
    parser.add_argument("--timeout", type=float, default=120)
    arguments = parser.parse_args()

    iso = arguments.iso.resolve(strict=True)
    firmware = arguments.uefi_firmware.resolve(strict=True)
    verify_boot(arguments.qemu, iso, None, "SeaBIOS", arguments.timeout)
    verify_boot(arguments.qemu, iso, firmware, "UEFI", arguments.timeout)
    verify_reset(arguments.qemu, iso, None, "SeaBIOS", arguments.timeout)
    verify_reset(arguments.qemu, iso, firmware, "UEFI", arguments.timeout)
    print("platform regression smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
