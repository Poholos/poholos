// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Poholos relay firmware for the BBC micro:bit v2.
//!
//! A full mesh node: scans continuously, relays frames with the shared
//! flood/TTL/dedup semantics, scrolls delivered messages on the 5×5 LED
//! matrix, and originates two canned messages - button **A** sends
//! an "I am OK" telegram to the preconfigured buddy node, button **B**
//! broadcasts "SOS - test".
//!
//! The buddy is baked in at compile time: set `POHOLOS_BUDDY` (a full
//! node id such as `alice-0001`) when building; pair it with a desktop
//! running `poholos-cli --id alice-0001 --transport ble`.
//!
//! Architecture (the embedded twin of `poholos-cli`'s chat loop):
//!
//! ```text
//! scan handler ──RX_FRAMES──▶ router task ──DISPLAY_MSGS──▶ display task
//! button tasks ──BUTTONS────▶  (Router+seq) ──OUTGOING────▶ advertiser
//! ```

#![no_std]
#![no_main]

use core::fmt::Write as _;

use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_nrf::config::Config;
use embassy_nrf::gpio::{Input, Level, Output, OutputDrive, Pin, Pull};
use embassy_nrf::interrupt::Priority;
use embassy_nrf::peripherals::PWM0;
use embassy_nrf::pwm::SimplePwm;
use embassy_nrf::{Peri, pac};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::{Channel, TrySendError};
use embassy_time::{Duration, Instant, Timer};
use heapless::String;
use microbit_bsp::display::{Brightness, LedMatrix as LedMatrixDriver};
use microbit_bsp::speaker::{NamedPitch, Note, Pitch, PwmSpeaker};
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use panic_probe as _;
use poholos::{Frame, Packet, RouteAction, Router, WireId};
use trouble_host::Address;

mod radio;

/// The 5×5 matrix driver from `microbit-bsp`, wired up by hand because
/// we bypass `Microbit::new()` (see `radio.rs` for why).
type LedMatrix = LedMatrixDriver<Output<'static>, 5, 5>;
type Button = Input<'static>;

/// Full node id of the buddy that button A's "I am OK" telegram goes
/// to. Compile-time configurable; the default pairs with
/// `poholos-cli --id alice-0001`.
const BUDDY_NAME: &str = match option_env!("POHOLOS_BUDDY") {
    Some(name) => name,
    None => "alice-0001",
};

/// A scrollable message; 15 payload bytes plus prefix fits comfortably.
type DisplayMsg = String<24>;

/// Frames heard by the scanner, awaiting routing.
static RX_FRAMES: Channel<CriticalSectionRawMutex, Frame, 8> = Channel::new();
/// Frames awaiting airtime, classed so the rotation can prioritize.
static OUTGOING: Channel<CriticalSectionRawMutex, Outgoing, 8> = Channel::new();
/// Button presses awaiting the router task.
static BUTTONS: Channel<CriticalSectionRawMutex, ButtonEvent, 4> = Channel::new();
/// Messages awaiting the (slow) scrolling display.
static DISPLAY_MSGS: Channel<CriticalSectionRawMutex, DisplayMsg, 4> = Channel::new();
/// Pending chimes for telegrams addressed to this node. Capacity 2:
/// back-to-back telegrams chime twice, anything more is one alert.
static CHIMES: Channel<CriticalSectionRawMutex, (), 2> = Channel::new();

/// Two ascending notes announcing "a telegram for *you*" - distinct
/// from silence (broadcasts stay quiet) without being alarming.
const CHIME: [Note; 2] = [
    Note(Pitch::Named(NamedPitch::E5), 120),
    Note(Pitch::Named(NamedPitch::A5), 180),
];

/// An outgoing framen - mirrors `poholos-cli`.
#[derive(Debug)]
pub enum Outgoing {
    /// Originated here: guaranteed a recurring share of airtime.
    Own(Frame),
    /// Forwarded for the mesh: gets one dwell, then sheds.
    Relay(Frame),
}

#[derive(Copy, Clone, Debug, defmt::Format)]
enum ButtonEvent {
    /// Button A: "I am OK" telegram to the buddy.
    A,
    /// Button B: "SOS - test" broadcast.
    B,
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // The MPSL claims the highest interrupt priorities for radio timing;
    // everything else must run at P2 or lower.
    let mut config = Config::default();
    config.gpiote_interrupt_priority = Priority::P2;
    config.time_interrupt_priority = Priority::P2;
    let p = embassy_nrf::init(config);

    let name = node_name();
    let wire_id = WireId::of_name(&name);
    let buddy = WireId::of_name(BUDDY_NAME);
    defmt::info!(
        "poholos node {=str} (wire id {=u32:08x}), buddy {=str} ({=u32:08x})",
        name.as_str(),
        wire_id.get(),
        BUDDY_NAME,
        buddy.get()
    );

    // LED matrix and buttons; pin assignments match microbit-bsp's
    // board.rs (micro:bit v2 schematic).
    let rows = [
        led_pin(p.P0_21),
        led_pin(p.P0_22),
        led_pin(p.P0_15),
        led_pin(p.P0_24),
        led_pin(p.P0_19),
    ];
    let cols = [
        led_pin(p.P0_28),
        led_pin(p.P0_11),
        led_pin(p.P0_31),
        led_pin(p.P1_05),
        led_pin(p.P0_30),
    ];
    let display = LedMatrixDriver::new(rows, cols);
    let btn_a = Input::new(p.P0_14, Pull::None);
    let btn_b = Input::new(p.P0_23, Pull::None);
    // Onboard speaker: P0.00 PWM-driven; PWM0 is free (the radio stack
    // claims RTC0/TIMER0/RADIO, never PWM).
    let speaker = PwmSpeaker::new(SimplePwm::new_1ch(p.PWM0, p.P0_00));

    spawner.must_spawn(button_task(btn_a, ButtonEvent::A));
    spawner.must_spawn(button_task(btn_b, ButtonEvent::B));
    spawner.must_spawn(display_task(display, name));
    spawner.must_spawn(speaker_task(speaker));
    spawner.must_spawn(router_task(wire_id, buddy));

    // Radio bring-up: MPSL + SoftDevice Controller (adv + scan).
    let (sdc, mpsl) = defmt::unwrap!(radio::init(radio::RadioPeripherals {
        rtc0: p.RTC0,
        timer0: p.TIMER0,
        temp: p.TEMP,
        rng: p.RNG,
        ppi_ch17: p.PPI_CH17,
        ppi_ch18: p.PPI_CH18,
        ppi_ch19: p.PPI_CH19,
        ppi_ch20: p.PPI_CH20,
        ppi_ch21: p.PPI_CH21,
        ppi_ch22: p.PPI_CH22,
        ppi_ch23: p.PPI_CH23,
        ppi_ch24: p.PPI_CH24,
        ppi_ch25: p.PPI_CH25,
        ppi_ch26: p.PPI_CH26,
        ppi_ch27: p.PPI_CH27,
        ppi_ch28: p.PPI_CH28,
        ppi_ch29: p.PPI_CH29,
        ppi_ch30: p.PPI_CH30,
        ppi_ch31: p.PPI_CH31,
    }));
    spawner.must_spawn(mpsl_task(mpsl));

    radio::run(sdc, ble_address()).await
}

/// The protocol brain: feeds received frames through the [`Router`] and
/// turns button presses into outgoing packets — the firmware twin of
/// the CLI's chat loop, with the LED matrix standing in for stdout.
#[embassy_executor::task]
async fn router_task(local: WireId, buddy: WireId) {
    let mut router = Router::new(local);
    // The first seq must be random so a rebooted node dodges its old
    // packets in peers' seen caches — but the RNG peripheral is owned
    // by the radio (radio::init). Substitute: the uptime tick of the
    // first button press, unpredictable enough at tick granularity.
    let mut seq: Option<u16> = None;

    loop {
        match select(RX_FRAMES.receive(), BUTTONS.receive()).await {
            Either::First(frame) => match router.ingest(frame.as_bytes()) {
                Ok(RouteAction::Deliver(packet)) => deliver(&packet, local),
                Ok(RouteAction::DeliverAndForward(packet, relay)) => {
                    deliver(&packet, local);
                    defmt::debug!("relaying frame from {=u32:08x}", frame_src(&relay));
                    OUTGOING.send(Outgoing::Relay(relay)).await;
                }
                Ok(RouteAction::Forward(relay)) => {
                    defmt::debug!("relaying frame from {=u32:08x}", frame_src(&relay));
                    OUTGOING.send(Outgoing::Relay(relay)).await;
                }
                // Duplicates, own echoes, expired telegrams, and foreign
                // or corrupt advertisements: routine radio noise.
                Ok(RouteAction::Ignore(_)) | Err(_) => {}
            },
            Either::Second(button) => {
                let next = seq
                    .unwrap_or_else(|| Instant::now().as_ticks() as u16);
                seq = Some(next.wrapping_add(1));

                let packet = match button {
                    ButtonEvent::A => Packet::telegram(local, buddy, next, b"I am OK"),
                    ButtonEvent::B => Packet::hearsay(local, next, b"SOS - test"),
                };
                // Static payloads are within the limits by construction.
                let Ok(packet) = packet else { continue };
                let frame = router.originate(&packet);
                defmt::info!("button {}: sending seq {=u16}", button, next);
                OUTGOING.send(Outgoing::Own(frame)).await;
                scroll(match button {
                    ButtonEvent::A => ">ok",
                    ButtonEvent::B => ">sos",
                });
            }
        }
    }
}

/// Shows a delivered packet: defmt for the log, LED scroll for the user.
/// Telegrams addressed to us get an `@` prefix and a chime.
fn deliver(packet: &Packet, local: WireId) {
    let text = core::str::from_utf8(packet.payload()).unwrap_or("<bin>");
    defmt::info!(
        "received from {=u32:08x}: {=str}",
        packet.src().get(),
        text
    );
    let mut msg = DisplayMsg::new();
    if packet.dest() == Some(local) {
        let _ = msg.push_str("@ ");
        // Full queue means a chime is already pending.
        let _ = CHIMES.try_send(());
    }
    let _ = msg.push_str(text);
    enqueue_display(msg);
}

fn scroll(text: &str) {
    let mut msg = DisplayMsg::new();
    let _ = msg.push_str(text);
    enqueue_display(msg);
}

/// Queues a message for the scrolling display, shedding the oldest
/// pending one under burst - scrolling is far slower than the mesh.
fn enqueue_display(msg: DisplayMsg) {
    if let Err(TrySendError::Full(msg)) = DISPLAY_MSGS.try_send(msg) {
        let _ = DISPLAY_MSGS.try_receive();
        let _ = DISPLAY_MSGS.try_send(msg);
    }
}

/// Derives the node's display name, `mb-` + 4 hex chars of the factory
/// device id - the embedded analogue of `NodeId`'s entropy suffix. Wire
/// ids derive from this full string, so desktop users can address the
/// board as `@mb-xxxx`.
fn node_name() -> String<8> {
    let suffix = pac::FICR.deviceid(0).read() & 0xFFFF;
    let mut name = String::new();
    defmt::unwrap!(write!(name, "mb-{suffix:04x}").map_err(|_| ()), "fits");
    name
}

/// Builds a static random BLE address from the factory-programmed
/// device address (FICR), as the spec requires the top two bits set.
fn ble_address() -> Address {
    let lo = pac::FICR.deviceaddr(0).read();
    let hi = pac::FICR.deviceaddr(1).read();
    let mut addr = [0u8; 6];
    addr[..4].copy_from_slice(&lo.to_le_bytes());
    addr[4..].copy_from_slice(&(hi as u16).to_le_bytes());
    addr[5] |= 0b1100_0000;
    Address::random(addr)
}

/// Reads the source wire id straight out of an encoded frame (bytes
/// 3..7, big-endian), for log lines that should not re-decode.
fn frame_src(frame: &Frame) -> u32 {
    let b = frame.as_bytes();
    u32::from_be_bytes([b[3], b[4], b[5], b[6]])
}

fn led_pin(pin: Peri<'static, impl Pin>) -> Output<'static> {
    Output::new(pin, Level::Low, OutputDrive::Standard)
}

/// The Multiprotocol Service Layer needs a task driving it forever.
#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static MultiprotocolServiceLayer<'static>) -> ! {
    mpsl.run().await
}

/// Plays the telegram chime on the onboard speaker, one per request.
#[embassy_executor::task]
async fn speaker_task(mut speaker: PwmSpeaker<'static, PWM0>) {
    loop {
        CHIMES.receive().await;
        for note in &CHIME {
            speaker.play(note).await;
        }
    }
}

/// Turns presses of one button (active low) into router events.
#[embassy_executor::task(pool_size = 2)]
async fn button_task(mut button: Button, event: ButtonEvent) {
    loop {
        button.wait_for_falling_edge().await;
        BUTTONS.send(event).await;
        // Crude debounce.
        Timer::after(Duration::from_millis(200)).await;
    }
}

/// Scrolls the node's own name once (so users learn its `@mb-xxxx`
/// address), then scrolls delivered messages as they arrive.
#[embassy_executor::task]
async fn display_task(mut display: LedMatrix, name: String<8>) {
    display.set_brightness(Brightness::MAX);
    scroll_text(&mut display, &name).await;
    loop {
        let msg = DISPLAY_MSGS.receive().await;
        scroll_text(&mut display, &msg).await;
    }
}

/// Per-character dwell for the scrolling display. Slower than the BSP
/// default, which proved too fast to read on hardware.
const SCROLL_MS_PER_CHAR: u64 = 750;

/// Scrolls `text` across the LED matrix at [`SCROLL_MS_PER_CHAR`] each.
async fn scroll_text(display: &mut LedMatrix, text: &str) {
    display
        .scroll_with_speed(
            text,
            Duration::from_millis(text.len() as u64 * SCROLL_MS_PER_CHAR),
        )
        .await;
}
