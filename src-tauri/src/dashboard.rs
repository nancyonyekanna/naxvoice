//! The settings window, and what it is allowed to say.
//!
//! Unlike the overlay, this window *should* take focus: the user asked for it,
//! and it is not open while anything is being dictated. The parking discipline
//! in `overlay.rs` exists to protect the paste target and does not apply here.
//!
//! **Nothing here invents a number.** Every figure comes from a store on disk:
//! the round trip and the word count from `history.rs`, and what has been spent
//! from `spend.rs`. Where a store has nothing to say, the screen says so. A
//! plausible-looking figure on a status screen is worse than an obvious gap,
//! because it cannot be told apart from a real one.
//!
//! Spend is this app's own, never the account's. One OpenRouter key is usually
//! shared with other tools, so the account-wide total would put spending on
//! naxvoice's dashboard that naxvoice never did.

use anyhow::{Context, Result};
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

use crate::config::Config;
use crate::secrets;

pub const LABEL: &str = "main";

/// Roughly 900x700, per DESIGN.md.
const WIDTH: f64 = 900.0;
const HEIGHT: f64 = 700.0;

#[derive(Serialize)]
pub struct Engine {
    name: String,
    /// "ok", "warn" or "bad". Never the only carrier of meaning — `state` says
    /// the same thing in words, which is the rule in DESIGN.md.
    dot: &'static str,
    state: String,
}

#[derive(Serialize)]
pub struct Snapshot {
    state: &'static str,
    round_trip_ms: Option<u64>,
    words_today: Option<u64>,
    dictations_today: Option<u64>,
    spend: crate::spend::Totals,
    engines: Vec<Engine>,
    warnings: Vec<String>,
}

/// Opens the settings window, building it the first time.
pub fn open<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    if let Some(window) = app.get_webview_window(LABEL) {
        window.show().ok();
        window.set_focus().ok();
        tracing::info!("settings window raised");
        return Ok(());
    }

    WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("naxvoice")
        .inner_size(WIDTH, HEIGHT)
        .min_inner_size(640.0, 480.0)
        .build()
        .context("building the settings window")?;

    // Logged because returning Ok silently is indistinguishable from never
    // having run, and the window cannot be inspected from outside: this machine
    // grants osascript no assistive access.
    tracing::info!(w = WIDTH, h = HEIGHT, "settings window open");
    Ok(())
}

/// The config as loaded at startup.
///
/// This is the in-memory copy, not a re-read of the file, so it matches what
/// the app is actually running with rather than what is currently on disk.
#[tauri::command]
pub fn get_config(app: AppHandle) -> Config {
    app.state::<Config>().inner().clone()
}

/// Writes the config to disk.
///
/// It does **not** change the running app: `Config` is managed state built once
/// during setup, and swapping it under the recorder, the shortcut watcher and
/// the synthesiser mid-flight would be a much larger change than writing a
/// file. The dashboard says so rather than implying a live reload happened.
#[tauri::command]
pub fn save_config(config: Config) -> Result<(), String> {
    config.save().map_err(|e| format!("{e:#}"))
}

/// The most recent dictations, newest first.
#[tauri::command]
pub fn history_recent(limit: usize) -> Vec<crate::history::Record> {
    crate::history::recent(limit.clamp(1, 500))
}

#[tauri::command]
pub fn clear_history() -> Result<(), String> {
    crate::history::clear().map_err(|e| format!("{e:#}"))
}

/// Wipes the spending ledger. Separate from `clear_history` on purpose: erasing
/// what you said and erasing what you paid are different intentions, and one
/// button doing both would destroy a record the user meant to keep.
#[tauri::command]
pub fn clear_spend() -> Result<(), String> {
    crate::spend::clear().map_err(|e| format!("{e:#}"))
}

/// Runs a sample transcript through a profile, without saving anything.
///
/// DESIGN.md calls the preview "the only way to tell whether a prompt edit did
/// what you wanted without leaving the screen", which means it has to use the
/// real cleanup path rather than an approximation of it.
///
/// The profile arrives from the editor rather than from config, so what is
/// previewed is what is on screen, including unsaved edits. Its `pattern` is
/// skipped by serde and defaults to empty, which is correct here: a preview
/// belongs to no particular app.
#[tauri::command]
pub async fn preview_cleanup(
    app: AppHandle,
    transcript: String,
    profile: crate::cleanup::Profile,
) -> Result<String, String> {
    // Both guards are dropped at the end of their statements, before the await.
    // Holding managed state across an await would make this command non-Send.
    let client = app.state::<crate::Cleanup>().0.clone();
    let dictionary = app.state::<Config>().dictionary.terms();

    let client = client.ok_or_else(|| {
        "No OpenRouter key is configured, so there is nothing to preview with.".to_string()
    })?;

    client
        .polish(&transcript, &profile, &dictionary)
        .await
        .map_err(|e| format!("{e:#}"))
}

/// Whether a key is installed, and where it came from. Never the key itself.
#[tauri::command]
pub fn api_key_status() -> secrets::KeyStatus {
    secrets::status()
}

#[tauri::command]
pub fn set_api_key(key: String) -> Result<secrets::KeyStatus, String> {
    secrets::set(&key).map_err(|e| format!("{e:#}"))?;
    Ok(secrets::status())
}

#[tauri::command]
pub fn clear_api_key() -> Result<secrets::KeyStatus, String> {
    secrets::clear().map_err(|e| format!("{e:#}"))?;
    Ok(secrets::status())
}

#[tauri::command]
pub fn status_snapshot(app: AppHandle) -> Snapshot {
    let config = app.state::<crate::config::Config>();
    let platform = app.state::<crate::Platforms>();

    let has_key = app.state::<crate::Stt>().0.is_some();
    let cleanup_ready = app.state::<crate::Cleanup>().0.is_some() && config.cleanup.enabled;
    let kokoro_loaded = app.state::<crate::tts::read_aloud::ReadAloud>().engine.is_loaded();

    let engines = vec![
        Engine {
            name: format!("Transcription · {}", config.transcription.model),
            dot: if has_key { "ok" } else { "bad" },
            state: if has_key {
                "cloud · ready".into()
            } else {
                "no API key".into()
            },
        },
        Engine {
            name: format!("Cleanup · {}", config.cleanup.model),
            dot: if cleanup_ready { "ok" } else { "warn" },
            state: if !has_key {
                "no API key".into()
            } else if !config.cleanup.enabled {
                "disabled in config".into()
            } else {
                "cloud · ready".into()
            },
        },
        Engine {
            name: "Kokoro".into(),
            dot: if kokoro_loaded { "ok" } else { "warn" },
            state: if kokoro_loaded {
                "local · loaded".into()
            } else {
                "local · loads on first use".into()
            },
        },
    ];

    // The same checks main.rs logs at launch. A permission that is missing is
    // the single most likely reason the app appears to do nothing at all, so it
    // belongs on the screen rather than only in a terminal nobody is reading.
    let mut warnings = Vec::new();
    if !platform.0.has_key_watch_permission() {
        warnings.push(
            "Input Monitoring is not granted, so the dictation key will never fire. \
             Grant it in System Settings, then relaunch."
                .into(),
        );
    }
    if !platform.0.has_input_permission() {
        warnings.push(
            "Accessibility is not granted, so the paste will be silently dropped. \
             Grant it in System Settings, then relaunch."
                .into(),
        );
    }
    if !has_key {
        warnings.push(format!(
            "{} is not set, so dictation will record but not transcribe.",
            crate::config::API_KEY_ENV
        ));
    }

    let state = if !warnings.is_empty() {
        "degraded"
    } else {
        "ready"
    };

    // Real figures now, from the history store — but only if it is being kept.
    // With recording off these stay absent and the screen shows dashes, which
    // is the truth rather than zeroes that look like "you dictated nothing".
    let stats = if app.state::<Config>().history.keep {
        crate::history::stats()
    } else {
        crate::history::Stats::default()
    };

    Snapshot {
        state,
        round_trip_ms: stats.median_ms,
        words_today: (stats.dictations > 0).then_some(stats.words),
        dictations_today: (stats.dictations > 0).then_some(stats.dictations),
        // Not gated on history.keep: the ledger is a separate file, so a user
        // who keeps no transcripts still gets an honest account of the cost.
        spend: crate::spend::totals(),
        engines,
        warnings,
    }
}
