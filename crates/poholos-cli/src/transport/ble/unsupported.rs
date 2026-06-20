// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Fallback advertiser for targets without a BLE implementation.
//!
//! Keeps the crate compiling everywhere (M-OOBE HAL pattern); attempting
//! to use BLE on such a target fails at startup with a clear message.
//! The UDP transport remains fully functional.

use anyhow::{Result, bail};
use poholos::Frame;

/// No BLE here, so no frame can go on air; the value is never read in
/// practice because the advertiser cannot be constructed.
pub const MAX_FRAME: usize = 0;

/// Placeholder advertiser that refuses to construct.
#[derive(Debug)]
pub struct Advertiser {
    // Uninhabited: this type can never actually exist.
    never: std::convert::Infallible,
}

impl Advertiser {
    /// Always fails: BLE advertising is not implemented for this OS.
    ///
    /// # Errors
    /// Always.
    pub async fn new() -> Result<Self> {
        bail!("BLE advertising is not implemented for this OS; use --transport udp");
    }

    /// Unreachable; `Self` cannot be constructed.
    pub async fn set_frame(&mut self, _frame: &Frame) -> Result<()> {
        match self.never {}
    }
}
