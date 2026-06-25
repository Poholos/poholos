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
//! # TTL semantics
//!
//! The TTL counts remaining hops and lives in 5 bits on the wire
//! ([`MAX_TTL`] = 31). The protocol invariant is that **a TTL of zero must
//! never be placed on the wire**. [`Packet::hop`] enforces this: it refuses
//! to decrement when `ttl <= 1`, so a packet received with TTL 1 can still
//! be delivered locally but will not be forwarded.

use crate::error::PacketError;
use crate::node_id::WireId;
use crate::seen::fnv64;
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

/// Maximum payload bytes in a hearsay (broadcast) packet.
///
/// Derived: [`MAX_FRAME_LEN`] (22) minus the 7-byte broadcast header.
pub const MAX_PAYLOAD_HEARSAY: usize = MAX_FRAME_LEN - HEADER_LEN_HEARSAY;

/// Maximum payload bytes in a telegram (unicast) packet.
///
/// Derived: [`MAX_FRAME_LEN`] (22) minus the 11-byte unicast header.
pub const MAX_PAYLOAD_TELEGRAM: usize = MAX_FRAME_LEN - HEADER_LEN_TELEGRAM;

/// Inline, fixed-capacity message payload (at most 15 bytes).
///
/// Stores bytes inline so [`Packet`] stays `Copy` and `no_std`-friendly.
/// The capacity equals [`MAX_PAYLOAD_HEARSAY`]; telegram packets are
/// further restricted to [`MAX_PAYLOAD_TELEGRAM`] by the constructors.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Payload {
    len: u8,
    buf: [u8; MAX_PAYLOAD_HEARSAY],
}

impl Payload {
    pub(crate) fn copy_from(bytes: &[u8], max: usize) -> Result<Self, PacketError> {
        if bytes.len() > max {
            return Err(PacketError::payload_too_long(bytes.len(), max));
        }
        let mut buf = [0u8; MAX_PAYLOAD_HEARSAY];
        buf[..bytes.len()].copy_from_slice(bytes);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "bytes.len() <= max <= 15, checked above"
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

impl AsRef<[u8]> for Payload {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

/// A parsed poholos protocol message.
///
/// Construct via [`Packet::hearsay`] / [`Packet::telegram`] (or their
/// `*_with` variants to control TTL). Encode with
/// [`encode`](crate::encode), decode with [`decode`](crate::decode), or let
/// a [`Router`](crate::Router) handle both.
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
pub struct Packet {
    ttl: u8,
    seq: u16,
    src: WireId,
    dest: Option<WireId>,
    payload: Payload,
}

impl Packet {
    /// Creates a broadcast packet with [`DEFAULT_TTL`].
    ///
    /// `seq` is a per-source sequence number used (together with the source
    /// and payload) for duplicate suppression; sources should increment it
    /// per message.
    ///
    /// # Errors
    /// Returns [`PacketError`] if `payload` exceeds
    /// [`MAX_PAYLOAD_HEARSAY`] bytes.
    pub fn hearsay(src: WireId, seq: u16, payload: &[u8]) -> Result<Self, PacketError> {
        Self::hearsay_with(src, seq, payload, DEFAULT_TTL)
    }

    /// Creates a broadcast packet with an explicit TTL.
    ///
    /// # Errors
    /// Returns [`PacketError`] if `payload` exceeds
    /// [`MAX_PAYLOAD_HEARSAY`] bytes, or if `ttl` is outside `1..=MAX_TTL`.
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
            payload: Payload::copy_from(payload, MAX_PAYLOAD_HEARSAY)?,
        })
    }

    /// Creates a unicast packet to `dest` with [`DEFAULT_TTL`].
    ///
    /// # Errors
    /// Returns [`PacketError`] if `payload` exceeds
    /// [`MAX_PAYLOAD_TELEGRAM`] bytes.
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
    /// Returns [`PacketError`] if `payload` exceeds
    /// [`MAX_PAYLOAD_TELEGRAM`] bytes, or if `ttl` is outside `1..=MAX_TTL`.
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
            payload: Payload::copy_from(payload, MAX_PAYLOAD_TELEGRAM)?,
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
    #[must_use]
    pub fn dedup_key(&self) -> u64 {
        // 1 flag + 4 src + 2 seq + 4 dest + 15 payload = 26 bytes max.
        let mut buf = [0u8; 1 + 4 + 2 + 4 + MAX_PAYLOAD_HEARSAY];
        let mut n = 0;
        buf[n] = u8::from(self.dest.is_some());
        n += 1;
        buf[n..n + 4].copy_from_slice(&self.src.get().to_be_bytes());
        n += 4;
        buf[n..n + 2].copy_from_slice(&self.seq.to_be_bytes());
        n += 2;
        if let Some(dest) = self.dest {
            buf[n..n + 4].copy_from_slice(&dest.get().to_be_bytes());
            n += 4;
        }
        let payload = self.payload.as_slice();
        buf[n..n + payload.len()].copy_from_slice(payload);
        n += payload.len();
        fnv64(&buf[..n])
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
}
