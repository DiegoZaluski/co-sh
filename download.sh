#!/usr/bin/env bash
set -eu

# Cosh CLI Install Script
#
# Downloads the latest Cosh CLI binary from GitHub releases and installs it.
#
# Supported OS: Linux, macOS (darwin), Windows (MSYS2/Git Bash/WSL)
# Supported Architectures: x86_64 (x64), arm64/aarch64 (arm64)
#   Note: Windows only supports x64
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/DiegoZaluski/cosh/main/download.sh | bash
#
# Environment variables:
#   COSH_BIN_DIR  - Installation directory (default: $HOME/.local/bin, Windows: %USERPROFILE%/cosh)
#   COSH_VERSION  - Specific version to install (e.g., "v0.1.0" or "0.1.0")
#   COSH_VARIANT  - Package variant: "slim" (default, no RAG, no Home) or
#                   "slim-embed" (local RAG via fastembed, no Home)
#   INSTALL_OS    - Override OS detection: "linux", "darwin", or "win32"
#
# Release artifacts are produced by .github/workflows/release.yml on tag push
# (v*) and published at github.com/DiegoZaluski/cosh/releases.

REPO="DiegoZaluski/cosh"
OUT_FILE="cosh"

if [[ "${WINDIR:-}" ]] || [[ "${windir:-}" ]] || [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]]; then
    DEFAULT_BIN_DIR="$USERPROFILE/cosh"
else
    DEFAULT_BIN_DIR="$HOME/.local/bin"
fi

COSH_BIN_DIR="${COSH_BIN_DIR:-$DEFAULT_BIN_DIR}"
COSH_VARIANT="${COSH_VARIANT:-slim}"

if ! command -v curl >/dev/null 2>&1; then
  echo "Error: 'curl' is required to download cosh. Please install curl and try again."
  exit 1
fi

if [ -n "${COSH_VERSION:-}" ]; then
  if [[ ! "$COSH_VERSION" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+(-.*)?$ ]]; then
    echo "Error: invalid version '$COSH_VERSION'. Expected semver format (e.g. 1.0.0 or v1.0.0)"
    exit 1
  fi
  COSH_VERSION="v${COSH_VERSION#v}"
else
  echo "Fetching latest version..."
  LATEST_TAG=$(curl -sL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/')
  if [ -z "$LATEST_TAG" ]; then
    echo "Error: Could not determine latest version. Set COSH_VERSION to specify one."
    exit 1
  fi
  COSH_VERSION="$LATEST_TAG"
fi

# Detect OS
if [ -n "${INSTALL_OS:-}" ]; then
  case "${INSTALL_OS}" in
    linux|darwin|win32) OS="${INSTALL_OS}" ;;
    *) echo "Error: unsupported INSTALL_OS='${INSTALL_OS}' (expected: linux|darwin|win32)"; exit 1 ;;
  esac
else
  if [[ "${WINDIR:-}" ]] || [[ "${windir:-}" ]] || [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "cygwin" ]]; then
    OS="win32"
  elif [[ -f "/proc/version" ]] && grep -q "Microsoft\|WSL" /proc/version 2>/dev/null; then
    OS="linux"
  elif [[ "$OSTYPE" == "darwin"* ]]; then
    OS="darwin"
  elif [[ "$PWD" =~ ^/[a-zA-Z]/ ]] && { [[ -d "/c" ]] || [[ -d "/d" ]] || [[ -d "/e" ]]; }; then
    OS="win32"
  else
    case "$(uname -s | tr '[:upper:]' '[:lower:]')" in
      linux) OS="linux" ;;
      darwin) OS="darwin" ;;
      mingw*|msys*|cygwin*) OS="win32" ;;
      *) echo "Error: Unsupported OS. cosh supports Linux, macOS, and Windows."; exit 1 ;;
    esac
  fi
fi

# Detect architecture
ARCH=$(uname -m)
case "$ARCH" in
  x86_64) ARCH="x64" ;;
  arm64|aarch64) ARCH="arm64" ;;
  *) echo "Error: Unsupported architecture '$ARCH'."; exit 1 ;;
esac

if [ "$OS" = "win32" ] && [ "$ARCH" != "x64" ]; then
  echo "Error: Windows currently only supports x64 architecture."
  exit 1
fi

# Variant note: both variants are built without the Home screen
# (--no-default-features). "slim-embed" additionally enables the workspace
# `embed` feature (cosh-tools/embed + fastembed) for local RAG. Both are
# produced by the release workflow.
case "$COSH_VARIANT" in
  slim|slim-embed) ;;
  *) echo "Error: Unsupported COSH_VARIANT '$COSH_VARIANT'. Expected 'slim' or 'slim-embed'."; exit 1 ;;
esac

SUFFIX="-$COSH_VARIANT"

VERSION="${COSH_VERSION#v}"
if [ "$OS" = "win32" ]; then
  FILE="cosh-v${VERSION}-${OS}-${ARCH}${SUFFIX}.zip"
  EXTRACT_CMD="unzip"
  BINARY="cosh.exe"
else
  FILE="cosh-v${VERSION}-${OS}-${ARCH}${SUFFIX}.tar.gz"
  EXTRACT_CMD="tar"
  BINARY="cosh"
fi

DOWNLOAD_URL="https://github.com/$REPO/releases/download/$COSH_VERSION/$FILE"

echo "Downloading cosh $COSH_VERSION ($COSH_VARIANT variant) for $OS-$ARCH..."
if ! curl -sLf "$DOWNLOAD_URL" --output "$FILE"; then
  echo "Error: Failed to download $DOWNLOAD_URL"
  echo ""
  echo "If you requested a variant, make sure it exists for this release."
  echo "Try setting COSH_VARIANT=slim or removing the environment variable."
  exit 1
fi

TMP_DIR=$(mktemp -d "/tmp/cosh_install_XXXXXX")
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Extracting..."
if [ "$EXTRACT_CMD" = "tar" ]; then
  tar xzf "$FILE" -C "$TMP_DIR"
else
  unzip -q "$FILE" -d "$TMP_DIR"
fi
rm "$FILE"

EXTRACT_DIR="$TMP_DIR"
if [ "$OS" = "win32" ] && [ -d "$TMP_DIR/cosh-package" ]; then
  EXTRACT_DIR="$TMP_DIR/cosh-package"
fi

if [ ! -f "$EXTRACT_DIR/$BINARY" ]; then
  echo "Error: $BINARY not found in extracted archive"
  exit 1
fi

chmod +x "$EXTRACT_DIR/$BINARY" "$EXTRACT_DIR/cosh-uninstall" 2>/dev/null || true

if [ ! -d "$COSH_BIN_DIR" ]; then
  mkdir -p "$COSH_BIN_DIR"
fi

echo "Installing cosh to $COSH_BIN_DIR/"
if [ -f "$COSH_BIN_DIR/$OUT_FILE" ]; then
  mv "$COSH_BIN_DIR/$OUT_FILE" "$COSH_BIN_DIR/$OUT_FILE.old"
  if ! mv "$EXTRACT_DIR/$BINARY" "$COSH_BIN_DIR/$OUT_FILE"; then
    echo "Error: failed to install new binary, restoring previous version"
    mv "$COSH_BIN_DIR/$OUT_FILE.old" "$COSH_BIN_DIR/$OUT_FILE"
    exit 1
  fi
  rm -f "$COSH_BIN_DIR/$OUT_FILE.old"
else
  mv "$EXTRACT_DIR/$BINARY" "$COSH_BIN_DIR/$OUT_FILE"
fi

# The standalone uninstaller ships next to the main binary (cosh::uninstall
# deletes BOTH images from the same directory).
if [ -f "$EXTRACT_DIR/cosh-uninstall" ]; then
  mv -f "$EXTRACT_DIR/cosh-uninstall" "$COSH_BIN_DIR/cosh-uninstall"
fi

if [ "$OS" = "win32" ]; then
  for dll in "$EXTRACT_DIR"/*.dll; do
    if [ -f "$dll" ]; then
      mv "$dll" "$COSH_BIN_DIR/"
    fi
  done
fi

# Check PATH
if [[ ":$PATH:" != *":$COSH_BIN_DIR:"* ]]; then
  echo ""
  echo "Warning: cosh installed, but $COSH_BIN_DIR is not in your PATH."
  echo ""
  SHELL_NAME=$(basename "$SHELL")
  echo "Add it by running:"
  echo "    echo 'export PATH=\"$COSH_BIN_DIR:\$PATH\"' >> ~/.${SHELL_NAME}rc"
  echo "Then reload your shell: source ~/.${SHELL_NAME}rc"
  echo ""
fi

echo "cosh $COSH_VERSION installed successfully at $COSH_BIN_DIR/$OUT_FILE"
