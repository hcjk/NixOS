param(
    [ValidateSet('bios', 'uefi')]
    [string]$Firmware = 'bios',
    [switch]$Headless
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$image = Join-Path $root 'build\nexos.img'
$qemuCandidates = @(
    'C:\Program Files\qemu\qemu-system-x86_64.exe',
    (Join-Path $env:LOCALAPPDATA 'NexOS\tools\qemu-11.0.0\qemu-system-x86_64.exe')
)
$qemu = $qemuCandidates |
    Where-Object { Test-Path -LiteralPath $_ } |
    Select-Object -First 1

if (-not $qemu) {
    throw 'qemu-system-x86_64.exe was not found. Run scripts\doctor.ps1.'
}
if (-not (Test-Path -LiteralPath $image)) {
    throw 'build\nexos.img is missing. Run scripts\build-image.ps1.'
}

$arguments = @(
    '-machine', 'q35',
    '-m', '256M',
    '-drive', "file=$image,format=raw,if=ide",
    '-serial', 'stdio',
    '-monitor', 'none',
    '-no-reboot'
)

if ($Firmware -eq 'uefi') {
    $firmwarePath = Join-Path (Split-Path -Parent $qemu) 'share\edk2-x86_64-code.fd'
    if (-not (Test-Path -LiteralPath $firmwarePath)) {
        $firmwarePath = Join-Path (Split-Path -Parent $qemu) 'share\qemu\edk2-x86_64-code.fd'
    }
    if (-not (Test-Path -LiteralPath $firmwarePath)) {
        throw 'QEMU UEFI firmware was not found.'
    }
    $arguments += @('-drive', "if=pflash,format=raw,readonly=on,file=$firmwarePath")
}

if ($Headless) {
    $arguments += @('-display', 'none')
}

& $qemu @arguments

