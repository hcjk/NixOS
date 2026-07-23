$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
$limine = Join-Path $root 'vendor\limine\limine-binary'
$limineTool = Join-Path $limine 'limine-tool-windows-x86\limine.exe'
$kernel = Join-Path $root 'target\x86_64-nexos\debug\nexos-kernel'
$image = Join-Path $root 'build\nexos.img'

if (-not (Test-Path -LiteralPath $limineTool)) {
    throw 'Limine is missing. Run scripts\fetch-limine.ps1 first.'
}
if (-not (Test-Path -LiteralPath $kernel)) {
    throw 'The kernel is missing. Run scripts\build-kernel.ps1 first.'
}
if (Test-Path -LiteralPath $image) {
    throw "Refusing to overwrite existing image: $image"
}

Push-Location $root
try {
    & $cargo run -p nexosctl -- boot-image $image $kernel $limine 128
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    & $limineTool bios-install $image
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    Write-Host "Bootable BIOS/UEFI image created: $image"
} finally {
    Pop-Location
}

