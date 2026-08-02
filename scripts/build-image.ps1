$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
$limine = Join-Path $root 'vendor\limine\limine-binary'
$kernel = Join-Path $root 'target\x86_64-nexos\debug\nexos-kernel'
$shell = Join-Path $root 'target\x86_64-nexos-user\release\nexsh'
$image = Join-Path $root 'build\nexos.img'

if (-not (Test-Path -LiteralPath $limine)) {
    throw 'Limine is missing. Run scripts\fetch-limine.ps1 first.'
}
if (-not (Test-Path -LiteralPath $kernel)) {
    throw 'The kernel is missing. Run scripts\build-kernel.ps1 first.'
}
if (-not (Test-Path -LiteralPath $shell)) {
    throw 'The userspace shell is missing. Run scripts\build-userspace.ps1 first.'
}
if (Test-Path -LiteralPath $image) {
    throw "Refusing to overwrite existing image: $image"
}

Push-Location $root
try {
    & $cargo run -p nexos-installer --bin nex-install -- `
        --target $image `
        --kernel $kernel `
        --shell $shell `
        --limine $limine `
        --size-mib 128 `
        --mode combined `
        --yes `
        --confirm $image
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    Write-Host "Installed and verified BIOS/UEFI image created: $image"
} finally {
    Pop-Location
}
