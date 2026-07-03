# poholos-ffi

A C ABI over the [`poholos`](../poholos) routing engine, so non-Rust hosts —
the iOS monitor app first — run the actual protocol core (the exact
duplicate-suppression and delivery semantics of every other node) instead of
re-implementing frame decoding.

The surface is four panic-safe functions:

| Function | Purpose |
| --- | --- |
| `poholos_router_new(name)` | Create a router; the wire id is derived from the display name, same as everywhere else. |
| `poholos_router_free(router)` | Release it. |
| `poholos_router_ingest(router, bytes, len, out_action)` | Feed received frame bytes in; get the flattened routing decision (deliver / deliver-and-forward / forward / ignore) with packet fields and relay bytes. |
| `poholos_wire_id_of_name(name)` | Derive a wire id for display purposes. |

Ingestion runs at the extended (wire version 1) frame capacity, so a host is
dual-stack: legacy 22-byte frames and BLE 5 extended-advertising frames both
decode. A receive-only monitor acts on the `Deliver*` variants and ignores the
relay bytes; a future relaying node uses the same surface unchanged.

## Building for iOS

The crate is a normal workspace member and builds on any host (`cargo test`
works without any Apple tooling). For the app, build the static library for
device and simulator and bundle an XCFramework:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cargo build -p poholos-ffi --release --target aarch64-apple-ios
cargo build -p poholos-ffi --release --target aarch64-apple-ios-sim
cbindgen --crate poholos-ffi --output include/poholos_ffi.h
xcodebuild -create-xcframework \
    -library ../../target/aarch64-apple-ios/release/libpoholos_ffi.a -headers include \
    -library ../../target/aarch64-apple-ios-sim/release/libpoholos_ffi.a -headers include \
    -output PoholosFFI.xcframework
```

The C header is generated on demand by `cbindgen` (see `cbindgen.toml`), not
by a `build.rs`, so plain cargo builds never need it installed.

## License

MIT OR Apache-2.0, following the protocol core (the applications in this
workspace are AGPL; see [LICENSING.md](../../LICENSING.md)).
