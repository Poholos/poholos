// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! macOS BLE advertising via a CoreBluetooth service UUID (`ble-peripheral-rust`).
//!
//! CoreBluetooth peripherals cannot advertise foreign manufacturer data,
//! but they *can* advertise arbitrary 128-bit service UUIDs - and unlike
//! the local name, an advertised UUID reaches scanners **inline in the
//! scan event**, not through a cached property. That matters: the name
//! channel (the previous approach) was frozen at first sight by WinRT and
//! BlueZ, so only a Mac's first message per session ever arrived. The
//! UUID channel changes every message and is re-reported each time.
//!
//! A 128-bit UUID is 16 bytes; one byte tags+lengths the frame (see
//! [`frame_to_service_uuid`](super::scan)), leaving [`MAX_FRAME`] = 15
//! raw bytes. A Mac can *hear* full 22-byte frames perfectly well; it
//! just cannot *send* more than 15, so oversized sends fail with an
//! explanation instead of truncating. In practice that limits hearsay
//! typed on a Mac to 8 payload bytes and telegrams to 4.


use std::time::Duration;

use anyhow::{Context, Result, bail};
use ble_peripheral_rust::{Peripheral, PeripheralImpl as _};
use poholos::Frame;

use super::scan::{MAX_UUID_FRAME, frame_to_service_uuid};

/// Largest frame this platform can put on air (the service-UUID budget),
/// under the `MAX_FRAME` alias every platform advertiser exports.
pub const MAX_FRAME: usize = MAX_UUID_FRAME;

/// Local name advertised alongside the frame UUID. Kept short so the
/// 128-bit UUID stays in the primary advertisement rather than being
/// pushed into Apple's iOS-only overflow area (invisible to other OSes).
const ADVERTISED_NAME: &str = "PHO";

/// How long to wait for the Bluetooth radio to power up before giving up.
/// CoreBluetooth reports powered-off briefly right after process start.
const POWER_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll interval while waiting for the radio to report powered-on.
const POWER_POLL: Duration = Duration::from_millis(100);

/// CoreBluetooth-backed advertiser encoding frames into a service UUID.
pub struct Advertiser {
    peripheral: Peripheral,
    advertising: bool,
}

impl std::fmt::Debug for Advertiser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Advertiser")
            .field("advertising", &self.advertising)
            .finish_non_exhaustive()
    }
}

impl Advertiser {
    /// Creates the peripheral and waits for the radio to power on.
    ///
    /// # Errors
    /// Fails if CoreBluetooth is unavailable or the radio stays off.
    pub async fn new() -> Result<Self> {
        // The event channel reports subscriber/read events we do not use;
        // poholos is connectionless. A small buffer absorbs the noise.
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let mut peripheral = Peripheral::new(tx)
            .await
            .context("creating CoreBluetooth peripheral")?;

        let deadline = tokio::time::Instant::now() + POWER_TIMEOUT;
        while !peripheral.is_powered().await.unwrap_or(false) {
            if tokio::time::Instant::now() >= deadline {
                bail!("Bluetooth radio did not power on within {POWER_TIMEOUT:?}");
            }
            tokio::time::sleep(POWER_POLL).await;
        }

        Ok(Self {
            peripheral,
            advertising: false,
        })
    }

    /// Replaces the advertisement on air with `frame`, UUID-encoded.
    ///
    /// # Errors
    /// Fails if `frame` exceeds [`MAX_FRAME`] bytes or CoreBluetooth
    /// rejects the advertisement.
    pub async fn set_frame(&mut self, frame: &Frame) -> Result<()> {
        if frame.len() > MAX_FRAME {
            bail!(
                "frame is {} bytes but macOS can only advertise {MAX_FRAME} \
                 (packed into a 128-bit service UUID) — keep hearsay under 9 \
                 payload bytes and telegrams under 5 when sending from a Mac",
                frame.len()
            );
        }

        let uuid = frame_to_service_uuid(frame);

        if self.advertising {
            self.peripheral
                .stop_advertising()
                .await
                .context("stopping previous advertisement")?;
            self.advertising = false;
        }
        self.peripheral
            .start_advertising(ADVERTISED_NAME, &[uuid])
            .await
            .context("starting CoreBluetooth advertisement")?;
        self.advertising = true;
        Ok(())
    }
}
