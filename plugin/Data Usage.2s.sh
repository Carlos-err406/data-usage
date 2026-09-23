#!/bin/bash
#
# SwiftBar plugin — live network data usage, split by mobile vs Wi-Fi.
#
# Filename .2s.sh => refresh every 2 seconds. Deliberately a refresh plugin
# rather than a streamable one: SwiftBar resets the menu item on every `~~~`
# block a streaming plugin emits, which dismisses the dropdown while you are
# reading it. Refreshes are deferred while the menu is open, so it stays put.
#
# <xbar.title>Data Usage</xbar.title>
# <xbar.version>v0.1.0</xbar.version>
# <xbar.desc>Live network data usage, split by mobile vs Wi-Fi, with daily history.</xbar.desc>
# <swiftbar.hideAbout>true</swiftbar.hideAbout>
# <swiftbar.hideRunInTerminal>true</swiftbar.hideRunInTerminal>
# <swiftbar.hideLastUpdated>true</swiftbar.hideLastUpdated>
# <swiftbar.hideDisablePlugin>true</swiftbar.hideDisablePlugin>
# <swiftbar.hideSwiftBar>true</swiftbar.hideSwiftBar>

# GUI apps launch with a minimal PATH, so look in the usual places explicitly.
DATA_USAGE=""
for _c in "$HOME/.local/bin/data-usage" /opt/homebrew/bin/data-usage /usr/local/bin/data-usage; do
  [ -x "$_c" ] && DATA_USAGE="$_c" && break
done
[ -n "$DATA_USAGE" ] || DATA_USAGE="$(command -v data-usage 2>/dev/null)"

if [ -z "$DATA_USAGE" ]; then
  echo "data-usage not found"
  echo "---"
  echo "Reinstall: https://github.com/Carlos-err406/data-usage"
  exit 0
fi

exec "$DATA_USAGE" once
