// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Ivan Petrouchtchak

//! poholos — a peer-to-peer Bluetooth mesh messaging protocol.
//!
//! This crate is currently a skeleton: the modules below lay out the intended
//! structure, with implementations to be filled in.

mod error;
mod node_id;
mod packet;
mod router;
mod seen;
mod wire;

pub mod codec;
pub mod rotation;
