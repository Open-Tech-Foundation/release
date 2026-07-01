#!/bin/sh
set -eu

REPO="Open-Tech-Foundation/release"
BIN_NAME="otf-release"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux) OS_NAME="linux" ;;
    darwin) OS_NAME="macos" ;;
    mingw*|msys*|cygwin*) OS_NAME="windows" ;;
    *) echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH_NAME="x86_64" ;;
    aarch64|arm64) ARCH_NAME="aarch64" ;;
    *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

EXE=""
if [ "$OS_NAME" = "windows" ]; then
    EXE=".exe"
fi

ASSET_NAME="${BIN_NAME}-${OS_NAME}-${ARCH_NAME}${EXE}"

# Download to a temp file first. Nothing touches the installed binary until the
# download has been fetched AND verified, so a failed/bogus response can never
# clobber a working install.
TMP_FILE="$(mktemp "${TMPDIR:-/tmp}/${BIN_NAME}.XXXXXX")"
cleanup() { rm -f "$TMP_FILE"; }
trap cleanup EXIT INT TERM

DOWNLOAD_URL="https://github.com/$REPO/releases/latest/download/$ASSET_NAME"

echo "Downloading from $DOWNLOAD_URL..."
if ! curl -fL -o "$TMP_FILE" "$DOWNLOAD_URL"; then
    echo "Error: download failed from $DOWNLOAD_URL" >&2
    exit 1
fi

# Verify we actually got an executable, not an HTML/JSON error page. This is the
# guard that prevents installing garbage over a good binary.
if [ ! -s "$TMP_FILE" ]; then
    echo "Error: downloaded file is empty; refusing to install." >&2
    exit 1
fi

MAGIC="$(dd if="$TMP_FILE" bs=4 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')"
case "$MAGIC" in
    7f454c46*) ;;                                          # ELF (Linux)
    feedface*|feedfacf*|cafebabe*|cffaedfe*|cefaedfe*) ;;  # Mach-O (macOS)
    4d5a*) ;;                                              # PE/COFF (Windows)
    *)
        echo "Error: downloaded file is not an executable binary." >&2
        case "$MAGIC" in
            7b*) echo "       Got a JSON/API response instead (likely a rate-limit or error page)." >&2 ;;
            3c*) echo "       Got an HTML page instead." >&2 ;;
        esac
        echo "       Refusing to install; your existing $BIN_NAME is left untouched." >&2
        exit 1
        ;;
esac

chmod +x "$TMP_FILE"

if [ "$OS_NAME" = "windows" ]; then
    INSTALL_DIR="${CARGO_HOME:-$HOME/.cargo}/bin"
    mkdir -p "$INSTALL_DIR"
else
    INSTALL_DIR="/usr/local/bin"
fi

DEST="$INSTALL_DIR/$BIN_NAME$EXE"
if [ "$OS_NAME" != "windows" ] && [ ! -w "$INSTALL_DIR" ]; then
    echo "Requires sudo to install to $INSTALL_DIR"
    sudo mv "$TMP_FILE" "$DEST"
else
    mv "$TMP_FILE" "$DEST"
fi
trap - EXIT INT TERM  # installed successfully; temp file has been moved

if [ -n "${GITHUB_PATH:-}" ]; then
    if command -v cygpath >/dev/null 2>&1; then
        cygpath -w "$INSTALL_DIR" >> "$GITHUB_PATH"
    else
        printf '%s\n' "$INSTALL_DIR" >> "$GITHUB_PATH"
    fi
fi

echo "$BIN_NAME installed successfully to $DEST"
