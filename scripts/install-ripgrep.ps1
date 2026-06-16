<#
.SYNOPSIS
    Ensures ripgrep (rg.exe) is installed, for Librarian's recursive search.

.DESCRIPTION
    Librarian uses ripgrep as its search engine. This script provisions it on
    machines that don't already have it. It is a standalone helper intended to be
    bundled and run by the SEPARATE Librarian installer project as a pre-install
    step; it is NOT part of the application build and the app does not invoke it.

    Behaviour:
      1. If rg.exe is already discoverable (on PATH, or in the install dir, or in
         a known winget/scoop/choco location), it does nothing unless -Force.
      2. Otherwise it resolves the latest ripgrep release from GitHub (falling
         back to a pinned version if the GitHub API is unreachable), downloads the
         x86_64-pc-windows-msvc archive, extracts rg.exe into -InstallDir, and
         (unless -NoPath) adds that directory to the current user's PATH.

    The default install directory matches the first location Librarian probes at
    runtime (%LOCALAPPDATA%\Programs\ripgrep), so search works immediately.

.PARAMETER InstallDir
    Directory to install rg.exe into. Default: %LOCALAPPDATA%\Programs\ripgrep.

.PARAMETER Force
    Reinstall even if ripgrep is already present.

.PARAMETER NoPath
    Don't modify the user PATH (e.g. when the installer adds it itself, or when
    placing rg.exe next to the Librarian executable instead).

.PARAMETER Version
    Install a specific ripgrep version (e.g. "15.1.0") instead of the latest.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\install-ripgrep.ps1

.NOTES
    Requires Windows PowerShell 5.1+ or PowerShell 7+. No admin rights needed for
    the default per-user install location.
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\ripgrep'),
    [switch]$Force,
    [switch]$NoPath,
    [string]$Version
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ripgrep target triple this script provisions, and a pinned fallback used only
# if the GitHub "latest release" lookup fails (offline, rate-limited, etc.).
$Triple = 'x86_64-pc-windows-msvc'
$FallbackVersion = '15.1.0'
$Repo = 'BurntSushi/ripgrep'

function Write-Step([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Ok([string]$Message)   { Write-Host "    $Message" -ForegroundColor Green }
function Write-Warn2([string]$Message) { Write-Host "    $Message" -ForegroundColor Yellow }

# Return the path to an existing rg.exe if one is discoverable, else $null. Mirrors
# the locations Librarian itself probes at runtime.
function Find-Ripgrep {
    param([string]$InstallDir)

    $onPath = Get-Command rg.exe -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }

    $candidates = @(
        (Join-Path $InstallDir 'rg.exe'),
        (Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Links\rg.exe'),
        (Join-Path $env:USERPROFILE 'scoop\shims\rg.exe'),
        'C:\ProgramData\chocolatey\bin\rg.exe'
    )
    foreach ($c in $candidates) {
        if ($c -and (Test-Path -LiteralPath $c)) { return $c }
    }
    return $null
}

# Resolve the download URL + version for the requested (or latest) release.
function Resolve-Release {
    param([string]$Version)

    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

    if ($Version) {
        $tag = $Version
    }
    else {
        try {
            Write-Step "Querying GitHub for the latest ripgrep release..."
            $headers = @{ 'User-Agent' = 'librarian-install-ripgrep' }
            $rel = Invoke-RestMethod -Headers $headers -Uri "https://api.github.com/repos/$Repo/releases/latest"
            $tag = $rel.tag_name

            # Prefer the asset URL straight from the release metadata when present.
            $asset = $rel.assets | Where-Object { $_.name -like "*$Triple.zip" } | Select-Object -First 1
            if ($asset) {
                return [pscustomobject]@{ Version = $tag; Url = $asset.browser_download_url }
            }
        }
        catch {
            Write-Warn2 "GitHub lookup failed ($($_.Exception.Message)); using pinned v$FallbackVersion."
            $tag = $FallbackVersion
        }
    }

    $name = "ripgrep-$tag-$Triple.zip"
    $url = "https://github.com/$Repo/releases/download/$tag/$name"
    return [pscustomobject]@{ Version = $tag; Url = $url }
}

# Append $Dir to the current user's PATH (persisted + this session), if missing.
function Add-ToUserPath {
    param([string]$Dir)

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @()
    if ($userPath) { $parts = $userPath -split ';' | Where-Object { $_ -ne '' } }

    if ($parts -contains $Dir) {
        Write-Ok "Already on the user PATH."
        return
    }

    $newPath = (($parts + $Dir) -join ';')
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    # Reflect it in the current session too, so a follow-on step sees rg.
    $env:Path = "$env:Path;$Dir"
    Write-Ok "Added to the user PATH (open a new terminal to pick it up globally)."
}

# --- main ---------------------------------------------------------------------

$existing = Find-Ripgrep -InstallDir $InstallDir
if ($existing -and -not $Force) {
    $rgVersion = (& $existing --version | Select-Object -First 1)
    Write-Step "ripgrep is already installed: $existing"
    Write-Ok $rgVersion
    Write-Ok "Nothing to do. Re-run with -Force to reinstall."
    return
}

$release = Resolve-Release -Version $Version
Write-Step "Installing ripgrep $($release.Version) ($Triple)"

$tempDir = Join-Path ([IO.Path]::GetTempPath()) ("librarian-rg-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
$zipPath = Join-Path $tempDir 'ripgrep.zip'

try {
    Write-Step "Downloading $($release.Url)"
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    # Progress rendering makes Invoke-WebRequest dramatically slower; suppress it.
    $oldProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        Invoke-WebRequest -Uri $release.Url -OutFile $zipPath -UseBasicParsing
    }
    finally {
        $ProgressPreference = $oldProgress
    }

    Write-Step "Extracting..."
    Expand-Archive -Path $zipPath -DestinationPath $tempDir -Force

    $rg = Get-ChildItem -Path $tempDir -Recurse -Filter 'rg.exe' | Select-Object -First 1
    if (-not $rg) {
        throw "rg.exe not found in the downloaded archive."
    }

    if (-not (Test-Path -LiteralPath $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    }
    $destExe = Join-Path $InstallDir 'rg.exe'
    Copy-Item -LiteralPath $rg.FullName -Destination $destExe -Force

    # Bring along the license/readme that ship alongside, where present.
    foreach ($extra in @('LICENSE-MIT', 'UNLICENSE', 'COPYING', 'README.md')) {
        $src = Get-ChildItem -Path $tempDir -Recurse -Filter $extra -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($src) { Copy-Item -LiteralPath $src.FullName -Destination $InstallDir -Force }
    }

    Write-Ok "Installed: $destExe"
    $rgVersion = (& $destExe --version | Select-Object -First 1)
    Write-Ok $rgVersion

    if (-not $NoPath) {
        Add-ToUserPath -Dir $InstallDir
    }

    Write-Step "ripgrep is ready for Librarian search."
}
finally {
    Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
}
