// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Flood routing: the pure, sans-io decision engine.
//!
//! A [`Router`] owns a node's routing state — its own
//! [`WireId`](crate::WireId) and a [`SeenCache`](crate::SeenCache) — and
//! makes one decision per received frame. It performs no I/O whatsoever:
//! the caller feeds in raw bytes via [`Router::ingest`] and acts on the
//! returned [`RouteAction`]. This makes the entire protocol testable
//! without radios and portable to `no_std` targets.
//!
//! # Routing rules
//!
//! In order, for every decodable frame:
//!
//! 1. Packets whose source is this node are ignored ([`IgnoreReason::Own`])
//!    — our own advertisements echo back from the radio.
//! 2. Duplicates (by [`dedup key`](crate::Packet::dedup_key)) are ignored.
//! 3. **Deliver locally first**: hearsay is always delivered; a telegram is
//!    delivered iff addressed to this node.
//! 4. **Then gate forwarding on [`hop`](crate::Packet::hop)**: hearsay is
//!    re-broadcast if the TTL allows; a telegram for another node is
//!    forwarded if the TTL allows; a telegram delivered here is never
//!    forwarded (the destination consumes it).

use crate::error::WireError;
use crate::node_id::WireId;
use crate::packet::Packet;
use crate::seen::SeenCache;
use crate::wire::{self, Frame};

/// What a node should do with a frame it just received.
///
/// `Deliver*` variants carry the decoded packet to show the user;
/// `*Forward` variants carry the re-encoded frame (TTL already
/// decremented) to hand back to the transport.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RouteAction {
    /// Show this packet to the local user; do not forward.
    Deliver(Packet),
    /// Show this packet to the local user and re-broadcast the frame.
    DeliverAndForward(Packet, Frame),
    /// Not for us: re-broadcast the frame without local delivery.
    Forward(Frame),
    /// Do nothing; the reason says why.
    Ignore(IgnoreReason),
}

/// Why a received frame produced no deliver or forward action.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum IgnoreReason {
    /// The packet originated from this node (our own echo).
    Own,
    /// The packet was already handled (seen-cache hit).
    Duplicate,
    /// A telegram for another node arrived with no hops left.
    ExpiredTtl,
}

/// Per-node routing state machine.
///
/// # Examples
///
/// A three-node line `A — B — C` where B relays for the others:
///
/// ```
/// use poholos::{Packet, RouteAction, Router, WireId};
///
/// let (a, b, c) = (WireId::new(1), WireId::new(2), WireId::new(3));
/// let mut node_a = Router::new(a);
/// let mut node_b = Router::new(b);
/// let mut node_c = Router::new(c);
///
/// // A sends a telegram to C; only B is in radio range of both.
/// let pkt = Packet::telegram(a, c, 1, b"psst")?;
/// let frame = node_a.originate(&pkt);
///
/// // B is not the destination: it forwards.
/// let RouteAction::Forward(relayed) = node_b.ingest(frame.as_bytes())? else {
///     panic!("B should forward");
/// };
///
/// // C is the destination: it delivers and does not forward.
/// let RouteAction::Deliver(received) = node_c.ingest(relayed.as_bytes())? else {
///     panic!("C should deliver");
/// };
/// assert_eq!(received.payload(), b"psst");
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Router {
    local: WireId,
    seen: SeenCache,
}

impl Router {
    /// Creates a router for the node identified by `local`.
    #[must_use]
    pub fn new(local: WireId) -> Self {
        Self {
            local,
            seen: SeenCache::new(),
        }
    }

    /// Returns this node's wire id.
    #[must_use]
    pub fn local(&self) -> WireId {
        self.local
    }

    /// Encodes a locally created packet and registers it as seen.
    ///
    /// Registering prevents the node from re-delivering its own message
    /// when the transport echoes it back. Hand the returned frame to the
    /// transport for broadcast.
    pub fn originate(&mut self, packet: &Packet) -> Frame {
        let _newly_seen = self.seen.insert(packet.dedup_key());
        wire::encode(packet)
    }

    /// Processes raw received bytes and decides what to do with them.
    ///
    /// Implements the deliver-locally-first, then-gate-forwarding rules
    /// described in the [module docs](self).
    ///
    /// # Errors
    /// Returns [`WireError`] if the bytes do not decode as a valid frame;
    /// callers in radio environments should expect and tolerate this for
    /// foreign advertisements that slip through transport filtering.
    pub fn ingest(&mut self, bytes: &[u8]) -> Result<RouteAction, WireError> {
        let mut packet = wire::decode(bytes)?;

        if packet.src() == self.local {
            return Ok(RouteAction::Ignore(IgnoreReason::Own));
        }
        if !self.seen.insert(packet.dedup_key()) {
            return Ok(RouteAction::Ignore(IgnoreReason::Duplicate));
        }

        match packet.dest() {
            // Hearsay: deliver to everyone, then forward if hops remain.
            None => {
                let deliver = packet;
                if packet.hop() {
                    Ok(RouteAction::DeliverAndForward(
                        deliver,
                        wire::encode(&packet),
                    ))
                } else {
                    Ok(RouteAction::Deliver(deliver))
                }
            }
            // Telegram for us: the destination consumes it.
            Some(dest) if dest == self.local => Ok(RouteAction::Deliver(packet)),
            // Telegram for someone else: relay if hops remain.
            Some(_) => {
                if packet.hop() {
                    Ok(RouteAction::Forward(wire::encode(&packet)))
                } else {
                    Ok(RouteAction::Ignore(IgnoreReason::ExpiredTtl))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: WireId = WireId::new(1);
    const B: WireId = WireId::new(2);
    const C: WireId = WireId::new(3);

    #[test]
    fn own_echo_is_ignored() {
        let mut router = Router::new(A);
        let pkt = Packet::hearsay(A, 1, b"hi").unwrap();
        let frame = router.originate(&pkt);
        assert_eq!(
            router.ingest(frame.as_bytes()).unwrap(),
            RouteAction::Ignore(IgnoreReason::Own)
        );
    }

    #[test]
    fn hearsay_delivers_then_forwards_with_decremented_ttl() {
        let mut router = Router::new(B);
        let pkt = Packet::hearsay_with(A, 1, b"hi", 16).unwrap();
        let frame = wire::encode(&pkt);

        let RouteAction::DeliverAndForward(delivered, relayed) =
            router.ingest(frame.as_bytes()).unwrap()
        else {
            panic!("expected DeliverAndForward");
        };
        assert_eq!(delivered.ttl(), 16, "delivered copy keeps the received TTL");
        assert_eq!(wire::decode(relayed.as_bytes()).unwrap().ttl(), 15);
    }

    #[test]
    fn hearsay_at_ttl_one_delivers_without_forwarding() {
        let mut router = Router::new(B);
        let pkt = Packet::hearsay_with(A, 1, b"hi", 1).unwrap();
        let frame = wire::encode(&pkt);
        assert!(matches!(
            router.ingest(frame.as_bytes()).unwrap(),
            RouteAction::Deliver(_)
        ));
    }

    #[test]
    fn duplicate_via_second_route_is_ignored() {
        let mut router = Router::new(B);
        // Same message, different remaining TTLs (two paths through the mesh).
        let first = wire::encode(&Packet::hearsay_with(A, 1, b"hi", 16).unwrap());
        let second = wire::encode(&Packet::hearsay_with(A, 1, b"hi", 9).unwrap());

        assert!(matches!(
            router.ingest(first.as_bytes()).unwrap(),
            RouteAction::DeliverAndForward(..)
        ));
        assert_eq!(
            router.ingest(second.as_bytes()).unwrap(),
            RouteAction::Ignore(IgnoreReason::Duplicate)
        );
    }

    #[test]
    fn telegram_for_us_is_consumed() {
        let mut router = Router::new(B);
        let frame = wire::encode(&Packet::telegram(A, B, 1, b"yo").unwrap());
        assert!(matches!(
            router.ingest(frame.as_bytes()).unwrap(),
            RouteAction::Deliver(_)
        ));
    }

    #[test]
    fn telegram_for_other_is_forwarded_or_expires() {
        let mut router = Router::new(B);

        let live = wire::encode(&Packet::telegram_with(A, C, 1, b"yo", 2).unwrap());
        assert!(matches!(
            router.ingest(live.as_bytes()).unwrap(),
            RouteAction::Forward(_)
        ));

        let dying = wire::encode(&Packet::telegram_with(A, C, 2, b"yo", 1).unwrap());
        assert_eq!(
            router.ingest(dying.as_bytes()).unwrap(),
            RouteAction::Ignore(IgnoreReason::ExpiredTtl)
        );
    }
}
