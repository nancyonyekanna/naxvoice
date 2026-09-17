//! The dictation key and the session it drives.
//!
//! One key does everything. Hold it and speak; release and the text appears.
//! Double-tap and it latches, so you can pause and keep going for as long as you
//! like; tap once more to stop.
//!
//! The key is *watched*, not registered. A bare modifier cannot be a global
//! hotkey on macOS at all, and watching has a second advantage: the key still
//! works normally in whatever app has focus, so Right Command is still Command.
//!
//! **Why a session rather than a press.** A double-tap begins with a press and
//! release indistinguishable from a short dictation. Waiting to disambiguate
//! would add the latch window to every short dictation — a third of the whole
//! latency budget, on the commonest case. Instead the *paste* is deferred, never
//! the transcription: transcription starts the instant you release, and since it
//! takes far longer than the latch window, the resume decision is already made
//! by the time there is anything to paste. The wait costs nothing.
//!
//! A session therefore spans however many segments you speak, with one stitcher
//! across all of them, and the text lands once at the end.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::audio::recorder::{chunk_to_wav, Chunk, Recorder};
use crate::config::{Config, OfflineBehavior};
use crate::platform::KeyEdge;
use crate::stt::openrouter::AudioFormat;
use crate::stt::stitch::Stitcher;

/// Below this fraction of the speaker's own words surviving, a "cleanup" is not
/// a cleanup. Measured against the real failure: when the model answered the
/// transcript instead of rewriting it, retention was 0.00-0.25, while every
/// genuine rewrite scored 1.00. Half is a wide margin either side.
const MIN_RETENTION: f32 = 0.5;

/// What the session task is told to do.
enum SessionEvent {
    /// Another stretch of speech. Segments never overlap: the previous one's
    /// channel has already closed before this arrives.
    Segment(UnboundedReceiver<Chunk>),
    /// No resume came. Finalize and paste.
    Finish { released: Instant },
}

/// The hold-or-latch state machine.
pub struct Dictation {
    state: Mutex<State>,
    latch_window: Duration,
}

enum State {
    Idle,
    /// Mic open. `latched` means releases are ignored until the next press.
    Recording { id: u64, latched: bool, events: UnboundedSender<SessionEvent> },
    /// Released, waiting to see whether a second tap lands.
    Pending { id: u64, released: Instant, events: UnboundedSender<SessionEvent> },
}

impl Dictation {
    pub fn new(latch_window: Duration) -> Self {
        Self { state: Mutex::new(State::Idle), latch_window }
    }
}

/// Parses a Tauri accelerator such as `CmdOrCtrl+Shift+R`.
///
/// Read-aloud still uses one of these, unlike dictation: it is a discrete press
/// rather than hold-to-talk, so it can be registered normally.
pub fn parse_accelerator(accelerator: &str) -> Result<Shortcut> {
    accelerator
        .parse::<Shortcut>()
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("parsing accelerator {accelerator:?}"))
}

/// Registers the read-aloud key. Pressing it again while speaking stops.
pub fn register_read_aloud<R: Runtime>(
    app: &AppHandle<R>,
    shortcut: Shortcut,
    accelerator: &str,
) -> Result<()> {
    let handle = app.clone();
    app.global_shortcut()
        .on_shortcut(shortcut, move |_, _, event| {
            // Fire on press only: the release edge would toggle straight back.
            if event.state() == ShortcutState::Pressed {
                crate::tts::read_aloud::toggle(&handle);
            }
        })
        .with_context(|| format!("registering read_aloud shortcut {accelerator:?}"))?;

    tracing::info!(accelerator, "read-aloud shortcut registered");
    Ok(())
}

/// Called for every press and release of the dictation key.
///
/// This runs on an OS event thread, so it starts work and returns rather than
/// doing any of it here.
pub fn on_key_edge<R: Runtime>(app: &AppHandle<R>, edge: KeyEdge) {
    if let Err(e) = handle_edge(app, edge) {
        tracing::error!(error = format!("{e:#}"), ?edge, "dictation key handling failed");
    }
}

fn handle_edge<R: Runtime>(app: &AppHandle<R>, edge: KeyEdge) -> Result<()> {
    let dictation = app.state::<Dictation>();
    let mut state = dictation
        .state
        .lock()
        .map_err(|_| anyhow::anyhow!("dictation lock poisoned"))?;

    match (&*state, edge) {
        // First press: open a session and start speaking into it.
        (State::Idle, KeyEdge::Down) => {
            let id = next_session_id();
            let (events_tx, events_rx) = unbounded_channel();

            let task_app = app.clone();
            tauri::async_runtime::spawn(async move {
                run_session(task_app, events_rx).await;
            });

            capture_paste_target(app);
            let chunks = start_recording(app)?;
            events_tx
                .send(SessionEvent::Segment(chunks))
                .map_err(|_| anyhow::anyhow!("session ended before it began"))?;

            tracing::info!(id, "dictating");
            *state = State::Recording { id, latched: false, events: events_tx };
        }

        // Second tap inside the window: reopen the mic and latch.
        (State::Pending { id, events, .. }, KeyEdge::Down) => {
            let (id, events) = (*id, events.clone());
            let chunks = start_recording(app)?;
            events
                .send(SessionEvent::Segment(chunks))
                .map_err(|_| anyhow::anyhow!("session ended before the resume"))?;

            tracing::info!(id, "latched — keep talking, tap again to stop");
            *state = State::Recording { id, latched: true, events };
        }

        // Tap while latched: stop.
        (State::Recording { id, latched: true, events }, KeyEdge::Down) => {
            let (id, events) = (*id, events.clone());
            stop_recording(app);
            let _ = events.send(SessionEvent::Finish { released: Instant::now() });
            tracing::info!(id, "stopped");
            *state = State::Idle;
        }

        // Release of a plain hold: stop the mic, then wait briefly to see
        // whether this was actually the first half of a double-tap.
        (State::Recording { id, latched: false, events }, KeyEdge::Up) => {
            let (id, events) = (*id, events.clone());
            stop_recording(app);
            let released = Instant::now();
            *state = State::Pending { id, released, events };

            let app = app.clone();
            let window = dictation.latch_window;
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(window).await;
                finish_if_still_pending(&app, id);
            });
        }

        // Key repeat while recording, or a release while latched: both are noise.
        _ => {}
    }

    Ok(())
}

/// Ends the session unless a second tap already resumed it.
fn finish_if_still_pending<R: Runtime>(app: &AppHandle<R>, id: u64) {
    let dictation = app.state::<Dictation>();
    let Ok(mut state) = dictation.state.lock() else { return };

    if let State::Pending { id: pending, released, events } = &*state {
        if *pending == id {
            let _ = events.send(SessionEvent::Finish { released: *released });
            *state = State::Idle;
        }
    }
}

fn next_session_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Decide where the text will go before any of it is spoken. By the time a
/// transcript exists the user may be somewhere else entirely.
fn capture_paste_target<R: Runtime>(app: &AppHandle<R>) {
    let platform = app.state::<crate::Platforms>();
    match platform.0.capture_focus() {
        Ok(target) => {
            if let Ok(mut slot) = app.state::<crate::PasteTarget>().0.lock() {
                *slot = Some(target);
            }
            let name = platform.0.focused_app().unwrap_or_else(|_| "unknown".into());
            tracing::debug!(target = name, "will paste back into");
        }
        Err(e) => tracing::warn!(error = format!("{e:#}"), "could not capture a paste target"),
    }
}

fn start_recording<R: Runtime>(app: &AppHandle<R>) -> Result<UnboundedReceiver<Chunk>> {
    app.state::<Recorder>().start().context("starting the recorder")
}

fn stop_recording<R: Runtime>(app: &AppHandle<R>) {
    match app.state::<Recorder>().stop() {
        Ok(summary) => tracing::debug!(
            chunks = summary.chunks,
            seconds = format!("{:.2}", summary.duration.as_secs_f64()),
            "segment captured"
        ),
        Err(e) => tracing::warn!(error = format!("{e:#}"), "could not finish a segment"),
    }
}

/// One dictation session: every segment, transcribed, reassembled, polished once
/// and pasted once.
async fn run_session<R: Runtime>(app: AppHandle<R>, mut events: UnboundedReceiver<SessionEvent>) {
    let (chunk_tx, mut chunk_rx) = unbounded_channel::<Chunk>();

    // Segments never overlap — a release closes one channel before the resume
    // opens the next — so draining them in turn preserves spoken order without
    // any interleaving machinery.
    let pump = tauri::async_runtime::spawn(async move {
        let mut released = None;
        while let Some(event) = events.recv().await {
            match event {
                SessionEvent::Segment(mut chunks) => {
                    while let Some(chunk) = chunks.recv().await {
                        if chunk_tx.send(chunk).is_err() {
                            break;
                        }
                    }
                }
                SessionEvent::Finish { released: at } => {
                    released = Some(at);
                    break;
                }
            }
        }
        released
    });

    let (results_tx, mut results_rx) = unbounded_channel::<(usize, Result<String>)>();
    let client = app.state::<crate::Stt>().0.as_ref().cloned();
    let bias = bias_terms(&app);
    let mut dispatched = 0usize;

    // Dispatch each chunk the moment it closes; these overlap with speech and
    // are effectively free.
    while let Some(chunk) = chunk_rx.recv().await {
        let index = dispatched;
        dispatched += 1;

        let Some(client) = client.clone() else { continue };
        let bias = bias.clone();
        let tx = results_tx.clone();

        tauri::async_runtime::spawn(async move {
            let sent = Instant::now();
            let outcome = match chunk_to_wav(&chunk.samples) {
                Ok(wav) => client.transcribe(wav, AudioFormat::Wav, &bias).await.map(|r| r.text),
                Err(e) => Err(e),
            };
            tracing::debug!(index, ms = sent.elapsed().as_millis(), "chunk transcribed");
            let _ = tx.send((index, outcome));
        });
    }
    drop(results_tx);

    let released = pump.await.ok().flatten().unwrap_or_else(Instant::now);

    if client.is_none() {
        tracing::error!(
            "{} is not set, so there is nothing to transcribe with",
            crate::config::API_KEY_ENV
        );
        return;
    }

    let mut stitcher = Stitcher::new();
    let mut failed = 0usize;
    while let Some((index, outcome)) = results_rx.recv().await {
        match outcome {
            Ok(text) => {
                stitcher.resolve(index, text);
            }
            Err(e) => {
                // Resolve as empty rather than skipping: the stitcher only emits
                // contiguous runs, so a hole would truncate everything after it.
                failed += 1;
                tracing::warn!(index, error = format!("{e:#}"), "chunk failed, leaving a gap");
                stitcher.resolve(index, String::new());
            }
        }
    }

    let transcript = stitcher.transcript().trim().to_string();
    tracing::info!(
        tail_ms = released.elapsed().as_millis(),
        chunks = dispatched,
        failed,
        chars = transcript.len(),
        "transcribed"
    );

    if transcript.is_empty() {
        tracing::warn!("nothing was transcribed, so there is nothing to paste");
        return;
    }

    let text = match finish_text(&app, &transcript).await {
        Ok(text) => text,
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "dictation failed");
            return;
        }
    };

    if let Err(e) = paste(&app, &text) {
        tracing::error!(error = format!("{e:#}"), "dictation failed");
        return;
    }

    // The number CLAUDE.md's budget is written against: release to text on
    // screen. Anything over ~800ms median means something regressed.
    tracing::info!(ms = released.elapsed().as_millis(), chars = text.len(), "release to pasted");
}

fn bias_terms<R: Runtime>(app: &AppHandle<R>) -> Vec<String> {
    let config = app.state::<Config>();
    config
        .dictionary
        .terms
        .iter()
        .take(config.dictionary.prompt_budget)
        .map(|t| t.term.clone())
        .collect()
}

/// Applies cleanup unless it is switched off in config.
async fn finish_text<R: Runtime>(app: &AppHandle<R>, transcript: &str) -> Result<String> {
    let enabled = app.state::<Config>().cleanup.enabled;
    if !enabled {
        tracing::debug!("cleanup is disabled in config, pasting the raw transcript");
        return Ok(transcript.to_string());
    }
    polish_transcript(app, transcript).await
}

/// Rewrites the transcript into what the speaker would have typed.
async fn polish_transcript<R: Runtime>(app: &AppHandle<R>, transcript: &str) -> Result<String> {
    // Cloned out before the await, so no state guard is held across it.
    let (profile, dictionary, offline) = {
        let config = app.state::<Config>();
        (
            config.default_profile()?.clone(),
            config.dictionary.terms(),
            config.cleanup.offline_behavior,
        )
    };

    let Some(client) = app.state::<crate::Cleanup>().0.as_ref().cloned() else {
        tracing::warn!("cleanup is unavailable, pasting the raw transcript");
        return Ok(transcript.to_string());
    };

    let started = Instant::now();
    match client.polish(transcript, &profile, &dictionary).await {
        Ok(polished) => {
            let polished = polished.trim().to_string();
            if polished.is_empty() {
                tracing::warn!("cleanup returned nothing, keeping the raw transcript");
                return Ok(transcript.to_string());
            }

            // The model sometimes answers the transcript rather than rewriting
            // it — "are you working properly" came back as "I'm ready to help"
            // and was pasted as if the speaker had said it. The prompt forbids
            // that now, but a prompt is a probability and this is a fact.
            let kept = kept_ratio(transcript, &polished);
            if kept < MIN_RETENTION {
                tracing::warn!(
                    kept = format!("{kept:.2}"),
                    "cleanup discarded most of what was said, keeping the raw transcript"
                );
                return Ok(transcript.to_string());
            }

            tracing::info!(
                ms = started.elapsed().as_millis(),
                before = transcript.len(),
                after = polished.len(),
                kept = format!("{kept:.2}"),
                "polished"
            );
            Ok(polished)
        }
        Err(e) => {
            tracing::warn!(error = format!("{e:#}"), "cleanup failed");
            on_cleanup_failure(offline, transcript)
        }
    }
}

/// The fraction of the speaker's own words that survive into the rewrite.
///
/// A genuine cleanup keeps nearly all of them — it fixes grammar and
/// punctuation, it does not replace the content. Text the model invented keeps
/// almost none.
fn kept_ratio(spoken: &str, rewritten: &str) -> f32 {
    let spoken_words = word_set(spoken);
    if spoken_words.is_empty() {
        return 1.0;
    }
    let rewritten_words = word_set(rewritten);
    let kept = spoken_words.intersection(&rewritten_words).count();
    kept as f32 / spoken_words.len() as f32
}

fn word_set(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(|c| c.to_lowercase())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// What to paste when cleanup fails.
fn on_cleanup_failure(behavior: OfflineBehavior, transcript: &str) -> Result<String> {
    match behavior {
        OfflineBehavior::PasteRaw => Ok(transcript.to_string()),
        OfflineBehavior::Queue => bail!(
            "offline_behavior is set to queue, which is not implemented yet, so this \
             dictation was not pasted. Set offline_behavior: paste_raw in config.yaml. \
             Transcript: {transcript}"
        ),
    }
}

/// Puts the captured app back in front and sends the paste chord.
fn paste<R: Runtime>(app: &AppHandle<R>, text: &str) -> Result<()> {
    let platform = app.state::<crate::Platforms>();

    let captured = app
        .state::<crate::PasteTarget>()
        .0
        .lock()
        .ok()
        .and_then(|slot| *slot);

    match captured {
        Some(target) => match platform.0.restore_focus(target) {
            Ok(true) => {}
            Ok(false) => bail!(
                "the app you dictated into can no longer receive text, so the \
                 transcript was not pasted: {text}"
            ),
            Err(e) => tracing::warn!(error = format!("{e:#}"), "could not restore focus"),
        },
        None => tracing::warn!("no paste target was captured; pasting wherever focus is"),
    }

    platform.0.paste_at_cursor(text)?;
    tracing::info!(chars = text.len(), "pasted");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::platform::DictateKey;

    const EXAMPLE: &str = include_str!("../../config.example.yaml");

    #[test]
    fn the_example_config_names_a_usable_dictation_key() {
        let h = Config::from_yaml(EXAMPLE).unwrap().hotkeys;
        assert_eq!(h.dictate().unwrap(), DictateKey::RightCommand);
        // read_aloud and stop are still accelerators, for steps 6 and 7.
        assert!(!h.read_aloud.is_empty());
    }

    #[test]
    fn paste_raw_keeps_the_dictation_when_cleanup_fails() {
        let out = on_cleanup_failure(OfflineBehavior::PasteRaw, "hello there").unwrap();
        assert_eq!(out, "hello there");
    }

    #[test]
    fn queue_is_refused_but_names_the_transcript() {
        let err = on_cleanup_failure(OfflineBehavior::Queue, "hello there").unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("hello there"));
        assert!(message.contains("paste_raw"));
    }

    #[test]
    fn an_empty_chunk_does_not_truncate_the_ones_after_it() {
        let mut s = Stitcher::new();
        s.resolve(0, "first part".into());
        s.resolve(1, String::new());
        s.resolve(2, "third part".into());
        let out = s.transcript();
        assert!(out.contains("first part"), "got {out:?}");
        assert!(out.contains("third part"), "got {out:?}");
    }

    /// The real observed failure: the model answered instead of rewriting.
    #[test]
    fn an_invented_answer_scores_far_below_the_threshold() {
        let spoken = "are you working properly";
        let invented = "I'm ready to help. Please provide the dictated speech \
                        you'd like me to rewrite into text.";
        let kept = kept_ratio(spoken, invented);
        assert!(kept < MIN_RETENTION, "kept {kept:.2}, should have been rejected");
    }

    #[test]
    fn a_genuine_rewrite_keeps_the_speakers_words() {
        let spoken = "its working perfectly now everything is fine";
        let rewritten = "It's working perfectly now. Everything is fine.";
        let kept = kept_ratio(spoken, rewritten);
        assert!(kept >= MIN_RETENTION, "kept {kept:.2}, should have been accepted");
    }

    /// Punctuation and capitalisation must not count as lost words, or every
    /// real cleanup would trip the guard.
    #[test]
    fn punctuation_and_case_do_not_count_as_loss() {
        assert_eq!(kept_ratio("hello there world", "Hello, there — world!"), 1.0);
    }

    #[test]
    fn an_empty_transcript_is_not_treated_as_a_loss() {
        assert_eq!(kept_ratio("", "anything"), 1.0);
    }
}
