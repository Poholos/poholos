// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! On-air frame codec — packing packets into BLE advertising frames.
//!
//! Legacy BLE advertising allows ~31 bytes of advertising data; after the
//! flags AD structure and the manufacturer-data AD header (length, type,
//! 16-bit company id), **22 bytes** remain on every desktop platform
//! surveyed (macOS, Windows, Linux/BlueZ). This module packs a
//! [`Packet`](crate::Packet) into that budget with a variable-length
//! header:
//!
//! ```text
//! byte 0       ver(2 bits) | has_dest(1 bit) | ttl(5 bits)
//! bytes 1..3   seq, u16 big-endian
//! bytes 3..7   src wire id, u32 big-endian
//! [bytes 7..11 dest wire id, u32 big-endian — present iff has_dest]
//! rest         payload (15 bytes hearsay / 11 bytes telegram)
//! ```
//!
//! All multi-byte fields are big-endian (network order). A TTL of zero is
//! invalid on the wire and rejected by [`decode`]; the
//! [`Packet`](crate::Packet) constructors and
//! [`hop`](crate::Packet::hop) guarantee [`encode`] never produces one.
//!
//! # Frame capacity
//!
//! [`FrameN`] is generic over a `CAP` const that sizes the inline buffer;
//! [`encode`]/[`decode`] are generic over the same `CAP`. The alias [`Frame`]
//! fixes `CAP` to [`MAX_FRAME_LEN`] (the legacy 22-byte frame) and is what
//! nearly all code uses; the [`ExtFrame`] alias ([`MAX_EXT_FRAME_LEN`]) reuses
//! the identical codec for BLE 5 extended advertising.
//!
//! # Wire versions
//!
//! Byte 0 carries a 2-bit version. [`encode`] writes version 0
//! ([`WIRE_VERSION`]) for any frame that fits [`MAX_FRAME_LEN`] and version 1
//! ([`WIRE_VERSION_EXT`]) for longer frames; the two share this identical
//! header layout. [`decode`] accepts either, so a node is dual-stack — it
//! emits the smallest-reach version a message needs (short messages stay
//! version 0 so every node, legacy included, can carry them) and understands
//! both on receipt.

use crate::error::WireError;
use crate::node_id::WireId;
use crate::packet::PacketN;

/// Bluetooth manufacturer-specific-data company identifier for poholos.
///
/// `0xF10C` (61,708) was chosen far above the currently assigned company id
/// range (~3,500 ids as of 2025; see the `nordic/bluetooth-numbers-database`
/// repository) to avoid colliding with real vendors, and deliberately not
/// `0xFFFF`, which BlueZ silently drops. Transports place the encoded frame
/// in the manufacturer data of this company id; scanners filter on it.
pub const COMPANY_ID: u16 = 0xF10C;

/// Wire protocol version carried in the top 2 bits of byte 0.
///
/// Version 0 is the legacy frame that fits the universal ~22-byte legacy
/// advertising budget. See also [`WIRE_VERSION_EXT`].
pub const WIRE_VERSION: u8 = 0;

/// Wire protocol version for extended-advertising frames (BLE 5+), which may
/// exceed the legacy 22-byte budget.
///
/// The header layout is identical to version 0 — only the permitted frame
/// length differs. [`encode`] selects this version automatically when a
/// frame does not fit [`MAX_FRAME_LEN`], and [`decode`] accepts it
/// alongside version 0.
pub const WIRE_VERSION_EXT: u8 = 1;

/// Maximum encoded frame length in bytes.
///
/// The 22-byte budget that survives the legacy-advertising AD overhead on
/// every supported desktop platform. Changing this breaks on-air
/// compatibility and the payload limits derived from it.
pub const MAX_FRAME_LEN: usize = 22;

/// Maximum encoded frame length for an extended (wire version 1) frame.
///
/// Sized so both packet shapes carry a 200-byte payload over BLE 5 extended
/// advertising: `211` = the 11-byte telegram header + 200. Hearsay frames,
/// with the shorter header, reach 204 payload bytes. Individual adapters may
/// cap their actual TX below this (one Windows adapter measured 156); that
/// is a transport-layer concern, not a wire limit.
pub const MAX_EXT_FRAME_LEN: usize = HEADER_LEN_TELEGRAM + 200;

/// Header length of a hearsay (broadcast) frame: flags + seq + src.
pub(crate) const HEADER_LEN_HEARSAY: usize = 1 + 2 + 4;

/// Header length of a telegram (unicast) frame: flags + seq + src + dest.
pub(crate) const HEADER_LEN_TELEGRAM: usize = HEADER_LEN_HEARSAY + 4;

const VERSION_SHIFT: u8 = 6;
const HAS_DEST_BIT: u8 = 0b0010_0000;
const TTL_MASK: u8 = 0b0001_1111;

/// An encoded on-air frame, generic over its inline capacity `CAP`.
///
/// `FrameN` is the unit transports send and receive. It is `Copy`,
/// allocation-free, and valid under `no_std`. Most code uses the [`Frame`]
/// alias (`CAP` = [`MAX_FRAME_LEN`]).
///
/// # Examples
/// ```
/// use poholos::{encode, decode, Packet, WireId};
///
/// let p = Packet::hearsay(WireId::new(42), 7, b"hello")?;
/// let frame = encode(&p);
/// assert_eq!(decode(frame.as_bytes())?, p);
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct FrameN<const CAP: usize> {
    len: u8,
    buf: [u8; CAP],
}

// Hand-written serde: serde's array impls don't cover const-generic `[u8;
// CAP]`, so we (de)serialize the meaningful frame bytes as a byte string.
#[cfg(feature = "serde")]
impl<const CAP: usize> serde::Serialize for FrameN<CAP> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.as_bytes())
    }
}

#[cfg(feature = "serde")]
impl<'de, const CAP: usize> serde::Deserialize<'de> for FrameN<CAP> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Vis<const CAP: usize>;
        impl<'de, const CAP: usize> serde::de::Visitor<'de> for Vis<CAP> {
            type Value = FrameN<CAP>;
            fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(formatter, "up to {CAP} frame bytes")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                FrameN::copy_from(v).map_err(serde::de::Error::custom)
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
                FrameN::copy_from(&buf[..n]).map_err(serde::de::Error::custom)
            }
        }
        deserializer.deserialize_bytes(Vis::<CAP>)
    }
}

/// A legacy (wire version 0) frame: [`FrameN`] with `CAP` = [`MAX_FRAME_LEN`].
pub type Frame = FrameN<MAX_FRAME_LEN>;

/// An extended (wire version 1) frame for BLE 5 extended advertising:
/// [`FrameN`] with `CAP` = [`MAX_EXT_FRAME_LEN`].
pub type ExtFrame = FrameN<MAX_EXT_FRAME_LEN>;

impl<const CAP: usize> FrameN<CAP> {
    /// The empty frame, used as ring-slot filler inside the crate.
    pub(crate) const EMPTY: Self = Self {
        len: 0,
        buf: [0u8; CAP],
    };

    /// Copies raw bytes received from a transport into a frame.
    ///
    /// This performs only a length check; use [`decode`] to validate and
    /// parse the contents.
    ///
    /// # Errors
    /// Returns [`WireError`] if `bytes` exceeds the capacity `CAP`.
    pub fn copy_from(bytes: &[u8]) -> Result<Self, WireError> {
        if bytes.len() > CAP {
            return Err(WireError::oversized(bytes.len()));
        }
        let mut buf = [0u8; CAP];
        buf[..bytes.len()].copy_from_slice(bytes);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "bytes.len() <= CAP <= 255 for every supported capacity, checked above"
        )]
        Ok(Self {
            len: bytes.len() as u8,
            buf,
        })
    }

    /// Returns the encoded bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..usize::from(self.len)]
    }

    /// Returns the encoded length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// Returns `true` if the frame contains no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const CAP: usize> AsRef<[u8]> for FrameN<CAP> {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// Encodes a packet into its on-air frame.
///
/// Infallible: the [`Packet`](crate::Packet) constructors and
/// [`hop`](crate::Packet::hop) maintain every invariant the wire format
/// needs (TTL in `1..=31`, payload within shape limits), so a `Packet`
/// always fits its frame.
#[must_use]
pub fn encode<const CAP: usize>(packet: &PacketN<CAP>) -> FrameN<CAP> {
    debug_assert!(
        packet.ttl() >= 1 && packet.ttl() <= TTL_MASK,
        "Packet TTL invariant broken"
    );

    let header_len = if packet.dest().is_some() {
        HEADER_LEN_TELEGRAM
    } else {
        HEADER_LEN_HEARSAY
    };
    // Frames within the legacy budget stay version 0 so every node (legacy
    // included) can carry them; only longer frames are tagged version 1,
    // restricting them to extended-advertising-capable transports.
    let version = if header_len + packet.payload().len() > MAX_FRAME_LEN {
        WIRE_VERSION_EXT
    } else {
        WIRE_VERSION
    };

    let mut buf = [0u8; CAP];
    let mut n = 0;

    buf[n] = (version << VERSION_SHIFT)
        | if packet.dest().is_some() {
            HAS_DEST_BIT
        } else {
            0
        }
        | (packet.ttl() & TTL_MASK);
    n += 1;
    buf[n..n + 2].copy_from_slice(&packet.seq().to_be_bytes());
    n += 2;
    buf[n..n + 4].copy_from_slice(&packet.src().get().to_be_bytes());
    n += 4;
    if let Some(dest) = packet.dest() {
        buf[n..n + 4].copy_from_slice(&dest.get().to_be_bytes());
        n += 4;
    }
    let payload = packet.payload();
    buf[n..n + payload.len()].copy_from_slice(payload);
    n += payload.len();

    #[expect(
        clippy::cast_possible_truncation,
        reason = "n <= CAP <= 255 by construction"
    )]
    FrameN { len: n as u8, buf }
}

/// Decodes raw received bytes into a packet.
///
/// # Errors
/// Returns [`WireError`] if the input is longer than the capacity `CAP`,
/// shorter than its header requires, carries an unsupported version, or
/// arrives with the forbidden TTL of zero.
///
/// # Panics
/// Never in practice: length and TTL are validated before the packet is
/// constructed, so the internal `expect` is unreachable unless the codec
/// itself is buggy.
pub fn decode<const CAP: usize>(bytes: &[u8]) -> Result<PacketN<CAP>, WireError> {
    if bytes.len() > CAP {
        return Err(WireError::oversized(bytes.len()));
    }
    if bytes.len() < HEADER_LEN_HEARSAY {
        return Err(WireError::truncated(bytes.len()));
    }

    let flags = bytes[0];
    let version = flags >> VERSION_SHIFT;
    // Versions 0 and 1 share this header layout (1 differs only in allowing
    // a longer frame), so a dual-stack node decodes both; reserve 2 and 3.
    if version != WIRE_VERSION && version != WIRE_VERSION_EXT {
        return Err(WireError::unsupported_version(version));
    }
    let has_dest = flags & HAS_DEST_BIT != 0;
    let ttl = flags & TTL_MASK;
    if ttl == 0 {
        return Err(WireError::zero_ttl());
    }

    let header_len = if has_dest {
        HEADER_LEN_TELEGRAM
    } else {
        HEADER_LEN_HEARSAY
    };
    if bytes.len() < header_len {
        return Err(WireError::truncated(bytes.len()));
    }

    let seq = u16::from_be_bytes([bytes[1], bytes[2]]);
    let src = WireId::new(u32::from_be_bytes([bytes[3], bytes[4], bytes[5], bytes[6]]));
    let payload = &bytes[header_len..];

    let packet = if has_dest {
        let dest = WireId::new(u32::from_be_bytes([
            bytes[7], bytes[8], bytes[9], bytes[10],
        ]));
        PacketN::telegram_with(src, dest, seq, payload, ttl)
    } else {
        PacketN::hearsay_with(src, seq, payload, ttl)
    };

    // Length and TTL were validated above, so constructor failure here
    // would be a codec bug, not a runtime condition (M-PANIC-ON-BUG).
    Ok(packet.expect("frame within capacity always satisfies packet invariants"))
}

/// The Bluetooth AD type tag for manufacturer-specific data.
const AD_TYPE_MANUFACTURER: u8 = 0xFF;

/// Bytes that precede the frame in a manufacturer-specific AD entry:
/// one AD-type byte plus the 2-byte company id.
const MANUFACTURER_HEADER_LEN: usize = 1 + 2;

/// Extracts the poholos frame bytes from raw BLE advertising data.
///
/// Advertising data is a sequence of *AD structures*, each
/// `[len, type, data...]` with `data` spanning `len - 1` bytes. This
/// walks them and returns the payload of the first manufacturer-specific
/// entry (type `0xFF`) carrying [`COMPANY_ID`] (little-endian, per the
/// Bluetooth specification). Returns `None` when no such entry exists or
/// the structures are malformed.
///
/// Transports built on stacks that pre-parse advertisements (btleplug on
/// desktop) do not need this; it exists for transports that see the raw
/// advertising payload, such as embedded radio drivers. Pass the result
/// to [`Frame::copy_from`] / [`decode`].
///
/// # Examples
/// ```
/// use poholos::{COMPANY_ID, manufacturer_frame};
///
/// let frame = [0x10, 0x00, 0x01, 0xAA, 0xBB, 0xCC, 0xDD, b'h', b'i'];
/// let mut ad = vec![0x02, 0x01, 0x06]; // Flags AD structure
/// ad.push(12); // 1 type byte + 2 company bytes + 9 frame bytes
/// ad.push(0xFF);
/// ad.extend_from_slice(&COMPANY_ID.to_le_bytes());
/// ad.extend_from_slice(&frame);
///
/// assert_eq!(manufacturer_frame(&ad), Some(&frame[..]));
/// assert_eq!(manufacturer_frame(&[0x02, 0x01, 0x06]), None);
/// ```
#[must_use]
pub fn manufacturer_frame(ad: &[u8]) -> Option<&[u8]> {
    let mut rest = ad;
    loop {
        let (&len, tail) = rest.split_first()?;
        if len == 0 {
            // A zero-length structure marks trailing padding; nothing
            // after it is valid.
            return None;
        }
        let len = usize::from(len);
        if len > tail.len() {
            return None; // truncated advertisement
        }
        let (entry, after) = tail.split_at(len);
        // `entry[0]` is the AD type (len >= 1 guarantees it); a
        // manufacturer entry needs at least the 2 company id bytes after.
        if entry[0] == AD_TYPE_MANUFACTURER
            && entry.len() >= MANUFACTURER_HEADER_LEN
            && u16::from_le_bytes([entry[1], entry[2]]) == COMPANY_ID
        {
            return Some(&entry[MANUFACTURER_HEADER_LEN..]);
        }
        rest = after;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::{MAX_PAYLOAD_HEARSAY, MAX_PAYLOAD_TELEGRAM, Packet};

    const SRC: WireId = WireId::new(0x1122_3344);
    const DEST: WireId = WireId::new(0x5566_7788);

    #[test]
    fn hearsay_round_trip_and_layout() {
        let p = Packet::hearsay_with(SRC, 0xBEEF, b"hi", 16).unwrap();
        let f = encode(&p);
        assert_eq!(f.len(), HEADER_LEN_HEARSAY + 2);
        let b = f.as_bytes();
        assert_eq!(b[0], 16); // ver 0, no dest, ttl 16
        assert_eq!(&b[1..3], &[0xBE, 0xEF]);
        assert_eq!(&b[3..7], &[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(&b[7..], b"hi");
        assert_eq!(decode(b).unwrap(), p);
    }

    #[test]
    fn telegram_round_trip_and_layout() {
        let p = Packet::telegram_with(SRC, DEST, 1, b"yo", 5).unwrap();
        let f = encode(&p);
        assert_eq!(f.len(), HEADER_LEN_TELEGRAM + 2);
        let b = f.as_bytes();
        assert_eq!(b[0], HAS_DEST_BIT | 5);
        assert_eq!(&b[7..11], &[0x55, 0x66, 0x77, 0x88]);
        assert_eq!(decode(b).unwrap(), p);
    }

    #[test]
    fn max_payloads_exactly_fill_the_frame() {
        let p = Packet::hearsay(SRC, 0, &[9u8; MAX_PAYLOAD_HEARSAY]).unwrap();
        assert_eq!(encode(&p).len(), MAX_FRAME_LEN);
        let p = Packet::telegram(SRC, DEST, 0, &[9u8; MAX_PAYLOAD_TELEGRAM]).unwrap();
        assert_eq!(encode(&p).len(), MAX_FRAME_LEN);
    }

    #[test]
    fn decode_rejects_bad_frames() {
        // These check error paths with no decoded packet to pin `CAP`, so
        // the capacity is named explicitly.
        assert!(
            decode::<MAX_FRAME_LEN>(&[0u8; 3])
                .unwrap_err()
                .is_truncated()
        );
        assert!(
            decode::<MAX_FRAME_LEN>(&[0u8; MAX_FRAME_LEN + 1])
                .unwrap_err()
                .is_oversized()
        );

        // Unicast flag set but header incomplete.
        let mut short = [0u8; HEADER_LEN_HEARSAY];
        short[0] = HAS_DEST_BIT | 5;
        assert!(decode::<MAX_FRAME_LEN>(&short).unwrap_err().is_truncated());

        // Unsupported version (2): versions 0 and 1 are accepted, 2 and 3
        // are reserved.
        let p = Packet::hearsay(SRC, 0, b"x").unwrap();
        let mut bytes = *encode(&p).as_bytes().first_chunk::<8>().unwrap();
        bytes[0] |= 0b1000_0000;
        assert!(
            decode::<MAX_FRAME_LEN>(&bytes)
                .unwrap_err()
                .is_unsupported_version()
        );

        // TTL 0 on the wire.
        let mut zero = [0u8; HEADER_LEN_HEARSAY + 1];
        zero[0] = 0;
        assert!(decode::<MAX_FRAME_LEN>(&zero).unwrap_err().is_zero_ttl());
    }

    #[test]
    fn frame_copy_from_checks_length() {
        Frame::copy_from(&[0u8; MAX_FRAME_LEN]).unwrap();
        assert!(
            Frame::copy_from(&[0u8; MAX_FRAME_LEN + 1])
                .unwrap_err()
                .is_oversized()
        );
    }

    #[test]
    fn ext_capacity_round_trips() {
        // The generic codec at a non-default capacity (the v1-shaped path):
        // a 200-byte payload no legacy 22-byte frame could ever hold.
        const CAP: usize = 211;
        let payload = [0xABu8; 200];

        let p = PacketN::<CAP>::hearsay_with(SRC, 0x1234, &payload, 16).unwrap();
        let f = encode(&p);
        assert_eq!(f.len(), HEADER_LEN_HEARSAY + payload.len());
        assert_eq!(decode::<CAP>(f.as_bytes()).unwrap(), p);

        let p = PacketN::<CAP>::telegram_with(SRC, DEST, 7, &payload[..150], 5).unwrap();
        let f = encode(&p);
        assert_eq!(f.len(), HEADER_LEN_TELEGRAM + 150);
        assert_eq!(decode::<CAP>(f.as_bytes()).unwrap(), p);
    }

    #[test]
    fn long_frame_is_tagged_version_one() {
        // A payload past the legacy budget forces wire version 1.
        let p = PacketN::<MAX_EXT_FRAME_LEN>::hearsay_with(SRC, 1, &[7u8; 100], 16).unwrap();
        let f = encode(&p);
        assert!(f.len() > MAX_FRAME_LEN);
        assert_eq!(f.as_bytes()[0] >> VERSION_SHIFT, WIRE_VERSION_EXT);
        assert_eq!(decode::<MAX_EXT_FRAME_LEN>(f.as_bytes()).unwrap(), p);
    }

    #[test]
    fn short_frame_stays_version_zero_at_ext_capacity() {
        // An extended-capacity packet whose payload fits the legacy budget
        // still encodes as version 0, byte-identical to the legacy encoding,
        // so legacy-only nodes can carry it.
        let p = PacketN::<MAX_EXT_FRAME_LEN>::hearsay_with(SRC, 0xBEEF, b"hi", 16).unwrap();
        let f = encode(&p);
        assert!(f.len() <= MAX_FRAME_LEN);
        assert_eq!(f.as_bytes()[0] >> VERSION_SHIFT, WIRE_VERSION);
        let legacy = encode(&Packet::hearsay_with(SRC, 0xBEEF, b"hi", 16).unwrap());
        assert_eq!(f.as_bytes(), legacy.as_bytes());
    }

    #[test]
    fn legacy_decode_rejects_oversized_ext_frame() {
        // A version-1 frame is longer than the legacy capacity, so a v0-only
        // decoder rejects it as oversized rather than misparsing it.
        let p = PacketN::<MAX_EXT_FRAME_LEN>::hearsay_with(SRC, 1, &[7u8; 100], 16).unwrap();
        let f = encode(&p);
        assert!(
            decode::<MAX_FRAME_LEN>(f.as_bytes())
                .unwrap_err()
                .is_oversized()
        );
    }

    #[test]
    fn ext_frame_copy_from_checks_capacity() {
        FrameN::<211>::copy_from(&[0u8; 211]).unwrap();
        assert!(
            FrameN::<211>::copy_from(&[0u8; 212])
                .unwrap_err()
                .is_oversized()
        );
    }

    #[test]
    fn manufacturer_frame_finds_poholos_entry_after_other_structures() {
        // Flags, then a foreign manufacturer entry, then ours.
        let ad = [
            0x02, 0x01, 0x06, // Flags
            0x04, 0xFF, 0x4C, 0x00, 0x10, // manufacturer 0x004C (foreign)
            0x06, 0xFF, 0x0C, 0xF1, b'p', b'h', b'o', // manufacturer 0xF10C (ours)
        ];
        assert_eq!(manufacturer_frame(&ad), Some(&b"pho"[..]));
    }

    #[test]
    fn manufacturer_frame_handles_empty_payload_and_zero_padding() {
        // Our company id with no payload, followed by zero padding.
        let ad = [0x03, 0xFF, 0x0C, 0xF1, 0x00, 0x00];
        assert_eq!(manufacturer_frame(&ad), Some(&[][..]));
    }

    #[test]
    fn manufacturer_frame_rejects_foreign_and_malformed_ads() {
        // No manufacturer entry at all.
        assert_eq!(manufacturer_frame(&[0x02, 0x01, 0x06]), None);
        // Foreign company id only.
        assert_eq!(manufacturer_frame(&[0x04, 0xFF, 0x4C, 0x00, 0x10]), None);
        // Manufacturer entry too short to carry a company id.
        assert_eq!(manufacturer_frame(&[0x02, 0xFF, 0x0C]), None);
        // Structure length runs past the buffer.
        assert_eq!(manufacturer_frame(&[0x09, 0xFF, 0x0C, 0xF1, b'x']), None);
        // Empty input and pure padding.
        assert_eq!(manufacturer_frame(&[]), None);
        assert_eq!(manufacturer_frame(&[0x00, 0x00]), None);
        // Padding hides anything after it (invalid placement per spec).
        assert_eq!(manufacturer_frame(&[0x00, 0x03, 0xFF, 0x0C, 0xF1]), None);
    }
}
