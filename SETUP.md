# Setup

From download to a running dev build. Do this on one machine first, push to
GitHub, then clone on the second.

## 1. Put the folder where you want it

Unzip `naxvoice-scaffold.zip`. Move the `naxvoice` folder into Documents, or
wherever you keep repos.

macOS:
```bash
mv ~/Downloads/naxvoice ~/Documents/naxvoice
cd ~/Documents/naxvoice
```

Windows (PowerShell):
```powershell
Move-Item $HOME\Downloads\naxvoice $HOME\Documents\naxvoice
cd $HOME\Documents\naxvoice
```

## 2. Prerequisites

Both machines need Rust and Node 20+.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then open a new terminal, or run `source "$HOME/.cargo/env"` in the current one,
and check that `cargo --version` answers. The installer only puts `~/.cargo/bin`
on your PATH for *new* shells. If it is missing, `npm run tauri dev` fails with

```
failed to run 'cargo metadata' command to get workspace directory:
No such file or directory (os error 2)
```

which names `cargo metadata` rather than the missing PATH entry, and sends you
looking in the wrong place.

macOS additionally needs Xcode command line tools:
```bash
xcode-select --install
```

Read-aloud also needs **cmake**, because the speech synthesis pulls in espeak-ng
and compiles its vendored C source:

```bash
brew install cmake
```

Two things about that dependency are worth knowing before you hit them.

Its default build runs a freshly compiled binary to generate data files, and
that step crashes on Apple Silicon with `SIGTRAP` at `[31%] Compile
intonations`. `Cargo.toml` therefore builds it with `default-features = false`,
which skips that step — and because the data it would have produced is then
missing, a trimmed copy is committed at `src-tauri/resources/espeak-ng-data`
(1.3MB: the phoneme tables, the English dictionary, and `lang/gmw`). Do not
delete it; espeak fails at runtime with `Error processing file 'phontab'`
without it.

The Kokoro weights are *not* committed. `src-tauri/assets/` needs
`kokoro-v1.0.quantized.onnx` (88MB) and `af_heart.bin`, both from
`onnx-community/Kokoro-82M-v1.0-ONNX` on Hugging Face. Read-aloud reports the
missing path rather than failing obscurely if they are absent.

Windows additionally needs the MSVC build tools and WebView2. Install
"Desktop development with C++" from the Visual Studio Installer. WebView2 ships
with Windows 11 and recent Windows 10.

### Type-checking the Windows code from a Mac

`cargo check --target x86_64-pc-windows-msvc` does not work from macOS, even
with the target installed. `tauri-build` compiles a Win32 resource for the
binary and panics without a resource compiler:

```
tauri-winres panicked: called `Result::unwrap()` on an `Err` value:
NotAttempted("llvm-rc")
```

This never happens on Windows itself, where `rc.exe` comes with the MSVC build
tools. From a Mac, either `brew install llvm` to get `llvm-rc` on PATH, or
type-check `platform/win32.rs` in a throwaway crate that has no Tauri build
script — a stub of the `Platform` trait plus the real `windows` dependency,
pulling in the real file with `#[path = "..."] mod win32;`. The second is
faster and checks exactly the code that can't be compiled in-tree.

## 3. Install the frontend tooling

The Tauri shell is committed — `src-tauri/Cargo.toml`, `tauri.conf.json`,
`build.rs`, `capabilities/` and the icons are already in the repo, so there is
no generator to run. There is no React app yet either; it arrives at build step
8, and until then `ui/index.html` is a placeholder that gives Tauri a frontend
directory to point at.

```bash
npm install
```

That installs one package, the Tauri CLI, which is what `npm run tauri dev`
uses.

`src-tauri/Cargo.toml` carries only what the code in the tree actually calls.
`ort`, `num2words`, `thiserror` and `tauri-plugin-stronghold` are listed there in
a comment against the step that introduces each one — adding them up front means
compiling native ONNX toolchains for code nothing calls yet. `cpal` and `hound`
joined at step 2, when capture became real.

Note that `cpal` is pinned to 0.18, not the 0.15 this file used to name. The API
moved in between: `Device::name` became `Device::description`, and `SampleRate`
is now an alias for `u32` rather than a newtype.

One substitution worth knowing about: `serde_yaml` is published as
`0.9.34+deprecated` and is unmaintained, so config parsing uses `serde_norway`,
the maintained drop-in fork. The API is identical.

## 4. Config

```bash
cp config.example.yaml config.yaml
```

If you already have a `config.yaml` from before the single-key interaction, it
will not load — the `hotkeys` section changed shape and the app stops with

```
parsing config.yaml: hotkeys: missing field `dictate_key`
```

That is deliberate rather than defaulted: a silent fallback would start using a
key your file never mentions. Replace the `hotkeys` block with the one in
`config.example.yaml` (`dictate_key` and `latch_ms` in place of `dictate` and
`dictate_raw`) and add `enabled: true` under `cleanup`.

Do not put your OpenRouter key in this file. It is gitignored either way, but the
key belongs in the OS keychain via the dashboard once that screen exists. Until
then, read it from an environment variable in dev.

## 5. Verify the logic before fighting the build

```bash
cargo test --manifest-path src-tauri/Cargo.toml
```

The stitcher and chunker tests need no microphone, no API key and no network.
A green run means the hard parts are correct. Do this before `tauri dev`, because
if the build fails you want to know it is the toolchain and not your code.

## 6. Run

```bash
npm run tauri dev
```

macOS needs Accessibility permission before the paste will work. Without it,
hotkeys register and dictation transcribes, but the paste silently does nothing.

**In dev, grant it to your terminal, not to naxvoice.** Under `tauri dev` the
binary is a bare, ad-hoc-signed executable with no `.app` bundle, so macOS
attributes the permission to the responsible process up the chain — Terminal, or
whichever app launched `npm run tauri dev`. Adding
`src-tauri/target/debug/naxvoice` to the Accessibility list looks right and does
nothing. The same rule is why the microphone prompt names your terminal.

Grant it in System Settings → Privacy & Security → Accessibility, then restart
the dev server: the permission is only read at launch. Granting the terminal has
a useful side effect — rebuilds no longer invalidate it, whereas an entry for the
binary lapses every time the binary is replaced.

A release build (`npm run tauri build`) produces a real `naxvoice.app` with a
stable bundle identity, which gets its own Accessibility entry and needs none of
the above.

### Input Monitoring is a second, separate permission

macOS 10.15 split **Input Monitoring** out from Accessibility. Dictation watches
a single key (Right Command by default) rather than registering a hotkey, and
that watching may require Input Monitoring even though pasting requires
Accessibility. Having one does not grant the other.

Grant it to the same thing you granted Accessibility — your terminal in dev, the
bundled app in a release build — under System Settings → Privacy & Security →
Input Monitoring. The app checks at startup and says so:

```
WARN  Input Monitoring is not granted: the dictation key will never fire
```

Without it the key is simply dead, with no error at the moment you press it,
which is why the check exists rather than leaving you to guess.

The app reports which state it is in at startup, so check the log rather than
guessing:

```
WARN  Accessibility permission is not granted: ...
```

Absence of that line means the paste will work.

## 7. Git

```bash
git init
git add .
git commit -m "Scaffold: core modules, design spec, build plan"
gh repo create naxvoice --public --source=. --push
```

Public from the first commit. A repo that appears fully formed in one commit
reads as generated; a real commit history reads as built.

Before pushing, confirm nothing sensitive is staged:

```bash
git status --porcelain
grep -r "sk-or-" . --exclude-dir=.git --exclude-dir=node_modules || echo "clean"
```

## 8. Second machine

```bash
git clone git@github.com:YOURUSER/naxvoice.git
cd naxvoice
npm install
cp config.example.yaml config.yaml
npm run tauri dev
```

Your key is in the first machine's keychain, not in the repo, so enter it again
on the second machine.

## 9. Hand it to Claude Code

```bash
cd ~/Documents/naxvoice
claude
```

Then:

> Read CLAUDE.md, README.md and DESIGN.md. Start at build step 1.

`CLAUDE.md` carries the build order, the architectural rules and the latency
budget, so it will pick up where the scaffold stops rather than redesigning it.
If it proposes skipping ahead or keeping the batch path alongside chunking at
step 5, stop it — both are called out in CLAUDE.md as mistakes.
