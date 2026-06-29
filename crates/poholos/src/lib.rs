// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Poholos mesh chat protocol over BLE advertising frames.
//!
//! Poholos (Ukrainian: *поголос* — rumor, hearsay) is a peer-to-peer mesh
//! chat protocol designed to ride inside Bluetooth Low Energy legacy
//! advertisements. The universal desktop baseline for those is ~31 bytes of
//! advertising data (AD), leaving a **22-byte on-air frame** once AD structure
//! overhead is accounted for. Everything in this crate is built around that
//! budget.
//!
//! # Design
//!
//! This crate is strictly **sans-io**: it never touches Bluetooth, sockets,
//! clocks, or randomness. You feed received bytes into a [`Router`] and it
//! tells you what to do — deliver a message locally, re-broadcast a frame,
//! or ignore it. This keeps the protocol engine fully unit-testable and
//! reusable from embedded (`no_std`) targets. All I/O lives in the
//! application layer (see the `poholos-cli` crate).
//!
//! The core types are:
//!
//! * [`Packet`] — a parsed protocol message, constructed via
//!   [`Packet::hearsay`] (broadcast) or [`Packet::telegram`] (unicast).
//! * [`Frame`] — an encoded on-air representation, at most
//!   [`MAX_FRAME_LEN`] (22) bytes.
//! * [`WireId`] — the compact 32-bit node identity used on the wire.
//! * [`NodeId`] *(requires the `std` feature)* — the human-friendly node
//!   name, e.g. `alice-3f2a`.
//! * [`Router`] — the pure routing state machine with built-in duplicate
//!   suppression via [`SeenCache`].
//! * [`rotation::Rotation`] — the airtime scheduler for transports with a
//!   single repeating broadcast slot (BLE advertising), shared by the
//!   desktop CLI and embedded targets.
//!
//! # Examples
//!
//! Two nodes exchanging a broadcast over a simulated link:
//!
//! ```
//! use poholos::{Packet, RouteAction, Router, WireId};
//!
//! let alice = WireId::of_name("alice-3f2a");
//! let bob = WireId::of_name("bob-9c01");
//!
//! let mut a = Router::new(alice);
//! let mut b = Router::new(bob);
//!
//! // Alice broadcasts. `originate` registers the packet as seen and
//! // returns the encoded frame to hand to a transport.
//! let pkt = Packet::hearsay(alice, 1, b"hi mesh")?;
//! let frame = a.originate(&pkt);
//!
//! // Bob receives the raw bytes from his transport.
//! match b.ingest(frame.as_bytes())? {
//!     RouteAction::DeliverAndForward(p, _relay) => {
//!         assert_eq!(p.payload(), b"hi mesh");
//!     }
//!     other => panic!("unexpected action: {other:?}"),
//! }
//!
//! // The same frame again is suppressed as a duplicate.
//! assert!(matches!(
//!     b.ingest(frame.as_bytes())?,
//!     RouteAction::Ignore(poholos::IgnoreReason::Duplicate)
//! ));
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```
//!
//! # Feature flags
//!
//! * `std` *(default)* — enables [`NodeId`], a `LinkedHashSet`-backed
//!   [`SeenCache`], and backtrace capture in errors. Without it the crate
//!   is `no_std` and allocation-free.
//! * `serde` — serde derives on the wire types.
//! * `postcard` — convenience codec in [`codec`].

#![cfg_attr(not(feature = "std"), no_std)]

mod error;
mod node_id;
mod packet;
mod router;
mod seen;
mod wire;

#[cfg(feature = "postcard")]
pub mod codec;
pub mod rotation;

#[doc(inline)]
pub use error::{PacketError, WireError};
#[doc(inline)]
pub use node_id::WireId;
#[cfg(feature = "std")]
#[doc(inline)]
pub use node_id::{NodeId, NodeIdError};
#[doc(inline)]
pub use packet::{
    DEFAULT_TTL, MAX_PAYLOAD_HEARSAY, MAX_PAYLOAD_TELEGRAM, MAX_TTL, Packet, PacketN, Payload,
    PayloadN,
};
#[doc(inline)]
pub use router::{IgnoreReason, RouteAction, RouteActionN, Router};
#[doc(inline)]
pub use seen::{SEEN_CAPACITY, SeenCache, fnv64};
#[doc(inline)]
pub use wire::{
    COMPANY_ID, Frame, FrameN, MAX_FRAME_LEN, WIRE_VERSION, decode, encode, manufacturer_frame,
};
