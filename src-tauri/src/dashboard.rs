//! The settings window, and what it is allowed to say.
//!
//! Unlike the overlay, this window *should* take focus: the user asked for it,
//! and it is not open while anything is being dictated. The parking discipline
//! in `overlay.rs` exists to protect the paste target and does not apply here.
//!
//! **Nothing here invents a number.** DESIGN.md's status screen wants a 7-day
//! median round trip, words dictated today and monthly spend. None of those are
//! recorded anywhere yet — there is no history store — so they are reported as
//! absent and the screen renders a dash. A plausible-looking figure on a status
//! screen is worse than an obvious gap, because it cannot be told from a real
//! one.

use anyhow::{Context, Result};
use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

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
    spend: Option<String>,
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

    Snapshot {
        state,
        // Not measured yet: there is no history store. See the module docs.
        round_trip_ms: None,
        words_today: None,
        dictations_today: None,
        spend: None,
        engines,
        warnings,
    }
}
