// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Duplicate suppression: the seen-packet cache and the FNV-1a hash.
//!
//! Flood routing re-broadcasts every packet, so each node must remember
//! what it has already handled. [`SeenCache`] keeps the
//! [`SEEN_CAPACITY`] most recently *heard* [`dedup
//! keys`](crate::Packet::dedup_key), evicting the stalest when full.
//!
//! Eviction order is LRU-style, not pure FIFO: re-inserting a key that is
//! already present refreshes it to newest. This matters on radio
//! transports — a BLE advertisement keeps repeating until its sender
//! replaces it, so a frame still on air is re-heard constantly. Refreshing
//! on every hearing keeps such a frame from aging out of the cache and
//! being re-delivered to the user as new.
//!
//! Two backends share an identical public API and observable behavior:
//!
//! * `std` — a [`hashlink::LinkedHashSet`], giving O(1) lookup and ordered
//!   eviction (its `insert` moves duplicates to the back).
//! * `no_std` — a fixed `[u64; 512]` ring buffer with linear scan: zero
//!   allocation, 4 KiB of state, fast enough at this size for embedded
//!   targets.
//!
//! Both use [`fnv64`] (FNV-1a, 64-bit) as the hash, chosen for its tiny
//! `no_std`-friendly implementation and good dispersion on short inputs —
//! collision *resistance* is not a goal here, matching the guideline that
//! Rust's default SipHash is overkill where adversarial collisions don't
//! matter.

/// Number of dedup keys remembered before the oldest is evicted.
///
/// 512 entries comfortably exceeds the number of distinct packets a small
/// mesh can circulate within any plausible re-broadcast window, while
/// costing only 4 KiB in the `no_std` ring backend. Lowering it risks
/// re-delivering looped packets; raising it costs memory (and scan time in
/// the ring backend) linearly.
pub const SEEN_CAPACITY: usize = 512;

/// Computes the 64-bit FNV-1a hash of `bytes`.
///
/// Used for packet dedup keys and for deriving
/// [`WireId`](crate::WireId)s from node names. Stable across platforms
/// and releases: changing it would break on-air interop.
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

/// Bounded FIFO cache of recently seen packet dedup keys.
///
/// # Examples
/// ```
/// use poholos::SeenCache;
///
/// let mut seen = SeenCache::new();
/// assert!(seen.insert(42));   // newly seen
/// assert!(!seen.insert(42));  // duplicate
/// ```
#[derive(Debug)]
pub struct SeenCache {
    #[cfg(feature = "std")]
    inner: hashlink::LinkedHashSet<u64>,
    #[cfg(not(feature = "std"))]
    inner: Ring,
}

impl SeenCache {
    /// Creates an empty cache with capacity [`SEEN_CAPACITY`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "std")]
            inner: hashlink::LinkedHashSet::with_capacity(SEEN_CAPACITY),
            #[cfg(not(feature = "std"))]
            inner: Ring::new(),
        }
    }

    /// Records `key`, returning `true` if it was not already present.
    ///
    /// Recording a key that is already present refreshes it to newest.
    /// When the cache is at capacity, recording a new key evicts the
    /// stalest one.
    pub fn insert(&mut self, key: u64) -> bool {
        #[cfg(feature = "std")]
        {
            if !self.inner.insert(key) {
                return false;
            }
            if self.inner.len() > SEEN_CAPACITY {
                self.inner.pop_front();
            }
            true
        }
        #[cfg(not(feature = "std"))]
        {
            self.inner.insert(key)
        }
    }

    /// Returns `true` if `key` is currently remembered.
    #[must_use]
    pub fn contains(&self, key: u64) -> bool {
        #[cfg(feature = "std")]
        {
            self.inner.contains(&key)
        }
        #[cfg(not(feature = "std"))]
        {
            self.inner.contains(key)
        }
    }

    /// Returns the number of keys currently remembered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns `true` if no keys are remembered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SeenCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed-size ring backend for `no_std` targets: linear-scan membership,
/// sequential fill, then overwrite-stalest once full. Duplicate inserts
/// refresh the key to newest, matching the `std` backend.
#[cfg(not(feature = "std"))]
#[derive(Debug)]
struct Ring {
    slots: [u64; SEEN_CAPACITY],
    /// Number of filled slots, saturating at `SEEN_CAPACITY`.
    len: usize,
    /// Physical index of logical position 0 (the stalest entry). Stays 0
    /// during the fill phase; advances once the ring is full.
    head: usize,
}

#[cfg(not(feature = "std"))]
impl Ring {
    fn new() -> Self {
        Self {
            slots: [0; SEEN_CAPACITY],
            len: 0,
            head: 0,
        }
    }

    /// Maps a logical position (0 = stalest, `len - 1` = newest) to its
    /// physical slot index.
    fn phys(&self, logical: usize) -> usize {
        (self.head + logical) % SEEN_CAPACITY
    }

    fn contains(&self, key: u64) -> bool {
        self.slots[..self.len].contains(&key)
    }

    fn insert(&mut self, key: u64) -> bool {
        if let Some(found) = (0..self.len).find(|&k| self.slots[self.phys(k)] == key) {
            // Refresh: shift everything newer down one logical slot and
            // re-append the key as newest. O(len) of u64 moves, on par
            // with the membership scan that just ran.
            for k in found..self.len - 1 {
                self.slots[self.phys(k)] = self.slots[self.phys(k + 1)];
            }
            self.slots[self.phys(self.len - 1)] = key;
            return false;
        }
        if self.len < SEEN_CAPACITY {
            self.slots[self.len] = key;
            self.len += 1;
        } else {
            // Overwrite the stalest entry; it becomes the newest.
            self.slots[self.head] = key;
            self.head = (self.head + 1) % SEEN_CAPACITY;
        }
        true
    }

    fn len(&self) -> usize {
        self.len
    }
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

    #[test]
    fn insert_reports_novelty() {
        let mut seen = SeenCache::new();
        assert!(seen.insert(1));
        assert!(seen.insert(2));
        assert!(!seen.insert(1));
        assert_eq!(seen.len(), 2);
        assert!(seen.contains(2));
        assert!(!seen.contains(3));
    }

    #[test]
    fn capacity_evicts_oldest_first() {
        let mut seen = SeenCache::new();
        for k in 0..SEEN_CAPACITY as u64 {
            assert!(seen.insert(k));
        }
        assert_eq!(seen.len(), SEEN_CAPACITY);

        // One past capacity evicts key 0, the oldest.
        assert!(seen.insert(SEEN_CAPACITY as u64));
        assert_eq!(seen.len(), SEEN_CAPACITY);
        assert!(!seen.contains(0));
        assert!(seen.contains(1));
        assert!(seen.contains(SEEN_CAPACITY as u64));

        // Re-inserting the evicted key counts as new again.
        assert!(seen.insert(0));
    }

    #[test]
    fn duplicate_hit_refreshes_age() {
        let mut seen = SeenCache::new();
        for k in 0..SEEN_CAPACITY as u64 {
            assert!(seen.insert(k));
        }

        // Re-hearing the stalest key (a frame still on air) refreshes it
        // to newest, so the next eviction takes key 1 instead.
        assert!(!seen.insert(0));
        assert!(seen.insert(SEEN_CAPACITY as u64));
        assert!(seen.contains(0));
        assert!(!seen.contains(1));
    }
}
