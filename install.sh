#!/bin/sh
set -eu

REPO="Open-Tech-Foundation/release"
BIN_NAME="otf-release"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux) PLATFORM_NAME="linux" ;;
    darwin) PLATFORM_NAME="macos" ;;
    freebsd) PLATFORM_NAME="freebsd" ;;
    *) echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH_NAME="x64" ;;
    aarch64|arm64) ARCH_NAME="arm64" ;;
    *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

PUBLIC_ARCH_NAME="$ARCH_NAME"
case "$ARCH" in
    x86_64|amd64) PUBLIC_ARCH_NAME="x86-64" ;;
esac

ASSET_NAME="${BIN_NAME}-${PLATFORM_NAME}-${PUBLIC_ARCH_NAME}"
LEGACY_ARCH_NAME="$ARCH_NAME"
case "$ARCH" in
    x86_64|amd64) LEGACY_ARCH_NAME="x86_64" ;;
    aarch64|arm64) LEGACY_ARCH_NAME="aarch64" ;;
esac

ASSET_NAMES="$ASSET_NAME ${PLATFORM_NAME}-${ARCH_NAME} ${BIN_NAME}-${PLATFORM_NAME}-${LEGACY_ARCH_NAME}"
case "$OS" in
    darwin) ASSET_NAMES="$ASSET_NAMES darwin-${ARCH_NAME} ${BIN_NAME}-darwin-${LEGACY_ARCH_NAME}" ;;
esac

# Releases ship the binary inside a .tar.gz. Older releases attached the raw binary
# under the same name, so try the archives first and fall back to the bare names —
# that keeps this script working against both old and new releases.
CANDIDATES=""
for candidate in $ASSET_NAMES; do
    CANDIDATES="$CANDIDATES ${candidate}.tar.gz"
done
CANDIDATES="$CANDIDATES $ASSET_NAMES"

# Download to a temp file first. Nothing touches the installed binary until the
# download has been fetched AND verified, so a failed/bogus response can never
# clobber a working install.
TMP_FILE="$(mktemp "${TMPDIR:-/tmp}/${BIN_NAME}.XXXXXX")"
EXTRACT_DIR=""
cleanup() {
    rm -f "$TMP_FILE"
    [ -n "$EXTRACT_DIR" ] && rm -rf "$EXTRACT_DIR"
    return 0
}
trap cleanup EXIT INT TERM

downloaded=false
for candidate in $CANDIDATES; do
    DOWNLOAD_URL="https://github.com/$REPO/releases/latest/download/$candidate"
    echo "Downloading from $DOWNLOAD_URL..."
    if curl -fL -o "$TMP_FILE" "$DOWNLOAD_URL"; then
        downloaded=true
        break
    fi
done
if [ "$downloaded" != true ]; then
    echo "Error: download failed for all known $OS/$ARCH asset names." >&2
    exit 1
fi

# Verify we actually got an executable, not an HTML/JSON error page. This is the
# guard that prevents installing garbage over a good binary.
if [ ! -s "$TMP_FILE" ]; then
    echo "Error: downloaded file is empty; refusing to install." >&2
    exit 1
fi

read_magic() {
    dd if="$1" bs=4 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n'
}

# A .tar.gz asset holds the binary under its own name; unpack it and carry on with
# the extracted file, so the executable check below applies to the real binary.
MAGIC="$(read_magic "$TMP_FILE")"
case "$MAGIC" in
    1f8b*)
        EXTRACT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/${BIN_NAME}-x.XXXXXX")"
        if ! tar -xzf "$TMP_FILE" -C "$EXTRACT_DIR" 2>/dev/null; then
            echo "Error: downloaded archive could not be extracted." >&2
            exit 1
        fi
        EXTRACTED="$EXTRACT_DIR/$BIN_NAME"
        if [ ! -f "$EXTRACTED" ]; then
            EXTRACTED="$(find "$EXTRACT_DIR" -type f -name "$BIN_NAME" 2>/dev/null | head -n 1)"
        fi
        if [ -z "$EXTRACTED" ] || [ ! -f "$EXTRACTED" ]; then
            echo "Error: archive did not contain a '$BIN_NAME' binary." >&2
            exit 1
        fi
        mv "$EXTRACTED" "$TMP_FILE"
        MAGIC="$(read_magic "$TMP_FILE")"
        ;;
esac

case "$MAGIC" in
    7f454c46*) ;;                                          # ELF (Linux)
    feedface*|feedfacf*|cafebabe*|cffaedfe*|cefaedfe*) ;;  # Mach-O (macOS)
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

INSTALL_DIR="/usr/local/bin"
DEST="$INSTALL_DIR/$BIN_NAME"
if [ ! -w "$INSTALL_DIR" ]; then
    echo "Requires sudo to install to $INSTALL_DIR"
    sudo mv "$TMP_FILE" "$DEST"
else
    mv "$TMP_FILE" "$DEST"
fi
trap - EXIT INT TERM  # installed successfully; temp file has been moved

echo "$BIN_NAME installed successfully to $DEST"
