param(
    [ValidatePattern('^[A-Za-z0-9._-]+$')]
    [string]$ArtifactName = 'nexos'
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$buildDirectory = Join-Path $root 'build'
$kernel = Join-Path $root 'target\x86_64-nexos\debug\nexos-kernel'
$limine = Join-Path $root 'vendor\limine\limine-binary'
$limineTool = Join-Path $limine 'limine-tool-windows-x86\limine.exe'
$iso = Join-Path $buildDirectory "$ArtifactName.iso"
$stagingName = if ($ArtifactName -eq 'nexos') {
    'iso-root'
} else {
    "$ArtifactName-iso-root"
}
$staging = Join-Path $buildDirectory $stagingName

$xorrisoCommand = Get-Command xorriso -ErrorAction SilentlyContinue
$xorrisoCandidates = @(
    'C:\Program Files\xorriso\xorriso.exe',
    'C:\tools\msys64\usr\bin\xorriso.exe',
    'C:\msys64\usr\bin\xorriso.exe',
    (Join-Path $env:LOCALAPPDATA `
        'NexOS\tools\xorriso-1.5.2\xorriso-exe-for-windows-master\xorriso.exe')
)
$xorriso = if ($xorrisoCommand) {
    $xorrisoCommand.Source
} else {
    $xorrisoCandidates |
        Where-Object { Test-Path -LiteralPath $_ } |
        Select-Object -First 1
}

if (-not $xorriso) {
    throw 'xorriso was not found. Install xorriso and ensure xorriso.exe is on PATH.'
}
if (-not (Test-Path -LiteralPath $limineTool)) {
    throw 'Limine is missing. Run scripts\fetch-limine.ps1 first.'
}
if (-not (Test-Path -LiteralPath $kernel)) {
    throw 'The kernel is missing. Run scripts\build-kernel.ps1 first.'
}
if (Test-Path -LiteralPath $iso) {
    throw "Refusing to overwrite existing ISO: $iso"
}
if (Test-Path -LiteralPath $staging) {
    throw "Refusing to reuse existing ISO staging directory: $staging"
}

$limineFiles = @(
    'limine-bios.sys',
    'limine-bios-hdd.h',
    'limine-bios-cd.bin',
    'limine-uefi-cd.bin',
    'BOOTX64.EFI'
)
foreach ($file in $limineFiles) {
    if (-not (Test-Path -LiteralPath (Join-Path $limine $file))) {
        throw "Required Limine file is missing: $file"
    }
}

$bootDirectory = Join-Path $staging 'boot\limine'
$efiDirectory = Join-Path $staging 'EFI\BOOT'
New-Item -ItemType Directory -Force -Path $bootDirectory, $efiDirectory | Out-Null

Copy-Item -LiteralPath $kernel -Destination (Join-Path $staging 'boot\nexos-kernel')
Copy-Item -LiteralPath (Join-Path $limine 'limine-bios.sys') -Destination $bootDirectory
Copy-Item -LiteralPath (Join-Path $limine 'limine-bios-cd.bin') -Destination $bootDirectory
Copy-Item -LiteralPath (Join-Path $limine 'limine-uefi-cd.bin') -Destination $bootDirectory
Copy-Item -LiteralPath (Join-Path $limine 'BOOTX64.EFI') -Destination $efiDirectory

$hddHeader = Get-Content -Raw -LiteralPath (Join-Path $limine 'limine-bios-hdd.h')
$hddMatches = [regex]::Matches($hddHeader, '0x([0-9a-fA-F]{2})')
if ($hddMatches.Count -lt 513) {
    throw 'Limine HDD stage header did not contain a valid boot image.'
}
$hddBytes = [byte[]]::new($hddMatches.Count)
for ($index = 0; $index -lt $hddMatches.Count; $index++) {
    $hddBytes[$index] = [Convert]::ToByte($hddMatches[$index].Groups[1].Value, 16)
}
[IO.File]::WriteAllBytes((Join-Path $bootDirectory 'limine-bios-hdd.bin'), $hddBytes)

$configuration = @'
timeout: 0

/NexOS
    protocol: limine
    path: boot():/boot/nexos-kernel
    module_path: boot():/EFI/BOOT/BOOTX64.EFI
    module_string: nexos-bootx64
    module_path: boot():/boot/limine/limine-bios-hdd.bin
    module_string: nexos-limine-hdd
    module_path: boot():/boot/limine/limine-bios.sys
    module_string: nexos-limine-bios
'@
Set-Content -LiteralPath (Join-Path $staging 'limine.conf') `
    -Value $configuration -Encoding Ascii

function ConvertTo-XorrisoPath([string]$Path) {
    if ($xorriso -like '*xorriso-exe-for-windows*' -and
        $Path -match '^([A-Za-z]):\\(.*)$') {
        $drive = $Matches[1].ToLowerInvariant()
        $remainder = $Matches[2] -replace '\\', '/'
        return "/cygdrive/$drive/$remainder"
    }
    return $Path
}

$xorrisoStaging = ConvertTo-XorrisoPath $staging
$xorrisoIso = ConvertTo-XorrisoPath $iso

& $xorriso -as mkisofs `
    -R -r -J `
    -b 'boot/limine/limine-bios-cd.bin' `
    -no-emul-boot -boot-load-size 4 -boot-info-table `
    -hfsplus -apm-block-size 2048 `
    --efi-boot 'boot/limine/limine-uefi-cd.bin' `
    -efi-boot-part --efi-boot-image --protective-msdos-label `
    $xorrisoStaging -o $xorrisoIso
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

& $limineTool bios-install $iso
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Host "Bootable BIOS/UEFI ISO created: $iso"
Write-Host 'Secure Boot must be disabled on the target computer.'
