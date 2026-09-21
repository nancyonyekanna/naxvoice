# Installing naxvoice

macOS only. Windows is partly written but the dictation key is not implemented
there — see `platform/win32.rs`.

## The short way

```bash
git clone https://github.com/nancyonyekanna/naxvoice.git
cd naxvoice
./install.sh
```

It checks what you need, downloads the speech model, builds, signs and installs
to `/Applications`. Re-running it is safe — every step skips work already done.

Then grant two permissions, which no script can do for you. Jump to
[Permissions](#permissions).

If you would rather not run a script you have not read, the same steps are
below.

## The long way

### 1. Prerequisites

| | why | install |
|---|---|---|
| Rust | the app | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| Node 20+ | the dashboard | `brew install node` |
| cmake | espeak compiles vendored C | `brew install cmake` |
| Xcode CLT | linking | `xcode-select --install` |

cmake is the one people miss. Without it the build fails deep inside a build
script with an error that does not mention cmake.

### 2. The speech model

Not committed — 88MB is too large for a repo. Read-aloud reports the missing
path rather than failing obscurely, but it will not speak without these.

```bash
mkdir -p src-tauri/assets
HF=https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main
curl -fL -o src-tauri/assets/kokoro-v1.0.quantized.onnx "$HF/onnx/model_quantized.onnx"
curl -fL -o src-tauri/assets/af_heart.bin               "$HF/voices/af_heart.bin"
```

The Silero voice-activity model *is* committed — it is 1.3MB and the build
fails without it.

### 3. Configuration

```bash
cp config.example.yaml config.yaml
```

`config.yaml` is gitignored. **Do not put your OpenRouter key in it.** The key
goes in the OS keychain, entered on the Models screen after launch. For a quick
start, `NAXVOICE_OPENROUTER_KEY` in the environment or a `.env` beside
`config.yaml` also works; a saved key takes precedence.

### 4. Build and install

```bash
npm install
npm run tauri build

cp -R src-tauri/target/release/bundle/macos/naxvoice.app /Applications/
codesign --force --deep --sign - /Applications/naxvoice.app
codesign --verify --deep --strict /Applications/naxvoice.app   # expect silence
```

**The re-sign is not optional.** Tauri's bundle fails verification — `spctl`
reports *"code has no resources but signature indicates they must be present"* —
and macOS will not reliably honour permission grants for a bundle whose
signature does not validate. Skipping this produces an app that looks installed,
appears in the permission lists, and silently does nothing.

## Permissions

naxvoice needs two, and they are **separate grants**:

- **Input Monitoring** — whether the dictation key fires at all. Without it
  nothing is recorded and the app looks dead.
- **Accessibility** — whether text can be pasted. Without it the dictation
  records, transcribes and polishes, then nothing appears.

Open the app first so it appears in the lists:

```bash
open /Applications/naxvoice.app
```

Then System Settings → Privacy & Security → add **naxvoice** to both lists.
**Quit it from the menu bar icon and reopen it** — both are only read at launch.

**Launch from Finder, not a terminal.** macOS attributes permissions to the
process that started the app, so launching from a terminal asks about your
terminal rather than about naxvoice.

## Using it

There is no Dock icon and no window at startup — naxvoice runs from the menu
bar, at the top right of the screen near the clock. That is deliberate: a Dock
app steals focus when it shows a window, which would lose the cursor position
the dictation is meant to paste into.

- **Hold Right Command**, speak, release. Text appears where your cursor is.
  Double-tap to latch if you want to pause mid-thought; tap once more to stop.
- **Tap Right Option** with text selected to hear it read aloud. Tap again to stop.
- **Menu bar icon → Settings…** for models, dictionary, hotkeys and history.

## When it does not work

**The key does nothing.** Input Monitoring. Remove the stale entry with **–**,
add it back, then quit and relaunch. Toggling the switch is not enough.

**It records and polishes, then nothing pastes.** Accessibility, same fix.

**It worked, then stopped after a rebuild.** Expected during development.
macOS ties grants to the binary's code signature and `tauri dev` re-signs
ad-hoc on every rebuild, so the grant stops applying while the entry still sits
in the list looking enabled. Install the bundled app for real use.

**Read-aloud pauses between sentences.** Not a fault. Synthesis runs about 1.4×
slower than speech, so playback catches up and waits at sentence boundaries.
CoreML, quantisation tiers and thread counts were all measured and none helped;
see the note at the top of `tts/kokoro.rs`.

**Gatekeeper refuses to open it.** The app is ad-hoc signed, not notarised.
Right-click → Open, once.
