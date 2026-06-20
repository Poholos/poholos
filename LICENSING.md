# Licensing

poholos is split-licensed by component.

## Open core — `crates/poholos`

The poholos protocol core library is dual-licensed under either of

- Apache License, Version 2.0
  ([crates/poholos/LICENSE-APACHE](crates/poholos/LICENSE-APACHE))
- MIT license
  ([crates/poholos/LICENSE-MIT](crates/poholos/LICENSE-MIT))

at your option. You may use the `poholos` crate under the terms of either
license.

## Applications — `crates/poholos-cli`, `crates/poholos-microbit`

The console application and the micro:bit firmware are licensed under the
**GNU Affero General Public License, version 3.0 only** (`AGPL-3.0-only`);
the full text is in [LICENSE](LICENSE). In particular, the AGPL's network-use
clause applies: if you run a modified version and let users interact with it
over a network, you must offer those users the corresponding source code.

## Commercial license

The AGPL-licensed components are also available under a separate **commercial
license** that permits use in closed-source products without the AGPL's
copyleft obligations. This license can be granted by the copyright holder (and
its successors and assigns). To arrange terms, contact <info@poholos.com>.

## Contributions

Unless you state otherwise, any contribution you intentionally submit for
inclusion in:

- the open-core `poholos` crate shall be dual-licensed `MIT OR Apache-2.0`; and
- the `poholos-cli` or `poholos-microbit` components shall be licensed
  `AGPL-3.0-only`,

with no additional terms or conditions.

In addition, by submitting a contribution to any part of this project (for
example, by opening a pull request), you grant the copyright holder and its
successors and assigns a perpetual,
worldwide, non-exclusive, royalty-free, irrevocable license to use, reproduce,
modify, prepare derivative works of, sublicense, and distribute your
contribution, and to relicense it under any terms — including proprietary and
commercial licenses. This lets the AGPL-licensed components continue to be
offered under the separate commercial license above without any further
agreement or sign-off from you.

## Third-party components

This project depends on third-party open-source components, each under its own
license; those licenses are unaffected by the licensing above. Run
`cargo deny check licenses` (see [deny.toml](deny.toml)) for a full inventory.

In particular, the micro:bit firmware (`crates/poholos-microbit`) links the
Nordic Semiconductor SoftDevice Controller and MPSL binary libraries, licensed
under the Nordic 5-Clause license (`LicenseRef-Nordic-5-Clause`). That license
permits use of those binaries only in conjunction with a Nordic Semiconductor
integrated circuit, prohibits reverse engineering of them, and requires
reproduction of Nordic's copyright notice when firmware images are distributed
other than embedded in the Nordic integrated circuit.
