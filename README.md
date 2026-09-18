# naxvoice

System-wide dictation and read-aloud for macOS and Windows. Hold a key and speak,
polished text lands at your cursor. Select any text, press a key, hear it read back
in a voice you chose.

Transcription and cleanup run through OpenRouter on one API key. Speech synthesis
runs locally, so reading is unlimited and works offline.

## Why it's built this way

**Rolling chunk transcription.** OpenRouter's transcription endpoint is HTTP
request/response with no realtime websocket, so we can't stream. Instead, voice
activity detection splits your speech at natural pauses and each segment uploads
as its own request while you keep talking. On key release only the final segment
is still in flight. Perceived latency lands around 600ms instead of 2.5s.

**Speculative cleanup.** The cleanup LLM fires on the stitched transcript at the
last detected pause rather than waiting for release. If the final chunk doesn't
change sentence structure, the polished text is already waiting.

**Local TTS, cloud STT.** Dictation bursts are small, so the network round trip is
cheap. Read-aloud is long and repetitive, so local synthesis means no per-character
cost, no waiting on megabytes of audio, and first audio in ~200ms.

**One TTS engine.** Kokoro (82M) runs locally and starts speaking after the
first clause.

This was meant to be two. The plan was Kokoro for the first sentence with
Chatterbox-Turbo rendering the rest behind it, for better quality and voice
cloning. Measured on an M-series Mac, Chatterbox-Turbo needs about 2.1 seconds
of compute per spoken second — 60ms per token at 25 tokens per second of audio,
plus 644ms per spoken second in the decoder. A voice that renders slower than it
speaks cannot hand off to anything. Every precision was tried (fp16 60ms/token,
q4f16 65ms, int8 798ms) and CoreML was worse than CPU at 105ms. Revisit only
with a GPU.

## Layout

```
src-tauri/            Rust core
  src/
    main.rs           Tauri entry, tray, window lifecycle
    state.rs          Shared app state
    config.rs         config.yaml load/save, keychain for secrets
    hotkeys.rs        Global shortcut registration
    audio/
      recorder.rs     Mic capture, Opus encode
      vad.rs          Silero VAD, pause detection, chunk boundaries
      player.rs       Interruptible playback queue
    stt/
      openrouter.rs   POST /api/v1/audio/transcriptions
      stitch.rs       Ordered reassembly of overlapping chunks
    cleanup/
      mod.rs          Per-app profile lookup, OpenRouter chat call
    tts/
      mod.rs          Engine manager (Kokoro only; see Two TTS engines)
      kokoro.rs       ONNX synthesis, espeak phonemes, punctuation prosody
      read_aloud.rs   Selection to speech, stop on second press
      normalize.rs    Text normalization before synthesis
      chunk.rs        Prosody-aware sentence splitting
    platform/
      darwin.rs       CGEvent paste, pasteboard, frontmost app
      win32.rs        SendInput paste, clipboard, foreground window

src/                  React dashboard
  screens/            Status, Models, Profiles, Dictionary, Voice, Hotkeys
  lib/                Tauri IPC bindings
```

## Setup

Prerequisites: Rust stable, Node 20+, and the Tauri prerequisites for your
platform (Xcode CLT on macOS, MSVC build tools + WebView2 on Windows).

```bash
git clone git@github.com:YOURUSER/naxvoice.git
cd naxvoice
npm install
cp config.example.yaml config.yaml
npm run tauri dev
```

Add your OpenRouter key in the dashboard, not in `config.yaml`. It goes to the OS
keychain. `config.yaml` is gitignored regardless.

The Kokoro weights are not committed and are not downloaded automatically:
put `kokoro-v1.0.quantized.onnx` (88MB) and `af_heart.bin` in
`src-tauri/assets/` yourself. SETUP.md says where to get them. Read-aloud
reports the missing path rather than failing obscurely if they are absent.

## Build order

The overlay is the product. The dashboard configures it. Build in this order and
you'll have something usable after step 3.

1. Hotkey to recorder to a WAV file on disk. Prove capture works on both platforms.
2. Single-shot transcription and paste at cursor. No chunking, no cleanup.
3. Cleanup pass with one global prompt.
4. Rolling chunks and VAD. This is where the latency win comes from.
5. Read-aloud with Kokoro only.
6. Normalizer and prosody chunking.
7. Dashboard.

Steps 1 to 3 are a batch pipeline and step 4 replaces it. That's deliberate. Get
something working end to end before making it fast.

## Platform notes

macOS needs Accessibility permission for global hotkeys and synthetic paste. The
app prompts on first run. Without it, hotkeys register but paste silently fails.

Windows needs no special permission, but some apps reject synthetic Ctrl+V. The
platform shim falls back to character-by-character `SendInput` in that case.

## License

MIT
