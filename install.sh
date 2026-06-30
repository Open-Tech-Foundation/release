#!/bin/sh
set -eu

REPO="Open-Tech-Foundation/release"
BIN_NAME="otf-release"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux) OS_NAME="linux" ;;
    darwin) OS_NAME="macos" ;;
    *) echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH_NAME="x86_64" ;;
    aarch64|arm64) ARCH_NAME="aarch64" ;;
    *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

ASSET_NAME="${BIN_NAME}-${OS_NAME}-${ARCH_NAME}"

# Download to a temp file first. Nothing touches the installed binary until the
# download has been fetched AND verified, so a failed/bogus response can never
# clobber a working install.
TMP_FILE="$(mktemp "${TMPDIR:-/tmp}/${BIN_NAME}.XXXXXX")"
cleanup() { rm -f "$TMP_FILE"; }
trap cleanup EXIT INT TERM

echo "Fetching latest version of $BIN_NAME..."
API_URL="https://api.github.com/repos/$REPO/releases/latest"
# -f makes curl exit non-zero on HTTP errors (e.g. 403 rate limiting) instead
# of handing us the error JSON as if it were data.
if ! RELEASE_JSON="$(curl -fsSL "$API_URL")"; then
    echo "Error: failed to query the GitHub API ($API_URL)." >&2
    echo "       The API may be rate limiting you; wait a bit and retry." >&2
    exit 1
fi

# Extract the release tag, splitting on commas first so this works whether the API returns
# pretty-printed OR minified JSON. A single-line (minified) response broke the old line-based
# `grep | cut -f4`: it grabbed the 4th quote-field of the whole blob — the release object's API
# `url` — and "downloaded" that JSON instead of the binary. Constructing the asset URL from the
# tag avoids parsing the asset array entirely.
TAG="$(printf '%s' "$RELEASE_JSON" | tr ',' '\n' | grep '"tag_name"' | head -n1 \
    | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"

if [ -z "$TAG" ]; then
    echo "Error: could not determine the latest release tag from the GitHub API." >&2
    exit 1
fi

DOWNLOAD_URL="https://github.com/$REPO/releases/download/$TAG/$ASSET_NAME"

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
