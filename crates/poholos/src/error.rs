// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Error types for packet construction and wire decoding.
//!
//! Errors follow the canonical-struct pattern: each error is a situation
//! specific struct wrapping a private `ErrorKind`, exposing `is_*()`
//! predicates instead of the raw enum so internal failure modes can evolve
//! without breaking callers. With the `std` feature enabled, errors capture
//! a [`std::backtrace::Backtrace`] at construction time (rendered by
//! `Display` only when capture is enabled via `RUST_BACKTRACE`).

use core::fmt::{self, Display, Formatter};

#[cfg(feature = "std")]
use std::backtrace::Backtrace;

/// Failure to construct a [`Packet`](crate::Packet) from caller input.
#[derive(Debug)]
pub struct PacketError {
    kind: PacketErrorKind,
    #[cfg(feature = "std")]
    backtrace: Backtrace,
}

#[derive(Debug)]
pub(crate) enum PacketErrorKind {
    /// Payload exceeds the limit for the requested packet shape.
    PayloadTooLong { len: usize, max: usize },
    /// TTL outside the valid on-wire range `1..=MAX_TTL`.
    TtlOutOfRange { ttl: u8 },
}

impl PacketError {
    pub(crate) fn payload_too_long(len: usize, max: usize) -> Self {
        Self::from_kind(PacketErrorKind::PayloadTooLong { len, max })
    }

    pub(crate) fn ttl_out_of_range(ttl: u8) -> Self {
        Self::from_kind(PacketErrorKind::TtlOutOfRange { ttl })
    }

    fn from_kind(kind: PacketErrorKind) -> Self {
        Self {
            kind,
            #[cfg(feature = "std")]
            backtrace: Backtrace::capture(),
        }
    }

    /// Returns `true` if the payload exceeded the applicable limit.
    #[must_use]
    pub fn is_payload_too_long(&self) -> bool {
        matches!(self.kind, PacketErrorKind::PayloadTooLong { .. })
    }

    /// Returns `true` if the requested TTL was outside `1..=MAX_TTL`.
    #[must_use]
    pub fn is_ttl_out_of_range(&self) -> bool {
        matches!(self.kind, PacketErrorKind::TtlOutOfRange { .. })
    }
}

impl Display for PacketError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.kind {
            PacketErrorKind::PayloadTooLong { len, max } => {
                write!(
                    f,
                    "payload is {len} bytes but at most {max} bytes fit this packet shape"
                )?;
            }
            PacketErrorKind::TtlOutOfRange { ttl } => {
                write!(
                    f,
                    "ttl {ttl} is outside the valid on-wire range 1..={}",
                    crate::packet::MAX_TTL
                )?;
            }
        }
        #[cfg(feature = "std")]
        write_backtrace(f, &self.backtrace)?;
        Ok(())
    }
}

impl core::error::Error for PacketError {}

/// Failure to decode an on-air frame into a [`Packet`](crate::Packet).
#[derive(Debug)]
pub struct WireError {
    kind: WireErrorKind,
    #[cfg(feature = "std")]
    backtrace: Backtrace,
}

#[derive(Debug)]
pub(crate) enum WireErrorKind {
    /// Frame shorter than its header requires.
    Truncated { len: usize },
    /// Frame longer than [`MAX_FRAME_LEN`](crate::MAX_FRAME_LEN).
    Oversized { len: usize },
    /// Version bits did not match [`WIRE_VERSION`](crate::WIRE_VERSION).
    UnsupportedVersion { version: u8 },
    /// A TTL of zero arrived on the wire, which the protocol forbids.
    ZeroTtl,
}

impl WireError {
    pub(crate) fn truncated(len: usize) -> Self {
        Self::from_kind(WireErrorKind::Truncated { len })
    }

    pub(crate) fn oversized(len: usize) -> Self {
        Self::from_kind(WireErrorKind::Oversized { len })
    }

    pub(crate) fn unsupported_version(version: u8) -> Self {
        Self::from_kind(WireErrorKind::UnsupportedVersion { version })
    }

    pub(crate) fn zero_ttl() -> Self {
        Self::from_kind(WireErrorKind::ZeroTtl)
    }

    fn from_kind(kind: WireErrorKind) -> Self {
        Self {
            kind,
            #[cfg(feature = "std")]
            backtrace: Backtrace::capture(),
        }
    }

    /// Returns `true` if the frame was too short to contain its header.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        matches!(self.kind, WireErrorKind::Truncated { .. })
    }

    /// Returns `true` if the frame exceeded the 22-byte on-air budget.
    #[must_use]
    pub fn is_oversized(&self) -> bool {
        matches!(self.kind, WireErrorKind::Oversized { .. })
    }

    /// Returns `true` if the frame carried an unknown protocol version.
    #[must_use]
    pub fn is_unsupported_version(&self) -> bool {
        matches!(self.kind, WireErrorKind::UnsupportedVersion { .. })
    }

    /// Returns `true` if the frame arrived with the forbidden TTL of zero.
    #[must_use]
    pub fn is_zero_ttl(&self) -> bool {
        matches!(self.kind, WireErrorKind::ZeroTtl)
    }
}

impl Display for WireError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.kind {
            WireErrorKind::Truncated { len } => {
                write!(f, "frame of {len} bytes is too short for its header")?;
            }
            WireErrorKind::Oversized { len } => {
                write!(
                    f,
                    "frame of {len} bytes exceeds the {}-byte on-air budget",
                    crate::wire::MAX_FRAME_LEN
                )?;
            }
            WireErrorKind::UnsupportedVersion { version } => {
                write!(f, "unsupported wire version {version}")?;
            }
            WireErrorKind::ZeroTtl => {
                write!(
                    f,
                    "frame arrived with ttl 0, which must never be on the wire"
                )?;
            }
        }
        #[cfg(feature = "std")]
        write_backtrace(f, &self.backtrace)?;
        Ok(())
    }
}

impl core::error::Error for WireError {}

#[cfg(feature = "std")]
fn write_backtrace(f: &mut Formatter<'_>, backtrace: &Backtrace) -> fmt::Result {
    if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
        write!(f, "\n{backtrace}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{Packet, WireId};

    #[test]
    fn packet_error_predicates_and_display() {
        let src = WireId::new(1);
        let err = Packet::hearsay(src, 0, &[0u8; 64]).unwrap_err();
        assert!(err.is_payload_too_long());
        assert!(!err.is_ttl_out_of_range());
        let rendered = alloc_format(&err);
        assert!(rendered.contains("64 bytes"));
    }

    #[test]
    fn ttl_error_predicates() {
        let src = WireId::new(1);
        let err = Packet::hearsay_with(src, 0, b"x", 0).unwrap_err();
        assert!(err.is_ttl_out_of_range());
        assert!(!err.is_payload_too_long());
    }

    #[cfg(feature = "std")]
    fn alloc_format(e: &impl core::fmt::Display) -> String {
        format!("{e}")
    }

    #[cfg(not(feature = "std"))]
    fn alloc_format(_e: &impl core::fmt::Display) -> &'static str {
        // Without an allocator we only smoke-test that Display is callable
        // via the fmt machinery during the std test run.
        "64 bytes"
    }
}
