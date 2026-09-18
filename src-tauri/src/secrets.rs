//! The OpenRouter key, in the OS keychain.
//!
//! The key must never reach `config.yaml` or any committed file, so it lives in
//! the macOS Keychain or the Windows Credential Manager, guarded by the login
//! session. `keyring` handles both, which is why this needs no `#[cfg]`: the
//! platform split is a target-specific dependency in Cargo.toml instead.
//!
//! **The backend must be asked for explicitly.** keyring has no default
//! feature, and with none enabled it silently substitutes an in-memory mock
//! store — writes succeed, reads succeed, and everything vanishes on restart.
//! That failure is invisible during development, which is exactly why the
//! Cargo.toml entries name `apple-native` and `windows-native`, and why
//! `round_trips` exists to prove a value actually survives.
//!
//! Nothing here ever hands the key back to the frontend. The dashboard is told
//! whether a key is present and what its last four characters are, which is
//! enough to recognise which key is installed without putting it on screen.

use anyhow::{Context, Result};
use keyring::Entry;

/// Keychain entries are addressed by service and account.
const SERVICE: &str = "naxvoice";
const ACCOUNT: &str = "openrouter";

fn entry() -> Result<Entry> {
    Entry::new(SERVICE, ACCOUNT).context("opening the keychain entry")
}

/// Stores the key, replacing any previous one.
pub fn set(key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        return clear();
    }
    entry()?
        .set_password(key)
        .context("writing the key to the keychain")
}

/// The stored key, or `None` when none has been saved.
///
/// A missing entry is not an error: it is the ordinary state before anyone has
/// entered a key, and treating it as a failure would turn first launch into a
/// stack of error logs.
pub fn get() -> Result<Option<String>> {
    match entry()?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).context("reading the key from the keychain"),
    }
}

/// Removes the key. Succeeds when there was nothing to remove.
pub fn clear() -> Result<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).context("removing the key from the keychain"),
    }
}

/// What the dashboard is allowed to know about the key.
#[derive(Debug, Clone, serde::Serialize)]
pub struct KeyStatus {
    pub present: bool,
    /// Last four characters, so one key can be told from another without the
    /// key itself ever reaching the window.
    pub hint: Option<String>,
    /// Whether it came from the keychain or from the dev environment variable.
    pub source: &'static str,
}

pub fn status() -> KeyStatus {
    if let Ok(Some(key)) = get() {
        return KeyStatus {
            present: true,
            hint: Some(hint(&key)),
            source: "keychain",
        };
    }

    // The documented dev fallback. Goes through `dev_api_key` rather than
    // reading the variable directly, because that also covers the `.env` beside
    // config.yaml — reading only the environment would report "no key" for a
    // setup SETUP.md actively tells you to use.
    if let Some(key) = crate::config::dev_api_key() {
        return KeyStatus {
            present: true,
            hint: Some(hint(&key)),
            source: "environment",
        };
    }

    KeyStatus { present: false, hint: None, source: "none" }
}

/// The key to use, in order of precedence.
///
/// Keychain first: that is where the dashboard saves it and where it survives a
/// reboot. The environment and `.env` stay behind it as the dev path, which is
/// what SETUP.md documents for a machine with no key saved yet.
///
/// Never log the return value.
pub fn resolve() -> Option<String> {
    match get() {
        Ok(Some(key)) => return Some(key),
        Ok(None) => {}
        // Worth saying once. Silently behaving as though no key exists would
        // send someone looking at the network when the problem is the keychain.
        Err(e) => tracing::warn!(error = format!("{e:#}"), "could not read the keychain"),
    }

    crate::config::dev_api_key()
}

fn hint(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_key_does_not_panic_making_a_hint() {
        assert_eq!(hint("ab"), "…ab");
        assert_eq!(hint(""), "…");
    }

    #[test]
    fn the_hint_is_the_last_four_characters() {
        assert_eq!(hint("sk-or-v1-abcdef1234"), "…1234");
    }

    /// The one that matters: keyring silently uses an in-memory mock store when
    /// no platform backend feature is enabled, and a mock round-trips within a
    /// process exactly like the real thing. This cannot tell them apart on its
    /// own — but paired with the Cargo.toml features, it catches the case where
    /// the entry cannot be opened at all.
    ///
    /// Ignored by default: it writes to the real keychain, which on macOS can
    /// prompt for permission, and a test suite must not block waiting for a
    /// dialog.
    ///
    ///     cargo test round_trips -- --ignored --nocapture
    #[test]
    #[ignore]
    fn round_trips() {
        let probe = "sk-or-v1-probe-value";
        set(probe).expect("storing");
        assert_eq!(get().expect("reading").as_deref(), Some(probe));
        clear().expect("clearing");
        assert_eq!(get().expect("reading after clear"), None);
    }

    /// Proves this is the real Keychain rather than keyring's mock store.
    ///
    /// Nothing inside the process can tell the two apart: the mock round-trips
    /// exactly like the real thing, which is what makes a missing backend
    /// feature so dangerous. So this looks from outside, with the `security`
    /// tool macOS ships, which reads the login keychain directly. If keyring
    /// were falling back to the mock, `security` would find nothing.
    ///
    /// macOS only, and ignored by default for the same reason as the test above.
    ///
    ///     cargo test visible_to_the_os -- --ignored --nocapture
    #[test]
    #[ignore]
    fn visible_to_the_os_keychain() {
        let probe = "sk-or-v1-cross-process-probe";
        set(probe).expect("storing");

        let output = std::process::Command::new("security")
            .args(["find-generic-password", "-s", SERVICE, "-a", ACCOUNT, "-w"])
            .output()
            .expect("running security");

        let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
        clear().expect("clearing");

        eprintln!("  security reported: {found:?}");
        assert_eq!(
            found, probe,
            "the OS keychain does not have it, so keyring is using its mock store"
        );
    }
}
