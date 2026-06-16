// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! End-to-end mesh simulation over the public API only.
#![cfg(feature = "std")] // NodeId requires `std`; the lib tests cover no_std.

use poholos::{DEFAULT_TTL, IgnoreReason, NodeId, Packet, RouteAction, Router, decode};

/// A — B — C — D line topology: hearsay floods the chain, every node
/// delivers exactly once, TTL decrements per hop, and the echo back along
/// the chain is suppressed as a duplicate.
#[test]
fn hearsay_floods_a_line_of_nodes() {
    let ids: Vec<_> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| NodeId::new(n, 0x1111).unwrap().wire_id())
        .collect();
    let mut nodes: Vec<_> = ids.iter().map(|id| Router::new(*id)).collect();

    let pkt = Packet::hearsay(ids[0], 1, b"flood").unwrap();
    let mut frame = nodes[0].originate(&pkt);

    for hop in 1..nodes.len() {
        let action = nodes[hop].ingest(frame.as_bytes()).unwrap();
        let RouteAction::DeliverAndForward(delivered, relayed) = action else {
            panic!("node {hop} should deliver and forward, got {action:?}");
        };
        assert_eq!(delivered.payload(), b"flood");
        assert_eq!(
            decode(relayed.as_bytes()).unwrap().ttl(),
            DEFAULT_TTL - u8::try_from(hop).unwrap(),
            "TTL decrements once per relay"
        );

        // The relay also reaches the node we came from. The originator
        // recognizes its own source id; everyone else dedups.
        let expected = if hop == 1 {
            IgnoreReason::Own
        } else {
            IgnoreReason::Duplicate
        };
        assert_eq!(
            nodes[hop - 1].ingest(relayed.as_bytes()).unwrap(),
            RouteAction::Ignore(expected)
        );
        frame = relayed;
    }
}

/// A telegram crosses an intermediate node that must relay without
/// delivering, and stops at its destination.
#[test]
fn telegram_relays_through_middle_node_only() {
    let alice = NodeId::new("alice", 0x3f2a).unwrap();
    let bob = NodeId::new("bob", 0x9c01).unwrap();
    let carol = NodeId::new("carol", 0x7e55).unwrap();

    let mut node_alice = Router::new(alice.wire_id());
    let mut node_bob = Router::new(bob.wire_id());
    let mut node_carol = Router::new(carol.wire_id());

    // Alice -> Carol, with Bob in the middle.
    let pkt = Packet::telegram(alice.wire_id(), carol.wire_id(), 9, b"secret").unwrap();
    let frame = node_alice.originate(&pkt);

    let RouteAction::Forward(relayed) = node_bob.ingest(frame.as_bytes()).unwrap() else {
        panic!("bob must relay without delivering");
    };

    let RouteAction::Deliver(received) = node_carol.ingest(relayed.as_bytes()).unwrap() else {
        panic!("carol must consume the telegram");
    };
    assert_eq!(received.payload(), b"secret");
    assert_eq!(received.src(), alice.wire_id());

    // Carol consumed it; nothing came back for Bob to dedup, but if the
    // relay echoes, Bob ignores his own forwarded copy as a duplicate.
    assert_eq!(
        node_bob.ingest(relayed.as_bytes()).unwrap(),
        RouteAction::Ignore(IgnoreReason::Duplicate)
    );
}

/// `@node` addressing works from the full display name alone: the sender
/// derives the destination wire id from the string the user typed.
#[test]
fn wire_ids_derive_consistently_from_display_names() {
    let bob = NodeId::new("bob", 0x9c01).unwrap();
    let typed = poholos::WireId::of_name("bob-9c01");
    assert_eq!(bob.wire_id(), typed);
}
