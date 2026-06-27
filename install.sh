#!/bin/sh
set -e

REPO="Open-Tech-Foundation/release"
BIN_NAME="otf-release"

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS" in
    linux) OS_NAME="linux" ;;
    darwin) OS_NAME="macos" ;;
    *) echo "Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH_NAME="x86_64" ;;
    aarch64|arm64) ARCH_NAME="aarch64" ;;
    *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

ASSET_NAME="${BIN_NAME}-${OS_NAME}-${ARCH_NAME}"

echo "Fetching latest version of $BIN_NAME..."
DOWNLOAD_URL=$(curl -s "https://api.github.com/repos/$REPO/releases/latest" | grep "browser_download_url.*$ASSET_NAME\"" | cut -d '"' -f 4)

if [ -z "$DOWNLOAD_URL" ]; then
    echo "Error: Could not find release asset for $OS_NAME $ARCH_NAME."
    exit 1
fi

echo "Downloading from $DOWNLOAD_URL..."
curl -L -o "$BIN_NAME" "$DOWNLOAD_URL"
chmod +x "$BIN_NAME"

INSTALL_DIR="/usr/local/bin"
if [ ! -w "$INSTALL_DIR" ]; then
    echo "Requires sudo to install to $INSTALL_DIR"
    sudo mv "$BIN_NAME" "$INSTALL_DIR/"
else
    mv "$BIN_NAME" "$INSTALL_DIR/"
fi

echo "$BIN_NAME installed successfully to $INSTALL_DIR/$BIN_NAME"
