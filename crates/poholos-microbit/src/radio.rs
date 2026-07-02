// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! BLE radio bring-up: MPSL + Nordic SoftDevice Controller + trouble host.
//!
//! This replicates the setup in `microbit-bsp`'s `ble.rs` but the
//! controller is built for the full poholos role set: a node is an
//! observer and a broadcaster at once, and it is **dual-stack** across
//! both wire versions. The BSP's builder hardcodes legacy advertising +
//! peripheral only, which is why we own this layer ourselves and add:
//!
//! * `support_ext_scan` + `support_le_2m_phy` + `support_le_coded_phy` —
//!   extended scanning on *both* primary channel sets (1M and Coded), which
//!   receives legacy, plain extended, and long-range coded advertisements,
//!   so the node hears wire version 0 and 1 alike;
//! * `support_ext_adv` + `adv_buffer_cfg(255)` — extended advertising for
//!   *all* outgoing frames: version 0 as legacy-PDU extended adverts (still
//!   heard by legacy-only scanners), version 1 as extended-PDU on the
//!   **Coded (long-range, S=8) PHY**. Legacy `support_adv` is deliberately
//!   absent — mixing legacy and extended HCI commands (we scan with the
//!   extended set) is forbidden by the controller and returns Command
//!   Disallowed.
//!
//! [`run`] drives three concerns forever:
//!
//! * the trouble host runner, whose scan-report handler decodes poholos
//!   frames out of manufacturer data and feeds `RX_FRAMES`;
//! * a continuous passive extended-scan session;
//! * the advertiser, which time-shares the single advertising slot via
//!   the shared [`poholos::rotation`] policy, fed from `OUTGOING` — the
//!   firmware twin of `poholos-cli`'s `advertise_loop`. It picks legacy or
//!   extended advertising per frame by size, exactly as the desktop
//!   Windows transport does.

use bt_hci::param::{LeExtAdvReportsIter, PhyKind};
use embassy_futures::select::{Either, select, select3};
use embassy_nrf::mode::Async;
use embassy_nrf::peripherals::{
    PPI_CH17, PPI_CH18, PPI_CH19, PPI_CH20, PPI_CH21, PPI_CH22, PPI_CH23, PPI_CH24, PPI_CH25,
    PPI_CH26, PPI_CH27, PPI_CH28, PPI_CH29, PPI_CH30, PPI_CH31, RNG, RTC0, TEMP, TIMER0,
};
use embassy_nrf::{Peri, bind_interrupts, rng};
use embassy_time::{Duration, Instant, Timer};
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::{self as sdc, mpsl};
use poholos::rotation::ExtRotation;
use poholos::{COMPANY_ID, ExtFrame, MAX_FRAME_LEN};
use static_cell::StaticCell;
use trouble_host::advertise::{
    AdStructure, Advertisement, AdvertisementParameters, AdvertisementSet,
};
use trouble_host::connection::{PhySet, ScanConfig};
use trouble_host::prelude::DefaultPacketPool;
use trouble_host::scan::Scanner;
use trouble_host::{Address, Host, HostResources};

use crate::{OUTGOING, Outgoing, RX_FRAMES};

/// Memory handed to the SoftDevice Controller.
///
/// The extended adv + extended scan + 2M PHY + 255-byte ext-adv buffer
/// configuration reported ~2792 bytes needed on hardware; 4 KiB leaves
/// headroom. If `build` reports a different size, trim or raise to match
/// the boot log — over-provisioning only logs a harmless "memory buffer
/// too big" note.
const SDC_MEMORY_SIZE: usize = 4096;

/// Buffer for an encoded extended advertisement: the largest frame plus
/// the manufacturer-data AD overhead (length + type + 2-byte company id).
const EXT_ADV_DATA_LEN: usize = poholos::MAX_EXT_FRAME_LEN + 8;

/// One rotation dwell, converted to the embassy clock.
const DWELL: Duration = Duration::from_millis(poholos::rotation::DWELL.as_millis() as u64);

bind_interrupts!(struct Irqs {
    RNG => rng::InterruptHandler<RNG>;
    EGU0_SWI0 => mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => mpsl::ClockInterruptHandler;
    RADIO => mpsl::HighPrioInterruptHandler;
    TIMER0 => mpsl::HighPrioInterruptHandler;
    RTC0 => mpsl::HighPrioInterruptHandler;
});

/// Low-frequency clock from the internal RC oscillator: the micro:bit
/// has no 32 kHz crystal.
const LFCLK_CFG: mpsl::raw::mpsl_clock_lfclk_cfg_t = mpsl::raw::mpsl_clock_lfclk_cfg_t {
    source: mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
    rc_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
    rc_temp_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
    accuracy_ppm: mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
    skip_wait_lfclk_started: mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
};

/// The radio peripherals the MPSL and the SDC claim for themselves.
pub struct RadioPeripherals {
    pub rtc0: Peri<'static, RTC0>,
    pub timer0: Peri<'static, TIMER0>,
    pub temp: Peri<'static, TEMP>,
    pub rng: Peri<'static, RNG>,
    pub ppi_ch17: Peri<'static, PPI_CH17>,
    pub ppi_ch18: Peri<'static, PPI_CH18>,
    pub ppi_ch19: Peri<'static, PPI_CH19>,
    pub ppi_ch20: Peri<'static, PPI_CH20>,
    pub ppi_ch21: Peri<'static, PPI_CH21>,
    pub ppi_ch22: Peri<'static, PPI_CH22>,
    pub ppi_ch23: Peri<'static, PPI_CH23>,
    pub ppi_ch24: Peri<'static, PPI_CH24>,
    pub ppi_ch25: Peri<'static, PPI_CH25>,
    pub ppi_ch26: Peri<'static, PPI_CH26>,
    pub ppi_ch27: Peri<'static, PPI_CH27>,
    pub ppi_ch28: Peri<'static, PPI_CH28>,
    pub ppi_ch29: Peri<'static, PPI_CH29>,
    pub ppi_ch30: Peri<'static, PPI_CH30>,
    pub ppi_ch31: Peri<'static, PPI_CH31>,
}

/// Brings up the MPSL and builds the SoftDevice Controller with
/// advertising *and* scanning support.
///
/// The returned MPSL reference must be driven by a spawned task calling
/// `mpsl.run()` before the controller is used.
pub fn init(
    p: RadioPeripherals,
) -> Result<
    (
        sdc::SoftdeviceController<'static>,
        &'static MultiprotocolServiceLayer<'static>,
    ),
    sdc::Error,
> {
    let mpsl_p =
        mpsl::Peripherals::new(p.rtc0, p.timer0, p.temp, p.ppi_ch19, p.ppi_ch30, p.ppi_ch31);
    static MPSL: StaticCell<MultiprotocolServiceLayer> = StaticCell::new();
    let mpsl = MPSL.init(mpsl::MultiprotocolServiceLayer::new(
        mpsl_p, Irqs, LFCLK_CFG,
    )?);

    let sdc_p = sdc::Peripherals::new(
        p.ppi_ch17, p.ppi_ch18, p.ppi_ch20, p.ppi_ch21, p.ppi_ch22, p.ppi_ch23, p.ppi_ch24,
        p.ppi_ch25, p.ppi_ch26, p.ppi_ch27, p.ppi_ch28, p.ppi_ch29,
    );
    static SDC_RNG: StaticCell<rng::Rng<'static, RNG, Async>> = StaticCell::new();
    let sdc_rng = SDC_RNG.init(rng::Rng::new(p.rng, Irqs));
    static SDC_MEM: StaticCell<sdc::Mem<SDC_MEMORY_SIZE>> = StaticCell::new();
    let sdc_mem = SDC_MEM.init(sdc::Mem::new());

    let sdc = sdc::Builder::new()?
        // ALL advertising goes through the *extended* command set
        // (`advertise_ext`), even wire-version-0 frames (sent as legacy-PDU
        // extended advertisements). This is mandatory, not an optimization:
        // the BLE controller forbids mixing legacy and extended HCI
        // commands, and we use extended *scanning* below — issuing a legacy
        // advertising command alongside it returns Command Disallowed.
        .support_ext_adv()?
        // Extended scanning receives both legacy and extended
        // advertisements; the 2M PHY lets it follow ext-adv AUX packets
        // (Windows places the data channel on 2M when not using coded).
        .support_ext_scan()?
        .support_le_2m_phy()?
        // Coded (long-range, S=8) PHY: wire-version-1 frames are advertised
        // on it, and the scanner listens on coded primaries (see `run`).
        .support_le_coded_phy()?
        // The default adv buffer only holds a legacy 31-byte advertisement;
        // raise it so the controller can store a ~200-byte ext advert,
        // else LeSetExtAdvData fails with Memory Capacity Exceeded.
        .adv_buffer_cfg(255)?
        .build(sdc_p, sdc_rng, mpsl, sdc_mem)?;
    Ok((sdc, mpsl))
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
pub async fn run(controller: sdc::SoftdeviceController<'static>, address: Address) -> ! {
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
        defmt::error!("BLE host runner stopped: {}", defmt::Debug2Format(&result));
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
        let _session = defmt::unwrap!(
            scanner.scan_ext(&config).await.map_err(|_| ()),
            "ext scan start"
        );
        defmt::info!("ext-scanning (1M + coded primaries) for poholos frames");
        core::future::pending::<()>().await
    };

    let advertise = async {
        let mut rotation = ExtRotation::new();
        // The frame currently on air and its advertiser handle, held
        // only for RAII (dropping it stops the broadcast — hence the
        // underscore: it is written, never read). Consecutive turns
        // often serve the same frame; re-advertising it would be
        // pointless churn. `advertise` and `advertise_ext` return the same
        // handle type, so one variable holds either.
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
                    // extended command set (see `init`). Legacy PDUs are heard
                    // by every scanner, legacy-only nodes included.
                    let mut adv_data = [0u8; 31];
                    let len = defmt::unwrap!(
                        AdStructure::encode_slice(
                            &[AdStructure::ManufacturerSpecificData {
                                company_identifier: COMPANY_ID,
                                payload: frame.as_bytes(),
                            }],
                            &mut adv_data,
                        )
                        .map_err(|_| ()),
                        "legacy frame + AD overhead always fits 31 bytes"
                    );
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
                    let len = defmt::unwrap!(
                        AdStructure::encode_slice(
                            &[AdStructure::ManufacturerSpecificData {
                                company_identifier: COMPANY_ID,
                                payload: frame.as_bytes(),
                            }],
                            &mut adv_data,
                        )
                        .map_err(|_| ()),
                        "ext frame + AD overhead fits the extended buffer"
                    );
                    let sets = [AdvertisementSet {
                        // Wire version 1 rides the Coded (long-range) PHY on
                        // both hops; the SDC codes advertising at S=8. Only
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
                        defmt::warn!("advertise failed: {}", defmt::Debug2Format(&e));
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
    defmt::panic!("BLE host stopped");
}

fn enqueue(rotation: &mut ExtRotation, out: Outgoing) {
    match out {
        Outgoing::Own(frame) => rotation.enqueue_own(frame),
        Outgoing::Relay(frame) => rotation.enqueue_relay(frame),
    }
}
