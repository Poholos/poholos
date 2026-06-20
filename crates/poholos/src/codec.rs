// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Optional serialization helpers for storing or relaying packets off-air.
//!
//! The 22-byte format in [`wire`](crate::encode) is the *on-air* codec and
//! never changes shape. This module instead offers a convenience helper for
//! serializing [`Packet`](crate::Packet)s in host-side contexts — log
//! files, IPC, bridging over TCP — using the serde data model:
//!
//! * `postcard` *(feature `postcard`)* — `no_std`-friendly encoding into a
//!   caller-provided buffer, suitable for embedded targets.
//!
//! Upstream error types are returned directly; this leakage is acceptable
//! because each is gated behind the feature that introduces the dependency.

use crate::packet::Packet;

/// Serializes a packet with postcard into `buf`, returning the used slice.
///
/// # Errors
/// Returns [`postcard::Error`] if `buf` is too small.
#[cfg(feature = "postcard")]
pub fn to_postcard_slice<'a>(
    packet: &Packet,
    buf: &'a mut [u8],
) -> Result<&'a mut [u8], postcard::Error> {
    postcard::to_slice(packet, buf)
}

/// Deserializes a packet from postcard bytes.
///
/// # Errors
/// Returns [`postcard::Error`] if `bytes` is not a valid encoding.
#[cfg(feature = "postcard")]
pub fn from_postcard(bytes: &[u8]) -> Result<Packet, postcard::Error> {
    postcard::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "postcard")]
    use crate::{Packet, WireId};

    #[cfg(feature = "postcard")]
    #[test]
    fn postcard_round_trip() {
        let p = Packet::hearsay(WireId::new(1), 7, b"hi").unwrap();
        let mut buf = [0u8; 64];
        let used = super::to_postcard_slice(&p, &mut buf).unwrap();
        assert_eq!(super::from_postcard(used).unwrap(), p);
    }
}
