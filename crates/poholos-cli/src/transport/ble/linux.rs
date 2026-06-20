// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Linux BLE advertising via BlueZ (`bluer`).
//!
//! BlueZ accepts the full 22-byte frame as manufacturer data.
//! POC findings listed below:
//!
//! * `Type::Peripheral`, **not** `Type::Broadcast` - BlueZ silently drops
//!   manufacturer data from broadcast-type advertisements.
//! * Keep the advertisement minimal; extra fields can push BlueZ over the
//!   legacy 31-byte PDU and make it reject the registration.
//! * Company id `0xFFFF` is silently dropped by BlueZ, which is one of
//!   the reasons poholos uses `0xF10C`.
//!
//! Replacing the frame means dropping the previous advertisement handle
//! (which unregisters it) and registering a new one.


use std::collections::BTreeMap;

use anyhow::{Context, Result};
use bluer::adv::{Advertisement, AdvertisementHandle, Type};
use poholos::{COMPANY_ID, Frame};

/// Largest frame this platform can put on air: BlueZ manufacturer data
/// carries the full protocol frame.
pub const MAX_FRAME: usize = poholos::MAX_FRAME_LEN;

/// BlueZ-backed advertiser holding the handle of the frame on air.
#[derive(Debug)]
pub struct Advertiser {
    adapter: bluer::Adapter,
    // Dropping the handle unregisters the advertisement, so holding it is
    // what keeps the frame on air.
    current: Option<AdvertisementHandle>,
}

impl Advertiser {
    /// Connects to bluetoothd and powers the default adapter.
    ///
    /// # Errors
    /// Fails if bluetoothd is unreachable or there is no adapter.
    pub async fn new() -> Result<Self> {
        let session = bluer::Session::new()
            .await
            .context("connecting to bluetoothd (is bluez running?)")?;
        let adapter = session
            .default_adapter()
            .await
            .context("no default Bluetooth adapter")?;
        adapter
            .set_powered(true)
            .await
            .context("powering Bluetooth adapter")?;
        Ok(Self {
            adapter,
            current: None,
        })
    }

    /// Replaces the advertisement on air with `frame`.
    ///
    /// # Errors
    /// Fails if BlueZ rejects the advertisement registration.
    pub async fn set_frame(&mut self, frame: &Frame) -> Result<()> {
        // Unregister the previous advertisement first; BlueZ instances
        // are limited and re-registering the same slot is not supported.
        self.current = None;

        let mut manufacturer_data = BTreeMap::new();
        manufacturer_data.insert(COMPANY_ID, frame.as_bytes().to_vec());

        let advertisement = Advertisement {
            advertisement_type: Type::Peripheral,
            manufacturer_data,
            ..Default::default()
        };

        let handle = self
            .adapter
            .advertise(advertisement)
            .await
            .context("registering BlueZ advertisement")?;
        self.current = Some(handle);
        Ok(())
    }
}
