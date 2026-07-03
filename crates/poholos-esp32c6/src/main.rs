// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Poholos relay firmware for the ESP32-C6-DevKitC-1.
//!
//! A full mesh node: scans continuously (1M + Coded primaries), relays
//! frames with the shared flood/TTL/dedup semantics, shows delivered
//! messages on an SSD1306 128x64 OLED, and originates one canned message —
//! the **BOOT button** broadcasts a long status (long enough that it rides
//! wire version 1 over Coded-PHY extended advertising).
//!
//! Architecture (the ESP twin of `poholos-microbit`):
//!
//! ```text
//! scan handler ──RX_FRAMES──▶ router task ──DISPLAY_MSGS──▶ display task
//! button task  ──BUTTONS────▶  (Router+seq) ──OUTGOING────▶ advertiser
//! ```
//!
//! Wiring: SSD1306 on I2C — SDA = GPIO6, SCL = GPIO7, VCC = 3V3, GND = GND.
//! The node runs headless if no display is attached.

#![no_std]
#![no_main]

use core::fmt::Write as _;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, TrySendError};
use embassy_time::{Duration, Instant, Timer};
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::i2c::master::{Config as I2cConfig, I2c};
use esp_hal::timer::timg::TimerGroup;
use heapless::String;
use poholos::{ExtFrame, ExtPacket, ExtRouteAction, Router, WireId};
use trouble_host::Address;

mod display;
mod radio;

esp_bootloader_esp_idf::esp_app_desc!();

/// BOOT button broadcast payload.
///
/// Deliberately longer than the 15-byte legacy hearsay budget so the
/// encoded frame exceeds [`poholos::MAX_FRAME_LEN`] and goes out as wire
/// version 1 over **Coded-PHY extended advertising** — this is what
/// exercises the C6's long-range TX path. Stays within the extended
/// hearsay limit ([`poholos::MAX_EXT_PAYLOAD_HEARSAY`]).
const STATUS_MESSAGE: &[u8] = b"esp32 checking in - long status over coded extended advertising";

/// A displayable message: sized for the largest deliverable payload
/// ([`poholos::MAX_EXT_PAYLOAD_HEARSAY`]) plus the 2-byte `@ `/`* ` prefix.
pub type DisplayMsg = String<{ poholos::MAX_EXT_PAYLOAD_HEARSAY + 2 }>;

/// Leading glyph (plus a space) on a delivered hearsay message; `@ ` marks
/// a telegram addressed to this node, `> ` echoes an own send.
const HEARSAY_MARK: &str = "* ";

/// Frames heard by the scanner, awaiting routing.
pub static RX_FRAMES: Channel<CriticalSectionRawMutex, ExtFrame, 8> = Channel::new();
/// Frames awaiting airtime, classed so the rotation can prioritize.
pub static OUTGOING: Channel<CriticalSectionRawMutex, Outgoing, 8> = Channel::new();
/// Button presses awaiting the router task.
static BUTTONS: Channel<CriticalSectionRawMutex, (), 4> = Channel::new();
/// Messages awaiting the display.
pub static DISPLAY_MSGS: Channel<CriticalSectionRawMutex, DisplayMsg, 4> = Channel::new();

/// An outgoing frame - mirrors `poholos-cli` and the micro:bit firmware.
#[derive(Debug)]
pub enum Outgoing {
    /// Originated here: guaranteed a recurring share of airtime.
    Own(ExtFrame),
    /// Forwarded for the mesh: gets one dwell, then sheds.
    Relay(ExtFrame),
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    // The radio blob allocates from this heap.
    esp_alloc::heap_allocator!(size: 72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let software_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, software_interrupt.software_interrupt0);

    let name = node_name();
    let wire_id = WireId::of_name(&name);
    log::info!(
        "poholos node {} (wire id {:08x})",
        name.as_str(),
        wire_id.get()
    );

    // SSD1306 on I2C0: SDA = GPIO6, SCL = GPIO7 (both free, non-strapping
    // pins on the DevKitC-1 headers).
    let i2c = I2c::new(peripherals.I2C0, I2cConfig::default())
        .expect("I2C init")
        .with_sda(peripherals.GPIO6)
        .with_scl(peripherals.GPIO7)
        .into_async();
    spawner.spawn(display::display_task(i2c, name).expect("display task"));

    // BOOT button (GPIO9, active low, external pull-up on the board).
    let boot = Input::new(
        peripherals.GPIO9,
        InputConfig::default().with_pull(Pull::Up),
    );
    spawner.spawn(button_task(boot).expect("button task"));
    spawner.spawn(router_task(wire_id).expect("router task"));

    let controller = radio::controller(peripherals.BT);
    radio::run(controller, ble_address()).await
}

/// The protocol brain: feeds received frames through the [`Router`] and
/// turns button presses into outgoing packets.
#[embassy_executor::task]
async fn router_task(local: WireId) {
    let mut router = Router::new(local);
    // The first seq must be unpredictable so a rebooted node dodges its
    // old packets in peers' seen caches; the uptime tick of the first
    // button press is unpredictable enough at tick granularity.
    let mut seq: Option<u16> = None;

    loop {
        match select(RX_FRAMES.receive(), BUTTONS.receive()).await {
            Either::First(frame) => match router.ingest(frame.as_bytes()) {
                Ok(ExtRouteAction::Deliver(packet)) => deliver(&packet, local),
                Ok(ExtRouteAction::DeliverAndForward(packet, relay)) => {
                    deliver(&packet, local);
                    log::debug!("relaying frame from {:08x}", frame_src(&relay));
                    OUTGOING.send(Outgoing::Relay(relay)).await;
                }
                Ok(ExtRouteAction::Forward(relay)) => {
                    log::debug!("relaying frame from {:08x}", frame_src(&relay));
                    OUTGOING.send(Outgoing::Relay(relay)).await;
                }
                // Duplicates, own echoes, expired telegrams, and foreign
                // or corrupt advertisements: routine radio noise.
                Ok(ExtRouteAction::Ignore(_)) | Err(_) => {}
            },
            Either::Second(()) => {
                let next = seq.unwrap_or_else(|| Instant::now().as_ticks() as u16);
                seq = Some(next.wrapping_add(1));

                // Static payload is within the limits by construction.
                let Ok(packet) = ExtPacket::hearsay(local, next, STATUS_MESSAGE) else {
                    continue;
                };
                let frame = router.originate(&packet);
                log::info!("boot button: sending seq {next}");
                OUTGOING.send(Outgoing::Own(frame)).await;
                show("> status");
            }
        }
    }
}

/// Shows a delivered packet: log for the console, OLED for the user.
fn deliver(packet: &ExtPacket, local: WireId) {
    let text = core::str::from_utf8(packet.payload()).unwrap_or("<bin>");
    log::info!("received from {:08x}: {text}", packet.src().get());
    let mut msg = DisplayMsg::new();
    let _ = msg.push_str(if packet.dest() == Some(local) {
        // Telegram for us.
        "@ "
    } else {
        // Hearsay (broadcast).
        HEARSAY_MARK
    });
    // The buffer is sized for a full payload, so this normally pushes the
    // whole message; go char by char (push_str is all-or-nothing) and stop
    // if it ever fills, truncating rather than blanking on overflow.
    for c in text.chars() {
        if msg.push(c).is_err() {
            break;
        }
    }
    enqueue_display(msg);
}

fn show(text: &str) {
    let mut msg = DisplayMsg::new();
    let _ = msg.push_str(text);
    enqueue_display(msg);
}

/// Queues a message for the display, shedding the oldest pending one
/// under burst.
fn enqueue_display(msg: DisplayMsg) {
    if let Err(TrySendError::Full(msg)) = DISPLAY_MSGS.try_send(msg) {
        let _ = DISPLAY_MSGS.try_receive();
        let _ = DISPLAY_MSGS.try_send(msg);
    }
}

/// Derives the node's display name, `esp-` + 4 hex chars of the factory
/// MAC - the ESP analogue of `NodeId`'s entropy suffix. Wire ids derive
/// from this full string, so desktop users can address the board as
/// `@esp-xxxx`.
fn node_name() -> String<9> {
    let mac = esp_hal::efuse::base_mac_address();
    let mac = mac.as_bytes();
    let suffix = u16::from_be_bytes([mac[4], mac[5]]);
    let mut name = String::new();
    write!(name, "esp-{suffix:04x}").expect("fits");
    name
}

/// Builds a static random BLE address from the factory MAC, as the spec
/// requires the top two bits set.
fn ble_address() -> Address {
    let mac = esp_hal::efuse::base_mac_address();
    let mac = mac.as_bytes();
    let mut addr = [mac[5], mac[4], mac[3], mac[2], mac[1], mac[0]];
    addr[5] |= 0b1100_0000;
    Address::random(addr)
}

/// Reads the source wire id straight out of an encoded frame (bytes
/// 3..7, big-endian), for log lines that should not re-decode.
fn frame_src(frame: &ExtFrame) -> u32 {
    let b = frame.as_bytes();
    u32::from_be_bytes([b[3], b[4], b[5], b[6]])
}

/// Turns presses of the BOOT button (active low) into router events.
#[embassy_executor::task]
async fn button_task(mut button: Input<'static>) {
    loop {
        button.wait_for_falling_edge().await;
        BUTTONS.send(()).await;
        // Crude debounce.
        Timer::after(Duration::from_millis(200)).await;
    }
}
