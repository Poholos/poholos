// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! BLE transport: connectionless mesh over advertising frames.
//!
//! Poholos never connects to anything. Every node continuously *scans*
//! (central role) and *advertises* (peripheral/broadcaster role). The
//! radio has a single advertising slot that it repeats continuously, so
//! outgoing frames are not put on air directly: they enter a rotation
//! ([`poholos::rotation`]) that time-shares the slot, alternating the
//! local user's message with queued relays and granting each enough
//! consecutive advertising events to actually be heard. (Naively
//! replacing the slot on every send would let the next relay evict your
//! message before it ever aired.) Receivers see each frame many times —
//! the router's seen-cache makes that harmless.
//!
//! Scanning is cross-platform via `btleplug` (see [`scan`]). Advertising
//! is platform specific and lives behind a small HAL, one module per OS,
//! each exporting an `Advertiser` with the same shape:
//!
//! | OS      | Mechanism | Frame budget |
//! |---------|-----------|--------------|
//! | Linux   | BlueZ manufacturer data (`bluer`) | full 22 bytes |
//! | Windows | `BluetoothLEAdvertisementPublisher` manufacturer data | full 22 bytes |
//! | macOS   | CoreBluetooth **128-bit service UUID** (1-byte tag+len) | 15 raw bytes |
//!
//! The macOS budget comes from CoreBluetooth "refusing" to advertise
//! foreign manufacturer data: the frame is packed into a 128-bit service
//! UUID (16 bytes) instead, one byte of which tags and lengths it. Frames
//! above a platform's budget make `send` fail with an explanation rather
//! than truncating. 

use anyhow::{Result, bail, ensure};
use poholos::Frame;
use poholos::rotation::{DWELL, Rotation};
use tokio::sync::mpsc;

mod scan;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// HAL glob re-exports per platform: an accepted exception to
// M-NO-GLOB-REEXPORTS, everything is forwarded from exactly one module.
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

// Dummy fallback so the crate still builds on other targets (M-OOBE HAL
// pattern); constructing the BLE transport there fails with a clear error.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use unsupported as platform;

/// How many scanned frames may queue before the scanner waits. BLE scan
/// reports trickle in at radio pace; 64 frames of 22 bytes is generous.
const RECV_QUEUE: usize = 64;

/// How many outgoing frames may queue between the chat loop and the
/// advertiser task. The rotation itself bounds what actually competes for
/// airtime; this only smooths bursts across the channel.
const SEND_QUEUE: usize = 16;

/// An outgoing frame, classed by origin so the rotation can prioritize.
#[derive(Debug)]
enum Outgoing {
    /// Typed by the local user: guaranteed a recurring share of airtime.
    Own(Frame),
    /// Forwarded for the mesh: gets one dwell, then sheds.
    Relay(Frame),
}

/// BLE advertising transport: btleplug scanner plus a rotation-fed
/// platform advertiser.
#[derive(Debug)]
pub struct BleTransport {
    out: mpsc::Sender<Outgoing>,
    rx: mpsc::Receiver<Frame>,
}

impl BleTransport {
    /// Starts scanning and the advertiser task that owns the radio slot.
    ///
    /// # Errors
    /// Fails if no Bluetooth adapter is present or powered, or if the
    /// platform cannot advertise at all.
    pub async fn new() -> Result<Self> {
        let (tx, rx) = mpsc::channel(RECV_QUEUE);
        scan::spawn(tx).await?;
        let advertiser = platform::Advertiser::new().await?;
        let (out, out_rx) = mpsc::channel(SEND_QUEUE);
        tokio::spawn(advertise_loop(advertiser, out_rx));
        Ok(Self { out, rx })
    }

    /// Queues the local user's message for its guaranteed airtime share.
    ///
    /// The message enters the rotation within one dwell and supersedes
    /// any previously typed message still rotating.
    ///
    /// # Errors
    /// Fails if the frame exceeds the platform TX budget (macOS: 16
    /// bytes via the name channel) or the advertiser task is gone.
    pub async fn send_own(&mut self, frame: &Frame) -> Result<()> {
        ensure!(
            frame.len() <= platform::MAX_FRAME,
            "frame is {} bytes but this platform can only advertise {} — \
             send a shorter message",
            frame.len(),
            platform::MAX_FRAME,
        );
        if self.out.send(Outgoing::Own(*frame)).await.is_err() {
            bail!("advertiser task stopped");
        }
        Ok(())
    }

    /// Queues a frame to relay for the mesh.
    ///
    /// Frames above the platform TX budget are skipped silently: a Mac
    /// hearing a full 22-byte frame can never re-advertise it, and that
    /// is routine capability mismatch, not an error.
    ///
    /// # Errors
    /// Fails if the advertiser task is gone.
    pub async fn send_relay(&mut self, frame: &Frame) -> Result<()> {
        if frame.len() > platform::MAX_FRAME {
            return Ok(());
        }
        if self.out.send(Outgoing::Relay(*frame)).await.is_err() {
            bail!("advertiser task stopped");
        }
        Ok(())
    }

    /// Waits for the next scanned frame; `None` if the scanner died.
    pub async fn recv(&mut self) -> Option<Frame> {
        self.rx.recv().await
    }
}

/// Owns the radio slot: feeds outgoing frames into the [`Rotation`] and
/// gives each its dwell on air.
///
/// Returns (stopping the rotation, with the last advertisement lingering
/// on air until the advertiser drops) when the transport is dropped.
async fn advertise_loop(mut advertiser: platform::Advertiser, mut rx: mpsc::Receiver<Outgoing>) {
    let mut rotation = Rotation::new();
    // The frame currently on air. Consecutive turns often serve the same
    // frame (an own message with no relays waiting); re-registering it
    // with the platform stack would be pointless churn.
    let mut on_air: Option<Frame> = None;

    loop {
        let Some(frame) = rotation.next_frame() else {
            // Nothing waiting: leave the current advertisement on air and
            // sleep until new work arrives.
            match rx.recv().await {
                Some(out) => {
                    enqueue(&mut rotation, &out);
                    continue;
                }
                None => return, // transport dropped
            }
        };

        if on_air != Some(frame) {
            let registered = match advertiser.set_frame(&frame).await {
                Ok(()) => true,
                // Radio hiccup: report, burn this dwell idle (avoiding a
                // tight error loop), and let the next turn retry.
                Err(e) => {
                    eprintln!("! advertise failed: {e:#}");
                    false
                }
            };
            record_on_air(&mut on_air, frame, registered);
        }

        // Hold the slot for one dwell, still accepting outgoing frames.
        let dwell = tokio::time::sleep(DWELL);
        tokio::pin!(dwell);
        loop {
            tokio::select! {
                () = &mut dwell => break,
                out = rx.recv() => match out {
                    Some(out) => enqueue(&mut rotation, &out),
                    None => return, // transport dropped
                },
            }
        }
    }
}

fn enqueue(rotation: &mut Rotation, out: &Outgoing) {
    match *out {
        Outgoing::Own(frame) => rotation.enqueue_own(frame),
        Outgoing::Relay(frame) => rotation.enqueue_relay(frame),
    }
}

/// Updates the belief of which frame is on air after a `set_frame` attempt.
///
/// Success records `frame`. Failure clears the belief to `None`,
/// because a failed registration leaves the radio advertising nothing —
/// platform advertisers drop the old advertisement *before* installing
/// the new one (e.g. BlueZ, which grants a single advertising instance).
/// Clearing the belief is what lets [`advertise_loop`]'s "already on air?" guard
/// retry the *same* frame next turn instead of assuming it is still up; leaving
/// a stale belief would strand a node that only ever sends one frame silent.
fn record_on_air(on_air: &mut Option<Frame>, frame: Frame, registered: bool) {
    *on_air = registered.then_some(frame);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_frame() -> Frame {
        // Smallest well-formed frame: 1 flag + 2 seq + 4 src, empty payload.
        Frame::copy_from(&[0, 0, 0, 0, 0, 0, 0]).expect("valid frame")
    }

    #[test]
    fn success_records_frame_on_air() {
        let frame = test_frame();
        let mut on_air = None;
        record_on_air(&mut on_air, frame, true);
        assert_eq!(on_air, Some(frame));
    }

    #[test]
    fn failed_advertise_clears_belief_so_same_frame_retries() {
        let frame = test_frame();
        let mut on_air = None;

        // Turn 1: the "already on air?" guard lets the first attempt
        // through, but the registration fails.
        assert_ne!(on_air, Some(frame));
        record_on_air(&mut on_air, frame, false);
        assert_eq!(on_air, None, "a failed advertise must not leave a stale belief");

        // Turn 2: the same frame is still wanted. Because the belief was
        // cleared, the guard permits a retry rather than assuming the frame
        // is already up — which would leave a single-frame sender silent.
        assert_ne!(on_air, Some(frame), "same frame is retried after a failure");
        record_on_air(&mut on_air, frame, true);
        assert_eq!(on_air, Some(frame));
    }
}
