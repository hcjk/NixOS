#!/usr/bin/env python3
"""Exercise NexOS NVMe discovery, I/O, recovery, and 4Kn media."""

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

    def tail(self) -> str:
        return bytes(self.transcript[-8192:]).decode("utf-8", errors="replace")

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
    sector_size: int,
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
        command += ["-drive", f"if=pflash,format=raw,readonly=on,file={firmware}"]
    if iso is not None:
        command += ["-drive", f"file={iso},media=cdrom,format=raw,readonly=on"]
    namespace_options = (
        "nvme-ns,drive=nvmedisk,bus=nvme,nsid=1,"
        f"logical_block_size={sector_size},physical_block_size={sector_size}"
    )
    if iso is None:
        namespace_options += ",bootindex=1"
    command += [
        "-drive",
        f"file={disk},format=raw,if=none,id=nvmedisk",
        "-device",
        "nvme,id=nvme,serial=NEXOSNVME",
        "-device",
        namespace_options,
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


def verify_media(
    qemu: pathlib.Path,
    iso: pathlib.Path,
    directory: pathlib.Path,
    sector_size: int,
    timeout: float,
) -> None:
    disk = directory / f"nvme-{sector_size}.img"
    with disk.open("wb") as image:
        image.truncate(256 * 1024 * 1024)
    port = reserve_tcp_port()
    with QemuSession(
        qemu_command(qemu, disk, port, sector_size, iso=iso), port
    ) as session:
        session.wait_for(b"nexos>", timeout)
        for marker in (
            b"NVMe probe: controllers=1",
            b"mapped=1, namespaces=1",
            f"x {sector_size} bytes".encode("ascii"),
        ):
            if marker not in session.transcript:
                raise RuntimeError(f"NVMe {sector_size} missing {marker!r}\n{session.tail()}")
        session.command("lsblk", b"/dev/nvme0", timeout)
        session.command("disktest 0", b"read-only test passed", timeout)
        session.command("diskstress 0 256", b"read stress passed: 256 rounds", timeout)
        session.command(
            "diskutil recover disk0",
            b"controller reset and I/O queues recreated",
            timeout,
        )
        session.command("disktest 0", b"read-only test passed", timeout)
        session.command("shutdown", b"Powering off through ACPI S5", timeout)
    print(f"NVMe {sector_size}-byte media and recovery passed")


def verify_four_kn_install(
    qemu: pathlib.Path,
    iso: pathlib.Path,
    firmware: pathlib.Path,
    directory: pathlib.Path,
    timeout: float,
) -> None:
    disk = directory / "nvme-4096-installed.img"
    with disk.open("wb") as image:
        image.truncate(768 * 1024 * 1024)
    install_port = reserve_tcp_port()
    with QemuSession(
        qemu_command(
            qemu,
            disk,
            install_port,
            4096,
            iso=iso,
            firmware=firmware,
        ),
        install_port,
    ) as session:
        session.wait_for(b"nexos>", timeout)
        session.command(
            "nex-install disk0 ERASE-disk0",
            b"100%  9/9 installation complete",
            timeout,
        )
        session.wait_for(b"NexOS installation verified", timeout)

    boot_port = reserve_tcp_port()
    with QemuSession(
        qemu_command(qemu, disk, boot_port, 4096, firmware=firmware), boot_port
    ) as session:
        session.wait_for(b"nexsh>", timeout)
        for marker in (
            b"rootfs: launching /bin/nexsh",
            b"x 4096 bytes",
        ):
            if marker not in session.transcript:
                raise RuntimeError(f"4Kn installed boot missing {marker!r}\n{session.tail()}")
        session.command("uname", b"NexOS 0.15.0-dev x86_64", timeout)
        session.command("cat /etc/nexos-release", b"VERSION=0.15.0-dev", timeout)
        session.command("shutdown", b"Requesting ACPI shutdown", timeout)
    print("NVMe 4Kn UEFI installation and no-ISO boot passed")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iso", required=True, type=pathlib.Path)
    parser.add_argument("--uefi-firmware", required=True, type=pathlib.Path)
    parser.add_argument(
        "--qemu", type=pathlib.Path, default=pathlib.Path("qemu-system-x86_64")
    )
    parser.add_argument("--timeout", type=float, default=180)
    arguments = parser.parse_args()
    iso = arguments.iso.resolve(strict=True)
    firmware = arguments.uefi_firmware.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="nexos-nvme-smoke-") as temporary:
        directory = pathlib.Path(temporary)
        verify_media(arguments.qemu, iso, directory, 512, arguments.timeout)
        verify_media(arguments.qemu, iso, directory, 4096, arguments.timeout)
        verify_four_kn_install(
            arguments.qemu, iso, firmware, directory, arguments.timeout
        )
    print("NVMe and 4Kn regression smoke test passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
