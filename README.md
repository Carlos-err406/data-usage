# data-usage

[![ci](https://github.com/Carlos-err406/data-usage/actions/workflows/ci.yml/badge.svg)](https://github.com/Carlos-err406/data-usage/actions/workflows/ci.yml)

Live network data usage in the macOS menu bar, split by **mobile** vs **Wi-Fi**,
resetting daily and keeping history for graphs.

A single 1.4 MB Rust binary that runs as a [SwiftBar](https://swiftbar.app)
plugin. No app bundle, no code signing, no webview.

```
↓1.2M ↑340K                     ← menu bar, refreshed every 2s
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

Requires [SwiftBar](https://swiftbar.app) and a Rust toolchain:

```bash
brew install --cask swiftbar
```

Then:

```bash
curl -fsSL https://raw.githubusercontent.com/Carlos-err406/data-usage/main/install.sh | bash
```

That fetches the source into a temporary directory, builds it, and installs the
binary to `~/.local/bin/data-usage` with a plugin wrapper in your SwiftBar
plugin folder. Open SwiftBar, or pick **Refresh All** from its menu, to load it.

The same script works from a checkout, where it builds in place instead:

```bash
git clone https://github.com/Carlos-err406/data-usage.git
cd data-usage
./install.sh
```

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

## Interactive chart

**Interactive chart…** in the dropdown opens a web view popover anchored under
the menu bar item, with hover tooltips giving the exact split for any hour or
day.

It is a separate view rather than hover on the charts already in the menu
because an `NSMenuItem` image cannot report which bar the pointer is over —
AppKit hands the plugin no per-pixel hover — so real per-bar tooltips need a
real web view. SwiftBar opens one for any line carrying `href=<url>
webview=true`.

The page is written to `report.html` beside the database and loaded over
`file://`. Everything in it is inlined: the popover has no network access and
WKWebView is given no read access beyond the page itself, so an external
stylesheet or script would silently fail to load.

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
data-usage once                          # print one menu and exit (what the plugin runs)
data-usage stream                        # continuous mode; see the note below
data-usage status                        # link classification + who is accounting
data-usage set-class en0 mobile          # pin an interface
data-usage set-class en0 auto            # clear the pin
```

## Why a refresh plugin, not a streaming one

SwiftBar supports streamable plugins, which push updates continuously and would
give a smoother menu bar. It resets the menu item on every `~~~` block, though,
which dismisses the dropdown a second after you open it. A refresh plugin is
deferred while the menu is open, so it stays put. `stream` is still there, and
still has that flaw.

One refresh costs about 10 ms, and each run measures its delta against the
checkpoint the previous run left in the database, so accounting is continuous
even though nothing is.

## Development

CI runs on every push and pull request to `main`: `cargo fmt --check`, clippy
with `-D warnings`, tests, and a release build.

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```
