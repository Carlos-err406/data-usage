#!/bin/bash
# Build and install the menu bar plugin.
#
# Works two ways: run from a checkout, or piped straight from the web, in which
# case it fetches the source into a temporary directory and cleans up after.
#
#   curl -fsSL https://raw.githubusercontent.com/Carlos-err406/data-usage/main/install.sh | bash
set -euo pipefail

REPO="https://github.com/Carlos-err406/data-usage.git"
BIN_DIR="$HOME/.local/bin"

die() { echo "$1" >&2; exit 1; }

[ -d /Applications/SwiftBar.app ] ||
  die "SwiftBar is not installed. Install it with:  brew install --cask swiftbar"
command -v cargo >/dev/null ||
  die "Rust is not installed. Get it from https://rustup.rs"

# Identify a checkout by the manifest, not just any Cargo.toml that happens to
# be in the working directory when this is piped from curl.
if [ -f Cargo.toml ] && grep -q '^name = "data-usage"' Cargo.toml 2>/dev/null; then
  SRC="$PWD"
else
  command -v git >/dev/null || die "git is required to fetch the source"
  SRC="$(mktemp -d)"
  trap 'rm -rf "$SRC"' EXIT
  echo "Fetching source..."
  git clone --depth 1 --quiet "$REPO" "$SRC"
fi

echo "Building..."
cargo build --release --manifest-path "$SRC/Cargo.toml"

# SwiftBar's plugin folder is configurable; ask it before guessing.
PLUGIN_DIR="$(defaults read com.ameba.SwiftBar PluginDirectory 2>/dev/null || echo "$HOME/.config/swiftbar")"
mkdir -p "$BIN_DIR" "$PLUGIN_DIR"
install -m 755 "$SRC/target/release/data-usage" "$BIN_DIR/data-usage"

# Drop any previously installed copy first. Both the name and the refresh
# interval live in the filename — SwiftBar takes the popover's title from the
# part before the first dot — so an older copy under a different name would be
# left behind and SwiftBar would run both.
find "$PLUGIN_DIR" -maxdepth 1 -name 'datausage.*.sh' -delete 2>/dev/null || true
find "$PLUGIN_DIR" -maxdepth 1 -name 'Data Usage.*.sh' -delete 2>/dev/null || true
install -m 755 "$SRC"/plugin/*.sh "$PLUGIN_DIR/"

echo
echo "Installed:"
echo "  $BIN_DIR/data-usage"
echo "  $PLUGIN_DIR/$(basename "$SRC"/plugin/*.sh)"
echo
echo "Open SwiftBar (or Refresh All in its menu) to pick up the plugin."
