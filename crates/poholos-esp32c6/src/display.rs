// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! SSD1306 128x64 OLED: node identity header + a scrollback of delivered
//! messages.
//!
//! The 6x10 font yields 21 columns x 6 rows: one header row (the node's
//! `esp-xxxx` name, so users learn its address) and 5 scrollback rows.
//! Each delivered message is wrapped into 21-char lines and pushed into
//! the scrollback, oldest lines shed on overflow — the OLED analogue of
//! the micro:bit's scrolling LED text, without the wait.
//!
//! If no display is attached the init fails once, the task parks, and the
//! node keeps relaying headless.

use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::BinaryColor;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;
use esp_hal::Async;
use esp_hal::i2c::master::I2c;
use heapless::{Deque, String};
use ssd1306::mode::DisplayConfigAsync;
use ssd1306::prelude::*;
use ssd1306::{I2CDisplayInterface, Ssd1306Async};

use crate::{DISPLAY_MSGS, DisplayMsg};

/// Characters per row at 6px per glyph on a 128px panel.
const COLS: usize = 21;
/// Scrollback rows below the header at 10px per row on a 64px panel.
const ROWS: usize = 5;

/// One rendered scrollback line.
type Line = String<COLS>;

/// The concrete panel: SSD1306 over async I2C in buffered-graphics mode.
type Oled = Ssd1306Async<
    I2CInterface<I2c<'static, Async>>,
    DisplaySize128x64,
    ssd1306::mode::BufferedGraphicsModeAsync<DisplaySize128x64>,
>;

/// Drives the OLED from [`DISPLAY_MSGS`].
#[embassy_executor::task]
pub async fn display_task(i2c: I2c<'static, Async>, name: String<9>) {
    let interface = I2CDisplayInterface::new(i2c);
    let mut display = Ssd1306Async::new(interface, DisplaySize128x64, DisplayRotation::Rotate0)
        .into_buffered_graphics_mode();
    if display.init().await.is_err() {
        // No display wired up: stay headless, keep the node alive.
        log::warn!("SSD1306 init failed; running headless");
        loop {
            let _ = DISPLAY_MSGS.receive().await;
        }
    }

    let style = MonoTextStyle::new(&FONT_6X10, BinaryColor::On);
    let mut lines: Deque<Line, ROWS> = Deque::new();

    // Header-only first paint, so the node's address shows at boot.
    redraw(&mut display, &name, &lines, style).await;

    loop {
        let msg = DISPLAY_MSGS.receive().await;
        push_wrapped(&mut lines, &msg);
        redraw(&mut display, &name, &lines, style).await;
    }
}

/// Wraps `msg` into `COLS`-char lines, shedding the oldest scrollback
/// lines to make room.
fn push_wrapped(lines: &mut Deque<Line, ROWS>, msg: &DisplayMsg) {
    let mut line = Line::new();
    for c in msg.chars() {
        if line.push(c).is_err() {
            push_line(lines, line);
            line = Line::new();
            let _ = line.push(c);
        }
    }
    if !line.is_empty() {
        push_line(lines, line);
    }
}

fn push_line(lines: &mut Deque<Line, ROWS>, line: Line) {
    if lines.is_full() {
        let _ = lines.pop_front();
    }
    let _ = lines.push_back(line);
}

/// Repaints the whole panel: header, then the scrollback.
async fn redraw(
    display: &mut Oled,
    name: &str,
    lines: &Deque<Line, ROWS>,
    style: MonoTextStyle<'_, BinaryColor>,
) {
    display.clear_buffer();
    // Baselines: FONT_6X10 renders above the given point; row n sits at
    // y = 8 + n*10 with a 2px gap under the header rule.
    let _ = Text::new(name, Point::new(0, 8), style).draw(display);
    for (n, line) in lines.iter().enumerate() {
        let y = 20 + (n as i32) * 10;
        let _ = Text::new(line, Point::new(0, y), style).draw(display);
    }
    if display.flush().await.is_err() {
        log::warn!("SSD1306 flush failed");
    }
}
