// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Morse-code text composition for poholos.
//!
//! A thin, `no_std` wrapper over [`morse_codec`] that turns the two-button
//! input scheme used by the firmware — **dot**, **dash**, and pause-driven
//! letter/word boundaries — into decoded text, capped at a fixed length.
//!
//! The firmware feeds discrete [`Symbol`]s (it already classifies a press as
//! dot or dash) and pause events; this crate keeps the running message and
//! exposes it as a `&str`. All timing decisions (how long a pause counts as a
//! letter or word gap) live in the caller, so the composer itself is pure and
//! host-testable.
//!
//! ```
//! use poholos_morse::{Composer, Symbol::{Dot, Dash}};
//!
//! let mut c = Composer::<15>::new();
//! for s in [Dot, Dot, Dot] { c.symbol(s); }   // S
//! c.letter_gap();
//! for s in [Dash, Dash, Dash] { c.symbol(s); } // O
//! c.letter_gap();
//! for s in [Dot, Dot, Dot] { c.symbol(s); }   // S
//! c.letter_gap();
//! assert_eq!(c.message(), "SOS");
//! ```

#![cfg_attr(not(test), no_std)]

use morse_codec::MorseSignal;
use morse_codec::decoder::{Decoder, MorseDecoder};

/// A single morse element entered by the operator.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Symbol {
    /// A `dit` (`.`).
    Dot,
    /// A `dah` (`-`).
    Dash,
}

/// Accumulates decoded text from dot/dash input and pause boundaries.
///
/// `MAX` is the maximum number of decoded characters kept — set it to the
/// transport payload budget (e.g. 15 bytes for a poholos hearsay). Input
/// beyond `MAX` is ignored rather than wrapping or corrupting the message.
pub struct Composer<const MAX: usize> {
    decoder: MorseDecoder<MAX>,
    /// At least one symbol has been entered since the last committed letter.
    letter_in_progress: bool,
    /// A word space is already pending, so repeated word gaps don't stack.
    space_pending: bool,
}

impl<const MAX: usize> Composer<MAX> {
    /// Creates an empty composer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            // Clamp at the end rather than wrapping if the message ever fills;
            // the `is_full` guards below make this defensive, not load-bearing.
            decoder: Decoder::<MAX>::new().with_message_pos_clamping().build(),
            letter_in_progress: false,
            space_pending: false,
        }
    }

    /// Adds a dot or dash to the letter currently being entered.
    ///
    /// Ignored once the message is full.
    pub fn symbol(&mut self, symbol: Symbol) {
        if self.is_full() {
            return;
        }
        let signal = match symbol {
            Symbol::Dot => MorseSignal::Short,
            Symbol::Dash => MorseSignal::Long,
        };
        self.decoder.add_signal_to_character(Some(signal));
        self.letter_in_progress = true;
        self.space_pending = false;
    }

    /// Commits the in-progress letter — call after a letter-length pause.
    ///
    /// A no-op if no symbols have been entered since the last commit.
    pub fn letter_gap(&mut self) {
        if self.letter_in_progress {
            self.decoder.add_current_char_to_message();
            self.letter_in_progress = false;
        }
    }

    /// Inserts a single word space — call after a word-length pause.
    ///
    /// Commits any in-progress letter first. Never produces a leading space,
    /// a doubled space, or a trailing space past the cap.
    pub fn word_gap(&mut self) {
        self.letter_gap();
        if !self.space_pending && !self.decoder.message.is_empty() && !self.is_full() {
            // An empty character commit is encoded as a space by morse-codec.
            self.decoder.add_current_char_to_message();
            self.space_pending = true;
        }
    }

    /// The decoded text entered so far.
    #[must_use]
    pub fn message(&self) -> &str {
        self.decoder.message.as_str()
    }

    /// Returns `true` if nothing has been entered yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.decoder.message.is_empty() && !self.letter_in_progress
    }

    /// Discards the whole message and resets state.
    pub fn clear(&mut self) {
        self.decoder.message.clear();
        self.letter_in_progress = false;
        self.space_pending = false;
    }

    /// `true` once `MAX` characters have been committed.
    fn is_full(&self) -> bool {
        self.decoder.message.len() >= MAX
    }
}

impl<const MAX: usize> Default for Composer<MAX> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const MAX: usize> core::fmt::Debug for Composer<MAX> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Composer")
            .field("message", &self.message())
            .field("letter_in_progress", &self.letter_in_progress)
            .field("space_pending", &self.space_pending)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::Symbol::{Dash, Dot};
    use super::{Composer, Symbol};

    /// Enters a letter from its symbols and commits it with a letter gap.
    fn letter<const MAX: usize>(composer: &mut Composer<MAX>, symbols: &[Symbol]) {
        for &symbol in symbols {
            composer.symbol(symbol);
        }
        composer.letter_gap();
    }

    #[test]
    fn decodes_sos() {
        let mut c = Composer::<15>::new();
        letter(&mut c, &[Dot, Dot, Dot]);
        letter(&mut c, &[Dash, Dash, Dash]);
        letter(&mut c, &[Dot, Dot, Dot]);
        assert_eq!(c.message(), "SOS");
    }

    #[test]
    fn single_symbol_letters() {
        let mut c = Composer::<15>::new();
        letter(&mut c, &[Dot, Dash]); // A
        assert_eq!(c.message(), "A");
    }

    #[test]
    fn word_gap_inserts_one_space_even_if_repeated() {
        let mut c = Composer::<15>::new();
        letter(&mut c, &[Dot, Dot, Dot]);
        letter(&mut c, &[Dash, Dash, Dash]);
        letter(&mut c, &[Dot, Dot, Dot]);
        c.word_gap();
        c.word_gap(); // a longer pause must not stack a second space
        letter(&mut c, &[Dot, Dot, Dot]);
        letter(&mut c, &[Dash, Dash, Dash]);
        letter(&mut c, &[Dot, Dot, Dot]);
        assert_eq!(c.message(), "SOS SOS");
    }

    #[test]
    fn leading_word_gap_inserts_no_space() {
        let mut c = Composer::<15>::new();
        c.word_gap(); // before any letter
        letter(&mut c, &[Dot, Dash]); // A
        assert_eq!(c.message(), "A");
    }

    #[test]
    fn clear_resets_everything() {
        let mut c = Composer::<15>::new();
        letter(&mut c, &[Dot, Dash]);
        assert!(!c.is_empty());
        c.clear();
        assert!(c.is_empty());
        assert_eq!(c.message(), "");
    }

    #[test]
    fn input_past_the_cap_is_ignored() {
        let mut c = Composer::<3>::new();
        letter(&mut c, &[Dot, Dot, Dot]); // S
        letter(&mut c, &[Dash, Dash, Dash]); // O
        letter(&mut c, &[Dot, Dot, Dot]); // S -> "SOS", now full
        letter(&mut c, &[Dot, Dash]); // A -> must be refused
        assert_eq!(c.message(), "SOS");
        assert_eq!(c.message().len(), 3);
    }
}
