$ErrorActionPreference = 'Stop'

$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
$rustc = Join-Path $env:USERPROFILE '.cargo\bin\rustc.exe'
$nasm = Get-ChildItem -Path (Join-Path $env:LOCALAPPDATA 'NexOS\tools') `
    -Filter nasm.exe -Recurse -ErrorAction SilentlyContinue |
    Select-Object -First 1 -ExpandProperty FullName
$qemuCandidates = @(
    'C:\Program Files\qemu\qemu-system-x86_64.exe',
    (Join-Path $env:LOCALAPPDATA 'NexOS\tools\qemu-11.0.0\qemu-system-x86_64.exe')
)
$qemu = $qemuCandidates |
    Where-Object { Test-Path -LiteralPath $_ } |
    Select-Object -First 1

function Report-Tool($name, $path, $argument) {
    if ($path -and (Test-Path -LiteralPath $path)) {
        $version = & $path $argument 2>&1 | Select-Object -First 1
        Write-Host "[ok]      $name - $version" -ForegroundColor Green
    } else {
        Write-Host "[missing] $name" -ForegroundColor Yellow
    }
}

Report-Tool 'rustc' $rustc '--version'
Report-Tool 'cargo' $cargo '--version'
Report-Tool 'NASM' $nasm '-v'
Report-Tool 'QEMU x86-64' $qemu '--version'

if (-not $qemu) {
    Write-Host 'QEMU is optional for host tests but required for boot smoke tests.'
}
