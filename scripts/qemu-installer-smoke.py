#!/usr/bin/env python3
"""Install NexOS in QEMU and prove the disk boots without its ISO."""

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
                raise RuntimeError(
                    f"QEMU exited before its serial console opened: {self.process.returncode}"
                )
            try:
                connection = socket.create_connection(("127.0.0.1", port), timeout=1)
                connection.settimeout(0.25)
                return connection
            except OSError:
                time.sleep(0.05)
        raise TimeoutError("QEMU did not open its serial console within 30 seconds")

    def wait_for(self, marker: bytes, timeout: float) -> None:
        self.wait_for_since(marker, 0, timeout)

    def wait_for_since(self, marker: bytes, start: int, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while marker not in self.transcript[start:]:
            if self.process.poll() is not None:
                raise RuntimeError(
                    f"QEMU exited while waiting for {marker!r}: {self.process.returncode}\n"
                    f"{self.tail()}"
                )
            if time.monotonic() >= deadline:
                raise TimeoutError(
                    f"Timed out waiting for {marker!r}\nSerial tail:\n{self.tail()}"
                )
            try:
                chunk = self.socket.recv(4096)
                if chunk:
                    self.transcript.extend(chunk)
                    if len(self.transcript) > 2 * 1024 * 1024:
                        del self.transcript[: 1024 * 1024]
                else:
                    time.sleep(0.02)
            except TimeoutError:
                continue

    def type_command(self, command: str) -> None:
        for byte in command.encode("ascii"):
            start = len(self.transcript)
            self.socket.sendall(bytes([byte]))
            # A ring-3 read and write syscall occurs for each byte. Waiting for
            # the echo makes the check independent of QEMU's UART FIFO timing.
            self.wait_for_since(bytes([byte]), start, 10)
            time.sleep(0.08)
        self.socket.sendall(b"\r")

    def tail(self) -> str:
        return bytes(self.transcript[-4096:]).decode("utf-8", errors="replace")

    def wait_for_exit(self, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while self.process.poll() is None and time.monotonic() < deadline:
            try:
                chunk = self.socket.recv(4096)
                if chunk:
                    self.transcript.extend(chunk)
            except (TimeoutError, OSError):
                pass
        if self.process.poll() is None:
            raise TimeoutError(f"QEMU did not exit after shutdown\n{self.tail()}")
        if self.process.returncode != 0:
            raise RuntimeError(f"QEMU exited with {self.process.returncode}\n{self.tail()}")

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


def qemu_command(
    qemu: pathlib.Path,
    disk: pathlib.Path,
    port: int,
    *,
    iso: pathlib.Path | None = None,
    firmware: pathlib.Path | None = None,
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
        command += [
            "-drive",
            f"if=pflash,format=raw,readonly=on,file={firmware}",
        ]
    if iso is not None:
        command += [
            "-drive",
            f"file={iso},media=cdrom,format=raw,readonly=on",
        ]
    command += [
        "-drive",
        f"file={disk},format=raw,if=ide",
        "-boot",
        f"order={'d' if iso is not None else 'c'},strict=on",
        "-serial",
        f"tcp:127.0.0.1:{port},server=on,wait=off",
        "-monitor",
        "none",
        "-display",
        "none",
        "-no-reboot",
    ]
    return command


def boot_and_require_prompt(
    qemu: pathlib.Path,
    disk: pathlib.Path,
    firmware: pathlib.Path | None,
    label: str,
    timeout: float,
) -> None:
    port = reserve_tcp_port()
    command = qemu_command(qemu, disk, port, firmware=firmware)
    with QemuSession(command, port) as session:
        session.wait_for(b"nexsh>", timeout)
        if b"rootfs: launching /bin/nexsh" not in session.transcript:
            raise RuntimeError(f"{label} boot did not launch the NexFS userspace shell")
        if b"SMP: requested=4, registered=4, online=4" not in session.transcript:
            raise RuntimeError(f"{label} boot did not start all four CPUs")
        session.type_command("uname")
        session.wait_for(b"NexOS 0.15.0-dev x86_64 (ring-3 userspace)", timeout)
        session.type_command("cat /etc/nexos-release")
        session.wait_for(b"VERSION=0.15.0-dev", timeout)
        session.type_command("ls /")
        session.wait_for(b"home/", timeout)
        session.type_command("smpinfo")
        session.wait_for(b"registered=4 online=4", timeout)
        session.type_command("uptime")
        session.wait_for(b"seconds", timeout)
        session.type_command("shutdown")
        session.wait_for(b"Requesting ACPI shutdown", timeout)
        session.wait_for_exit(20)
        print(
            f"{label} installed-disk boot exercised /bin/nexsh and ACPI shutdown"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iso", required=True, type=pathlib.Path)
    parser.add_argument("--uefi-firmware", required=True, type=pathlib.Path)
    parser.add_argument(
        "--qemu",
        type=pathlib.Path,
        default=pathlib.Path("qemu-system-x86_64"),
    )
    parser.add_argument("--timeout", type=float, default=240)
    arguments = parser.parse_args()

    iso = arguments.iso.resolve(strict=True)
    firmware = arguments.uefi_firmware.resolve(strict=True)
    qemu = arguments.qemu

    with tempfile.TemporaryDirectory(prefix="nexos-installer-smoke-") as temporary:
        disk = pathlib.Path(temporary) / "installed.img"
        with disk.open("wb") as image:
            image.truncate(256 * 1024 * 1024)

        install_port = reserve_tcp_port()
        install_command = qemu_command(qemu, disk, install_port, iso=iso)
        with QemuSession(install_command, install_port) as session:
            session.wait_for(b"nexos>", arguments.timeout)
            session.type_command("nex-install disk0 ERASE-disk0")
            session.wait_for(b"100%  9/9 installation complete", arguments.timeout)
            session.wait_for(b"NexOS installation verified", arguments.timeout)
            print("guided installation completed and verified")

        boot_and_require_prompt(
            qemu,
            disk,
            None,
            "SeaBIOS",
            arguments.timeout,
        )
        boot_and_require_prompt(
            qemu,
            disk,
            firmware,
            "UEFI",
            arguments.timeout,
        )

    print("installer regression smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
