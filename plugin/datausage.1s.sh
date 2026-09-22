#!/bin/bash
# <xbar.title>Data Usage</xbar.title>
# <xbar.version>v0.1.0</xbar.version>
# <xbar.desc>Live network data usage, split by mobile vs Wi-Fi, with daily history.</xbar.desc>
# <xbar.dependencies>rust</xbar.dependencies>
# <swiftbar.type>streamable</swiftbar.type>
# <swiftbar.hideAbout>true</swiftbar.hideAbout>
# <swiftbar.hideRunInTerminal>true</swiftbar.hideRunInTerminal>
# <swiftbar.hideLastUpdated>true</swiftbar.hideLastUpdated>
# <swiftbar.hideSwiftBar>true</swiftbar.hideSwiftBar>
#
# Thin wrapper so SwiftBar can read the metadata above, which it parses out of
# the plugin file itself — a compiled binary has nowhere to put it.

exec "$HOME/.local/bin/data-usage" stream
