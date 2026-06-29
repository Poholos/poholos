# poholos

A peer-to-peer mesh chat protocol that rides inside Bluetooth Low Energy
legacy advertisements. *Poholos* (Ukrainian *по́голос*, ˈpo-ho-los — rumor, hearsay) floods
short messages hop-by-hop across nearby devices with no infrastructure, no
pairing, and no connections: every node simply broadcasts and re-broadcasts what
it hears. The universal desktop baseline for legacy advertising data is ~31
bytes, leaving a **22-byte on-air frame** once AD structure overhead is
accounted for, and everything in this crate is built around that budget.

This crate is the protocol core of the poholos workspace. For the console
client, the micro:bit firmware, the wire format, and end-to-end usage, see the
[project README](https://github.com/poholos/poholos#readme).

## Sans-io

This crate is strictly **sans-io**: it never touches Bluetooth, sockets, clocks,
or randomness. You feed received bytes into a `Router` and it tells you what to
do — deliver a message locally, re-broadcast a frame, or ignore it. This keeps
the protocol engine fully unit-testable and reusable from embedded targets. All
I/O lives in the application layer (see the `poholos-cli` crate).

## Core types

- `Packet` — a parsed protocol message, constructed via `Packet::hearsay`
  (broadcast) or `Packet::telegram` (unicast).
- `Frame` — an encoded on-air representation, at most `MAX_FRAME_LEN` (22) bytes.
- `WireId` — the compact 32-bit node identity used on the wire.
- `NodeId` *(requires the `std` feature)* — the human-friendly node name, e.g.
  `alice-3f2a`.
- `Router` — the pure routing state machine, with built-in duplicate suppression
  via `SeenCache`.
- `rotation::Rotation` — the airtime scheduler for transports with a single
  repeating broadcast slot (BLE advertising), shared by the desktop CLI and
  embedded targets.

### Frame capacity and wire versions

`Packet`, `Frame`, `Payload`, `RouteAction`, and `Rotation` are type aliases
over capacity-generic types — `PacketN<CAP>`, `FrameN<CAP>`, and friends — fixed
to `MAX_FRAME_LEN` (22), the BLE-legacy budget. `encode`/`decode` and
`Router::originate`/`ingest` are likewise generic over `CAP`, so the same engine
can carry larger frames on transports with more room without duplicating the
protocol logic.

For BLE 5 extended advertising there are matching `Ext*` aliases — `ExtPacket`,
`ExtFrame`, `ExtPayload` — fixed to `MAX_EXT_FRAME_LEN` (211, i.e. a 200-byte
payload). Byte 0 of every frame carries a 2-bit **wire version**: `encode`
writes version 0 (`WIRE_VERSION`) for frames that fit the 22-byte budget and
version 1 (`WIRE_VERSION_EXT`) for longer ones, which share the identical header
layout. `decode` accepts both, so a node is dual-stack: short messages stay
version 0 (every node, legacy included, can carry them) and only oversized
messages become version 1, reaching extended-advertising-capable nodes only.

Reach for the legacy aliases unless you specifically need the extended capacity.

## Duplicate suppression

Flood routing re-broadcasts every packet, so each node remembers what it has
already handled. `SeenCache` keeps the most recently *heard* dedup keys and
evicts the stalest when full. Eviction is LRU-style: re-hearing a key refreshes
it, which matters on radio transports where an advertisement keeps repeating
until its sender replaces it. Keys are hashed with `fnv64` (FNV-1a, 64-bit),
chosen for its tiny `no_std`-friendly implementation — collision *resistance* is
a non-goal here.

## Example

Two nodes exchanging a broadcast over a simulated link:

```rust
use poholos::{Packet, RouteAction, Router, WireId};

let alice = WireId::of_name("alice-3f2a");
let bob = WireId::of_name("bob-9c01");

let mut a = Router::new(alice);
let mut b = Router::new(bob);

// Alice broadcasts. `originate` registers the packet as seen and
// returns the encoded frame to hand to a transport.
let pkt = Packet::hearsay(alice, 1, b"hi mesh")?;
let frame = a.originate(&pkt);

// Bob receives the raw bytes from his transport.
match b.ingest(frame.as_bytes())? {
    RouteAction::DeliverAndForward(p, _relay) => {
        assert_eq!(p.payload(), b"hi mesh");
    }
    other => panic!("unexpected action: {other:?}"),
}
```

## Feature flags

- `std` *(default)* — enables `NodeId`, a `LinkedHashSet`-backed `SeenCache`, and
  backtrace capture in errors. Without it the crate is `no_std` and
  allocation-free.
- `serde` — `Serialize`/`Deserialize` impls on the wire types (hand-written, as
  serde lacks const-generic array support).
- `postcard` — convenience codec in `poholos::codec`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Any contribution intentionally submitted for inclusion in this crate 
shall be dual licensed as above, without any additional terms or conditions.
