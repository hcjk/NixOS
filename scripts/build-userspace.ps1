$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'

if (-not (Test-Path -LiteralPath $cargo)) {
    throw 'Cargo was not found. Run scripts\doctor.ps1 for setup information.'
}

Push-Location $root
try {
    & $cargo rustc `
        -p nexos-shell `
        --release `
        --target (Join-Path $root 'userspace\x86_64-nexos-user.json') `
        -Z json-target-spec `
        -Z build-std=core,compiler_builtins `
        -Z build-std-features=compiler-builtins-mem `
        -- `
        -C "link-arg=-Tuserspace/linker.ld"
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    Write-Host 'Userspace shell ELF built under target\x86_64-nexos-user\release.'
} finally {
    Pop-Location
}
