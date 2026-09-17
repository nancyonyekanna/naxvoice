//! Audio capture, chunk boundaries and playback.
//!
//! `player.rs` arrives with step 6. `vad.rs` is written but not yet wired in —
//! `recorder.rs` already emits the 16kHz mono i16 it expects, so step 5 can feed
//! one into the other without either side changing.

pub mod recorder;
pub mod vad;
