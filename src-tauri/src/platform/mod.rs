//! The only OS-specific surface in the app.
//!
//! Everything above this line is shared. If you find yourself adding a `#[cfg]`
//! anywhere outside this module, the abstraction is leaking — add a method here
//! instead. That discipline is what makes the second platform an afternoon
//! rather than a rewrite.

use anyhow::Result;

/// Opaque handle to whatever the OS uses to name a paste target: a process id
/// on macOS, a window handle on Windows. Shared code stores and returns it but
/// never interprets it, which is what keeps the meaning inside this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusTarget(pub i64);

/// Which way a watched key moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEdge {
    Down,
    Up,
}

/// The key that drives dictation.
///
/// Only the two Command keys, because these are the only keycodes verified
/// against a real table. Adding Option or Control is a matter of confirming
/// their values rather than guessing them — a wrong keycode here produces a key
/// that silently never fires, which is painful to diagnose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictateKey {
    RightCommand,
    LeftCommand,
}

impl std::str::FromStr for DictateKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().replace(['_', '-', ' '], "").as_str() {
            "rightcommand" | "rightcmd" | "rcmd" => Ok(Self::RightCommand),
            "leftcommand" | "leftcmd" | "lcmd" => Ok(Self::LeftCommand),
            other => anyhow::bail!(
                "unknown dictate_key {other:?}. Supported: RightCommand, LeftCommand"
            ),
        }
    }
}

#[cfg(target_os = "macos")]
pub mod darwin;
#[cfg(target_os = "windows")]
pub mod win32;

pub trait Platform: Send + Sync {
    /// Identifier of the focused app, used to pick a cleanup profile.
    /// macOS returns the bundle id, Windows the executable name. For browsers,
    /// implementations should return the host of the active tab where they can
    /// reach it, so per-site profiles work.
    fn focused_app(&self) -> Result<String>;

    /// Write to clipboard and send the paste chord.
    ///
    /// Implementations must save and restore the previous clipboard contents.
    /// Silently eating whatever the user had copied is the fastest way to make
    /// this tool feel hostile.
    fn paste_at_cursor(&self, text: &str) -> Result<()>;

    /// Copy the current selection and return it, restoring the clipboard after.
    /// Returns None when nothing is selected.
    fn read_selection(&self) -> Result<Option<String>>;

    /// Remembers where to paste, captured when the user starts speaking.
    ///
    /// This has to happen at the start rather than at the end. Transcription
    /// takes seconds, and focus moves: a notification, a background agent
    /// briefly activating, or the user simply clicking elsewhere all mean the
    /// app in front on release is not the one they were dictating into.
    fn capture_focus(&self) -> Result<FocusTarget>;

    /// Brings a captured target back to the front so the paste lands in it.
    ///
    /// Returns `false` when the target cannot take typed text — it has quit, or
    /// it is a background agent with no windows. Pasting into one of those
    /// silently loses the dictation, so callers should treat `false` as a
    /// reason not to send the keystroke at all.
    fn restore_focus(&self, target: FocusTarget) -> Result<bool>;

    /// Watches the dictation key globally, reporting every press and release.
    ///
    /// This observes rather than registers. A registered hotkey consumes the
    /// key, which would be wrong for a modifier — Right Command has to keep
    /// working as Command in whatever app has focus. It is also the only option:
    /// a bare modifier cannot be registered as a hotkey on macOS at all.
    ///
    /// `on_edge` is called from an OS event thread, so it must return promptly
    /// and do its real work elsewhere.
    fn watch_dictate_key(
        &self,
        key: DictateKey,
        on_edge: Box<dyn Fn(KeyEdge) + Send + Sync + 'static>,
    ) -> Result<()>;

    /// Whether the OS will actually deliver those key events.
    ///
    /// Separate from `has_input_permission`: macOS split Input Monitoring out
    /// from Accessibility in 10.15, and having one does not imply the other.
    /// Without this the monitor installs successfully and then never fires,
    /// which is indistinguishable from a broken key unless we check.
    fn has_key_watch_permission(&self) -> bool;

    /// Whether the OS has granted whatever permission synthetic input needs.
    /// macOS: Accessibility. Windows: always true.
    fn has_input_permission(&self) -> bool;

    /// Open the OS permission prompt or settings pane. No-op where unneeded.
    fn request_input_permission(&self) -> Result<()>;

    /// Stops this app from being one the OS will bring to the front.
    ///
    /// Measured, because the obvious reasoning is wrong: building the overlay
    /// window non-focusable is *not* enough. Showing it moved the frontmost
    /// application from `com.google.Chrome` to `naxvoice`, because activation
    /// happens at the application level — ordering any window front activates a
    /// normal app whatever the window itself permits. That costs the paste
    /// target, which is the one failure DESIGN.md says ends the interaction.
    ///
    /// Must be called on the main thread, before any window is shown.
    fn hide_from_dock(&self) -> Result<()>;
}

pub fn current() -> Box<dyn Platform> {
    #[cfg(target_os = "macos")]
    return Box::new(darwin::Darwin::new());
    #[cfg(target_os = "windows")]
    return Box::new(win32::Win32::new());
}
