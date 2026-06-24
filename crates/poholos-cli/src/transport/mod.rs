// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Frame transports: how encoded packets physically reach other nodes.
//!
//! The core `poholos` library is sans-io; everything that touches a radio
//! or a socket lives here. Per M-DI-HIERARCHY we dispatch over a concrete
//! enum rather than `dyn Trait`: there are exactly two transports, both
//! known at compile time.
//!
//! Semantics shared by all transports:
//!
//! * `send_own` / `send_relay` make a frame *available* to nearby nodes.
//!   For UDP both are a single broadcast datagram. BLE has one
//!   advertising slot that the radio repeats continuously, so frames are
//!   *rotated* through it - own messages and queued relays take turns
//!   holding the slot long enough to be heard (see `poholos::rotation`),
//!   rather than each send evicting the last. The split exists because
//!   the rotation prioritizes the local user's message over relay
//!   traffic, and skips relays the platform cannot carry.
//! * `recv` yields the next frame heard from *any* node, including echoes
//!   of our own sends (UDP loopback, or our advert scanned by ourselves).
//!   Deduplication and own-echo suppression are the router's job, not the
//!   transport's.

use anyhow::Result;
use poholos::Frame;

mod ble;
mod udp;

pub use ble::BleTransport;
pub use udp::UdpTransport;

/// A running frame transport (UDP broadcast or BLE advertising).
#[derive(Debug)]
pub enum Transport {
    /// UDP broadcast on the local network, for protocol testing.
    Udp(UdpTransport),
    /// Bluetooth Low Energy advertising, the real thing.
    Ble(BleTransport),
}

impl Transport {
    /// Starts the UDP broadcast transport on `port`.
    ///
    /// # Errors
    /// Fails if the socket cannot be bound or broadcast cannot be enabled.
    pub async fn udp(port: u16) -> Result<Self> {
        Ok(Self::Udp(UdpTransport::new(port).await?))
    }

    /// Starts the BLE transport (scanner plus platform advertiser).
    ///
    /// # Errors
    /// Fails if no Bluetooth adapter is available, or the platform cannot
    /// advertise (see `transport/ble` for per-OS constraints).
    pub async fn ble() -> Result<Self> {
        Ok(Self::Ble(BleTransport::new().await?))
    }

    /// Makes the local user's message available to nearby nodes.
    ///
    /// On BLE the message is guaranteed a recurring share of the
    /// advertising slot, no matter how much relay traffic competes.
    ///
    /// # Errors
    /// Fails on socket errors (UDP) or when the frame exceeds the
    /// platform TX budget (BLE on macOS: 16 bytes).
    pub async fn send_own(&mut self, frame: &Frame) -> Result<()> {
        match self {
            Self::Udp(t) => t.send(frame).await,
            Self::Ble(t) => t.send_own(frame).await,
        }
    }

    /// Makes a frame the router asked us to forward available to nearby
    /// nodes.
    ///
    /// On BLE relays queue for one rotation turn each; frames above the
    /// platform TX budget are skipped silently (routine capability
    /// mismatch, e.g. a Mac hearing a full-size frame).
    ///
    /// # Errors
    /// Fails on socket errors (UDP) or if the advertiser task died (BLE).
    pub async fn send_relay(&mut self, frame: &Frame) -> Result<()> {
        match self {
            Self::Udp(t) => t.send(frame).await,
            Self::Ble(t) => t.send_relay(frame).await,
        }
    }

    /// Waits for the next received frame; `None` means the transport died.
    ///
    /// Cancel-safe: backed by an `mpsc` receiver in both transports, so it
    /// can be used directly inside `tokio::select!`.
    pub async fn recv(&mut self) -> Option<Frame> {
        match self {
            Self::Udp(t) => t.recv().await,
            Self::Ble(t) => t.recv().await,
        }
    }
}
