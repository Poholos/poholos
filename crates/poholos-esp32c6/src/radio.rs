// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! BLE radio bring-up: esp-radio controller + trouble host.
//!
//! The architectural twin of `poholos-microbit`'s `radio.rs`, on Espressif
//! silicon: the ESP32-C6's BLE controller is driven over HCI through
//! `esp-radio`'s [`BleConnector`], wrapped in bt-hci's
//! [`ExternalController`], so everything above the controller is the same
//! `trouble-host` API the micro:bit uses. A node is an observer and a
//! broadcaster at once, and it is **dual-stack** across both wire versions:
//!
//! * extended scanning on *both* primary channel sets (1M and Coded), so
//!   the node hears legacy, plain extended, and long-range coded
//!   advertisements — wire version 0 and 1 alike;
//! * extended advertising for *all* outgoing frames: version 0 as
//!   legacy-PDU extended adverts (still heard by legacy-only scanners),
//!   version 1 as extended-PDU on the **Coded (long-range, S=8) PHY**.
//!   Legacy advertising commands are never issued — mixing legacy and
//!   extended HCI command sets alongside extended scanning is forbidden
//!   by the spec and controllers reject it with Command Disallowed.
//!
//! [`run`] drives three concerns forever:
//!
//! * the trouble host runner, whose scan-report handler decodes poholos
//!   frames out of manufacturer data and feeds `RX_FRAMES`;
//! * a continuous passive extended-scan session;
//! * the advertiser, which time-shares the single advertising slot via
//!   the shared [`poholos::rotation`] policy, fed from `OUTGOING`. It
//!   picks legacy-PDU or coded extended-PDU per frame by size, exactly
//!   as the micro:bit firmware and the desktop Windows transport do.
//!
//! Whether this controller accepts the extended/coded HCI parameters is
//! pending hardware validation; failures surface in the log rather than
//! panicking the node.

use bt_hci::param::{LeExtAdvReportsIter, PhyKind};
use embassy_futures::select::{Either, select, select3};
use embassy_time::{Duration, Instant, Timer};
use esp_radio::ble::controller::BleConnector;
use poholos::rotation::ExtRotation;
use poholos::{COMPANY_ID, ExtFrame, MAX_FRAME_LEN};
use trouble_host::advertise::{
    AdStructure, Advertisement, AdvertisementParameters, AdvertisementSet,
};
use trouble_host::connection::{PhySet, ScanConfig};
use trouble_host::prelude::{DefaultPacketPool, ExternalController};
use trouble_host::scan::Scanner;
use trouble_host::{Address, Host, HostResources};

use crate::{OUTGOING, Outgoing, RX_FRAMES};

/// HCI command/event slots for the external controller transport.
const CONTROLLER_SLOTS: usize = 20;

/// The controller type the trouble host runs on.
pub type Controller = ExternalController<BleConnector<'static>, CONTROLLER_SLOTS>;

/// Buffer for an encoded extended advertisement: the largest frame plus
/// the manufacturer-data AD overhead (length + type + 2-byte company id).
const EXT_ADV_DATA_LEN: usize = poholos::MAX_EXT_FRAME_LEN + 8;

/// One rotation dwell, converted to the embassy clock.
const DWELL: Duration = Duration::from_millis(poholos::rotation::DWELL.as_millis() as u64);

/// Wraps the C6's BLE peripheral in the HCI controller trouble expects.
pub fn controller(bt: esp_hal::peripherals::BT<'static>) -> Controller {
    let connector = BleConnector::new(bt, Default::default())
        .expect("BLE connector init (is esp-rtos started?)");
    ExternalController::new(connector)
}

/// Decodes poholos frames out of scan reports and feeds the router.
///
/// Called from the host runner's event context, so it must not block:
/// on overflow the oldest pending frame is shed — the same overload
/// policy as everywhere else in the stack, and duplicates are routine
/// on radio anyway.
struct ScanHandler;

impl trouble_host::prelude::EventHandler for ScanHandler {
    // Extended scanning reports legacy *and* extended advertisements
    // through this one callback, so both wire versions arrive here.
    fn on_ext_adv_reports(&self, mut reports: LeExtAdvReportsIter<'_>) {
        while let Some(Ok(report)) = reports.next() {
            let Some(bytes) = poholos::manufacturer_frame(report.data) else {
                continue;
            };
            let Ok(frame) = ExtFrame::copy_from(bytes) else {
                continue;
            };
            if let Err(err) = RX_FRAMES.try_send(frame) {
                let embassy_sync::channel::TrySendError::Full(frame) = err;
                let _ = RX_FRAMES.try_receive();
                let _ = RX_FRAMES.try_send(frame);
            }
        }
    }
}

/// Runs the BLE host forever: scanning continuously and rotating the
/// advertising slot through outgoing own/relay frames.
pub async fn run(controller: Controller, address: Address) -> ! {
    let mut resources: HostResources<DefaultPacketPool, 1, 1> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);
    let Host {
        mut peripheral,
        central,
        mut runner,
        ..
    } = stack.build();

    let handler = ScanHandler;
    let host = async {
        let result = runner.run_with_handler(&handler).await;
        log::error!("BLE host runner stopped: {result:?}");
    };

    let scan = async {
        let mut scanner = Scanner::new(central);
        // Passive scan (we never send scan requests); interval and window
        // are left at trouble-host's defaults. Scanning covers both primary
        // channel sets — 1M (legacy + plain extended announcements) and
        // Coded (long-range wire-version-1) — time-shared by the controller.
        let config = ScanConfig {
            active: false,
            phys: PhySet::M1Coded,
            ..Default::default()
        };
        match scanner.scan_ext(&config).await {
            Ok(_session) => {
                log::info!("ext-scanning (1M + coded primaries) for poholos frames");
                core::future::pending::<()>().await
            }
            Err(e) => {
                log::error!("ext scan start failed: {e:?}");
                core::future::pending::<()>().await
            }
        }
    };

    let advertise = async {
        let mut rotation = ExtRotation::new();
        // The frame currently on air and its advertiser handle, held
        // only for RAII (dropping it stops the broadcast — hence the
        // underscore: it is written, never read). Consecutive turns
        // often serve the same frame; re-advertising it would be
        // pointless churn.
        let mut on_air: Option<ExtFrame> = None;
        let mut _handle = None;

        loop {
            let Some(frame) = rotation.next_frame() else {
                // Nothing waiting: leave the current advertisement on
                // air and sleep until new work arrives.
                enqueue(&mut rotation, OUTGOING.receive().await);
                continue;
            };

            if on_air != Some(frame) {
                // Stop the previous advertisement before starting the
                // replacement on the single slot.
                _handle = None;
                // Frames within the legacy budget go out as legacy
                // advertisements so every node hears them; only larger
                // (wire version 1) frames use extended advertising.
                let result = if frame.len() <= MAX_FRAME_LEN {
                    // Wire version 0: a legacy-PDU advertisement, sent via the
                    // extended command set. Legacy PDUs are heard by every
                    // scanner, legacy-only nodes included.
                    let mut adv_data = [0u8; 31];
                    let len = AdStructure::encode_slice(
                        &[AdStructure::ManufacturerSpecificData {
                            company_identifier: COMPANY_ID,
                            payload: frame.as_bytes(),
                        }],
                        &mut adv_data,
                    )
                    .expect("legacy frame + AD overhead always fits 31 bytes");
                    let sets = [AdvertisementSet {
                        params: AdvertisementParameters::default(),
                        data: Advertisement::NonconnectableNonscannableUndirected {
                            adv_data: &adv_data[..len],
                        },
                    }];
                    let mut handles = AdvertisementSet::handles(&sets);
                    peripheral.advertise_ext(&sets, &mut handles).await
                } else {
                    let mut adv_data = [0u8; EXT_ADV_DATA_LEN];
                    let len = AdStructure::encode_slice(
                        &[AdStructure::ManufacturerSpecificData {
                            company_identifier: COMPANY_ID,
                            payload: frame.as_bytes(),
                        }],
                        &mut adv_data,
                    )
                    .expect("ext frame + AD overhead fits the extended buffer");
                    let sets = [AdvertisementSet {
                        // Wire version 1 rides the Coded (long-range) PHY on
                        // both hops (S=8 — the coding a controller defaults
                        // to absent an explicit selection). Only
                        // coded-capable scanners receive these — v0 frames
                        // (above) keep universal reach.
                        params: AdvertisementParameters {
                            primary_phy: PhyKind::LeCoded,
                            secondary_phy: PhyKind::LeCoded,
                            ..Default::default()
                        },
                        data: Advertisement::ExtNonconnectableNonscannableUndirected {
                            adv_data: &adv_data[..len],
                            anonymous: false,
                        },
                    }];
                    let mut handles = AdvertisementSet::handles(&sets);
                    peripheral.advertise_ext(&sets, &mut handles).await
                };
                match result {
                    Ok(adv) => {
                        _handle = Some(adv);
                        on_air = Some(frame);
                    }
                    // Radio hiccup: the old advertisement was already
                    // dropped above, so nothing is on air. Clear the belief
                    // so the next turn re-advertises this same frame instead
                    // of assuming it is still up; burn this dwell to avoid a
                    // tight error loop.
                    Err(e) => {
                        log::warn!("advertise failed: {e:?}");
                        on_air = None;
                    }
                }
            }

            // Hold the slot for one dwell, still accepting outgoing
            // frames into the rotation.
            let deadline = Instant::now() + DWELL;
            loop {
                match select(Timer::at(deadline), OUTGOING.receive()).await {
                    Either::First(()) => break,
                    Either::Second(out) => enqueue(&mut rotation, out),
                }
            }
        }
    };

    // The scan and advertise arms never complete; only a host runner
    // failure can get here — and that is fatal for a radio node.
    let _ = select3(host, scan, advertise).await;
    panic!("BLE host stopped");
}

fn enqueue(rotation: &mut ExtRotation, out: Outgoing) {
    match out {
        Outgoing::Own(frame) => rotation.enqueue_own(frame),
        Outgoing::Relay(frame) => rotation.enqueue_relay(frame),
    }
}
