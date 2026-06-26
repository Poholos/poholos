// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Poholos morse-code mesh node for the BBC micro:bit v2.
//!
//! A full mesh node — scans, relays, and shows delivered messages — whose
//! *input* method is morse code tapped on the two buttons instead of canned
//! messages. Composed messages are broadcast as hearsay.
//!
//! Input scheme:
//!
//! * Button **A** tap = **dot**, Button **B** tap = **dash**. The first tap
//!   begins a message; there is no separate "start" gesture.
//! * A short pause auto-commits the current letter; a longer pause inserts a
//!   word space (see [`LETTER_GAP`]/[`WORD_GAP`]).
//! * **Hold A** = finish & send the message. **Hold B** = clear it.
//!
//! Feedback: each dot/dash is flashed as a glyph on the 5×5 matrix, beeped on
//! the speaker, and logged to `defmt` (with the running pattern); each
//! completed letter is then flashed as a character. Incoming messages scroll
//! on the matrix (telegrams to this node get an `@` prefix and a chime),
//! exactly as in `poholos-microbit`.
//!
//! Decoding lives in the host-tested [`poholos_morse`] crate; this binary
//! only owns timing, I/O, and the radio.

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
use microbit_bsp::display::fonts::{ARROW_LEFT, ARROW_RIGHT, CROSS_MARK};
use microbit_bsp::display::{Bitmap, Brightness, Frame as Glyph, LedMatrix as LedMatrixDriver};
use microbit_bsp::speaker::{NamedPitch, Note, Pitch, PwmSpeaker};
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use panic_probe as _;
use poholos::{Frame, Packet, RouteAction, Router, WireId};
use poholos_morse::{Composer, Symbol};
use trouble_host::Address;

mod radio;

/// The 5×5 matrix driver from `microbit-bsp`, wired up by hand because we
/// bypass `Microbit::new()` (see `radio.rs` for why).
type LedMatrix = LedMatrixDriver<Output<'static>, 5, 5>;
type Button = Input<'static>;

/// A scrollable incoming message; 15 payload bytes plus prefix fits.
type DisplayMsg = String<24>;

/// Maximum composed-message length: the hearsay payload budget.
const MSG_LEN: usize = poholos::MAX_PAYLOAD_HEARSAY;
/// An owned composed message handed to the router for broadcast.
type Message = String<MSG_LEN>;
/// Maximum dot/dash elements in a valid morse character (matches
/// `morse-codec`'s per-character buffer). More than this means the previous
/// letter never got its pause and the two have merged.
const MAX_MORSE: usize = 6;

/// Frames heard by the scanner, awaiting routing.
static RX_FRAMES: Channel<CriticalSectionRawMutex, Frame, 8> = Channel::new();
/// Frames awaiting airtime, classed so the rotation can prioritize.
static OUTGOING: Channel<CriticalSectionRawMutex, Outgoing, 8> = Channel::new();
/// Raw button events from the input tasks, awaiting the composer.
static INPUT: Channel<CriticalSectionRawMutex, InputEvent, 16> = Channel::new();
/// Finished composed messages, awaiting the router for broadcast.
static COMPOSE: Channel<CriticalSectionRawMutex, Message, 4> = Channel::new();
/// Incoming messages awaiting the (slow) scrolling display.
static DISPLAY_MSGS: Channel<CriticalSectionRawMutex, DisplayMsg, 4> = Channel::new();
/// Glyphs to flash on the matrix: a dot/dash per press, a letter per commit.
static GLYPHS: Channel<CriticalSectionRawMutex, Show, 8> = Channel::new();
/// Pending chimes for telegrams addressed to this node.
static CHIMES: Channel<CriticalSectionRawMutex, (), 2> = Channel::new();
/// Sidetone beeps, one per dot/dash entered.
static SIDETONE: Channel<CriticalSectionRawMutex, (), 8> = Channel::new();

/// Press longer than this on either button is a Send (A) or Clear (B),
/// not a dot/dash symbol.
const HOLD: Duration = Duration::from_millis(600);
/// Settle time after a button is released before watching it again.
const DEBOUNCE: Duration = Duration::from_millis(15);
/// Idle this long after a symbol → commit the current letter.
const LETTER_GAP_MS: u64 = 800;
/// Idle this long after a symbol → also insert a word space.
const WORD_GAP_MS: u64 = 2000;
const LETTER_GAP: Duration = Duration::from_millis(LETTER_GAP_MS);
/// Remaining idle, measured from the letter commit, before a word space.
const WORD_AFTER_LETTER: Duration = Duration::from_millis(WORD_GAP_MS - LETTER_GAP_MS);
/// How long each dot/dash glyph stays lit on the matrix.
const GLYPH_SHOW: Duration = Duration::from_millis(150);
/// How long each decoded character stays lit on the matrix.
const CHAR_SHOW: Duration = Duration::from_millis(400);
/// Per-character dwell for the scrolling display.
const SCROLL_MS_PER_CHAR: u64 = 750;

/// Two ascending notes announcing "a telegram for *you*".
const CHIME: [Note; 2] = [
    Note(Pitch::Named(NamedPitch::E5), 120),
    Note(Pitch::Named(NamedPitch::A5), 180),
];
/// A short blip played as each dot/dash is entered.
const SIDETONE_NOTE: Note = Note(Pitch::Named(NamedPitch::A5), 30);

/// Dot glyph: a small centered diamond.
const DOT_GLYPH: Glyph<5, 5> = Glyph::new([
    Bitmap::new(0b00000, 5),
    Bitmap::new(0b00100, 5),
    Bitmap::new(0b01110, 5),
    Bitmap::new(0b00100, 5),
    Bitmap::new(0b00000, 5),
]);
/// Dash glyph: a centered horizontal bar.
const DASH_GLYPH: Glyph<5, 5> = Glyph::new([
    Bitmap::new(0b00000, 5),
    Bitmap::new(0b00000, 5),
    Bitmap::new(0b11111, 5),
    Bitmap::new(0b00000, 5),
    Bitmap::new(0b00000, 5),
]);

/// An outgoing frame, classed by origin — mirrors `poholos-cli`.
#[derive(Debug)]
pub enum Outgoing {
    /// Originated here: guaranteed a recurring share of airtime.
    Own(Frame),
    /// Forwarded for the mesh: gets one dwell, then sheds.
    Relay(Frame),
}

/// A button gesture, resolved by the input tasks into composer intent.
#[derive(Copy, Clone)]
enum InputEvent {
    /// Button A tap.
    Dot,
    /// Button B tap.
    Dash,
    /// Button A hold: finish and send.
    Send,
    /// Button B hold: clear the message.
    Clear,
}

/// What the composer's armed idle timer will do when it fires.
#[derive(Copy, Clone)]
enum Phase {
    /// Commit the in-progress letter.
    Letter,
    /// Insert a word space.
    Word,
}

/// Something to flash on the LED matrix.
#[derive(Copy, Clone)]
enum Show {
    /// A dot glyph (button A press).
    Dot,
    /// A dash glyph (button B press).
    Dash,
    /// A decoded letter, as its ASCII byte.
    Char(u8),
    /// A rejected press: the current letter is already at the morse maximum.
    Error,
    /// A right arrow: the composed message was broadcast.
    Sent,
    /// A left arrow: the composed message was cleared.
    Cleared,
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
    defmt::info!(
        "poholos morse node {=str} (wire id {=u32:08x})",
        name.as_str(),
        wire_id.get()
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

    spawner.must_spawn(button_task(btn_a, InputEvent::Dot, InputEvent::Send));
    spawner.must_spawn(button_task(btn_b, InputEvent::Dash, InputEvent::Clear));
    spawner.must_spawn(compose_task());
    spawner.must_spawn(display_task(display, name));
    spawner.must_spawn(speaker_task(speaker));
    spawner.must_spawn(router_task(wire_id));

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

/// Turns presses of one button (active low) into composer events.
///
/// A short tap emits `tap` (dot or dash); a press held past [`HOLD`] emits
/// `hold` (send or clear) once released.
#[embassy_executor::task(pool_size = 2)]
async fn button_task(mut button: Button, tap: InputEvent, hold: InputEvent) {
    loop {
        button.wait_for_falling_edge().await;
        let event = match select(button.wait_for_rising_edge(), Timer::after(HOLD)).await {
            // Released before HOLD: a short tap.
            Either::First(()) => tap,
            // Still held at HOLD: the gesture; wait for release, then emit.
            Either::Second(()) => {
                button.wait_for_rising_edge().await;
                hold
            }
        };
        INPUT.send(event).await;
        Timer::after(DEBOUNCE).await;
    }
}

/// The composer brain: turns button events and idle pauses into decoded
/// text via [`poholos_morse`], and hands finished messages to the router.
#[embassy_executor::task]
async fn compose_task() {
    let mut composer = Composer::<MSG_LEN>::new();
    // Dots and dashes of the letter being entered, kept only for the log.
    let mut elements: String<MAX_MORSE> = String::new();
    // `Some((phase, when))` while an idle commit is armed; `None` when idle.
    let mut pending: Option<(Phase, Instant)> = None;

    loop {
        let event = match pending {
            Some((_, when)) => select(INPUT.receive(), Timer::at(when)).await,
            None => Either::First(INPUT.receive().await),
        };

        match event {
            Either::First(InputEvent::Dot | InputEvent::Dash) => {
                let (symbol, element, glyph) = match event {
                    Either::First(InputEvent::Dash) => (Symbol::Dash, '-', Show::Dash),
                    _ => (Symbol::Dot, '.', Show::Dot),
                };
                // A letter longer than the morse maximum is invalid: the
                // previous letter never got its pause, so this one merged
                // with it. Flag it (the matrix shows a cross-mark) instead of
                // silently dropping the symbol — morse-codec drops it too.
                if elements.len() >= MAX_MORSE {
                    defmt::warn!("morse: letter too long — pause to commit");
                    let _ = GLYPHS.try_send(Show::Error);
                } else {
                    composer.symbol(symbol);
                    let _ = elements.push(element);
                    defmt::info!("dits {=str}", elements.as_str());
                    let _ = GLYPHS.try_send(glyph);
                    let _ = SIDETONE.try_send(());
                }
                pending = Some((Phase::Letter, Instant::now() + LETTER_GAP));
            }
            Either::First(InputEvent::Send) => {
                commit_letter(&mut composer, &mut elements);
                if !composer.is_empty() {
                    let mut text = Message::new();
                    let _ = text.push_str(composer.message());
                    COMPOSE.send(text).await;
                    let _ = GLYPHS.try_send(Show::Sent);
                }
                composer.clear();
                elements.clear();
                pending = None;
            }
            Either::First(InputEvent::Clear) => {
                if !composer.is_empty() || !elements.is_empty() {
                    let _ = GLYPHS.try_send(Show::Cleared);
                }
                composer.clear();
                elements.clear();
                pending = None;
                defmt::info!("morse: cleared");
            }
            // Idle timer fired.
            Either::Second(()) => match pending {
                Some((Phase::Letter, _)) => {
                    commit_letter(&mut composer, &mut elements);
                    pending = Some((Phase::Word, Instant::now() + WORD_AFTER_LETTER));
                }
                Some((Phase::Word, _)) => {
                    composer.word_gap();
                    pending = None;
                }
                None => {}
            },
        }
    }
}

/// Commits the in-progress letter: flashes the decoded character on the
/// matrix and logs the running text. A no-op if no element was entered.
fn commit_letter(composer: &mut Composer<MSG_LEN>, elements: &mut String<MAX_MORSE>) {
    if elements.is_empty() {
        return;
    }
    composer.letter_gap();
    if let Some(&byte) = composer.message().as_bytes().last() {
        let _ = GLYPHS.try_send(Show::Char(byte));
    }
    defmt::info!("text {=str}", composer.message());
    elements.clear();
}

/// The protocol brain: feeds received frames through the [`Router`] and
/// broadcasts composed messages — the firmware twin of the CLI's chat loop.
#[embassy_executor::task]
async fn router_task(local: WireId) {
    let mut router = Router::new(local);
    // The first seq must be random so a rebooted node dodges its old packets
    // in peers' seen caches — but the RNG is owned by the radio. Substitute:
    // the uptime tick of the first composed send.
    let mut seq: Option<u16> = None;

    loop {
        match select(RX_FRAMES.receive(), COMPOSE.receive()).await {
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
                // Duplicates, own echoes, expired telegrams, and foreign or
                // corrupt advertisements: routine radio noise.
                Ok(RouteAction::Ignore(_)) | Err(_) => {}
            },
            Either::Second(text) => {
                let next = seq.unwrap_or_else(|| Instant::now().as_ticks() as u16);
                seq = Some(next.wrapping_add(1));
                // The composer caps text at the hearsay budget by construction.
                if let Ok(packet) = Packet::hearsay(local, next, text.as_bytes()) {
                    let frame = router.originate(&packet);
                    defmt::info!("sending {=str} (seq {=u16})", text.as_str(), next);
                    OUTGOING.send(Outgoing::Own(frame)).await;
                }
            }
        }
    }
}

/// Shows a delivered packet: defmt for the log, LED scroll for the user.
/// Telegrams addressed to us get an `@` prefix and a chime.
fn deliver(packet: &Packet, local: WireId) {
    let text = core::str::from_utf8(packet.payload()).unwrap_or("<bin>");
    defmt::info!("received from {=u32:08x}: {=str}", packet.src().get(), text);
    let mut msg = DisplayMsg::new();
    if packet.dest() == Some(local) {
        let _ = msg.push_str("@ ");
        // Full queue means a chime is already pending: it covers this one too.
        let _ = CHIMES.try_send(());
    }
    let _ = msg.push_str(text);
    enqueue_display(msg);
}

/// Queues a message for the scrolling display, shedding the oldest pending
/// one under burst — scrolling is far slower than the mesh.
fn enqueue_display(msg: DisplayMsg) {
    if let Err(TrySendError::Full(msg)) = DISPLAY_MSGS.try_send(msg) {
        let _ = DISPLAY_MSGS.try_receive();
        let _ = DISPLAY_MSGS.try_send(msg);
    }
}

/// Derives the node's display name, `mb-` + 4 hex chars of the factory
/// device id — the embedded analogue of `NodeId`'s entropy suffix.
fn node_name() -> String<8> {
    let suffix = pac::FICR.deviceid(0).read() & 0xFFFF;
    let mut name = String::new();
    defmt::unwrap!(write!(name, "mb-{suffix:04x}").map_err(|_| ()), "fits");
    name
}

/// Builds a static random BLE address from the factory-programmed device
/// address (FICR); the spec requires the top two bits set.
fn ble_address() -> Address {
    let lo = pac::FICR.deviceaddr(0).read();
    let hi = pac::FICR.deviceaddr(1).read();
    let mut addr = [0u8; 6];
    addr[..4].copy_from_slice(&lo.to_le_bytes());
    addr[4..].copy_from_slice(&(hi as u16).to_le_bytes());
    addr[5] |= 0b1100_0000;
    Address::random(addr)
}

/// Reads the source wire id straight out of an encoded frame (bytes 3..7,
/// big-endian), for log lines that should not re-decode.
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

/// Plays the telegram chime and per-symbol sidetone on the onboard speaker.
#[embassy_executor::task]
async fn speaker_task(mut speaker: PwmSpeaker<'static, PWM0>) {
    loop {
        match select(CHIMES.receive(), SIDETONE.receive()).await {
            Either::First(()) => {
                for note in &CHIME {
                    speaker.play(note).await;
                }
            }
            Either::Second(()) => speaker.play(&SIDETONE_NOTE).await,
        }
    }
}

/// Owns the LED matrix: scrolls the node's own name once at boot, then flashes
/// a dot/dash per press and the decoded letter per commit, and scrolls
/// delivered messages.
#[embassy_executor::task]
async fn display_task(mut display: LedMatrix, name: String<8>) {
    display.set_brightness(Brightness::MAX);
    scroll_text(&mut display, &name).await;
    loop {
        match select(GLYPHS.receive(), DISPLAY_MSGS.receive()).await {
            Either::First(show) => match show {
                Show::Dot => display.display(DOT_GLYPH, GLYPH_SHOW).await,
                Show::Dash => display.display(DASH_GLYPH, GLYPH_SHOW).await,
                Show::Char(byte) => {
                    let glyph: Glyph<5, 5> = (byte as char).into();
                    display.display(glyph, CHAR_SHOW).await;
                }
                Show::Error => display.display(CROSS_MARK, GLYPH_SHOW).await,
                Show::Sent => display.display(ARROW_RIGHT, CHAR_SHOW).await,
                Show::Cleared => display.display(ARROW_LEFT, CHAR_SHOW).await,
            },
            Either::Second(msg) => scroll_text(&mut display, &msg).await,
        }
    }
}

/// Scrolls `text` across the LED matrix at [`SCROLL_MS_PER_CHAR`] each.
async fn scroll_text(display: &mut LedMatrix, text: &str) {
    display
        .scroll_with_speed(
            text,
            Duration::from_millis(text.len() as u64 * SCROLL_MS_PER_CHAR),
        )
        .await;
}
