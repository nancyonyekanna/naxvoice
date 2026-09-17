//! Windows implementation of `Platform`.
//!
//! Same contract as `darwin.rs`: the clipboard is borrowed and put back, and
//! `focused_app` returns an identifier that profiles can match on — the
//! executable name here, where macOS returns a bundle id.
//!
//! Windows needs no permission to synthesize input, so `has_input_permission`
//! is always true and `request_input_permission` is a no-op. Some applications
//! do reject synthetic Ctrl+V, which the README notes; the character-by-character
//! fallback belongs here once there is a real app to test it against.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::ProcessStatus::GetModuleBaseNameW;
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_C, VK_CONTROL, VK_V,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
};

use super::{DictateKey, FocusTarget, KeyEdge, Platform};

/// Let the target app service the synthetic paste before the old clipboard
/// goes back. See the same constant in `darwin.rs`.
const PASTE_SETTLE: Duration = Duration::from_millis(120);

pub struct Win32;

impl Win32 {
    pub fn new() -> Self {
        Win32
    }
}

impl Default for Win32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for Win32 {
    fn focused_app(&self) -> Result<String> {
        unsafe {
            let window = GetForegroundWindow();
            if window.0.is_null() {
                bail!("no foreground window");
            }

            let mut pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut pid));
            if pid == 0 {
                bail!("no process owns the foreground window");
            }

            // QUERY_LIMITED_INFORMATION rather than QUERY_INFORMATION: it is the
            // one that works against elevated and protected processes, which the
            // user will absolutely have focused at some point.
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .context("opening the foreground process")?;

            let mut buffer = [0u16; MAX_PATH as usize];
            let written = GetModuleBaseNameW(process, None, &mut buffer);
            let _ = CloseHandle(process);

            if written == 0 {
                bail!("could not read the foreground process name");
            }
            Ok(String::from_utf16_lossy(&buffer[..written as usize]))
        }
    }

    fn paste_at_cursor(&self, text: &str) -> Result<()> {
        let borrowed = read_clipboard().unwrap_or(None);

        write_clipboard(text).context("writing the transcript to the clipboard")?;
        send_chord(VK_V)?;
        std::thread::sleep(PASTE_SETTLE);

        if let Some(previous) = borrowed {
            if let Err(e) = write_clipboard(&previous) {
                // The dictation already landed; a failed restore must not turn
                // that into a failed dictation.
                tracing::warn!(error = %e, "could not restore the previous clipboard contents");
            }
        }
        Ok(())
    }

    fn read_selection(&self) -> Result<Option<String>> {
        let borrowed = read_clipboard().unwrap_or(None);

        send_chord(VK_C)?;
        std::thread::sleep(PASTE_SETTLE);
        let selection = read_clipboard()?;

        if let Some(previous) = &borrowed {
            if let Err(e) = write_clipboard(previous) {
                tracing::warn!(error = %e, "could not restore the previous clipboard contents");
            }
        }

        // An unchanged clipboard means the copy produced nothing, which is what
        // "no selection" looks like from here.
        if selection == borrowed {
            return Ok(None);
        }
        Ok(selection)
    }

    fn capture_focus(&self) -> Result<FocusTarget> {
        unsafe {
            let window = GetForegroundWindow();
            if window.0.is_null() {
                bail!("no foreground window to paste into");
            }
            Ok(FocusTarget(window.0 as i64))
        }
    }

    fn restore_focus(&self, target: FocusTarget) -> Result<bool> {
        unsafe {
            let window = HWND(target.0 as *mut core::ffi::c_void);
            if !IsWindow(Some(window)).as_bool() {
                tracing::warn!(hwnd = target.0, "the window dictated into is gone");
                return Ok(false);
            }
            // Windows refuses this when the calling process does not own the
            // foreground, so a false return is normal rather than an error.
            Ok(SetForegroundWindow(window).as_bool())
        }
    }

    fn watch_dictate_key(
        &self,
        _key: DictateKey,
        _on_edge: Box<dyn Fn(KeyEdge) + Send + Sync + 'static>,
    ) -> Result<()> {
        // Windows has no AppKit-style global monitor. The equivalent is a
        // low-level keyboard hook (SetWindowsHookEx with WH_KEYBOARD_LL), which
        // needs a message pump on its own thread. Deliberately unimplemented
        // while Windows is parked, and it says so rather than silently doing
        // nothing — a key that never fires is the worst failure to diagnose.
        bail!("watching a single dictation key is not implemented on Windows yet")
    }

    fn has_key_watch_permission(&self) -> bool {
        true
    }

    fn has_input_permission(&self) -> bool {
        true
    }

    fn request_input_permission(&self) -> Result<()> {
        Ok(())
    }
}

fn read_clipboard() -> Result<Option<String>> {
    unsafe {
        OpenClipboard(None).context("opening the clipboard")?;
        let result = (|| {
            let handle = match GetClipboardData(CF_UNICODETEXT.0 as u32) {
                Ok(h) => h,
                // No text on the clipboard is a normal state, not a failure.
                Err(_) => return Ok(None),
            };
            let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(handle.0)) as *const u16;
            if ptr.is_null() {
                return Ok(None);
            }
            let mut len = 0usize;
            while *ptr.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(handle.0));
            Ok(Some(text))
        })();
        let _ = CloseClipboard();
        result
    }
}

fn write_clipboard(text: &str) -> Result<()> {
    let mut utf16: Vec<u16> = text.encode_utf16().collect();
    utf16.push(0);

    unsafe {
        OpenClipboard(None).context("opening the clipboard")?;
        let result = (|| {
            EmptyClipboard().context("emptying the clipboard")?;

            let bytes = utf16.len() * std::mem::size_of::<u16>();
            let global = GlobalAlloc(GMEM_MOVEABLE, bytes).context("allocating clipboard memory")?;

            let ptr = GlobalLock(global) as *mut u16;
            if ptr.is_null() {
                bail!("could not lock the clipboard allocation");
            }
            std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr, utf16.len());
            let _ = GlobalUnlock(global);

            // Ownership of the allocation transfers to the clipboard here, so it
            // must not be freed on this side.
            SetClipboardData(CF_UNICODETEXT.0 as u32, Some(windows::Win32::Foundation::HANDLE(global.0)))
                .context("setting clipboard data")?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

/// Sends Ctrl + `key` as a press and release pair.
fn send_chord(key: VIRTUAL_KEY) -> Result<()> {
    let press = |vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    let inputs = [
        press(VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
        press(key, KEYBD_EVENT_FLAGS(0)),
        press(key, KEYEVENTF_KEYUP),
        press(VK_CONTROL, KEYEVENTF_KEYUP),
    ];

    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        bail!("SendInput delivered {sent} of {} events", inputs.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_needs_no_input_permission() {
        assert!(Win32::new().has_input_permission());
        assert!(Win32::new().request_input_permission().is_ok());
    }
}
