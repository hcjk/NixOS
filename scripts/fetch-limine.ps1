$ErrorActionPreference = 'Stop'

$version = '12.3.2'
$root = Split-Path -Parent $PSScriptRoot
$archive = Join-Path $env:TEMP "limine-binary-$version.zip"
$destination = Join-Path $root 'vendor\limine'
$url = "https://github.com/Limine-Bootloader/Limine/releases/download/v$version/limine-binary.zip"

New-Item -ItemType Directory -Force -Path $destination | Out-Null
curl.exe --proto '=https' --tlsv1.2 -fL $url -o $archive
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
Expand-Archive -LiteralPath $archive -DestinationPath $destination -Force
Write-Host "Limine $version downloaded to $destination"

