# data-usage

[![ci](https://github.com/Carlos-err406/data-usage/actions/workflows/ci.yml/badge.svg)](https://github.com/Carlos-err406/data-usage/actions/workflows/ci.yml)

Live network data usage in the macOS menu bar, split by **mobile** vs **Wi-Fi**,
resetting daily and keeping history for graphs.

A single 1.4 MB Rust binary that runs as a [SwiftBar](https://swiftbar.app)
streamable plugin. No app bundle, no code signing, no webview.

```
↓ 1.2 MB/s  ↑ 340 KB/s          ← menu bar, updates every second
────────────────────────────
Today · Tuesday 22 September
 Mobile      2.41 GB   ↓2.10 GB ↑312 MB
 Wi-Fi       14.8 GB   ↓13.9 GB ↑904 MB
 Total       17.2 GB
────────────────────────────
Last 24 hours   ▁▁▂▅█▆▄▃▂  (stacked bar chart)
Last 30 days    ▄▅▃█▆▅▇▄▂
────────────────────────────
Now on en0 · Wi-Fi
Count as Mobile
```

## Install

Requires SwiftBar: `brew install --cask swiftbar`.

**From a release** — download the latest `macos-universal` archive from
[Releases](https://github.com/Carlos-err406/data-usage/releases), then:

```bash
tar -xzf data-usage-*-macos-universal.tar.gz
cd data-usage-*-macos-universal
./install.sh
```

**From source** — needs a Rust toolchain:

```bash
./install.sh
```

Either way the binary lands in `~/.local/bin/data-usage` and a plugin wrapper in
your SwiftBar plugin folder. Open SwiftBar, or pick **Refresh All** from its
menu, to load it.

The two install scripts are not the same: the one at the repo root builds from
source, while `packaging/install.sh` (shipped inside the archive) installs the
prebuilt binary and clears its quarantine flag.

## How mobile data is detected

Three signals, in order of preference.

**1. Cellular interface.** A real cellular modem reports
`nw_interface_type_cellular` and is always counted as mobile.

**2. `expensive`.** `Network.framework` raises this for an **iPhone** Personal
Hotspot, over Wi-Fi *and* over USB. It's the same signal macOS uses to hold back
iCloud sync and App Store downloads.

It does **not** fire for an Android hotspot — verified against a Samsung phone,
which presents as an ordinary access point and reports `expensive=false` on both
Wi-Fi and USB tethering. Apple raises the flag from its own device signalling,
so treat it as an Apple-only convenience.

**3. `constrained`, i.e. Low Data Mode.** This is the native signal that works
for *any* phone. macOS lets you mark an individual Wi-Fi network as Low Data
Mode in **System Settings → Wi-Fi → Details… → Low Data Mode**, which sets
`constrained` on that path, and this app counts a constrained link as mobile.
It has the side benefit of making macOS itself throttle background traffic on
that network.

Reading the Wi-Fi **SSID** would be the obvious alternative and is deliberately
avoided: it triggers a Location Services prompt on macOS 14+.

### Pinning a network by hand

**Count this network as Mobile** / **as Wi-Fi** pins it; **Back to automatic**
clears the pin.

Pins are keyed on the **network**, not the interface. A laptop reaches both home
Wi-Fi and a phone hotspot over the same `en0`, so an interface-keyed pin meant
for one would silently relabel the other.

The key is the default gateway's MAC address — stable per network, different for
every router and hotspot, and readable without any permission prompt. Until the
gateway answers ARP the menu shows *Identifying network…* rather than offering a
pin, because a key derived from the gateway IP would change to the MAC moments
later and orphan the pin.

## Which interfaces are counted

Only links `Network.framework` reports as physical: Wi-Fi, Ethernet, cellular.

That deliberately excludes `utun*` (VPN), `awdl0`/`llw0` (AirDrop), `bridge*`,
`ap1` and `lo0`. Excluding VPN tunnels matters most: tunnel traffic is *also*
counted on the physical interface carrying it, so counting both would double
every byte sent over a VPN.

## Where the data lives

`~/Library/Application Support/data-usage/usage.db` — SQLite, WAL mode.

Usage accumulates into fixed 5-minute buckets keyed by class: 288 rows a day per
active class, small enough to keep indefinitely and fine-grained enough to draw
an hourly graph. "Resets daily" is a query boundary, not a destructive reset, so
yesterday and last month stay readable.

## Notes and limits

* **Accounting only happens while the plugin is running.** Traffic used while
  SwiftBar is closed is not attributed. On restart, a gap of up to 5 minutes is
  caught up from the last checkpoint; anything longer resyncs silently rather
  than dumping hours of bytes into the current minute.
* Counters are per-interface, not per-application. Per-app attribution would
  need a Network Extension and a signed, provisioned app bundle.
* Interface counters reset when a link is torn down and rebuilt; a counter going
  backwards is treated as a reset, not as negative usage.
* Only one instance accounts at a time, guarded by an `flock` on
  `accounting.lock`. Every instance reads the same global kernel counters, so
  two accounting at once would each attribute the same bytes and inflate the
  totals by however many are running. A second instance still renders the menu,
  just from stored history. `data-usage status` reports which role it has.

## Implementation notes

Two plausible-looking sources of byte counters are wrong on macOS, both silently:

* `getifaddrs()` returns `struct if_data`, whose `ifi_ibytes`/`ifi_obytes` are
  32-bit and wrap every 4 GB.
* `sysctl(NET_RT_IFLIST2)` returns `struct if_data64`, which *should* be 64-bit —
  but the kernel leaves the high words zeroed on that path, so it wraps at 4 GB
  too. This one is particularly convincing because the struct and every other
  field in it are correct.

`net.link.generic.ifdata.<index>.general` is the one that carries true 64-bit
counters, and it's what `netstat -ib` agrees with.

Those structs also sit under `#pragma pack(4)`, which lands `if_data64` at offset
52 of `ifmibdata` — 4-byte aligned, not 8. A `#[repr(C)]` Rust struct re-aligns it
to 56 and reads garbage, so fields are read at explicit byte offsets.

## Commands

```bash
data-usage stream                        # SwiftBar streaming mode (default)
data-usage once                          # print one menu and exit
data-usage status                        # link classification + who is accounting
data-usage set-class en0 mobile          # pin an interface
data-usage set-class en0 auto            # clear the pin
```

## Releasing

CI runs on every push and pull request to `main` — fmt, clippy with
`-D warnings`, tests, release build. **It never publishes anything.**

A release happens only when a version tag is pushed:

```bash
# bump the version in Cargo.toml first, and commit it
git tag v0.1.0
git push origin v0.1.0
```

The release workflow refuses to run if the tag and the `Cargo.toml` version
disagree, which is the easy way to ship a binary that misreports itself.

It builds `aarch64` and `x86_64`, joins them with `lipo` into one universal
binary with a deployment target of macOS 11, ad-hoc signs it, and attaches a
`.tar.gz` plus a SHA-256 checksum to a GitHub release.

Releases are **not notarised** — that needs a paid Apple Developer ID. macOS
quarantines the archive on download, and the bundled `install.sh` clears the
flag on the way in.
