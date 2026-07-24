param(
    [switch]$SkipChecks
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $root 'Cargo.toml'
$releaseWorkflow = Join-Path $root '.github\workflows\prerelease.yml'
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'

Push-Location $root
try {
    $versionMatch = Select-String -LiteralPath $manifest `
        -Pattern '^version = "([^"]+)"$' |
        Select-Object -First 1
    if (-not $versionMatch) {
        throw 'Could not read the workspace version from Cargo.toml.'
    }

    $version = $versionMatch.Matches[0].Groups[1].Value
    if (-not $version.EndsWith('-dev')) {
        throw "Refusing to publish non-development version '$version' as a prerelease."
    }
    if (-not (Test-Path -LiteralPath $releaseWorkflow)) {
        throw 'The GitHub prerelease workflow is missing.'
    }
    if (-not (Test-Path -LiteralPath $cargo)) {
        throw 'Cargo was not found. Run scripts\doctor.ps1 for setup information.'
    }

    $tag = "v$version"
    $notes = Join-Path $root "docs\releases\$tag.md"
    if (-not (Test-Path -LiteralPath $notes)) {
        throw "Release notes are missing: $notes"
    }

    $changes = & git status --porcelain
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    if ($changes) {
        throw 'The worktree must be clean before publishing a prerelease.'
    }

    $branch = (& git branch --show-current).Trim()
    if ($LASTEXITCODE -ne 0 -or -not $branch) {
        throw 'A named Git branch must be checked out before publishing.'
    }

    & git show-ref --verify --quiet "refs/tags/$tag"
    if ($LASTEXITCODE -eq 0) {
        throw "Tag $tag already exists locally."
    }

    $remoteTag = & git ls-remote --tags origin "refs/tags/$tag"
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    if ($remoteTag) {
        throw "Tag $tag already exists on origin."
    }

    if (-not $SkipChecks) {
        & $cargo test --workspace --exclude nexos-kernel
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
        & $cargo clippy --all-targets -- -D warnings
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
        & powershell -NoProfile -ExecutionPolicy Bypass `
            -File (Join-Path $root 'scripts\build-kernel.ps1')
        if ($LASTEXITCODE -ne 0) {
            exit $LASTEXITCODE
        }
    }

    & git push origin $branch
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    & git tag -a $tag -m "NexOS $tag prerelease"
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
    & git push origin $tag
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }

    Write-Host "Pushed $tag. GitHub Actions is building and publishing the prerelease."
} finally {
    Pop-Location
}
