//! The floating status widget.
//!
//! Dictation is otherwise silent: you hold a key, nothing visible happens for a
//! couple of seconds, and then text appears. This says which of the three slow
//! things is currently happening.
//!
//! **It must never take focus.** The paste target is captured when the key goes
//! down and restored when the text is ready, so a window that takes key status
//! in between would send the paste somewhere else — the failure DESIGN.md calls
//! out as ending the whole interaction.
//!
//! Getting there took four measurements and three wrong answers. Building the
//! window `focusable(false)` does not do it. Nor does dropping the app to
//! accessory activation. Nor does adding `focused(false)` on top. With all
//! three in place the frontmost application still moved to naxvoice on every
//! appearance, because tao's `set_visible(true)` calls `makeKeyAndOrderFront`,
//! and that activates the application regardless of what the window permits.
//!
//! What works is never calling `show()`: the window is created visible and
//! parked off-screen, and appearing is a move. The three flags above are kept —
//! they are cheap and right in intent — but `PARK` is what does the work.
//! `self_test` measures it, because every one of the wrong answers looked
//! correct when reasoned about from the source.

use anyhow::{Context, Result};
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Listener, LogicalPosition, LogicalSize, Manager, Runtime, WebviewUrl,
    WebviewWindowBuilder,
};

pub const LABEL: &str = "overlay";

/// Roughly 200x40, per DESIGN.md. Logical pixels, so it keeps its size on a
/// Retina display rather than rendering at half the intended size.
const WIDTH: f64 = 200.0;
const HEIGHT: f64 = 40.0;

/// Enough clearance that the widget sits beside the pointer rather than under
/// it, where it would swallow the next click.
const CURSOR_GAP: f64 = 18.0;

/// Where the widget waits when it is not wanted.
///
/// It is parked rather than hidden, because hiding and showing is what steals
/// focus. `show()` goes through tao's `set_visible(true)`, which calls
/// `makeKeyAndOrderFront` — and that activates the application. Measured three
/// times: with the activation policy confirmed Accessory, with the window built
/// `focusable(false)`, and with `focused(false)` as well, the frontmost app
/// still moved to naxvoice every time. Moving a window that is already visible
/// does not go through that path at all.
const PARK: (f64, f64) = (-10_000.0, -10_000.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Listening,
    Polishing,
    Reading,
}

#[derive(Clone, Serialize)]
struct Payload {
    state: Status,
    chunks: usize,
    total_ms: u64,
    /// Whether the page should restart its clock. A chunk count arriving during
    /// a dictation must not reset the elapsed time the user is watching.
    restart: bool,
}

/// Builds the window once, hidden, at launch.
///
/// Built eagerly rather than on the first dictation so the webview has already
/// loaded and subscribed by the time it is first shown — creating it on demand
/// would mean the first press showed an empty box while the page booted.
pub fn create<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("overlay.html".into()))
        .title("naxvoice")
        .inner_size(WIDTH, HEIGHT)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        // The point of the thing is to be visible while you work in another
        // app, which includes another desktop.
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .shadow(false)
        // Two different settings, and both matter. `focusable` decides whether
        // the window may ever take key status; `focused` decides whether it
        // asks for it. tao defaults the latter to true, so setting only the
        // first leaves the window still requesting focus when it appears.
        .focusable(false)
        .focused(false)
        // Created visible, but off-screen: see PARK. The one activation this
        // costs happens at launch, before anyone is dictating, rather than on
        // every press of the key.
        .visible(true)
        .position(PARK.0, PARK.1)
        .build()
        .context("building the overlay window")?;

    // The page reports what it drew. Since the widget is usually parked
    // off-screen and screenshots need a permission that may not be granted,
    // this is the only evidence that it loaded, received the event, and
    // rendered the right text.
    window.listen("overlay:rendered", |event| {
        tracing::debug!(drew = event.payload(), "overlay rendered");
    });

    Ok(())
}

/// Shows the widget in `status`, moved to wherever the pointer is now.
///
/// Failures are logged and swallowed: a status widget that cannot appear is a
/// cosmetic problem, and letting it abort a dictation would turn it into a real
/// one.
pub fn show<R: Runtime>(app: &AppHandle<R>, status: Status) {
    if let Err(e) = try_show(app, status) {
        tracing::debug!(error = format!("{e:#}"), ?status, "overlay could not be shown");
    }
}

fn try_show<R: Runtime>(app: &AppHandle<R>, status: Status) -> Result<()> {
    let window = app
        .get_webview_window(LABEL)
        .context("the overlay window does not exist")?;

    // Position before showing, so it never appears at the old spot and jumps.
    if let Ok(cursor) = app.cursor_position() {
        let scale = window.scale_factor().unwrap_or(1.0);
        let cursor = LogicalPosition::new(cursor.x / scale, cursor.y / scale);
        window.set_position(place(&window, cursor)).ok();
    }

    window
        .emit_to(LABEL, "overlay:state", Payload {
            state: status,
            chunks: 0,
            total_ms: 0,
            restart: true,
        })
        .context("sending the overlay state")?;

    // Deliberately no `show()`: the window is already visible, and moving it is
    // what keeps the paste target. See PARK.
    Ok(())
}

/// Keeps the widget on screen when the pointer is near an edge.
///
/// Placed below-right of the pointer by default, which is where a tooltip goes
/// and so the least surprising place for it; it flips rather than hangs off.
fn place<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    cursor: LogicalPosition<f64>,
) -> LogicalPosition<f64> {
    let mut x = cursor.x + CURSOR_GAP;
    let mut y = cursor.y + CURSOR_GAP;

    if let Ok(Some(monitor)) = window.current_monitor() {
        let scale = monitor.scale_factor();
        let size: LogicalSize<f64> = monitor.size().to_logical(scale);
        let origin: LogicalPosition<f64> = monitor.position().to_logical(scale);

        if x + WIDTH > origin.x + size.width {
            x = cursor.x - WIDTH - CURSOR_GAP;
        }
        if y + HEIGHT > origin.y + size.height {
            y = cursor.y - HEIGHT - CURSOR_GAP;
        }
    }

    LogicalPosition::new(x, y)
}

/// Updates the chunk count without disturbing the elapsed timer.
pub fn chunks_sent<R: Runtime>(app: &AppHandle<R>, chunks: usize) {
    let Some(window) = app.get_webview_window(LABEL) else { return };
    window
        .emit_to(LABEL, "overlay:state", Payload {
            state: Status::Listening,
            chunks,
            total_ms: 0,
            restart: false,
        })
        .ok();
}

/// Parks the widget off-screen. Not `hide()` — see PARK.
pub fn hide<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(LABEL) else { return };
    if let Err(e) = window.set_position(LogicalPosition::new(PARK.0, PARK.1)) {
        tracing::debug!(error = %e, "overlay could not be parked");
    }
}

/// Walks the three states with the focused app sampled either side of the first
/// appearance.
///
/// Kept rather than temporary. It disproved three separate fixes that all
/// looked right on paper — `focusable(false)`, then accessory activation, then
/// `focused(false)` — and caught a fourth result that was green only because
/// the baseline had been poisoned. The same question is still unanswered on
/// Windows, so the instrument should outlive the bug.
///
///     NAXVOICE_OVERLAY_SELFTEST=1 npm run tauri dev
pub async fn self_test<R: Runtime>(app: &AppHandle<R>) {
    // Scoped so the state guard never spans an await.
    let focused = |app: &AppHandle<R>| {
        app.state::<crate::Platforms>()
            .0
            .focused_app()
            .unwrap_or_else(|e| format!("<error: {e}>"))
    };

    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    // Checked again at the moment that actually matters, in case the runtime
    // overwrote the policy after setup. It has to run on the main thread —
    // `sharedApplication` demands the marker — and this task is not it, which
    // is exactly why calling it directly here failed last time.
    let handle = app.clone();
    if let Err(e) = app.run_on_main_thread(move || {
        if let Err(e) = handle.state::<crate::Platforms>().0.hide_from_dock() {
            tracing::warn!(error = format!("{e:#}"), "re-applying accessory failed");
        }
    }) {
        tracing::warn!(error = %e, "could not reach the main thread for the policy readback");
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // The baseline must not be naxvoice itself. Creating the window visible
    // activates the app once at launch, and without putting another app in
    // front first, "focus did not change" comes out true for the wrong reason —
    // it was already ours. That reading looked like a fix and was not.
    tracing::info!(frontmost = %focused(app), "overlay self-test: frontmost at launch");
    let _ = std::process::Command::new("osascript")
        .args(["-e", r#"tell application "Finder" to activate"#])
        .status();
    tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

    let before = focused(app);
    show(app, Status::Listening);
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    let after = focused(app);

    tracing::info!(
        before = %before,
        after = %after,
        stolen = before != after,
        // Without a baseline belonging to some other app, `stolen` means nothing.
        valid = !before.contains("naxvoice"),
        "overlay self-test: focus either side of the first show"
    );

    chunks_sent(app, 2);
    tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
    show(app, Status::Polishing);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    show(app, Status::Reading);
    tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
    hide(app);

    // Parking only works if macOS lets the window sit off-screen. If it
    // constrains the frame back onto the display, the widget would be left
    // stranded in view, so the resting position is measured rather than assumed.
    if let Some(window) = app.get_webview_window(LABEL) {
        match window.outer_position() {
            Ok(p) => tracing::info!(x = p.x, y = p.y, "overlay self-test: parked position"),
            Err(e) => tracing::warn!(error = %e, "could not read the parked position"),
        }
    }

    tracing::info!("overlay self-test: done");
}

/// The Stop button, which only exists while reading.
#[tauri::command]
pub fn overlay_stop(app: AppHandle) {
    app.state::<crate::tts::read_aloud::ReadAloud>().player.stop();
    hide(&app);
}

/// Escape. Only reaches us if the window can take key events, which it
/// deliberately cannot — see the module docs.
#[tauri::command]
pub fn overlay_dismiss(app: AppHandle) {
    hide(&app);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything the widget reports is a state name the page knows. A renamed
    /// variant that serialises to something `overlay.html` does not match would
    /// leave the widget blank rather than failing loudly.
    #[test]
    fn the_states_serialise_to_what_the_page_matches_on() {
        for (status, expected) in [
            (Status::Listening, "\"listening\""),
            (Status::Polishing, "\"polishing\""),
            (Status::Reading, "\"reading\""),
        ] {
            assert_eq!(serde_json::to_string(&status).unwrap(), expected);
        }
    }

    /// The page reads these keys by name, so renaming a field in Rust would
    /// silently stop the widget updating.
    #[test]
    fn the_payload_carries_the_keys_the_page_reads() {
        let json = serde_json::to_string(&Payload {
            state: Status::Listening,
            chunks: 2,
            total_ms: 0,
            restart: true,
        })
        .unwrap();

        for key in ["state", "chunks", "total_ms", "restart"] {
            assert!(json.contains(key), "{key} is missing from {json}");
        }
    }
}
