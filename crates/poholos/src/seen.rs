// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Duplicate suppression: the FNV-1a hash behind dedup keys and wire ids.
//!
//! Only [`fnv64`] is implemented for now; the `SeenCache` that consumes it
//! lands together with the router.

/// Computes the 64-bit FNV-1a hash of `bytes`.
///
/// Used for packet dedup keys and for deriving [`WireId`](crate::WireId)s
/// from node names. Stable across platforms and releases: changing it would
/// break on-air interop.
///
/// # Examples
/// ```
/// use poholos::fnv64;
///
/// assert_eq!(fnv64(b""), 0xcbf2_9ce4_8422_2325);
/// assert_ne!(fnv64(b"alice-3f2a"), fnv64(b"bob-9c01"));
/// ```
#[must_use]
pub fn fnv64(bytes: &[u8]) -> u64 {
    // FNV-1a 64-bit offset basis and prime, per the FNV reference spec.
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv64_matches_reference_vectors() {
        // Reference vectors from the FNV-1a specification.
        assert_eq!(fnv64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv64(b"foobar"), 0x8594_4171_f739_67e8);
    }
}
