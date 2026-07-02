// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Windows BLE advertising via `BluetoothLEAdvertisementPublisher`.
//!
//! Windows 11 cannot act as a GATT peripheral from desktop apps (this
//! consistently hits an HRESULT failure), but the advertising
//! publisher with manufacturer data works fine. Windows is therefore
//! central + broadcaster only, which is all poholos needs.
//!
//! The publisher carries both wire versions. A frame within the legacy AD
//! budget ([`MAX_FRAME_LEN`](poholos::MAX_FRAME_LEN)) is published as a
//! legacy advertisement so every node — including legacy-only BLE 4.x
//! scanners — can hear it; a larger (wire version 1) frame enables
//! extended advertising (`SetUseExtendedAdvertisement`), which only
//! extended-scan-capable nodes receive. The per-frame choice is what keeps
//! the node dual-stack on air. Where the adapter and OS support it, wire
//! version 1 additionally rides the **Coded (long-range) PHY** on both
//! hops, matching the micro:bit firmware; otherwise it falls back to plain
//! extended advertising.
//!
//! Replacing the frame means retiring the previous publisher and starting
//! a fresh one carrying the new manufacturer-data section. `Stop()` is
//! asynchronous in WinRT (the status passes through `Stopping` before
//! `Stopped`), so restarting the *same* publisher races that transition;
//! a new publisher per frame sidesteps it, and the brief overlap of old
//! and new advertisements is absorbed by every receiver's seen-cache.

use anyhow::{Context, Result};
use poholos::{COMPANY_ID, ExtFrame};
use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementPhyType, BluetoothLEAdvertisementPublisher, BluetoothLEManufacturerData,
};
use windows::Devices::Bluetooth::BluetoothAdapter;
use windows::Storage::Streams::DataWriter;

/// Largest frame this adapter can put on air.
///
/// Legacy frames (up to [`MAX_FRAME_LEN`](poholos::MAX_FRAME_LEN), 22) ride
/// a legacy advertisement; extended advertising carries the rest. Extended
/// TX is capped per adapter/driver, and above the cap WinRT aborts the
/// advertisement *silently* (status `Aborted`, `error=0`) — so this budget
/// is set to the largest payload the test adapter actually transmitted
/// (156 bytes; 157 aborted), measured identically on the 2M and Coded PHYs,
/// so one budget covers both. Frames above it fail the send with a clear
/// message instead of vanishing on air. Adapters vary; raising this to the
/// protocol ceiling awaits runtime cap detection.
pub const MAX_FRAME: usize = 156;

/// WinRT-backed advertiser holding the publisher currently on air.
#[derive(Debug)]
pub struct Advertiser {
    current: Option<BluetoothLEAdvertisementPublisher>,
    /// Wire-version-1 frames ride the Coded (long-range) PHY when the
    /// adapter and OS both support it; detected once at startup.
    coded: bool,
}

impl Advertiser {
    /// Probes that WinRT advertising is available (idle until the first
    /// frame) and whether the radio can transmit on the Coded PHY.
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
        let probe = BluetoothLEAdvertisementPublisher::new()
            .context("creating BluetoothLEAdvertisementPublisher")?;

        // Coded TX needs both the adapter capability and the PHY-selection
        // API (newer Windows 11 builds) — probe both; the property set on
        // the never-started probe publisher is side-effect free. Any
        // failure means v1 frames fall back to plain extended advertising.
        let coded = BluetoothAdapter::GetDefaultAsync()
            .and_then(|op| op.join())
            .and_then(|adapter| adapter.IsLowEnergyCodedPhySupported())
            .unwrap_or(false)
            && probe.SetUseExtendedAdvertisement(true).is_ok()
            && probe
                .SetPrimaryPhy(BluetoothLEAdvertisementPhyType::CodedPhy)
                .is_ok();

        Ok(Self {
            current: None,
            coded,
        })
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
    pub async fn set_frame(&mut self, frame: &ExtFrame) -> Result<()> {
        // Retire the previous publisher; Stop completes asynchronously
        // while the replacement already advertises.
        if let Some(old) = self.current.take() {
            old.Stop().context("stopping previous publisher")?;
        }

        let publisher = BluetoothLEAdvertisementPublisher::new()
            .context("creating BluetoothLEAdvertisementPublisher")?;

        // Legacy frames stay on legacy advertisements (universal reach);
        // only oversized wire-version-1 frames switch to extended
        // advertising, which legacy-only scanners do not receive.
        let extended = frame.len() > poholos::MAX_FRAME_LEN;
        publisher
            .SetUseExtendedAdvertisement(extended)
            .context("selecting extended advertising")?;
        if extended && self.coded {
            // Long-range: both hops on the Coded PHY (the coding rate is
            // the controller's choice — WinRT has no S=2/S=8 knob). The
            // adapter's TX size cap is PHY-independent (see [`MAX_FRAME`]).
            publisher
                .SetPrimaryPhy(BluetoothLEAdvertisementPhyType::CodedPhy)
                .context("selecting coded primary PHY")?;
            publisher
                .SetSecondaryPhy(BluetoothLEAdvertisementPhyType::CodedPhy)
                .context("selecting coded secondary PHY")?;
        }

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
