// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Windows BLE advertising via `BluetoothLEAdvertisementPublisher`.
//!
//! Windows 11 cannot act as a GATT peripheral from desktop apps (the
//! spike consistently hit an HRESULT failure), but the legacy advertising
//! publisher with manufacturer data works fine and carries the full
//! 22-byte frame. Windows is therefore central + broadcaster only, which
//! is all poholos needs.
//!
//! Replacing the frame means retiring the previous publisher and starting
//! a fresh one carrying the new manufacturer-data section. `Stop()` is
//! asynchronous in WinRT (the status passes through `Stopping` before
//! `Stopped`), so restarting the *same* publisher races that transition;
//! a new publisher per frame sidesteps it, and the brief overlap of old
//! and new advertisements is absorbed by every receiver's seen-cache.

use anyhow::{Context, Result};
use poholos::{COMPANY_ID, Frame};
use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementPublisher, BluetoothLEManufacturerData,
};
use windows::Storage::Streams::DataWriter;

/// Largest frame this platform can put on air: WinRT manufacturer data
/// carries the full protocol frame.
pub const MAX_FRAME: usize = poholos::MAX_FRAME_LEN;

/// WinRT-backed advertiser holding the publisher currently on air.
#[derive(Debug)]
pub struct Advertiser {
    current: Option<BluetoothLEAdvertisementPublisher>,
}

impl Advertiser {
    /// Probes that WinRT advertising is available (idle until the first frame).
    ///
    /// # Errors
    /// Fails if WinRT cannot construct a publisher, e.g. no radio.
    #[expect(
        clippy::unused_async,
        reason = "platform HAL signature; other OSes await here"
    )]
    pub async fn new() -> Result<Self> {
        // Fail at startup rather than on the first send if advertising is
        // unavailable; the probe publisher is never started.
        let _probe = BluetoothLEAdvertisementPublisher::new()
            .context("creating BluetoothLEAdvertisementPublisher")?;
        Ok(Self { current: None })
    }

    /// Replaces the advertisement on air with `frame`.
    ///
    /// # Errors
    /// Fails if the radio rejects stopping the old publisher or starting
    /// the new one.
    #[expect(
        clippy::unused_async,
        reason = "platform HAL signature; other OSes await here"
    )]
    pub async fn set_frame(&mut self, frame: &Frame) -> Result<()> {
        // Retire the previous publisher; Stop completes asynchronously
        // while the replacement already advertises.
        if let Some(old) = self.current.take() {
            old.Stop().context("stopping previous publisher")?;
        }

        let publisher = BluetoothLEAdvertisementPublisher::new()
            .context("creating BluetoothLEAdvertisementPublisher")?;
        let sections = publisher
            .Advertisement()
            .context("getting advertisement")?
            .ManufacturerData()
            .context("getting manufacturer data list")?;

        let writer = DataWriter::new().context("creating DataWriter")?;
        writer
            .WriteBytes(frame.as_bytes())
            .context("writing frame bytes")?;
        let buffer = writer.DetachBuffer().context("detaching buffer")?;

        let section = BluetoothLEManufacturerData::Create(COMPANY_ID, &buffer)
            .context("creating manufacturer data section")?;
        sections.Append(&section).context("appending section")?;

        publisher.Start().context("starting publisher")?;
        self.current = Some(publisher);
        Ok(())
    }
}
