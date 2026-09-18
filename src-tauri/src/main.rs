//! naxvoice — system-wide dictation and read-aloud.
//!
//! Through build step 5: the app sits in the tray and holds two dictation keys.
//! While you speak, voice activity detection splits the audio at natural pauses
//! and each segment uploads immediately, so on release only the tail is still in
//! flight. The segments are reassembled in order, polished with the default
//! profile unless the raw key was used, and pasted where you were typing.

// The modules carrying the allow are complete ahead of their callers. audio,
// cleanup, stt and tts are scaffolding main does not reach yet — recorder at
// step 2, transcription at step 3, TTS at step 6 — and config mirrors every key
// in config.example.yaml, so most of its fields have no reader until the step
// that consumes them. Without the allow a build prints a wall of dead_code
// warnings and the real ones get lost in it.
//
// Per-module rather than crate-wide on purpose: hotkeys and main are fully live,
// so anything unreachable in those is a genuine mistake and still gets reported.
// Drop each allow as its step wires that module in.
#[allow(dead_code)]
mod audio;
#[allow(dead_code)]
mod cleanup;
#[allow(dead_code)]
mod config;
mod dashboard;
mod history;
mod hotkeys;
mod overlay;
mod platform;
mod secrets;
#[allow(dead_code)]
mod stt;
#[allow(dead_code)]
mod tts;

use anyhow::{Context, Result};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime};

use config::Config;

/// Tauri's managed state is keyed by type, so a trait object and an optional
/// client each need a name of their own. If step 4 adds much more than this,
/// the `state.rs` the README describes starts earning its place.
pub struct Platforms(pub Box<dyn platform::Platform>);
pub struct Stt(pub Option<std::sync::Arc<stt::openrouter::SttClient>>);
pub struct Cleanup(pub Option<std::sync::Arc<cleanup::CleanupClient>>);

/// Where to paste, captured when the key goes down. Transcription takes seconds
/// and focus moves in the meantime, so the target is decided at the start of a
/// dictation rather than the end.
///
/// The app's identifier rides along with the focus handle rather than sitting
/// in a second slot: they are captured at the same instant and describe the
/// same thing, and two slots that must agree eventually disagree.
#[derive(Clone)]
pub struct Target {
    pub focus: platform::FocusTarget,
    pub app: String,
}

pub struct PasteTarget(pub std::sync::Mutex<Option<Target>>);

fn main() {
    init_tracing();

    if let Err(error) = run() {
        // `{:#}` gives the full anyhow chain on one line — the context from the
        // config loader is usually the part that says what to fix.
        tracing::error!(error = format!("{error:#}"), "naxvoice could not start");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let config = Config::load()?;

    tauri::Builder::default()
        // Dictation watches a bare modifier and needs no plugin, but read-aloud
        // is a discrete press and registers an ordinary accelerator.
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        // The overlay's Stop button and Escape are the only things the frontend
        // calls into Rust for; everything else is driven from this side.
        .invoke_handler(tauri::generate_handler![
            overlay::overlay_stop,
            overlay::overlay_dismiss,
            dashboard::status_snapshot,
            dashboard::get_config,
            dashboard::save_config,
            dashboard::api_key_status,
            dashboard::set_api_key,
            dashboard::clear_api_key,
            dashboard::preview_cleanup,
            dashboard::history_recent,
            dashboard::clear_history
        ])
        .setup(move |app| {
            // Held in managed state so the shortcut handler, which only ever
            // gets an AppHandle, can reach it on both edges of the key.
            let recorder = audio::recorder::Recorder::new(
                audio::recorder::default_output_dir()?,
                config.chunking.policy(),
            );
            tracing::info!(dir = %recorder.output_dir().display(), "recordings directory");
            app.manage(recorder);
            // Before any window exists. A normal macOS app is activated by
            // having a window ordered in front of everything else, which would
            // take the paste target with it — measured, not assumed.
            let platforms = Platforms(platform::current());
            if let Err(e) = platforms.0.hide_from_dock() {
                tracing::warn!(
                    error = format!("{e:#}"),
                    "could not drop to accessory activation, so the overlay may steal focus"
                );
            }
            app.manage(platforms);

            // No key is a normal state until the dashboard can store one, so it
            // warns rather than failing: recording still works, and the failure
            // then names the missing key instead of surfacing as a 401.
            let client = match secrets::resolve() {
                Some(key) => {
                    tracing::info!(model = %config.transcription.model, "transcription enabled");
                    let client = std::sync::Arc::new(stt::openrouter::SttClient::new(
                        config.transcription.base_url.clone(),
                        key,
                        config.transcription.model.clone(),
                        config.transcription.language.clone(),
                    )?);

                    // Open the connection now rather than on the first dictation.
                    // Spawned, because launch must not wait on the network.
                    let warming = std::sync::Arc::clone(&client);
                    tauri::async_runtime::spawn(async move {
                        let started = std::time::Instant::now();
                        warming.warm().await;
                        tracing::debug!(ms = started.elapsed().as_millis(), "connection warmed");
                    });

                    Some(client)
                }
                None => {
                    tracing::warn!(
                        "{} is not set, so dictation will record but not transcribe",
                        config::API_KEY_ENV
                    );
                    None
                }
            };
            app.manage(Stt(client));

            // Same key, separate client: transcription and cleanup are different
            // endpoints with different timeouts, and pooling them separately
            // keeps a slow cleanup from blocking the next transcription.
            let polisher = match secrets::resolve() {
                Some(key) => {
                    tracing::info!(model = %config.cleanup.model, "cleanup enabled");
                    Some(std::sync::Arc::new(cleanup::CleanupClient::new(
                        config.cleanup.base_url.clone(),
                        key,
                        config.cleanup.model.clone(),
                        config.cleanup.max_tokens,
                    )?))
                }
                None => None,
            };
            app.manage(Cleanup(polisher));

            if config.cleanup.speculative {
                // The config key is honoured from step 5. Speculative cleanup
                // fires at a detected pause, and pauses only exist once VAD is
                // splitting the audio, so saying nothing here would look like
                // the setting was silently ignored.
                tracing::debug!("speculative cleanup is configured but needs chunking (step 5)");
            }

            app.manage(PasteTarget(std::sync::Mutex::new(None)));

            // Read-aloud. The weights are downloaded rather than committed, so
            // the engine is constructed eagerly but loads lazily: a missing
            // model should be an error when you press the key, naming the path,
            // not a failure to launch.
            let kokoro = std::sync::Arc::new(tts::kokoro::Kokoro::from_project_layout());
            tracing::info!(model = %kokoro.model_path().display(), "read-aloud engine");
            app.manage(tts::read_aloud::ReadAloud {
                engine: kokoro,
                player: std::sync::Arc::new(audio::player::Player::new()),
            });

            let read_aloud_key = hotkeys::parse_accelerator(&config.hotkeys.read_aloud)?;
            let config_read_aloud = config.hotkeys.read_aloud.clone();
            let dictate_key = config.hotkeys.dictate()?;
            app.manage(hotkeys::Dictation::new(config.hotkeys.latch_window()));
            app.manage(config);

            build_tray(app.handle())?;

            // Built now rather than on the first dictation so the page is
            // already loaded and listening when it is first shown.
            overlay::create(app.handle())?;

            // Kept as a diagnostic: walks the widget through its three states
            // and reports whether the frontmost application changed.
            if std::env::var_os("NAXVOICE_OVERLAY_SELFTEST").is_some() {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move { overlay::self_test(&handle).await });
            }

            // Opens the settings window at launch. The tray menu is the real
            // way in, but a menu item cannot be clicked from a test, and a
            // window that fails to build should not need a human to discover it.
            if std::env::var_os("NAXVOICE_OPEN_SETTINGS").is_some() {
                if let Err(e) = dashboard::open(app.handle()) {
                    tracing::error!(error = format!("{e:#}"), "settings window failed to open");
                }
            }

            // Read-aloud is a discrete press rather than hold-to-talk, so an
            // ordinary registered accelerator is the right shape for it — and
            // pressing it again stops. `stop` in config.yaml stays unregistered
            // on purpose: binding Escape globally would swallow it from every
            // other application.
            hotkeys::register_read_aloud(app.handle(), read_aloud_key, &config_read_aloud)?;

            // Watch the key rather than registering it: a bare modifier cannot
            // be a global hotkey, and watching leaves it working normally in
            // whatever app has focus.
            let handle = app.handle().clone();
            app.state::<Platforms>().0.watch_dictate_key(
                dictate_key,
                Box::new(move |edge| hotkeys::on_key_edge(&handle, edge)),
            )?;

            // Input Monitoring is a different grant from Accessibility. Without
            // it the monitor installs cleanly and then never fires, which is
            // indistinguishable from a dead key.
            if !app.state::<Platforms>().0.has_key_watch_permission() {
                tracing::warn!(
                    "Input Monitoring is not granted: the dictation key will never fire. \
                     Grant it in System Settings, then relaunch."
                );
            }

            // Registration succeeds without Accessibility, but the paste would
            // be dropped silently. Say so at launch rather than letting it look
            // like a transcription failure later.
            if !app.state::<Platforms>().0.has_input_permission() {
                tracing::warn!(
                    "Accessibility permission is not granted: the hotkey will fire but the \
                     paste will be silently dropped until it is granted and naxvoice relaunched"
                );
            }

            tracing::info!("naxvoice ready");
            Ok(())
        })
        .run(tauri::generate_context!())
        .context("running the naxvoice app")
}

/// `NAXVOICE_LOG` takes the usual env-filter syntax, e.g.
/// `NAXVOICE_LOG=naxvoice=trace`. The default keeps our own logs at debug and
/// everything else quiet, so a dictation is readable in the terminal.
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_env("NAXVOICE_LOG")
        .unwrap_or_else(|_| EnvFilter::new("naxvoice=debug,warn"));

    fmt().with_env_filter(filter).with_target(false).init();
}

/// The tray is the whole UI at this step, so it needs a way out of the app.
///
/// The icon is a template image: black plus alpha, which macOS recolours for a
/// light or dark menu bar. A fixed-colour icon is invisible in one of the two.
fn build_tray<R: Runtime>(app: &AppHandle<R>) -> Result<()> {
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit naxvoice", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&settings, &quit])?;

    TrayIconBuilder::with_id("naxvoice")
        .icon(tauri::include_image!("./icons/tray.png"))
        .icon_as_template(true)
        .tooltip("naxvoice")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "settings" => {
                if let Err(e) = dashboard::open(app) {
                    tracing::error!(error = format!("{e:#}"), "could not open settings");
                }
            }
            "quit" => {
                tracing::info!("quit from tray menu");
                app.exit(0);
            }
            other => tracing::debug!(id = other, "unhandled tray menu item"),
        })
        .build(app)
        .context("building tray icon")?;

    Ok(())
}

// Note for step 3: a tray-only app still shows a Dock icon on macOS. The fix is
// NSApplicationActivationPolicyAccessory, which Tauri exposes as a macOS-only
// method — so it belongs behind a `Platform` trait method rather than a `#[cfg]`
// here. Cosmetic, and it can wait for the file where that trait is implemented.
