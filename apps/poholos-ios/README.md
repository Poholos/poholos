# poholos-ios — mesh monitor

A receive-only iOS monitor ("mesh pager") for the poholos mesh: it scans BLE
advertisements for poholos frames and runs them through the actual Rust
routing engine (via [`poholos-ffi`](../../crates/poholos-ffi)), so duplicate
suppression and delivery semantics are exactly those of every other node.

Foreground-only by design for v1: without a service UUID to filter on, iOS
stops undirected scans in the background — measuring that behavior is part of
this app's job.

## Layout

- `Sources/PoholosKit/` — the Swift face of the engine (`Router`,
  `RouteAction`, `WireID`), the CoreBluetooth scanner (`MeshScanner`), the
  device identity (`NodeIdentity`, e.g. `iph-3f2a`), and the UI: the
  aggregation model (`MonitorModel` — feed, per-source stats, traffic
  counters; unit-tested with synthetic events) and the feed + diagnostics
  screens. Compiles for iOS **and** macOS.
- `Sources/poholos-monitor/` — a terminal monitor for the Mac: the same
  pipeline printing a live feed, so scanner → engine → formatting is
  debuggable against the real mesh without provisioning a phone.
- `PoholosMonitor/` + `PoholosMonitor.xcodeproj` — the iOS app; just the
  `@main` entry point, everything else comes from the package.
- `Frameworks/` *(gitignored)* — `PoholosFFI.xcframework`, installed by
  `refresh-ffi.sh`.

## Getting started

```sh
# one-time: Rust targets + header generator (plus Xcode)
rustup target add aarch64-apple-ios aarch64-apple-ios-sim \
    aarch64-apple-darwin x86_64-apple-darwin
cargo install cbindgen

./refresh-ffi.sh     # build the Rust engine, install the XCFramework
swift test           # wrapper tests against the real engine, on the Mac
swift run poholos-monitor   # live mesh feed in the terminal
```

`refresh-ffi.sh` must be re-run after any change on the Rust side. On first
`swift run`, macOS asks for Bluetooth access on behalf of your terminal
(System Settings > Privacy & Security > Bluetooth).

The monitor takes an optional node name and `--verbose` (also prints ignored
frames — duplicates being most mesh traffic):

```sh
swift run poholos-monitor mac-3f2a --verbose
```

## The iOS app

Open `PoholosMonitor.xcodeproj` in Xcode and run. For a physical iPhone,
set your development team under Signing & Capabilities (a free Apple ID
suffices for sideloading); the simulator needs no signing — but note the
simulator has no Bluetooth, so the feed only moves on hardware or the Mac.

Bundle id `com.poholos.monitor`, deployment target iOS 16, Bluetooth usage
description set via build settings (no checked-in Info.plist).

## Hardware validation results (2026-07-03, iPhone 15 Pro, iOS 26.5)

- ✅ **v0 RX from every sender class** — Windows CLI, macOS CLI, and
  micro:bit legacy-PDU frames all appear in the feed; one line per message
  despite continuously repeating advertisers (engine dedup working).
- ✅ **Telegram detection** — `@iph-xxxx …` renders as `@ … → you`.
- ✅ **Background scan stops** (measured, as expected): no service UUID to
  filter on means iOS delivers nothing to a backgrounded app — the
  empirical premise for the service-UUID-in-wire-v1 idea.
- ❌ **Wire v1 / extended advertising is not received on iPhone.**
  `CBCentralManager.supports(.extendedScanAndConnect)` reports **true**
  (when queried at powered-on; earlier it reads false — query timing
  matters), yet a 200-byte extended advertisement (1M primary + 2M
  secondary, from the `poc/ext-adv` micro:bit transmitter) never reaches
  `didDiscover`, while **macOS on the same desk surfaces the full 200
  bytes** via this package's own scanner. Conclusion: the flag describes
  OS-internal capability, not what CoreBluetooth passes to apps —
  iPhones are v0-only mesh participants.
- ❌ **Coded PHY is not received on macOS either.** The same transmitter
  switched to coded primary + secondary (the POC's default) never
  appears, while its 1M + 2M shape does. CoreBluetooth exposes no
  coded-scan option on any platform, so long-range coded frames remain
  the domain of the patched-btleplug desktops and the micro:bits.

In short: Apple platforms receive **v0 everywhere**; **v1 on macOS only**
(standard PHYs); **Coded nowhere**.

## License

AGPL-3.0-only, like the other poholos applications (see
[LICENSING.md](../../LICENSING.md)). The engine it embeds stays MIT/Apache.
