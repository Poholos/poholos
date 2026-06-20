// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Cross-platform BLE scanning via `btleplug` (central role).
//!
//! Poholos frames arrive in two shapes, and the scanner accepts both:
//!
//! 1. **Manufacturer data** keyed by [`COMPANY_ID`] - what Linux and
//!    Windows nodes put on air. Arrives inline in the scan event.
//! 2. **Service UUID** `0xF_`-tagged - what macOS nodes use, since
//!    CoreBluetooth peripherals cannot advertise foreign manufacturer
//!    data but *can* advertise arbitrary 128-bit service UUIDs. Also
//!    arrives inline in the event.
//!
//! How those shapes reach the scanner depends on the platform backend —
//! some deliver the data inline in the event, Windows only through the
//! peripheral properties; [`scan_loop`] handles both.
//!
//! The scanner performs no deduplication; the same frame will be reported
//! every time the radio hears it, and the router's seen-cache absorbs it.


use anyhow::{Context, Result};
use btleplug::api::{Central as _, CentralEvent, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager};
use futures::StreamExt as _;
use poholos::{COMPANY_ID, Frame};
use tokio::sync::mpsc;
use uuid::Uuid;

/// High nibble marking a poholos frame packed into a 128-bit service
/// UUID. The low nibble carries the frame length, so this one byte is
/// both the type tag and the length — costing the 16-byte UUID only a
/// single byte of frame budget. `0xF` echoes the manufacturer id `0xF10C`.
const UUID_TAG: u8 = 0xF0;

/// Largest frame the macOS service-UUID channel can carry: a 128-bit
/// UUID is 16 bytes, of which byte 0 is the tag+length, leaving 15.
pub(crate) const MAX_UUID_FRAME: usize = 15;

/// Smallest valid on-wire frame: 1 flag byte + 2 seq + 4 src (a hearsay
/// with an empty payload); telegrams are longer. [`Frame::copy_from`] is
/// only a length-bounded container and does not enforce this, so the
/// UUID decoder must, otherwise a foreign UUID whose tag nibble happens
/// to be `0xF` would slip a 1–6 byte "frame" through.
const MIN_FRAME: usize = 7;

/// Starts scanning on the first Bluetooth adapter, feeding `tx`.
///
/// # Errors
/// Fails if no adapter is found or the scan cannot be started.
pub async fn spawn(tx: mpsc::Sender<Frame>) -> Result<()> {
    let manager = Manager::new().await.context("creating BLE manager")?;
    let adapter = manager
        .adapters()
        .await
        .context("listing Bluetooth adapters")?
        .into_iter()
        .next()
        .context("no Bluetooth adapter found")?;

    let events = adapter.events().await.context("opening BLE event stream")?;
    adapter
        .start_scan(ScanFilter::default())
        .await
        .context("starting BLE scan (is the adapter powered on?)")?;

    tokio::spawn(scan_loop(adapter, events, tx));
    Ok(())
}

/// Forwards every decodable poholos frame from scan events into `tx`.
///
/// Two delivery paths, because btleplug backends differ:
///
/// * Linux (`bluez`) and macOS (CoreBluetooth) emit the frame data
///   *inline* in `ManufacturerDataAdvertisement` / `ServicesAdvertisement`
///   events.
/// * Windows (WinRT) emits only `DeviceDiscovered` / `DeviceUpdated`, and
///   stashes the advertisement data in the peripheral's *properties*. So
///   we also read `manufacturer_data` and `services` back from there -
///   both are refreshed on every advertisement.
///
/// The two paths overlap on some backends; the router's seen-cache makes
/// the duplicates harmless.
async fn scan_loop(
    adapter: Adapter,
    mut events: std::pin::Pin<Box<dyn futures::Stream<Item = CentralEvent> + Send>>,
    tx: mpsc::Sender<Frame>,
) {
    while let Some(event) = events.next().await {
        match event {
            // Inline manufacturer data (Linux/macOS backends).
            CentralEvent::ManufacturerDataAdvertisement {
                manufacturer_data, ..
            } => {
                if let Some(frame) = frame_from_manufacturer(&manufacturer_data)
                    && tx.send(frame).await.is_err()
                {
                    return; // transport dropped
                }
            }
            // Inline service UUIDs (Linux/macOS backends).
            CentralEvent::ServicesAdvertisement { services, .. } => {
                for uuid in &services {
                    if let Some(frame) = frame_from_service_uuid(uuid)
                        && tx.send(frame).await.is_err()
                    {
                        return; // transport dropped
                    }
                }
            }
            // WinRT's only advertisement events: pull the same data out of
            // the peripheral properties, where it is kept fresh.
            CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) => {
                let Ok(p) = adapter.peripheral(&id).await else {
                    continue; // peripheral vanished between events
                };
                let Some(props) = p.properties().await.ok().flatten() else {
                    continue;
                };
                if let Some(frame) = frame_from_manufacturer(&props.manufacturer_data)
                    && tx.send(frame).await.is_err()
                {
                    return; // transport dropped
                }
                for uuid in &props.services {
                    if let Some(frame) = frame_from_service_uuid(uuid)
                        && tx.send(frame).await.is_err()
                    {
                        return; // transport dropped
                    }
                }
            }
            _ => {}
        }
    }
}

/// Pulls a poholos frame out of a manufacturer-data map keyed by company id.
fn frame_from_manufacturer(data: &std::collections::HashMap<u16, Vec<u8>>) -> Option<Frame> {
    Frame::copy_from(data.get(&COMPANY_ID)?).ok()
}

/// Packs a frame into a 128-bit service UUID: byte 0 = `0xF0 | len`,
/// bytes `1..=len` = the frame, the rest zero padding.
///
/// The caller guarantees `frame.len() <= MAX_UUID_FRAME` (the macOS TX
/// budget, enforced at the prompt); the assert catches a logic error.
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only macOS transmits via the service-UUID channel"
    )
)]
#[allow(
    clippy::cast_possible_truncation,
    reason = "len <= MAX_UUID_FRAME (15) fits a nibble, by the budget check + assert"
)]
pub(crate) fn frame_to_service_uuid(frame: &Frame) -> Uuid {
    let bytes = frame.as_bytes();
    let len = bytes.len();
    debug_assert!(
        len <= MAX_UUID_FRAME,
        "frame exceeds the service-UUID budget"
    );
    let mut raw = [0u8; 16];
    raw[0] = UUID_TAG | len as u8;
    raw[1..=len].copy_from_slice(bytes);
    Uuid::from_bytes(raw)
}

/// Decodes a poholos-tagged service UUID into a frame, if it is one.
///
/// Foreign UUIDs (the Bluetooth base pattern, other apps' services) fail
/// the high-nibble tag check or the frame structural check and return
/// `None`.
fn frame_from_service_uuid(uuid: &Uuid) -> Option<Frame> {
    let raw = uuid.as_bytes();
    if raw[0] & 0xF0 != UUID_TAG {
        return None;
    }
    let len = usize::from(raw[0] & 0x0F);
    if !(MIN_FRAME..=MAX_UUID_FRAME).contains(&len) {
        return None;
    }
    Frame::copy_from(&raw[1..=len]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_uuid_roundtrip_decodes_to_original_frame() {
        let raw = [0x10, 0x00, 0x2A, 0xDE, 0xAD, 0xBE, 0xEF, b'h', b'i'];
        let frame = Frame::copy_from(&raw).expect("valid frame");
        let uuid = frame_to_service_uuid(&frame);
        // Tag byte = 0xF0 | length (9 here).
        assert_eq!(uuid.as_bytes()[0], 0xF0 | 9);
        let back = frame_from_service_uuid(&uuid).expect("decodes");
        assert_eq!(back.as_bytes(), raw);
    }

    #[test]
    fn foreign_service_uuids_are_ignored() {
        // Bluetooth base UUID for a 16-bit service (battery, 0x180F):
        // byte 0 is 0x00, so the tag check rejects it.
        let battery = Uuid::from_u128(0x0000180f_0000_1000_8000_00805f9b34fb);
        assert!(frame_from_service_uuid(&battery).is_none());
        // Tagged byte but structurally invalid (length 1 is too short).
        let bogus = Uuid::from_bytes([0xF1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(frame_from_service_uuid(&bogus).is_none());
    }
}
