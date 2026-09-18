# naxvoice

System-wide dictation and read-aloud for macOS and Windows. Tauri app, Rust core,
React dashboard. Transcription and cleanup via OpenRouter, speech synthesis local.

Read `README.md` for architecture and `DESIGN.md` for the UI spec before writing
code. `design/wireframes.html` is the visual reference — open it in a browser to
see all six screens and the overlay laid out. Build the React dashboard to match
its structure and hierarchy, not its exact pixel values.

`config.example.yaml` is the contract between the Rust core and the dashboard —
every control in the UI maps to a key in there.

## Current state

Scaffold only. These files are written, with passing unit tests:

- `src-tauri/src/stt/stitch.rs` — overlap dedupe, out-of-order chunk reassembly
- `src-tauri/src/tts/chunk.rs` — prosody-aware sentence splitting
- `src-tauri/src/cleanup/mod.rs` — profile matching, dictionary substitution
- `src-tauri/src/stt/openrouter.rs` — transcription client with pooled connections
- `src-tauri/src/tts/normalize.rs` — text normalization before synthesis
- `src-tauri/src/audio/vad.rs` — chunk boundary policy
- `src-tauri/src/platform/mod.rs` — the platform trait

Everything else does not exist yet: `main.rs`, `state.rs`, `config.rs`,
`hotkeys.rs`, `audio/recorder.rs`, `audio/player.rs`, `platform/darwin.rs`,
`platform/win32.rs`, the TTS engine implementations, and the entire `src/` React app.

Two functions are `unimplemented!()` on purpose: `SileroSession::is_speech`
(wire up via `ort`) and the number-to-words helpers in `normalize.rs`
(wire up via `num2words`).

## Build in this order

Do not skip ahead. Each step must run before the next starts.

1. `main.rs`, tray icon, `config.rs` reading `config.yaml`, `hotkeys.rs`
   registering the dictate shortcut. Pressing the key logs a line. Nothing else.
2. `audio/recorder.rs` with `cpal`. Hold key, capture mic, write a WAV to disk.
   Verify on both platforms before continuing.
3. `platform/darwin.rs` and `platform/win32.rs`. Batch transcription: one request
   on key release, paste result at cursor. No chunking, no cleanup.
4. Cleanup pass with the default profile only.
5. Replace batch with rolling chunks: wire `vad.rs` to `ort`, dispatch chunks on
   pause, reassemble with `stitch.rs`. **Delete the batch path — do not keep both.**
6. Read-aloud with Kokoro only. Selection capture, normalize, chunk, play.
7. ~~Chatterbox-Turbo and the two-engine handoff in `tts/mod.rs`.~~ Abandoned
   after measurement: Chatterbox-Turbo costs ~2.1s of compute per spoken second
   on Apple silicon (1503ms in the language model, 644ms in the decoder), so it
   cannot render faster than it plays and has nothing to hand off to. fp16,
   q4f16, int8 and CoreML were all measured; fp16 on CPU was the best at 60ms
   per token and still 2x too slow. Read-aloud is Kokoro only.
8. The React dashboard, six screens per `DESIGN.md`.

Steps 1 to 4 build a working but slow tool. Step 5 is where the latency win is.
That ordering is deliberate: prove the pipeline before optimizing it.

## Rules

**No `#[cfg]` outside `src-tauri/src/platform/`.** If shared code needs to know
the OS, add a method to the `Platform` trait instead. This is what keeps porting
to the second machine an afternoon rather than a rewrite.

**The API key never touches disk in plaintext and never enters git.**
Use `tauri-plugin-stronghold` for the OS keychain. `config.yaml` is gitignored;
only `config.example.yaml` is committed, with placeholders.

**The overlay is the product.** The small floating widget near the cursor is what
the user sees 99% of the time. The dashboard is a settings screen visited maybe
weekly. Build and polish the overlay first.

**Clipboard is borrowed, not taken.** `paste_at_cursor` and `read_selection` must
save and restore whatever the user already had copied.

**Never invent user-facing content.** The cleanup system prompt forbids the model
adding facts, figures or names the speaker did not say. Keep that clause.

**Run `cargo test` before every commit.** The stitcher and chunker tests need no
audio hardware and no API key, so there is no excuse for a red run.

## Latency budget

Target is ~600ms from key release to pasted text. Where it goes:

| Stage | Budget |
|---|---|
| Final chunk upload (Opus, warm connection) | ~80ms |
| Transcription of final chunk | ~210ms |
| Cleanup (usually already done speculatively) | ~0-390ms |
| Paste | ~10ms |

Earlier chunks resolve while the user is still speaking, so they cost nothing.
If a change pushes the median past 800ms, that change is wrong.

## Conventions

- Rust 2021, `anyhow::Result` at boundaries, `thiserror` for library errors
- `tracing` for logs, never `println!`
- Tests live in `#[cfg(test)] mod tests` beside the code
- React: function components, hooks, no class components
- Dashboard talks to Rust only through Tauri commands in `src/lib/ipc.ts`
