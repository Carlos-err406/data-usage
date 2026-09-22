#!/bin/bash
# Installs a prebuilt release. Run this from inside the extracted archive.
# (The install.sh at the repo root builds from source instead.)
set -euo pipefail

cd "$(dirname "$0")"

BIN_DIR="$HOME/.local/bin"
PLUGIN_DIR="$(defaults read com.ameba.SwiftBar PluginDirectory 2>/dev/null || echo "$HOME/.config/swiftbar")"

if [ ! -d "/Applications/SwiftBar.app" ]; then
  echo "SwiftBar is not installed. Install it with:  brew install --cask swiftbar" >&2
  exit 1
fi

# The binary is ad-hoc signed but not notarised, so anything downloaded through
# a browser arrives quarantined and macOS refuses to run it.
xattr -d com.apple.quarantine data-usage 2>/dev/null || true
xattr -d com.apple.quarantine datausage.1s.sh 2>/dev/null || true

mkdir -p "$BIN_DIR" "$PLUGIN_DIR"
install -m 755 data-usage "$BIN_DIR/data-usage"
install -m 755 datausage.1s.sh "$PLUGIN_DIR/datausage.1s.sh"

echo "Installed:"
echo "  $BIN_DIR/data-usage"
echo "  $PLUGIN_DIR/datausage.1s.sh"
echo
echo "Open SwiftBar (or Refresh All in its menu) to pick up the plugin."
