# naxvoice

System-wide dictation and read-aloud for macOS and Windows. Tauri app, Rust core,
React dashboard. Transcription and cleanup via OpenRouter, speech synthesis local.

Read `README.md` for architecture and `DESIGN.md` for the UI spec before writing
code. `design/wireframes.html` is the visual reference — open it in a browser to
see the screens and the overlay laid out. Build the React dashboard to match its
structure and hierarchy, not its exact pixel values.

`config.example.yaml` is the contract between the Rust core and the dashboard —
every control in the UI maps to a key in there.

## Current state

Built and in daily use on macOS. All eight steps below are done. The Rust core,
the seven-screen React dashboard and the floating overlay all exist; `cargo test`
is green.

**Windows does not run.** The clipboard, paste and foreground-window code are
written, but `platform/win32.rs` bails explicitly on watching a bare modifier
key, so dictation never fires there. That is the main thing left.

Two things are scaffolded but inert, and should not be described as working:

- **Speculative cleanup.** `config.cleanup.speculative` and
  `vad.rs::ready_for_speculative_cleanup()` exist; nothing fires on them.
  Cleanup runs once, on the stitched transcript, after key release.
- **Opus upload.** `AudioFormat::Opus` is a reserved variant. Capture writes
  16kHz mono WAV.

## Build order

Kept as the record of how it was built. Steps 1-6 and 8 are complete; 7 was
measured and abandoned.

1. `main.rs`, tray icon, `config.rs` reading `config.yaml`, `hotkeys.rs`
   watching the dictate key.
2. `audio/recorder.rs` with `cpal`. Hold key, capture mic, write a WAV to disk.
3. `platform/darwin.rs` and `platform/win32.rs`. Batch transcription: one request
   on key release, paste result at cursor.
4. Cleanup pass with the default profile only.
5. Replace batch with rolling chunks: `vad.rs` via `ort`, dispatch chunks on
   pause, reassemble with `stitch.rs`. The batch path was deleted, not kept.
6. Read-aloud with Kokoro only. Selection capture, normalize, chunk, play.
7. ~~Chatterbox-Turbo and the two-engine handoff.~~ Abandoned after measurement:
   ~2.1s of compute per spoken second on Apple silicon (1503ms in the language
   model, 644ms in the decoder), so it cannot render faster than it plays. fp16,
   q4f16, int8 and CoreML were all measured; fp16 on CPU was best at 60ms per
   token and still 2x too slow. Read-aloud is Kokoro only.
8. The React dashboard, seven screens per `DESIGN.md`: Status, Models, Profiles,
   Dictionary, Voice, History and Hotkeys. Settings write `config.yaml` and take
   effect on restart, not live.

## Rules

**No `#[cfg]` outside `src-tauri/src/platform/`.** If shared code needs to know
the OS, add a method to the `Platform` trait instead. This is what keeps porting
to the second machine an afternoon rather than a rewrite.

**The API key never touches disk in plaintext and never enters git.** It lives in
the OS keychain via the `keyring` crate (`src-tauri/src/secrets.rs`), with
`apple-native` / `windows-native` features — keyring has no default feature and
silently uses an in-memory mock store without them. `tauri-plugin-stronghold` was
the original plan and is deliberately unused: it needs a password a tray app has
nowhere good to keep. `config.yaml` is gitignored; only `config.example.yaml` is
committed, with placeholders.

**The overlay is the product.** The small floating widget is what the user sees
99% of the time. The dashboard is a settings screen visited maybe weekly.

**The overlay must never take focus.** It is created visible and parked
off-screen; showing it moves it, and hiding it parks it again. Do not call
`show()` — `tao`'s `set_visible(true)` calls `makeKeyAndOrderFront`, which
activates the app regardless of `focusable`/`focused`, and taking focus loses the
paste target.

**Clipboard is borrowed, not taken.** `paste_at_cursor` and `read_selection` must
save and restore whatever the user already had copied.

**Never invent user-facing content.** The cleanup system prompt forbids the model
adding facts, figures or names the speaker did not say. Keep that clause.

**Run `cargo test` before every commit.** The stitcher and chunker tests need no
audio hardware and no API key, so there is no excuse for a red run. Note that
`cargo` is only on PATH in shells opened after rustup ran; otherwise prepend
`$HOME/.cargo/bin`.

**Do not claim a number you have not measured.** An earlier README advertised
"~600ms perceived latency" and "first audio in ~200ms"; the second was a budget
printed by a test, not a measurement. Both were wrong, and the code comments had
started citing the README as their source.

## Latency, as measured

There is no per-stage breakdown — the old budget table in this file assumed Opus
upload and speculative cleanup, neither of which was ever implemented, so it
described a pipeline that does not exist.

What is actually measured:

- **Dictation**: the Status screen reports the end-to-end median round trip from
  the history store, over 24 hours. It has been running a little over 2.5s.
- **Read-aloud**: Kokoro renders about 585ms per spoken second, roughly 1.7x
  faster than real time, flat from 29 to 197 characters. What the listener waits
  for is the opening unit alone: 2884ms for a whole 80-character sentence, and
  2046ms once `chunk::split_opening` cuts it at the first comma. The older
  figures in this file (1480ms per spoken second, 1.44x slower than speech, 9.8s
  to the first word) came from a loaded machine and do not reproduce. CoreML is
  about 10% slower than CPU, measured under that same load, so trust the ranking
  and not the absolutes. See the module doc at the top of `tts/kokoro.rs`.

If a change moves the dictation median materially, say so with before/after
numbers from the same session — thermal drift on this machine has faked a 17%
win before, so alternate runs rather than running one config after the other.

## Conventions

- Rust 2021, `anyhow::Result` at boundaries, `thiserror` for library errors
- `tracing` for logs, never `println!`
- Tests live in `#[cfg(test)] mod tests` beside the code
- React: function components, hooks, no class components
- Dashboard talks to Rust only through Tauri commands; the shared config hook is
  `src/lib/useConfig.js`

## Licensing

Own code is MIT. The tree redistributes GPL-3.0-or-later eSpeak NG data and the
build links eSpeak NG, so a distributed binary carries GPL obligations. See
`THIRD_PARTY_LICENSES.md` before adding a dependency or cutting a release.
