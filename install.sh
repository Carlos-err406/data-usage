#!/bin/bash
# Build and install the menu bar plugin.
set -euo pipefail

BIN_DIR="$HOME/.local/bin"
PLUGIN_DIR="$(defaults read com.ameba.SwiftBar PluginDirectory 2>/dev/null || echo "$HOME/.config/swiftbar")"

if [ ! -d "/Applications/SwiftBar.app" ]; then
  echo "SwiftBar is not installed. Install it with:  brew install --cask swiftbar" >&2
  exit 1
fi

echo "Building release binary…"
cargo build --release

mkdir -p "$BIN_DIR" "$PLUGIN_DIR"
install -m 755 target/release/data-usage "$BIN_DIR/data-usage"
install -m 755 plugin/datausage.1s.sh "$PLUGIN_DIR/datausage.1s.sh"

echo "Installed:"
echo "  $BIN_DIR/data-usage"
echo "  $PLUGIN_DIR/datausage.1s.sh"
echo
echo "Open SwiftBar (or Refresh All in its menu) to pick up the plugin."
