// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! Sans-io mesh messaging protocol over BLE advertising frames.
//!
//! Poholos (Ukrainian: *поголос* — rumour, hearsay) is a peer-to-peer mesh
//! protocol designed to ride inside Bluetooth Low Energy legacy
//! advertisements. The universal desktop baseline for those is ~31 bytes of
//! advertising data, leaving a **22-byte on-air frame** once AD structure
//! overhead is accounted for. Everything in this crate is built around that
//! budget.
//!
//! This crate is strictly **sans-io**: it never touches Bluetooth, sockets,
//! clocks, or randomness, which keeps the protocol fully unit-testable and
//! reusable from embedded (`no_std`) targets.
//!
//! # Status
//!
//! The core types are implemented: [`Packet`] and [`Payload`], the [`WireId`]
//! and [`NodeId`] identities, the on-air [`Frame`] codec ([`encode`] /
//! [`decode`]), and the [`fnv64`] hash. The routing state machine, the
//! seen-cache, and the airtime scheduler are still to come.
//!
//! # Example
//!
//! Encode a broadcast packet and decode it back:
//!
//! ```
//! use poholos::{Packet, WireId, decode, encode};
//!
//! let src = WireId::of_name("alice-3f2a");
//! let pkt = Packet::hearsay(src, 1, b"hi mesh")?;
//!
//! let frame = encode(&pkt);
//! assert_eq!(decode(frame.as_bytes())?, pkt);
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```
//!
//! # Feature flags
//!
//! * `std` *(default)* — enables [`NodeId`] and backtrace capture in errors.
//!   Without it the crate is `no_std` and allocation-free.
//! * `serde` — serde derives on the wire types.
//! * `postcard` — convenience `codec` helpers for postcard encoding.

#![cfg_attr(not(feature = "std"), no_std)]

mod error;
mod node_id;
mod packet;
mod seen;
mod wire;

// Declared as skeletons; their implementations are still to come.
mod router;
pub mod rotation;

#[cfg(feature = "postcard")]
pub mod codec;

#[doc(inline)]
pub use error::{PacketError, WireError};
#[doc(inline)]
pub use node_id::WireId;
#[cfg(feature = "std")]
#[doc(inline)]
pub use node_id::{NodeId, NodeIdError};
#[doc(inline)]
pub use packet::{
    DEFAULT_TTL, MAX_PAYLOAD_HEARSAY, MAX_PAYLOAD_TELEGRAM, MAX_TTL, Packet, Payload,
};
#[doc(inline)]
pub use seen::fnv64;
#[doc(inline)]
pub use wire::{
    COMPANY_ID, Frame, MAX_FRAME_LEN, WIRE_VERSION, decode, encode, manufacturer_frame,
};
