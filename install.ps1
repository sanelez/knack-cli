# knack CLI installer for Windows (PowerShell 5.1+).
#
# Usage:
#   irm https://getknack.ai/install.ps1 | iex
#   $env:KNACK_VERSION = '0.2.0'; irm https://getknack.ai/install.ps1 | iex
#   $env:KNACK_BIN_DIR = 'C:\tools\knack'; irm https://getknack.ai/install.ps1 | iex
#
# Detects arch, downloads the matching zip from R2 (cli.getknack.ai by
# default, overridable via $env:KNACK_R2_BASE), extracts to
# %LOCALAPPDATA%\knack\bin (or $env:KNACK_BIN_DIR), and appends that path
# to the *user* PATH if missing. Idempotent.

[CmdletBinding()]
param(
    [string]$Version = $env:KNACK_VERSION,
    [string]$BinDir  = $env:KNACK_BIN_DIR,
    [string]$R2Base  = $(if ($env:KNACK_R2_BASE) { $env:KNACK_R2_BASE } else { 'https://cli.getknack.ai' })
)

$ErrorActionPreference = 'Stop'

if (-not $Version) { $Version = 'latest' }
if (-not $BinDir)  { $BinDir  = Join-Path $env:LOCALAPPDATA 'knack\bin' }

# Detect arch. Windows only ships x86_64 for v1.
$arch = if ([Environment]::Is64BitOperatingSystem) { 'x86_64' } else {
    Write-Error 'knack-install: 32-bit Windows is not supported.'
    exit 1
}
$target = "$arch-pc-windows-msvc"

# Resolve version. R2 holds /cli/latest/version.txt with the current version.
if ($Version -eq 'latest') {
    $resolved = (Invoke-WebRequest -UseBasicParsing -Uri "$R2Base/cli/latest/version.txt").Content.Trim()
    if (-not $resolved) {
        Write-Error "knack-install: couldn't resolve latest version from $R2Base/cli/latest/version.txt"
        exit 1
    }
    $Version = $resolved
}

$archive = "knack-$target.zip"
$url = "$R2Base/cli/v$Version/$archive"

Write-Host "-> knack v$Version for $target"
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

# Add to user PATH idempotently. We *only* touch user PATH, never machine.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$BinDir*") {
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $BinDir } else { "$BinDir;$userPath" }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host ""
    Write-Host "Added $BinDir to user PATH. Restart your shell for changes to take effect."
}

# Verify: invoke the freshly installed binary directly so we don't depend on
# the new PATH being in this session yet.
$knackExe = Join-Path $BinDir 'knack.exe'
& $knackExe --version

# Register knack with the AI agent the user is running in (Claude Code,
# Codex, Cursor, ...). Best-effort: if the detector finds nothing, it writes
# the generic AGENTS.md fallback. Non-fatal — agent integration is a
# courtesy, not a requirement for the CLI to work.
try {
    & $knackExe install --auto | Out-Null
    Write-Host "[OK] registered with detected agent (re-run 'knack install' to refresh)"
} catch {
    Write-Host "  Tip: run 'knack install' to register the CLI with your AI agent."
}

Write-Host ""
Write-Host "To uninstall later: iwr https://cli.getknack.ai/uninstall.ps1 | iex"
