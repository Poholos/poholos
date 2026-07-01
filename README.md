<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-dark.svg">
  <img src="assets/logo.svg" alt="Poholos" width="480">
</picture>

*по́голос* (poholos, ˈpo-ho-los) — rumor, hearsay.

</div>

A peer-to-peer mesh chat, carried entirely by Bluetooth Low Energy
**advertising frames**. No connections, no pairing, no GATT:
every node scans and advertises simultaneously, and messages flood
the mesh hop by hop until their TTL runs out.

## Workspace layout

```
crates/
├── poholos                  # protocol core: packets, wire codec, seen-cache,
│                            # router, airtime rotation - no_std, zero I/O
├── poholos-cli              # tokio console app: chat loop + UDP/BLE transports
├── poholos-morse            # no_std morse-code composer: dot/dash -> text
├── poholos-microbit         # micro:bit v2 mesh-node firmware (Embassy; own
│                            # workspace, validated end-to-end on hardware)
└── poholos-microbit-morse   # micro:bit v2 morse-input node (Embassy; own
                             # workspace, validated end-to-end on hardware)
```

The core library never touches a socket or a radio. The router is a pure
state machine (`ingest(bytes) -> RouteAction`), which keeps the entire
protocol unit-testable and reusable from embedded targets. See
[`crates/poholos/README.md`](crates/poholos/README.md) for the core crate's
own API overview.

## Quick start

Two terminals on one machine (or any LAN), no Bluetooth needed:

```sh
cargo run -p poholos-cli -- --name alice --transport udp
cargo run -p poholos-cli -- --name bob   --transport udp
```

Real BLE radio:

```sh
cargo run -p poholos-cli -- --name alice     # BLE is the default (no need for --transport ble)
```

`--name` appends a random 4-hex-digit suffix on every start; use
`--id alice-0001` instead to pin the full identity (and thus the wire
id) across restarts — required when something addresses you by a baked-in
name, like the micro:bit's buddy telegram.

Type a short message to broadcast (*hearsay*). 
`@bob-9c01 hi` sends a unicast (*telegram*) — the wire id derives from the target's full display name,
no peer directory required.
`/quit` exits.

Received messages print with a local receive timestamp and addressing,
so logs from several nodes can be correlated during testing:

```
2026-06-11 14:32:07 [mb-60c6 → all] SOS - test
2026-06-11 14:32:09 [mb-60c6 → you] I am OK - long status sent over BLE 5 extended advertising
```

## Wire format (version 0, max 22 bytes)

```
byte 0      : ver(2 bits)=0 | has_dest(1 bit) | ttl(5 bits)
bytes 1–2   : seq, u16 BE (starts random, wraps)
bytes 3–6   : src wire id, u32 BE (fnv64 of node name, truncated)
[bytes 7–10]: dest wire id, u32 BE - present iff has_dest
rest        : payload - ≤ 15 bytes hearsay, ≤ 11 bytes telegram
```

* `DEFAULT_TTL` = 16, `MAX_TTL` = 31. `hop()` refuses at `ttl <= 1`, so a
  TTL of 0 never appears on the wire.
* Dedup key = FNV-1a 64 over (flags, src, seq, dest?, payload) — TTL
  excluded so the same packet at different hop counts dedups correctly.
* Manufacturer-data company id `0xF10C` (above the assigned range; BlueZ
  silently drops `0xFFFF`).
* Messages above the protocol maximum (200-byte payload) are rejected at
  the prompt, not fragmented; a frame a given transport cannot physically
  carry fails that send with a platform-specific message.

### Wire version 1 (extended advertising)

Frames past the 22-byte legacy budget are tagged **wire version 1** and
carry up to a 200-byte payload over BLE 5 extended advertising. The header
layout is identical — only the permitted length differs — and `encode`
picks the version by size: short messages stay version 0 (heard by every
node, legacy included) and only long ones become version 1 (heard by
extended-scan-capable nodes). Today Windows and the micro:bit *send*
version 1 — Windows capped at ~156 bytes by the test adapter, the micro:bit
the full ~200; extended-scan-capable nodes *receive* it, and the UDP test
transport carries it in full. See *Platform notes* for what's validated
where.

## Library features

| Feature    | Default | Effect |
|------------|---------|--------|
| `std`      | yes     | `hashlink` seen-cache, `Backtrace` in errors |
| `serde`    | no      | `Serialize`/`Deserialize` on core types |
| `postcard` | no      | `to_postcard_slice`/`from_postcard` (no_std-friendly) |

Without `std` the crate is `no_std`: the seen-cache becomes a fixed
`[u64; 512]` ring and the wire codec works on borrowed buffers.

## Platform notes

These three platforms were validated on real radio.

| OS        | Scan | Advertise | Send budget |
|-----------|------|-----------|-----------------|
| Linux     | btleplug | BlueZ manufacturer data (`bluer`, `Type::Peripheral`) | 22 bytes |
| Windows 11| btleplug | `BluetoothLEAdvertisementPublisher` manufacturer data | 22 legacy / ~156 extended |
| macOS     | btleplug | CoreBluetooth **128-bit service UUID** (1-byte tag+len) | **15 bytes** |

Windows cannot act as a GATT peripheral (HRESULT failure) — irrelevant
here, since poholos only broadcasts. macOS can *hear* full 22-byte frames
but can only *send* what fits a single 128-bit service UUID: 15 raw bytes
(one byte tags and lengths the frame), so hearsay typed on a Mac is capped
at 8 payload bytes and telegrams at 4.

Windows and the micro:bit are **dual-stack**: each sends wire version 0 as
a legacy advertisement (heard by every node) and oversized wire version 1
via BLE 5 extended advertising, and each scans with extended scanning,
which receives both. Windows TX is capped ~156 bytes by the test adapter;
the micro:bit carries the full ~200. Linux (my test box is BLE 4.2-only)
and macOS remain version-0 senders. Short messages stay version 0, so the
mesh stays fully connected regardless of who speaks version 1.

## Verifying a fresh checkout

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test  --workspace
cargo test  -p poholos --no-default-features   # no_std ring backend

# Embedded core (rustup target add thumbv7em-none-eabihf):
cargo build -p poholos --no-default-features --target thumbv7em-none-eabihf

# micro:bit v2 firmware (own workspaces; build from inside each crate so
# its .cargo/config.toml target settings apply):
(cd crates/poholos-microbit && cargo build --release)
(cd crates/poholos-microbit-morse && cargo build --release)
```

The Linux, Windows, and macOS advertisers, the btleplug scanner (both
frame shapes), and both micro:bit firmwares (canned and morse-input) are
all validated on real radio, including a two-hop Mac → Windows →
micro:bit telegram relay across encodings.

## micro:bit v2 firmware

`crates/poholos-microbit` is an Embassy-based full mesh node for the BBC
micro:bit v2 (nRF52833, `thumbv7em-none-eabihf`), radio via the Nordic
SoftDevice Controller + `trouble` (linked into the image — no separate
SoftDevice flash), validated end-to-end against Windows and macOS desktop
nodes, including a two-hop Mac → Windows → micro:bit relay.
It scans continuously with **extended scanning** (so it hears both legacy
and BLE 5 extended advertisements), relays with the same flood/TTL/dedup
semantics and rotation airtime policy as the desktops, and is dual-stack:
short frames go out as legacy advertisements, oversized (wire version 1)
frames via extended advertising. Delivered messages scroll on the 5×5 LED
matrix, each with a leading glyph for its kind — `*` for a broadcast, `@`
(plus a chime on the onboard speaker) for a telegram addressed to it.
It originates two canned messages:

* **Button A** — a long "I am OK" status telegram to the preconfigured
  buddy node, sized to ride wire version 1 / extended advertising.
* **Button B** — "SOS - test" broadcast.

Known gap: the board only parses manufacturer-data advertisements, so it
cannot hear macOS nodes (which advertise a service UUID) directly yet —
they reach it relayed through a Linux or Windows node.

Identity is `mb-xxxx`, derived from the factory device id; the board
scrolls its own name at boot so you can `@mb-xxxx hello` it. The buddy
is baked in at compile time via `POHOLOS_BUDDY` (default `alice-0001`)
and pairs with a desktop holding a stable identity:

```sh
cargo install probe-rs-tools         # once; flashes via the onboard probe
# once: LLVM is needed at build time (bindgen for the Nordic SDC blob);
# on Windows: winget install LLVM.LLVM, then set LIBCLANG_PATH to its bin
cd crates/poholos-microbit
cargo run --release                  # flash + stream defmt logs

# on the desktop (--id pins the suffix so the buddy address stays valid):
cargo run -p poholos-cli -- --id alice-0001
```

## micro:bit v2 morse-code node

`crates/poholos-microbit-morse` is a variant firmware whose *input* is morse
code keyed on the two buttons instead of canned messages — otherwise a full
mesh node, identical on the radio: dual-stack across both wire versions like
the canned firmware, so a short keyed message broadcasts as a legacy
advertisement and a long one rides BLE 5 extended advertising. It builds on
`poholos-morse`, a small `no_std`, host-tested decoder that turns dot/dash
elements plus pause boundaries into text.

Keying:

* Button **A** tap = **dot**, Button **B** tap = **dash**. The first tap
  begins a message; a pause auto-commits the current letter, a longer pause
  inserts a word space.
* **Hold A** = finish & send (broadcast). **Hold B** = clear.

Feedback on the 5×5 matrix (with a speaker sidetone per press):

* each press flashes its **dot/dash** glyph;
* each completed **letter** is flashed as a character;
* **✗** — press rejected (the letter is past the 6-element morse maximum:
  pause to let it commit);
* **→** sent, **←** cleared.

The decoded text and the running dot/dash pattern also stream to the `defmt`
log. Build and flash exactly like the canned firmware:

```sh
cd crates/poholos-microbit-morse
cargo run --release
```

## License

Split-licensed by component:

- **`poholos` core** (`crates/poholos`) — dual-licensed **MIT OR Apache-2.0**,
  at your option.
- **Everything else** (`poholos-cli`, `poholos-morse`, `poholos-microbit`,
  `poholos-microbit-morse`) — **AGPL-3.0-only**.

See [LICENSING.md](LICENSING.md) for details.
