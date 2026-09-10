# Cosh CLI Install Script for Windows PowerShell
#
# Downloads the latest Cosh CLI binary from GitHub releases and installs it.
#
# Supported OS: Windows
# Supported Architectures: x86_64 (x64)
#
# Usage:
#   iwr -Uri "https://raw.githubusercontent.com/DiegoZaluski/cosh/main/download.ps1" -OutFile download.ps1; .\download.ps1
#
# Environment variables:
#   $env:COSH_BIN_DIR  - Installation directory (default: $env:USERPROFILE\cosh)
#   $env:COSH_VERSION  - Specific version to install (e.g., "v0.1.0" or "0.1.0")
#   $env:COSH_VARIANT  - Package variant: "base" (default) or "embed" (includes fastembed/embed support)
#
# TODO(release): this script expects release artifacts named
#   cosh-v<VERSION>-<OS>-<ARCH>[<SUFFIX>].zip published at
#   github.com/DiegoZaluski/cosh/releases. There is no release workflow in this
#   repo yet (no .github/workflows/release.yml) — wire one up before
#   announcing these install commands.

$ErrorActionPreference = "Stop"

$REPO = "DiegoZaluski/cosh"
$OUT_FILE = "cosh.exe"

if (-not $env:COSH_BIN_DIR) {
    $env:COSH_BIN_DIR = Join-Path $env:USERPROFILE "cosh"
}

$COSH_VARIANT = if ($env:COSH_VARIANT) { $env:COSH_VARIANT.ToLowerInvariant() } else { "base" }

# Determine release tag
if ($env:COSH_VERSION) {
    if ($env:COSH_VERSION -notmatch '^v?[0-9]+\.[0-9]+\.[0-9]+(-.*)?$') {
        Write-Error "Invalid version '$env:COSH_VERSION'. Expected semver format (e.g. 1.0.0 or v1.0.0)"
        exit 1
    }
    $RELEASE_TAG = if ($env:COSH_VERSION.StartsWith("v")) { $env:COSH_VERSION } else { "v$env:COSH_VERSION" }
} else {
    Write-Host "Fetching latest version..." -ForegroundColor Yellow
    try {
        $api = Invoke-RestMethod -Uri "https://api.github.com/repos/$REPO/releases/latest" -UseBasicParsing
        $RELEASE_TAG = $api.tag_name
    } catch {
        Write-Error "Could not determine latest version. Set COSH_VERSION to specify one."
        exit 1
    }
}

# Detect architecture
$ARCH = $env:PROCESSOR_ARCHITECTURE
if ($ARCH -eq "AMD64") {
    $ARCH = "x64"
} elseif ($ARCH -eq "ARM64") {
    Write-Error "Windows ARM64 is not currently supported."
    exit 1
} else {
    Write-Error "Unsupported architecture '$ARCH'."
    exit 1
}

# TODO(variant): only "base" and "embed" are planned. "embed" should build with
# the workspace `embed` feature (cosh-tools/embed + fastembed) but there is no
# release pipeline producing that artifact yet. Until then only "base" will
# resolve against published releases.
if ($COSH_VARIANT -ne "base" -and $COSH_VARIANT -ne "embed") {
    Write-Error "Unsupported COSH_VARIANT '$COSH_VARIANT'. Expected 'base' or 'embed'."
    exit 1
}

$VERSION = $RELEASE_TAG.TrimStart("v")
$SUFFIX = if ($COSH_VARIANT -eq "embed") { "-embed" } else { "" }
$FILE = "cosh-v${VERSION}-win32-${ARCH}${SUFFIX}.zip"
$DOWNLOAD_URL = "https://github.com/$REPO/releases/download/$RELEASE_TAG/$FILE"

Write-Host "Downloading cosh $RELEASE_TAG ($COSH_VARIANT variant)..." -ForegroundColor Green

try {
    Invoke-WebRequest -Uri $DOWNLOAD_URL -OutFile $FILE -UseBasicParsing
} catch {
    Write-Error "Failed to download $DOWNLOAD_URL"
    Write-Host ""
    Write-Host "If you requested a variant, make sure it exists for this release." -ForegroundColor Yellow
    Write-Host "Try setting `$env:COSH_VARIANT = 'base' or removing the environment variable." -ForegroundColor Yellow
    exit 1
}

$TMP_DIR = Join-Path $env:TEMP "cosh_install_$(Get-Random)"
try {
    New-Item -ItemType Directory -Path $TMP_DIR -Force | Out-Null
} catch {
    Write-Error "Could not create temporary directory."
    exit 1
}

Write-Host "Extracting..." -ForegroundColor Green
try {
    Expand-Archive -Path $FILE -DestinationPath $TMP_DIR -Force
} catch {
    Write-Error "Failed to extract $FILE."
    Remove-Item -Path $TMP_DIR -Recurse -Force -ErrorAction SilentlyContinue
    exit 1
}
Remove-Item -Path $FILE -Force

$EXTRACT_DIR = $TMP_DIR
if (Test-Path (Join-Path $TMP_DIR "cosh-package")) {
    $EXTRACT_DIR = Join-Path $TMP_DIR "cosh-package"
}

$SOURCE = Join-Path $EXTRACT_DIR "cosh.exe"
if (-not (Test-Path $SOURCE)) {
    Write-Error "cosh.exe not found in extracted archive"
    Remove-Item -Path $TMP_DIR -Recurse -Force -ErrorAction SilentlyContinue
    exit 1
}

if (-not (Test-Path $env:COSH_BIN_DIR)) {
    New-Item -ItemType Directory -Path $env:COSH_BIN_DIR -Force | Out-Null
}

$DEST = Join-Path $env:COSH_BIN_DIR $OUT_FILE
Write-Host "Installing cosh to $DEST" -ForegroundColor Green

if (Test-Path $DEST) {
    Remove-Item -Path $DEST -Force
}
Move-Item -Path $SOURCE -Destination $DEST -Force

# Copy runtime DLLs if any
$DLL_FILES = Get-ChildItem -Path $EXTRACT_DIR -Filter "*.dll" -ErrorAction SilentlyContinue
foreach ($dll in $DLL_FILES) {
    $DEST_DLL = Join-Path $env:COSH_BIN_DIR $dll.Name
    if (Test-Path $DEST_DLL) {
        Remove-Item -Path $DEST_DLL -Force
    }
    Move-Item -Path $dll.FullName -Destination $DEST_DLL -Force
}

Remove-Item -Path $TMP_DIR -Recurse -Force -ErrorAction SilentlyContinue

# PATH check
$CURRENT_PATH = $env:PATH
if ($CURRENT_PATH -notlike "*$env:COSH_BIN_DIR*") {
    Write-Host ""
    Write-Host "Warning: cosh installed, but $env:COSH_BIN_DIR is not in your PATH." -ForegroundColor Yellow
    Write-Host "To add it permanently (user scope, no admin required):" -ForegroundColor Yellow
    Write-Host "    [Environment]::SetEnvironmentVariable('PATH', [Environment]::GetEnvironmentVariable('PATH','User') + ';$env:COSH_BIN_DIR', 'User')" -ForegroundColor Cyan
    Write-Host ""
    Write-Host "For this session only:" -ForegroundColor Yellow
    Write-Host "    `$env:PATH += ';$env:COSH_BIN_DIR'" -ForegroundColor Cyan
    Write-Host ""
}

Write-Host "cosh $RELEASE_TAG installed successfully at $DEST" -ForegroundColor Green
