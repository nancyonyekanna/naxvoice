//! macOS implementation of `Platform`.
//!
//! Two things here are easy to get subtly wrong.
//!
//! **The clipboard is borrowed.** Both `paste_at_cursor` and `read_selection`
//! snapshot whatever the user had, do their work, and put it back. Silently
//! eating someone's clipboard is the fastest way to make this tool feel hostile,
//! and it is the kind of bug that gets blamed on the OS rather than on us.
//!
//! **Synthetic keystrokes need Accessibility.** Without it `CGEvent::post` is
//! accepted and then silently dropped — no error, no paste. That is why
//! `has_input_permission` exists and why the dictation path checks it before
//! blaming the network.
//!
//! Every AppKit call here bar one is free of `MainThreadMarker`, which is what
//! lets the paste run on the task that finished the transcription rather than
//! having to hop back to the main thread. `hide_from_dock` is the exception:
//! `NSApplication::sharedApplication` demands the marker, so it is called once
//! from `setup`, which already runs on the main thread.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use block2::RcBlock;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSApplicationActivationPolicy, NSEvent,
    NSEventMask, NSEventModifierFlags, NSPasteboard, NSPasteboardTypeString, NSRunningApplication,
    NSWorkspace,
};
use objc2_application_services::AXIsProcessTrusted;
use objc2_foundation::{MainThreadMarker, NSString};

use super::{FocusTarget, KeyEdge, Platform, WatchedKey};

/// Virtual keycodes (`kVK_ANSI_*`, and the Command keys). These are physical key
/// positions rather than characters, so they do not change with the layout.
const KEY_V: u16 = 0x09;
const KEY_C: u16 = 0x08;
/// Left Command. Verified against tao's macOS table: SuperLeft => 0x37.
const KEY_COMMAND: u16 = 0x37;
/// Right Command. Same table: SuperRight => 0x36.
const KEY_RIGHT_COMMAND: u16 = 0x36;
/// Right Option. Apple's Events.h: kVK_RightOption = 0x3D; tao: AltRight.
const KEY_RIGHT_OPTION: u16 = 0x3D;

/// IOKit's Input Monitoring check. There is no Rust binding for this in the
/// tree, and it is three lines, so it is declared here rather than pulling in a
/// crate for one call.
#[repr(C)]
#[derive(Clone, Copy)]
enum IOHIDRequestType {
    #[allow(dead_code)]
    PostEvent = 0,
    ListenEvent = 1,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
enum IOHIDAccessType {
    Granted = 0,
    #[allow(dead_code)]
    Denied = 1,
    #[allow(dead_code)]
    Unknown = 2,
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request: IOHIDRequestType) -> IOHIDAccessType;
}

/// How long to let the target app service a synthetic paste before the old
/// clipboard goes back. Restoring too eagerly means the app pastes whatever was
/// there before instead of the dictation.
const PASTE_SETTLE: Duration = Duration::from_millis(250);

/// How long to let a reactivated app actually come forward before typing into
/// it. Activation is asynchronous, so sending the chord immediately races it.
const ACTIVATE_SETTLE: Duration = Duration::from_millis(90);

/// How long to wait for a copy to land before deciding nothing was selected.
const COPY_TIMEOUT: Duration = Duration::from_millis(300);
const COPY_POLL: Duration = Duration::from_millis(10);

pub struct Darwin;

impl Darwin {
    pub fn new() -> Self {
        Darwin
    }
}

impl Default for Darwin {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for Darwin {
    fn focused_app(&self) -> Result<String> {
        let workspace = NSWorkspace::sharedWorkspace();
        let app = workspace
            .frontmostApplication()
            .context("no frontmost application")?;

        // Bundle id is the stable identifier profiles match on. Fall back to the
        // display name, because a few processes genuinely have no bundle id and
        // a worse identifier beats none.
        if let Some(id) = app.bundleIdentifier() {
            return Ok(id.to_string());
        }
        app.localizedName()
            .map(|n| n.to_string())
            .context("frontmost application has neither a bundle id nor a name")
    }

    fn paste_at_cursor(&self, text: &str) -> Result<()> {
        let pasteboard = NSPasteboard::generalPasteboard();
        let kind = unsafe { NSPasteboardTypeString };

        if !self.has_input_permission() {
            // Leave the text on the clipboard instead of discarding it.
            //
            // Without Accessibility the keystroke cannot be sent, and this used
            // to bail before the pasteboard was touched at all — so a dictation
            // that had been recorded, transcribed and polished was thrown away
            // at the very last step. The permission is still the problem, but
            // losing the words on top of it is a separate and worse failure.
            // Deliberately not restored afterwards: the point is that it stays.
            pasteboard.clearContents();
            let payload = NSString::from_str(text);
            if pasteboard.setString_forType(&payload, kind) {
                bail!(
                    "Accessibility permission is not granted, so the paste could not \
                     be sent. The text is on the clipboard, so press Cmd+V to place it. \
                     Grant Accessibility in System Settings and relaunch to have it \
                     pasted for you."
                );
            }
            bail!(
                "Accessibility permission is not granted, so the paste would be \
                 silently dropped. Grant it in System Settings, then relaunch."
            );
        }

        let borrowed = pasteboard.stringForType(kind).map(|s| s.to_string());

        pasteboard.clearContents();
        let payload = NSString::from_str(text);
        if !pasteboard.setString_forType(&payload, kind) {
            bail!("could not write the transcript to the pasteboard");
        }

        send_chord(KEY_V)?;
        std::thread::sleep(PASTE_SETTLE);

        restore(&pasteboard, kind, borrowed.as_deref());
        Ok(())
    }

    fn read_selection(&self) -> Result<Option<String>> {
        if !self.has_input_permission() {
            bail!("Accessibility permission is not granted, so the copy would do nothing");
        }

        let pasteboard = NSPasteboard::generalPasteboard();
        let kind = unsafe { NSPasteboardTypeString };

        let borrowed = pasteboard.stringForType(kind).map(|s| s.to_string());
        // changeCount is how we tell "nothing was selected" from "the copy has
        // not landed yet". Comparing string contents cannot: copying the same
        // text that was already on the clipboard looks identical either way.
        let before = pasteboard.changeCount();

        send_chord(KEY_C)?;

        let mut selection = None;
        let deadline = std::time::Instant::now() + COPY_TIMEOUT;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(COPY_POLL);
            if pasteboard.changeCount() != before {
                selection = pasteboard.stringForType(kind).map(|s| s.to_string());
                break;
            }
        }

        restore(&pasteboard, kind, borrowed.as_deref());
        Ok(selection)
    }

    fn capture_focus(&self) -> Result<FocusTarget> {
        let workspace = NSWorkspace::sharedWorkspace();

        // frontmostApplication can name a background agent that activated for a
        // moment — an antivirus helper did exactly that here, and the paste went
        // into something with nowhere to put it. Prefer whoever owns the menu
        // bar when the frontmost app is not a normal windowed application.
        let candidate = workspace
            .frontmostApplication()
            .filter(|app| app.activationPolicy() == NSApplicationActivationPolicy::Regular)
            .or_else(|| workspace.menuBarOwningApplication())
            .context("no foreground application to paste into")?;

        // AppKit returns -1 for applications that have no pid at all. Storing
        // that would give us a target we could never find again, and the failure
        // would surface much later as a confusing "cannot receive text".
        let pid = candidate.processIdentifier();
        if pid <= 0 {
            bail!("the foreground application has no process id to return to");
        }
        Ok(FocusTarget(pid as i64))
    }

    fn restore_focus(&self, target: FocusTarget) -> Result<bool> {
        let Some(app) = NSRunningApplication::runningApplicationWithProcessIdentifier(target.0 as i32)
        else {
            tracing::warn!(pid = target.0, "the app dictated into is no longer running");
            return Ok(false);
        };

        if app.isTerminated() {
            return Ok(false);
        }

        // Accessory and Prohibited apps have no windows and no text fields.
        // Sending a paste chord at one throws the dictation away silently.
        if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
            tracing::warn!(pid = target.0, "the target is a background agent, not a normal app");
            return Ok(false);
        }

        if app.isActive() {
            return Ok(true);
        }

        // ActivateAllWindows only. ActivateIgnoringOtherApps is deprecated as of
        // macOS 14 and does nothing, so relying on it would be a silent no-op.
        let activated = app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        if activated {
            std::thread::sleep(ACTIVATE_SETTLE);
        }
        Ok(activated)
    }

    fn watch_key(
        &self,
        key: WatchedKey,
        on_edge: Box<dyn Fn(KeyEdge) + Send + Sync + 'static>,
    ) -> Result<()> {
        // The modifier bit travels with the keycode. `keyCode` says which
        // physical key moved; whether it is now *down* is read from that key's
        // own flag, so watching an Option key while testing the Command bit
        // would report every press as a release.
        let (keycode, flag) = match key {
            WatchedKey::RightCommand => (KEY_RIGHT_COMMAND, NSEventModifierFlags::Command),
            WatchedKey::LeftCommand => (KEY_COMMAND, NSEventModifierFlags::Command),
            WatchedKey::RightOption => (KEY_RIGHT_OPTION, NSEventModifierFlags::Option),
        };

        let block = RcBlock::new(move |event: std::ptr::NonNull<NSEvent>| {
            // Safety: AppKit hands the monitor a live event for the duration of
            // the call and does not retain it beyond that.
            let event = unsafe { event.as_ref() };

            // flagsChanged fires for every modifier. keyCode says which physical
            // key moved, and it is the only way to tell the two Command keys
            // apart — NSEventModifierFlags has a single Command bit with no
            // left/right distinction.
            if event.keyCode() != keycode {
                return;
            }

            // The Command bit being set means a Command key is now down. Caveat:
            // holding both Command keys and releasing one leaves the bit set, so
            // that release reads as a press. Rare enough to accept, and the
            // alternative is tracking per-key state the OS does not expose.
            let down = event.modifierFlags().contains(flag);

            on_edge(if down { KeyEdge::Down } else { KeyEdge::Up });
        });

        let monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::FlagsChanged,
            &block,
        )
        .context("installing the global key monitor")?;

        // The handle is Retained<AnyObject>, which is Send only when the inner
        // type is, and AnyObject is not — so it cannot be stored in shared
        // state. The monitor is meant to live for the whole process and
        // removeMonitor is unsafe, so it is deliberately leaked instead. The
        // block must outlive the monitor too.
        std::mem::forget(monitor);
        std::mem::forget(block);

        tracing::info!(?key, keycode, "watching key");
        Ok(())
    }

    fn has_key_watch_permission(&self) -> bool {
        unsafe { IOHIDCheckAccess(IOHIDRequestType::ListenEvent) == IOHIDAccessType::Granted }
    }

    fn has_input_permission(&self) -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    fn request_input_permission(&self) -> Result<()> {
        // Opening the pane rather than using AXIsProcessTrustedWithOptions: the
        // system prompt only appears once per app, and after the user has
        // dismissed it the prompt never returns, which leaves no way to recover.
        // The settings pane always works.
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .status()
            .context("opening the Accessibility settings pane")?;
        Ok(())
    }

    fn request_key_watch_permission(&self) -> Result<()> {
        // Same reasoning as the pane above. `IOHIDRequestAccess` prompts once
        // per app and never again, so for anyone who has already dismissed it
        // the settings pane is the only route that still works.
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
            .status()
            .context("opening the Input Monitoring settings pane")?;
        Ok(())
    }

    fn hide_from_dock(&self) -> Result<()> {
        // `sharedApplication` wants proof we are on the main thread. Getting
        // that wrong is a crash rather than a misbehaviour, so it is checked
        // and reported instead of asserted.
        let mtm = MainThreadMarker::new()
            .context("hide_from_dock has to run on the main thread")?;

        // Accessory also drops the Dock icon, which a tray app should not have
        // had in the first place — see the note this replaces in main.rs.
        let app = NSApplication::sharedApplication(mtm);

        // Read before writing. Setting the policy it already has reports failure,
        // which previously looked like the policy being refused when in fact it
        // was already correct — an ambiguity that cost a measurement.
        if app.activationPolicy() == NSApplicationActivationPolicy::Accessory {
            tracing::debug!(accessory = true, "activation policy already accessory");
            return Ok(());
        }

        if !app.setActivationPolicy(NSApplicationActivationPolicy::Accessory) {
            bail!("macOS refused the accessory activation policy");
        }

        // Read back rather than trust the return value.
        tracing::debug!(
            accessory = app.activationPolicy() == NSApplicationActivationPolicy::Accessory,
            "activation policy after setting it"
        );

        Ok(())
    }
}

/// Puts the previous clipboard contents back. Failures here are logged rather
/// than propagated: the dictation already succeeded, and turning a failed
/// restore into a failed dictation would be the worse outcome.
fn restore(pasteboard: &NSPasteboard, kind: &NSString, previous: Option<&str>) {
    pasteboard.clearContents();
    if let Some(previous) = previous {
        let ns = NSString::from_str(previous);
        if !pasteboard.setString_forType(&ns, kind) {
            tracing::warn!("could not restore the previous clipboard contents");
        }
    }
}

fn event_source() -> Result<CGEventSource> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow!("could not create a CGEventSource"))
}

/// Sends Command + `keycode` as a full press and release sequence.
///
/// The modifier gets its own key events as well as being set as a flag on the
/// letter. Flags alone satisfy most apps, but some watch for the Command key's
/// own transitions, and "paste works in one app but not another" is usually
/// this difference rather than anything to do with permissions.
fn send_chord(keycode: u16) -> Result<()> {
    let cmd = CGEventFlags::CGEventFlagCommand;
    post_key(KEY_COMMAND, true, cmd)?;
    post_key(keycode, true, cmd)?;
    post_key(keycode, false, cmd)?;
    post_key(KEY_COMMAND, false, CGEventFlags::empty())?;
    Ok(())
}

fn post_key(keycode: u16, keydown: bool, flags: CGEventFlags) -> Result<()> {
    let event = CGEvent::new_keyboard_event(event_source()?, keycode, keydown)
        .map_err(|_| anyhow!("could not create a keyboard event"))?;
    event.set_flags(flags);
    event.post(CGEventTapLocation::HID);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These are physical key positions from Apple's kVK_ANSI table. A wrong
    /// value here pastes nothing, or worse, triggers some other shortcut.
    #[test]
    fn keycodes_match_apple_virtual_key_table() {
        assert_eq!(KEY_V, 0x09);
        assert_eq!(KEY_C, 0x08);
        // The modifier keycodes were claimed as verified but never pinned here.
        // All three are from Apple's Events.h and agree with tao's table.
        assert_eq!(KEY_RIGHT_COMMAND, 0x36, "kVK_RightCommand");
        assert_eq!(KEY_COMMAND, 0x37, "kVK_Command");
        assert_eq!(KEY_RIGHT_OPTION, 0x3D, "kVK_RightOption");
    }

    /// The permission check must never panic — it runs on every paste, and a
    /// panic inside the shortcut handler takes dictation down for the session.
    #[test]
    fn permission_check_is_callable() {
        let _ = Darwin::new().has_input_permission();
    }
}
