//! Read-aloud: take the selection, speak it.
//!
//! The pipeline is selection → normalize → split into prosody units →
//! synthesise → play, and the ordering matters. Normalisation runs before
//! splitting because it expands abbreviations, and "Dr." would otherwise look
//! like the end of a sentence to the splitter.
//!
//! Units are handed to the player **as each one finishes**, never in a batch.
//! Synthesis costs roughly 1480ms per spoken second — measured, and flat across
//! sentence lengths — so a thirty second passage rendered up front would mean
//! forty-five seconds of silence before the first word. Rendering one unit at a
//! time gets audio started after the first clause instead.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use tauri::{AppHandle, Manager, Runtime};

use crate::audio::player::Player;
use crate::config::Config;
use crate::tts::normalize::Normalizer;
use crate::tts::{chunk, AudioSegment, Synthesizer};

/// Everything read-aloud needs, held in managed state.
pub struct ReadAloud {
    pub engine: Arc<dyn Synthesizer>,
    pub player: Arc<Player>,
}

/// Handles a press of the read-aloud key.
///
/// A second press while speaking stops, which is why `stop` in config.yaml is
/// left unregistered: binding Escape globally would swallow it from every other
/// application, and a key that breaks Escape everywhere is worse than no key.
pub fn toggle<R: Runtime>(app: &AppHandle<R>) {
    let state = app.state::<ReadAloud>();

    if state.player.is_playing() {
        state.player.stop();
        crate::overlay::hide(app);
        tracing::info!("stopped reading");
        return;
    }

    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = read_selection_aloud(&app).await {
            tracing::error!(error = format!("{e:#}"), "read aloud failed");
        }
    });
}

async fn read_selection_aloud<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let selection = {
        let platform = app.state::<crate::Platforms>();
        platform
            .0
            .read_selection()
            .context("reading the selection")?
    };

    let Some(text) = selection else {
        tracing::info!("nothing is selected");
        return Ok(());
    };

    let text = text.trim().to_string();
    if text.is_empty() {
        tracing::info!("the selection is empty");
        return Ok(());
    }

    // Normalise before splitting: expanding "Dr." to "Doctor" first is what
    // stops the splitter treating that full stop as a sentence boundary.
    let spoken = {
        let config = app.state::<Config>();
        let normalizer = Normalizer::new(
            &config.pronunciation_rules(),
            config.tts.skip_code_blocks,
            config.tts.skip_urls,
            config.tts.expand_numbers,
        )?;
        normalizer.run(&text)
    };

    let units = chunk::split(&spoken);
    if units.is_empty() {
        bail!("nothing left to read after normalisation");
    }

    let (engine, player, voice) = {
        let state = app.state::<ReadAloud>();
        let config = app.state::<Config>();
        (
            Arc::clone(&state.engine),
            Arc::clone(&state.player),
            config.tts.first_voice()?,
        )
    };

    tracing::info!(
        chars = text.len(),
        units = units.len(),
        voice = %voice.voice,
        "reading aloud"
    );

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AudioSegment>();

    // The player owns a cpal stream, which must not cross threads, so it runs
    // on a blocking thread of its own and receives segments through a channel.
    let playing = player.clone();
    let playback = tauri::async_runtime::spawn_blocking(move || {
        if let Err(e) = playing.play(rx) {
            tracing::error!(error = format!("{e:#}"), "playback failed");
        }
    });

    crate::overlay::show(app, crate::overlay::Status::Reading);

    let started = std::time::Instant::now();
    let mut first_audio: Option<std::time::Duration> = None;

    for (index, unit) in units.iter().enumerate() {
        // A stop mid-passage must not keep rendering the rest.
        if index > 0 && !player.is_playing() {
            tracing::debug!(index, "stopped, abandoning the remaining units");
            break;
        }

        match engine.synthesize(&unit.text, &voice).await {
            Ok(segment) => {
                if first_audio.is_none() {
                    first_audio = Some(started.elapsed());
                }
                if tx.send(AudioSegment { index, ..segment }).is_err() {
                    break;
                }
            }
            // One bad unit must not end the read: the player treats a missing
            // index as silence and carries on.
            Err(e) => tracing::warn!(index, error = format!("{e:#}"), "unit failed, skipping"),
        }
    }
    drop(tx);

    if let Some(first) = first_audio {
        tracing::info!(
            ms = first.as_millis(),
            "first audio"
        );
    }

    let _ = playback.await;
    crate::overlay::hide(app);
    tracing::info!(ms = started.elapsed().as_millis(), "finished reading");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Three levels: this file is in src/tts/, and the config lives at the repo
    // root. hotkeys.rs uses ../../ because it sits one directory shallower.
    const EXAMPLE: &str = include_str!("../../../config.example.yaml");

    /// Normalisation has to run before splitting, or an abbreviation's full
    /// stop is read as the end of a sentence and the read breaks mid-phrase.
    #[test]
    fn abbreviations_are_expanded_before_sentences_are_split() {
        let config = Config::from_yaml(EXAMPLE).unwrap();
        let normalizer = Normalizer::new(
            &config.pronunciation_rules(),
            config.tts.skip_code_blocks,
            config.tts.skip_urls,
            false,
        )
        .unwrap();

        let raw = "Ask Dr. Ada about the deploy. She signed it off.";
        let naive = chunk::split(raw);
        let ordered = chunk::split(&normalizer.run(raw));

        assert!(
            ordered.len() <= naive.len(),
            "normalising first should not create more units: {} vs {}",
            ordered.len(),
            naive.len()
        );
        assert!(
            ordered.iter().all(|u| !u.text.contains("Dr.")),
            "the abbreviation survived: {ordered:?}"
        );
    }

    #[test]
    fn a_selection_of_only_whitespace_produces_nothing_to_read() {
        assert!(chunk::split("   \n  ").is_empty());
    }
}
