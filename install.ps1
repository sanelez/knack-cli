# knack CLI installer — Windows (PowerShell 5.1+).
#
# Usage:
#   irm https://getknack.ai/install.ps1 | iex
#   $env:KNACK_VERSION = '0.2.0'; irm https://getknack.ai/install.ps1 | iex
#   $env:KNACK_BIN_DIR = 'C:\tools\knack'; irm https://getknack.ai/install.ps1 | iex
#
# Detects arch, downloads the matching zip from GitHub Releases, extracts to
# %LOCALAPPDATA%\knack\bin (or $env:KNACK_BIN_DIR), and appends that path to
# the *user* PATH if missing. Idempotent.

[CmdletBinding()]
param(
    [string]$Version = $env:KNACK_VERSION,
    [string]$BinDir  = $env:KNACK_BIN_DIR,
    [string]$Repo    = $(if ($env:KNACK_REPO) { $env:KNACK_REPO } else { 'jordan-gibbs/knack' })
)

$ErrorActionPreference = 'Stop'

if (-not $Version) { $Version = 'latest' }
if (-not $BinDir)  { $BinDir  = Join-Path $env:LOCALAPPDATA 'knack\bin' }

# Detect arch — Windows only ships x86_64 for v1.
$arch = if ([Environment]::Is64BitOperatingSystem) { 'x86_64' } else {
    Write-Error 'knack-install: 32-bit Windows is not supported.'
    exit 1
}
$target = "$arch-pc-windows-msvc"

# Resolve version → tag.
if ($Version -eq 'latest') {
    $latest = Invoke-RestMethod -UseBasicParsing -Uri "https://api.github.com/repos/$Repo/releases/latest"
    $tag = $latest.tag_name
    if (-not $tag) {
        Write-Error "knack-install: couldn't resolve latest tag from $Repo."
        exit 1
    }
} else {
    $tag = "cli-v$Version"
}

$archive = "knack-$target.zip"
$url = "https://github.com/$Repo/releases/download/$tag/$archive"

Write-Host "-> knack $tag for $target"
Write-Host "-> $url"

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP ("knack-install-{0}" -f ([guid]::NewGuid())))
try {
    $zipPath = Join-Path $tmp $archive
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $zipPath
    Expand-Archive -LiteralPath $zipPath -DestinationPath $tmp -Force

    $bin = Get-ChildItem -Path $tmp -Filter 'knack.exe' -Recurse | Select-Object -First 1
    if (-not $bin) {
        Write-Error 'knack-install: knack.exe not found in archive.'
        exit 1
    }

    if (-not (Test-Path $BinDir)) { New-Item -ItemType Directory -Path $BinDir | Out-Null }
    Copy-Item -LiteralPath $bin.FullName -Destination (Join-Path $BinDir 'knack.exe') -Force

    Write-Host "[OK] installed to $BinDir\knack.exe"
} finally {
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp }
}

# Add to user PATH idempotently. We *only* touch user PATH — never machine.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$BinDir*") {
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $BinDir } else { "$BinDir;$userPath" }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host ""
    Write-Host "Added $BinDir to user PATH. Restart your shell for changes to take effect."
}

# Verify — invoke the freshly installed binary directly so we don't depend on
# the new PATH being in this session yet.
& (Join-Path $BinDir 'knack.exe') --version
