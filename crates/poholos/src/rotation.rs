// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Airtime rotation: time-sharing a single repeating broadcast slot.
//!
//! Radio transports built on BLE advertising have exactly one
//! advertisement on air at a time, and a frame must hold that slot across
//! several advertising events (hundreds of milliseconds) before nearby
//! scanners reliably hear it. If every send simply replaced the slot, a
//! busy mesh would evict frames long before they ever reached the air —
//! typically your own message, displaced by the next relay milliseconds
//! after you typed it.
//!
//! [`Rotation`] is the pure scheduling state machine that fixes this.
//! Callers enqueue frames as *own* (originated by the local user) or
//! *relay* (forwarded for the mesh), and repeatedly ask for the next
//! frame to put on air for one [`DWELL`]. Policy:
//!
//! * Own and relay turns alternate, so the local message is guaranteed
//!   half the airtime no matter how chatty the mesh gets.
//! * An own message stays in rotation for [`OWN_DWELLS`] turns (its
//!   guaranteed audibility window); a newly originated message supersedes
//!   it.
//! * Each relay gets exactly one dwell — it had its airtime, and flood
//!   redundancy across the other nodes covers the rest. At most
//!   [`RELAY_QUEUE`] relays wait; beyond that the oldest is shed, which
//!   is the right overload behavior: every neighbor heard the same frame
//!   and most of them will relay it too.
//!
//! Like everything in this crate the module is sans-io: no clocks, no
//! radio, no allocation. The transport's driver owns those — the BLE
//! advertise loop in `poholos-cli` on desktops, or an advertiser task on
//! embedded targets — and is expected to call [`Rotation::next_frame`]
//! once per [`DWELL`], keeping the previous frame on air when it returns
//! `None`.

use core::time::Duration;

use crate::wire::Frame;

/// How long each frame holds the advertising slot per turn.
///
/// Roughly 3–5 legacy advertising events at platform-default intervals
/// (~100–250 ms), enough for a continuously scanning receiver to hear the
/// frame with high probability. Raising it improves per-frame reliability
/// but adds latency per relay hop; lowering it risks frames airing zero
/// times.
pub const DWELL: Duration = Duration::from_millis(500);

/// Total dwells granted to an own message before it leaves the rotation.
///
/// 20 dwells of 500 ms give the local message ~10 s of guaranteed
/// rotation presence — far past the point where every neighbor in range
/// has heard it. After that the slot is freed for relays (the frame may
/// linger on air if nothing else is waiting).
pub const OWN_DWELLS: u32 = 20;

/// Maximum relays awaiting airtime before the oldest is shed.
///
/// Bounds the worst-case relay latency through this node to
/// `RELAY_QUEUE` × [`DWELL`] = 4 s per hop while still absorbing bursts.
pub const RELAY_QUEUE: usize = 8;

/// Pure scheduler deciding which frame holds the advertising slot next.
///
/// # Examples
/// ```
/// use poholos::rotation::Rotation;
/// use poholos::{Packet, WireId, encode};
///
/// let own = encode(&Packet::hearsay(WireId::new(1), 0, b"mine")?);
/// let relay = encode(&Packet::hearsay(WireId::new(2), 0, b"theirs")?);
///
/// let mut rotation = Rotation::new();
/// rotation.enqueue_own(own);
/// rotation.enqueue_relay(relay);
///
/// // Own and relay turns alternate.
/// assert_eq!(rotation.next_frame(), Some(own));
/// assert_eq!(rotation.next_frame(), Some(relay));
/// assert_eq!(rotation.next_frame(), Some(own));
/// # Ok::<(), poholos::PacketError>(())
/// ```
#[derive(Debug)]
pub struct Rotation {
    own: Option<Own>,
    relays: RelayRing,
    /// Whether the previous turn served the own frame (alternation state).
    last_was_own: bool,
}

#[derive(Debug)]
struct Own {
    frame: Frame,
    dwells_left: u32,
}

impl Rotation {
    /// Creates an empty rotation.
    #[must_use]
    pub fn new() -> Self {
        Self {
            own: None,
            relays: RelayRing::new(),
            last_was_own: false,
        }
    }

    /// Enqueues the local user's message, superseding any previous one
    /// and resetting its airtime budget to [`OWN_DWELLS`].
    pub fn enqueue_own(&mut self, frame: Frame) {
        self.own = Some(Own {
            frame,
            dwells_left: OWN_DWELLS,
        });
    }

    /// Enqueues a frame to relay for the mesh.
    ///
    /// When [`RELAY_QUEUE`] relays are already waiting, the oldest is
    /// dropped to make room.
    pub fn enqueue_relay(&mut self, frame: Frame) {
        self.relays.push_back(frame);
    }

    /// Returns the frame that should hold the slot for the next dwell.
    ///
    /// `None` means nothing is waiting; the driver may leave whatever is
    /// currently on air and sleep until new work arrives.
    pub fn next_frame(&mut self) -> Option<Frame> {
        let relay_turn = !self.relays.is_empty() && (self.last_was_own || self.own.is_none());
        if relay_turn {
            self.last_was_own = false;
            return self.relays.pop_front();
        }
        let own = self.own.as_mut()?;
        self.last_was_own = true;
        let frame = own.frame;
        own.dwells_left -= 1;
        if own.dwells_left == 0 {
            self.own = None;
        }
        Some(frame)
    }
}

impl Default for Rotation {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed-capacity FIFO of pending relays: allocation-free so the
/// scheduler works identically on embedded targets. Pushing onto a full
/// ring sheds the oldest entry.
#[derive(Debug)]
struct RelayRing {
    slots: [Frame; RELAY_QUEUE],
    /// Physical index of the oldest pending relay.
    head: usize,
    len: usize,
}

impl RelayRing {
    fn new() -> Self {
        Self {
            slots: [Frame::EMPTY; RELAY_QUEUE],
            head: 0,
            len: 0,
        }
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push_back(&mut self, frame: Frame) {
        if self.len == RELAY_QUEUE {
            // Shed the oldest: under overload its information is the
            // most likely to have already been relayed by neighbors.
            self.head = (self.head + 1) % RELAY_QUEUE;
            self.len -= 1;
        }
        self.slots[(self.head + self.len) % RELAY_QUEUE] = frame;
        self.len += 1;
    }

    fn pop_front(&mut self) -> Option<Frame> {
        if self.len == 0 {
            return None;
        }
        let frame = self.slots[self.head];
        self.head = (self.head + 1) % RELAY_QUEUE;
        self.len -= 1;
        Some(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(tag: u8) -> Frame {
        Frame::copy_from(&[tag]).unwrap()
    }

    #[test]
    fn own_and_relays_alternate() {
        let mut r = Rotation::new();
        r.enqueue_own(frame(0));
        r.enqueue_relay(frame(1));
        r.enqueue_relay(frame(2));

        assert_eq!(r.next_frame(), Some(frame(0)));
        assert_eq!(r.next_frame(), Some(frame(1)));
        assert_eq!(r.next_frame(), Some(frame(0)));
        assert_eq!(r.next_frame(), Some(frame(2)));
        // Relays exhausted: the own message keeps the slot.
        assert_eq!(r.next_frame(), Some(frame(0)));
    }

    #[test]
    fn relays_each_get_exactly_one_dwell() {
        let mut r = Rotation::new();
        r.enqueue_relay(frame(1));
        r.enqueue_relay(frame(2));

        assert_eq!(r.next_frame(), Some(frame(1)));
        assert_eq!(r.next_frame(), Some(frame(2)));
        assert_eq!(r.next_frame(), None);
    }

    #[test]
    fn own_budget_exhausts() {
        let mut r = Rotation::new();
        r.enqueue_own(frame(7));
        for _ in 0..OWN_DWELLS {
            assert_eq!(r.next_frame(), Some(frame(7)));
        }
        assert_eq!(r.next_frame(), None);
    }

    #[test]
    fn new_own_supersedes_and_resets_budget() {
        let mut r = Rotation::new();
        r.enqueue_own(frame(1));
        assert_eq!(r.next_frame(), Some(frame(1)));

        r.enqueue_own(frame(2));
        for _ in 0..OWN_DWELLS {
            assert_eq!(r.next_frame(), Some(frame(2)));
        }
        assert_eq!(r.next_frame(), None);
    }

    #[test]
    fn relay_overflow_sheds_oldest() {
        let mut r = Rotation::new();
        let overflow = u8::try_from(RELAY_QUEUE).unwrap() + 1;
        for tag in 0..overflow {
            r.enqueue_relay(frame(tag));
        }
        // Frame 0 was shed; the queue starts at 1.
        assert_eq!(r.next_frame(), Some(frame(1)));
    }

    #[test]
    fn relay_ring_survives_wraparound() {
        let mut r = Rotation::new();
        // Interleave fills and drains so head laps the physical array
        // several times, exercising the modular index math.
        let cap = u8::try_from(RELAY_QUEUE).unwrap();
        for round in 0..4u8 {
            for tag in 0..cap {
                r.enqueue_relay(frame(round * cap + tag));
            }
            for tag in 0..cap {
                assert_eq!(r.next_frame(), Some(frame(round * cap + tag)));
            }
            assert_eq!(r.next_frame(), None);
        }
    }

    #[test]
    fn idle_rotation_yields_nothing() {
        let mut r = Rotation::new();
        assert_eq!(r.next_frame(), None);
        r.enqueue_relay(frame(1));
        assert_eq!(r.next_frame(), Some(frame(1)));
        assert_eq!(r.next_frame(), None);
    }
}
