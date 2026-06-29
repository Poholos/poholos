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

    /// The hand-written `PayloadN`/`FrameN` serde impls (serde has no
    /// const-generic array impls) at a non-default capacity, including the
    /// byte-visitor's empty and capacity-max edge cases.
    #[cfg(feature = "postcard")]
    #[test]
    fn postcard_round_trips_extended_capacity() {
        use crate::{FrameN, PacketN, encode};

        let payload = [0xA5u8; 200];
        let p = PacketN::<211>::hearsay(WireId::new(1), 7, &payload).unwrap();
        let mut buf = [0u8; 256];
        let used = postcard::to_slice(&p, &mut buf).unwrap();
        assert_eq!(postcard::from_bytes::<PacketN<211>>(used).unwrap(), p);

        // Empty and full (211 - HEADER_LEN_HEARSAY = 204) hearsay payloads.
        for n in [0usize, 204] {
            let pl = [0x33u8; 204];
            let pe = PacketN::<211>::hearsay(WireId::new(2), 1, &pl[..n]).unwrap();
            let mut b = [0u8; 256];
            let u = postcard::to_slice(&pe, &mut b).unwrap();
            assert_eq!(postcard::from_bytes::<PacketN<211>>(u).unwrap(), pe);
        }

        // The frame's serde impl too.
        let f = encode(&p);
        let mut fb = [0u8; 256];
        let fu = postcard::to_slice(&f, &mut fb).unwrap();
        assert_eq!(postcard::from_bytes::<FrameN<211>>(fu).unwrap(), f);
    }
}
