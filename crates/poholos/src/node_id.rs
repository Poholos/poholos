// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Node identity: compact on-wire ids and human-friendly names.
//!
//! Two identity types exist at different layers:
//!
//! * [`WireId`] — the 32-bit identity that actually travels in frames.
//!   Available on all targets, including `no_std`.
//! * [`NodeId`] *(`std` feature)* — the human-friendly form a person types
//!   and sees, e.g. `alice-3f2a`: a chosen name plus an auto-generated
//!   4-character hex suffix that keeps two `alice`s from colliding.
//!
//! A [`WireId`] is derived deterministically from the full `NodeId` string
//! via [`fnv64`](crate::fnv64) truncated to 32 bits, so any node that knows
//! a peer's full name (for `@bob-9c01 hello` unicast) can compute the same
//! wire id without a directory service.

use core::fmt::{self, Display, Formatter};

use crate::seen::fnv64;

/// Compact 32-bit node identity used inside on-air frames.
///
/// Derived from a node's full display name (see [`WireId::of_name`]) or
/// constructed from a raw value. Collisions are possible in principle
/// (32-bit space) but irrelevant at mesh scales of dozens of nodes.
///
/// # Examples
/// ```
/// use poholos::WireId;
///
/// let id = WireId::of_name("alice-3f2a");
/// assert_eq!(id, WireId::of_name("alice-3f2a"));
/// assert_ne!(id, WireId::of_name("alice-3f2b"));
/// ```
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WireId(u32);

impl WireId {
    /// Creates a wire id from a raw 32-bit value.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Derives the wire id of a node from its full display name.
    ///
    /// This is FNV-1a 64 of the UTF-8 bytes, truncated to the low 32 bits.
    /// The same name always yields the same id on every platform.
    #[must_use]
    pub fn of_name(name: impl AsRef<str>) -> Self {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "truncation to the low 32 bits is the documented derivation"
        )]
        Self(fnv64(name.as_ref().as_bytes()) as u32)
    }

    /// Returns the raw 32-bit value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Display for WireId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}

/// Maximum length of the user-chosen name part, in bytes.
///
/// Chosen so that the full display form (`name` + `-` + 4 hex chars, max 21
/// bytes) stays comfortably printable in a console line and well under any
/// advertising name budget should a future transport carry it verbatim.
#[cfg(feature = "std")]
pub const MAX_NAME_LEN: usize = 16;

/// Human-friendly node identity, e.g. `alice-3f2a`.
///
/// Composed of a user-chosen name (lowercase ASCII letters, digits, and
/// interior `-`) plus a `-xxxx` suffix of 4 lowercase hex characters taken
/// from caller-provided entropy. The suffix disambiguates nodes that chose
/// the same name. This type requires the `std` feature.
///
/// # Examples
/// ```
/// use poholos::NodeId;
///
/// let id = NodeId::new("alice", 0x0000_3f2a)?;
/// assert_eq!(id.as_str(), "alice-3f2a");
/// let same = NodeId::parse("alice-3f2a")?;
/// assert_eq!(id.wire_id(), same.wire_id());
/// # Ok::<(), poholos::NodeIdError>(())
/// ```
#[cfg(feature = "std")]
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct NodeId {
    full: String,
}

#[cfg(feature = "std")]
impl NodeId {
    /// Creates a node id from a chosen name and caller-provided entropy.
    ///
    /// The low 16 bits of `entropy` become the 4-character hex suffix. The
    /// crate is sans-io and therefore does not generate randomness itself;
    /// pass e.g. `rand::random()` from the application.
    ///
    /// # Errors
    /// Returns [`NodeIdError`] if `name` is empty, longer than
    /// [`MAX_NAME_LEN`], contains characters outside `[a-z0-9-]`, or starts
    /// or ends with `-`.
    pub fn new(name: &str, entropy: u32) -> Result<Self, NodeIdError> {
        validate_name(name)?;
        let suffix = entropy & 0xFFFF;
        Ok(Self {
            full: format!("{name}-{suffix:04x}"),
        })
    }

    /// Parses a full display form such as `alice-3f2a`.
    ///
    /// # Errors
    /// Returns [`NodeIdError`] if the string lacks the `-xxxx` hex suffix
    /// or the name part fails validation (see [`NodeId::new`]).
    pub fn parse(s: &str) -> Result<Self, NodeIdError> {
        let (name, suffix) = s.rsplit_once('-').ok_or_else(NodeIdError::new)?;
        if suffix.len() != 4
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(NodeIdError::new());
        }
        validate_name(name)?;
        Ok(Self { full: s.to_owned() })
    }

    /// Returns the full display form, e.g. `alice-3f2a`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.full
    }

    /// Derives the compact on-wire identity for this node.
    #[must_use]
    pub fn wire_id(&self) -> WireId {
        WireId::of_name(&self.full)
    }
}

#[cfg(feature = "std")]
impl Display for NodeId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.full)
    }
}

#[cfg(feature = "std")]
impl core::str::FromStr for NodeId {
    type Err = NodeIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(feature = "std")]
fn validate_name(name: &str) -> Result<(), NodeIdError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if ok { Ok(()) } else { Err(NodeIdError::new()) }
}

/// Failure to create or parse a [`NodeId`].
///
/// Names must be 1–[`MAX_NAME_LEN`] bytes of `[a-z0-9-]`, must not start or
/// end with `-`, and the parsed form must end in a 4-character lowercase
/// hex suffix.
#[cfg(feature = "std")]
#[derive(Debug)]
pub struct NodeIdError {
    backtrace: std::backtrace::Backtrace,
}

#[cfg(feature = "std")]
impl NodeIdError {
    fn new() -> Self {
        Self {
            backtrace: std::backtrace::Backtrace::capture(),
        }
    }
}

#[cfg(feature = "std")]
impl Display for NodeIdError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid node id: expected 1-{MAX_NAME_LEN} chars of [a-z0-9-] \
             (no leading/trailing '-'), with a 4-hex-char suffix when parsing"
        )?;
        if self.backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

#[cfg(feature = "std")]
impl core::error::Error for NodeIdError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_id_is_deterministic() {
        assert_eq!(WireId::of_name("alice-3f2a"), WireId::of_name("alice-3f2a"));
        assert_ne!(WireId::of_name("alice-3f2a"), WireId::of_name("bob-9c01"));
    }

    #[cfg(feature = "std")]
    #[test]
    fn node_id_suffix_uses_low_16_bits_of_entropy() {
        let id = NodeId::new("alice", 0xDEAD_3F2A).unwrap();
        assert_eq!(id.as_str(), "alice-3f2a");
    }

    #[cfg(feature = "std")]
    #[test]
    fn node_id_round_trips_through_parse() {
        let id = NodeId::new("node-b", 0x1C2D).unwrap();
        let parsed = NodeId::parse(id.as_str()).unwrap();
        assert_eq!(id, parsed);
        assert_eq!(id.wire_id(), parsed.wire_id());
    }

    #[cfg(feature = "std")]
    #[test]
    fn node_id_rejects_bad_names() {
        NodeId::new("", 0).unwrap_err();
        NodeId::new("-alice", 0).unwrap_err();
        NodeId::new("alice-", 0).unwrap_err();
        NodeId::new("Alice", 0).unwrap_err();
        NodeId::new("seventeen-chars-x", 0).unwrap_err();
        NodeId::parse("alice").unwrap_err();
        NodeId::parse("alice-3F2A").unwrap_err();
        NodeId::parse("alice-3f2").unwrap_err();
    }
}
