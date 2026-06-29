// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Protocol packets: construction, TTL semantics, and payload limits.
//!
//! A [`Packet`] is the parsed form of a poholos message. Two shapes exist,
//! named after how gossip travels:
//!
//! * **hearsay** — a broadcast to everyone in earshot ([`Packet::hearsay`]).
//! * **telegram** — a unicast addressed to one node ([`Packet::telegram`]).
//!
//! Both have `*_with` variants for full control over the TTL. Packets are
//! plain `Copy` values with an inline fixed-capacity [`Payload`]; nothing
//! here allocates, so the module works identically under `no_std`.
//!
//! # Frame capacity
//!
//! [`PacketN`] and [`PayloadN`] are generic over a `CAP` const that sizes the
//! inline payload buffer to match an on-air [`FrameN`](crate::FrameN) of the
//! same capacity. The aliases [`Packet`] and [`Payload`] fix `CAP` to
//! [`MAX_FRAME_LEN`] — the legacy 22-byte (wire version 0) frame — and are
//! what nearly all code uses. A larger `CAP` (for BLE 5 extended advertising)
//! reuses the identical logic with a bigger buffer.
//!
//! # TTL semantics
//!
//! The TTL counts remaining hops and lives in 5 bits on the wire
//! ([`MAX_TTL`] = 31). The protocol invariant is that **a TTL of zero must
//! never be placed on the wire**. [`Packet::hop`] enforces this: it refuses
//! to decrement when `ttl <= 1`, so a packet received with TTL 1 can still
//! be delivered locally but will not be forwarded.

use crate::error::PacketError;
use crate::node_id::WireId;
use crate::seen::{FNV_OFFSET_BASIS, fnv64_update};
use crate::wire::{HEADER_LEN_HEARSAY, HEADER_LEN_TELEGRAM, MAX_FRAME_LEN};

/// Default number of hops a new packet may travel.
///
/// 16 comfortably covers any realistic ad-hoc mesh diameter while bounding
/// flood traffic, and sits well inside the 5-bit wire field (max 31).
/// Raising it increases worst-case rebroadcast load linearly; lowering it
/// risks partitioning sparse meshes.
pub const DEFAULT_TTL: u8 = 16;

/// Maximum TTL representable in the 5-bit wire field.
///
/// Derived from the frame layout, which packs version (2 bits), the
/// `has_dest` flag (1 bit), and the TTL (5 bits) into byte 0.
pub const MAX_TTL: u8 = 31;

/// Maximum payload bytes in a legacy (wire version 0) hearsay packet.
///
/// Derived: [`MAX_FRAME_LEN`] (22) minus the 7-byte broadcast header. A
/// `PacketN<CAP>` with a larger `CAP` permits a correspondingly larger
/// payload (`CAP - 7`).
pub const MAX_PAYLOAD_HEARSAY: usize = MAX_FRAME_LEN - HEADER_LEN_HEARSAY;

/// Maximum payload bytes in a legacy (wire version 0) telegram packet.
///
/// Derived: [`MAX_FRAME_LEN`] (22) minus the 11-byte unicast header.
pub const MAX_PAYLOAD_TELEGRAM: usize = MAX_FRAME_LEN - HEADER_LEN_TELEGRAM;

/// Inline, fixed-capacity message payload, generic over the frame capacity.
///
/// Stores bytes inline so [`PacketN`] stays `Copy` and `no_std`-friendly.
/// The buffer is `CAP` bytes (the frame capacity); the usable payload is
/// bounded by the packet shape's header at construction. Most code uses the
/// [`Payload`] alias (`CAP` = [`MAX_FRAME_LEN`]).
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct PayloadN<const CAP: usize> {
    len: u8,
    buf: [u8; CAP],
}

// Hand-written serde: serde's array impls don't cover const-generic `[u8;
// CAP]`, and serializing the whole buffer would waste space anyway. We
// (de)serialize just the meaningful bytes.
#[cfg(feature = "serde")]
impl<const CAP: usize> serde::Serialize for PayloadN<CAP> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.as_slice())
    }
}

#[cfg(feature = "serde")]
impl<'de, const CAP: usize> serde::Deserialize<'de> for PayloadN<CAP> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Vis<const CAP: usize>;
        impl<'de, const CAP: usize> serde::de::Visitor<'de> for Vis<CAP> {
            type Value = PayloadN<CAP>;
            fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(formatter, "up to {CAP} payload bytes")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                PayloadN::copy_from(v, CAP).map_err(serde::de::Error::custom)
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut buf = [0u8; CAP];
                let mut n = 0;
                while let Some(b) = seq.next_element::<u8>()? {
                    if n >= CAP {
                        return Err(serde::de::Error::invalid_length(n + 1, &self));
                    }
                    buf[n] = b;
                    n += 1;
                }
                PayloadN::copy_from(&buf[..n], CAP).map_err(serde::de::Error::custom)
            }
        }
        deserializer.deserialize_bytes(Vis::<CAP>)
    }
}

/// A legacy (wire version 0) payload: [`PayloadN`] with `CAP` =
/// [`MAX_FRAME_LEN`].
pub type Payload = PayloadN<MAX_FRAME_LEN>;

impl<const CAP: usize> PayloadN<CAP> {
    pub(crate) fn copy_from(bytes: &[u8], max: usize) -> Result<Self, PacketError> {
        if bytes.len() > max {
            return Err(PacketError::payload_too_long(bytes.len(), max));
        }
        let mut buf = [0u8; CAP];
        buf[..bytes.len()].copy_from_slice(bytes);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "bytes.len() <= max <= CAP - 7 <= 255 for every supported capacity"
        )]
        Ok(Self {
            len: bytes.len() as u8,
            buf,
        })
    }

    /// Returns the payload bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..usize::from(self.len)]
    }

    /// Returns the payload length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// Returns `true` if the payload is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const CAP: usize> AsRef<[u8]> for PayloadN<CAP> {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

/// A parsed poholos protocol message, generic over the frame capacity `CAP`.
///
/// Construct via [`Packet::hearsay`] / [`Packet::telegram`] (or their
/// `*_with` variants to control TTL). Encode with
/// [`encode`](crate::encode), decode with [`decode`](crate::decode), or let
/// a [`Router`](crate::Router) handle both. Most code uses the [`Packet`]
/// alias (`CAP` = [`MAX_FRAME_LEN`]).
///
/// # Examples
/// ```
/// use poholos::{Packet, WireId, DEFAULT_TTL};
///
/// let src = WireId::of_name("alice-3f2a");
/// let dest = WireId::of_name("bob-9c01");
///
/// let shout = Packet::hearsay(src, 7, b"hello mesh")?;
/// assert_eq!(shout.ttl(), DEFAULT_TTL);
/// assert!(shout.dest().is_none());
///
/// let whisper = Packet::telegram(src, dest, 8, b"hi bob")?;
/// assert_eq!(whisper.dest(), Some(dest));
/// # Ok::<(), poholos::PacketError>(())
/// ```
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PacketN<const CAP: usize> {
    ttl: u8,
    seq: u16,
    src: WireId,
    dest: Option<WireId>,
    payload: PayloadN<CAP>,
}

/// A legacy (wire version 0) packet: [`PacketN`] with `CAP` =
/// [`MAX_FRAME_LEN`].
pub type Packet = PacketN<MAX_FRAME_LEN>;

impl<const CAP: usize> PacketN<CAP> {
    /// Maximum payload bytes for a hearsay packet at this capacity.
    const MAX_HEARSAY: usize = CAP - HEADER_LEN_HEARSAY;
    /// Maximum payload bytes for a telegram packet at this capacity.
    const MAX_TELEGRAM: usize = CAP - HEADER_LEN_TELEGRAM;

    /// Creates a broadcast packet with [`DEFAULT_TTL`].
    ///
    /// `seq` is a per-source sequence number used (together with the source
    /// and payload) for duplicate suppression; sources should increment it
    /// per message.
    ///
    /// # Errors
    /// Returns [`PacketError`] if `payload` exceeds the hearsay payload limit
    /// (`CAP - 7`; [`MAX_PAYLOAD_HEARSAY`] for a legacy [`Packet`]).
    pub fn hearsay(src: WireId, seq: u16, payload: &[u8]) -> Result<Self, PacketError> {
        Self::hearsay_with(src, seq, payload, DEFAULT_TTL)
    }

    /// Creates a broadcast packet with an explicit TTL.
    ///
    /// # Errors
    /// Returns [`PacketError`] if `payload` exceeds the hearsay payload
    /// limit, or if `ttl` is outside `1..=MAX_TTL`.
    pub fn hearsay_with(
        src: WireId,
        seq: u16,
        payload: &[u8],
        ttl: u8,
    ) -> Result<Self, PacketError> {
        validate_ttl(ttl)?;
        Ok(Self {
            ttl,
            seq,
            src,
            dest: None,
            payload: PayloadN::copy_from(payload, Self::MAX_HEARSAY)?,
        })
    }

    /// Creates a unicast packet to `dest` with [`DEFAULT_TTL`].
    ///
    /// # Errors
    /// Returns [`PacketError`] if `payload` exceeds the telegram payload
    /// limit (`CAP - 11`; [`MAX_PAYLOAD_TELEGRAM`] for a legacy [`Packet`]).
    pub fn telegram(
        src: WireId,
        dest: WireId,
        seq: u16,
        payload: &[u8],
    ) -> Result<Self, PacketError> {
        Self::telegram_with(src, dest, seq, payload, DEFAULT_TTL)
    }

    /// Creates a unicast packet with an explicit TTL.
    ///
    /// # Errors
    /// Returns [`PacketError`] if `payload` exceeds the telegram payload
    /// limit, or if `ttl` is outside `1..=MAX_TTL`.
    pub fn telegram_with(
        src: WireId,
        dest: WireId,
        seq: u16,
        payload: &[u8],
        ttl: u8,
    ) -> Result<Self, PacketError> {
        validate_ttl(ttl)?;
        Ok(Self {
            ttl,
            seq,
            src,
            dest: Some(dest),
            payload: PayloadN::copy_from(payload, Self::MAX_TELEGRAM)?,
        })
    }

    /// Consumes one hop, returning whether the packet may be forwarded.
    ///
    /// Returns `false` without modifying the packet when `ttl <= 1`: a
    /// packet at TTL 1 has reached its last receiver and decrementing
    /// would put the forbidden TTL 0 on the wire. Otherwise decrements the
    /// TTL and returns `true`.
    ///
    /// # Examples
    /// ```
    /// use poholos::{Packet, WireId};
    ///
    /// let mut p = Packet::hearsay_with(WireId::new(1), 0, b"x", 2)?;
    /// assert!(p.hop());          // 2 -> 1, forward allowed
    /// assert!(!p.hop());         // at 1: do not forward
    /// assert_eq!(p.ttl(), 1);    // never reaches 0
    /// # Ok::<(), poholos::PacketError>(())
    /// ```
    #[must_use = "ignoring the result would forward packets past their TTL"]
    pub fn hop(&mut self) -> bool {
        if self.ttl <= 1 {
            false
        } else {
            self.ttl -= 1;
            true
        }
    }

    /// Returns the remaining hop count.
    #[must_use]
    pub fn ttl(&self) -> u8 {
        self.ttl
    }

    /// Returns the per-source sequence number.
    #[must_use]
    pub fn seq(&self) -> u16 {
        self.seq
    }

    /// Returns the originating node's wire id.
    #[must_use]
    pub fn src(&self) -> WireId {
        self.src
    }

    /// Returns the destination wire id, or `None` for hearsay (broadcast).
    #[must_use]
    pub fn dest(&self) -> Option<WireId> {
        self.dest
    }

    /// Returns `true` if this packet is addressed to a single node.
    #[must_use]
    pub fn is_telegram(&self) -> bool {
        self.dest.is_some()
    }

    /// Returns the payload bytes.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        self.payload.as_slice()
    }

    /// Computes the duplicate-suppression key for this packet.
    ///
    /// The key is FNV-1a 64 over the TTL-independent identity of the
    /// message: a shape flag, source, sequence number, destination (if
    /// any), and payload. Excluding the TTL is essential — the same message
    /// arriving via different routes carries different TTLs but must dedup
    /// to the same key.
    ///
    /// The fields are folded into the hash one at a time (FNV-1a is a
    /// streaming hash), which yields the same value as hashing their
    /// concatenation without needing a scratch buffer sized by `CAP`.
    #[must_use]
    pub fn dedup_key(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        hash = fnv64_update(hash, &[u8::from(self.dest.is_some())]);
        hash = fnv64_update(hash, &self.src.get().to_be_bytes());
        hash = fnv64_update(hash, &self.seq.to_be_bytes());
        if let Some(dest) = self.dest {
            hash = fnv64_update(hash, &dest.get().to_be_bytes());
        }
        fnv64_update(hash, self.payload.as_slice())
    }
}

fn validate_ttl(ttl: u8) -> Result<(), PacketError> {
    if ttl == 0 || ttl > MAX_TTL {
        Err(PacketError::ttl_out_of_range(ttl))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: WireId = WireId::new(0xAAAA_AAAA);
    const DEST: WireId = WireId::new(0xBBBB_BBBB);

    #[test]
    fn hearsay_uses_default_ttl_and_max_payload() {
        let p = Packet::hearsay(SRC, 1, &[7u8; MAX_PAYLOAD_HEARSAY]).unwrap();
        assert_eq!(p.ttl(), DEFAULT_TTL);
        assert_eq!(p.payload().len(), MAX_PAYLOAD_HEARSAY);
        Packet::hearsay(SRC, 1, &[7u8; MAX_PAYLOAD_HEARSAY + 1]).unwrap_err();
    }

    #[test]
    fn telegram_payload_is_tighter() {
        Packet::telegram(SRC, DEST, 1, &[7u8; MAX_PAYLOAD_TELEGRAM]).unwrap();
        Packet::telegram(SRC, DEST, 1, &[7u8; MAX_PAYLOAD_TELEGRAM + 1]).unwrap_err();
    }

    #[test]
    fn ttl_bounds_are_enforced() {
        Packet::hearsay_with(SRC, 1, b"x", 0).unwrap_err();
        Packet::hearsay_with(SRC, 1, b"x", MAX_TTL).unwrap();
        Packet::hearsay_with(SRC, 1, b"x", MAX_TTL + 1).unwrap_err();
    }

    #[test]
    fn hop_never_reaches_zero() {
        let mut p = Packet::hearsay_with(SRC, 1, b"x", 3).unwrap();
        assert!(p.hop());
        assert!(p.hop());
        assert_eq!(p.ttl(), 1);
        assert!(!p.hop());
        assert_eq!(p.ttl(), 1);
    }

    #[test]
    fn dedup_key_ignores_ttl_but_not_identity() {
        let a = Packet::hearsay_with(SRC, 1, b"x", 16).unwrap();
        let b = Packet::hearsay_with(SRC, 1, b"x", 3).unwrap();
        assert_eq!(a.dedup_key(), b.dedup_key());

        let c = Packet::hearsay_with(SRC, 2, b"x", 16).unwrap();
        assert_ne!(a.dedup_key(), c.dedup_key());

        let d = Packet::telegram(SRC, DEST, 1, b"x").unwrap();
        assert_ne!(a.dedup_key(), d.dedup_key());
    }

    #[test]
    fn larger_capacity_allows_larger_payload() {
        // A 211-capacity packet carries a 200-byte hearsay payload, which a
        // legacy 22-byte packet would reject.
        let p = PacketN::<211>::hearsay(SRC, 1, &[9u8; 200]).unwrap();
        assert_eq!(p.payload().len(), 200);
        PacketN::<211>::hearsay(SRC, 1, &[9u8; 205]).unwrap_err();
    }
}
