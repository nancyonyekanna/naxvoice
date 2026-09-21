#!/usr/bin/env bash
#
# naxvoice installer, macOS.
#
# Goes from a fresh clone to an app in /Applications. Safe to re-run: every
# step checks whether it has already been done, so an interrupted run can
# simply be started again.
#
#     ./install.sh
#
# What it will NOT do: grant permissions. macOS does not allow that from a
# script, by design — see the end of this file.

set -euo pipefail

cd "$(dirname "$0")"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
ok()   { printf '  \033[32mok\033[0m   %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m    %s\n' "$*"; }
die()  { printf '  \033[31mx\033[0m    %s\n' "$*" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "macOS only for now. The dictation key is unimplemented on Windows; see platform/win32.rs."

# ---------------------------------------------------------------- prerequisites
bold "Checking prerequisites"

# rustup installs into ~/.cargo/bin and only adds it to PATH for *new* shells,
# so someone who just installed Rust in this terminal would otherwise be told
# to install something they already have.
[ -d "$HOME/.cargo/bin" ] && PATH="$HOME/.cargo/bin:$PATH"

command -v cargo >/dev/null 2>&1 \
  && ok "rust $(cargo --version | awk '{print $2}')" \
  || die "rust missing. Install it, then open a new terminal:
         curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"

if command -v node >/dev/null 2>&1; then
  NODE_MAJOR=$(node --version | sed 's/^v//' | cut -d. -f1)
  [ "$NODE_MAJOR" -ge 20 ] \
    && ok "node $(node --version)" \
    || die "node $(node --version) is too old; 20+ is required."
else
  die "node missing. Install Node 20+ from https://nodejs.org or: brew install node"
fi

# espeak-rs compiles vendored C with cmake. Without it the build fails deep
# inside a build script, which is a miserable way to find out.
command -v cmake >/dev/null 2>&1 \
  && ok "cmake $(cmake --version | head -1 | awk '{print $3}')" \
  || die "cmake missing — espeak needs it to compile. Install: brew install cmake"

xcode-select -p >/dev/null 2>&1 \
  && ok "xcode command line tools" \
  || die "xcode command line tools missing. Install: xcode-select --install"

# ------------------------------------------------------------------- the weights
bold "Speech synthesis model"

ASSETS=src-tauri/assets
HF=https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main
mkdir -p "$ASSETS"

# Not committed: 88MB is too large for a git repo, and it is redistributable
# from source. Read-aloud reports the missing path rather than failing
# obscurely, but it will not speak without these.
fetch() {
  local url="$1" dest="$2" name="$3"
  if [ -s "$dest" ]; then ok "$name already present"; return; fi
  printf '  ..   downloading %s\n' "$name"
  curl -fL --progress-bar -o "$dest.part" "$url" || die "could not download $name"
  mv "$dest.part" "$dest"
  ok "$name"
}

fetch "$HF/onnx/model_quantized.onnx" "$ASSETS/kokoro-v1.0.quantized.onnx" "kokoro model (88MB)"
fetch "$HF/voices/af_heart.bin"       "$ASSETS/af_heart.bin"               "af_heart voice"

# ------------------------------------------------------------------------ config
bold "Configuration"

if [ -f config.yaml ]; then
  ok "config.yaml exists, leaving it alone"
else
  cp config.example.yaml config.yaml
  ok "config.yaml created from the example"
fi

# The key belongs in the OS keychain, entered on the Models screen. An
# environment variable also works for a quick start. It is never written here.
if security find-generic-password -s naxvoice -a openrouter >/dev/null 2>&1; then
  ok "OpenRouter key found in the keychain"
elif [ -n "${NAXVOICE_OPENROUTER_KEY:-}" ] || [ -f .env ]; then
  ok "OpenRouter key available from the environment"
else
  warn "No OpenRouter key yet. Add it after launch: tray icon > Settings > Models."
  warn "Dictation records and read-aloud works without one; transcription does not."
fi

# ------------------------------------------------------------------------- build
bold "Building"

npm install --silent
ok "npm dependencies"

npm run tauri build
[ -d src-tauri/target/release/bundle/macos/naxvoice.app ] || die "the build produced no app bundle"
ok "app bundle built"

# ----------------------------------------------------------------------- install
bold "Installing"

# Signed after the copy, at its final path. Tauri's own bundle fails
# verification ("code has no resources but signature indicates they must be
# present"), and macOS will not reliably honour Accessibility or Input
# Monitoring grants for a bundle whose signature does not validate. This is
# the single step that turns a mysteriously dead app into a working one.
pkill -f "/Applications/naxvoice.app" 2>/dev/null || true
sleep 1
rm -rf /Applications/naxvoice.app
cp -R src-tauri/target/release/bundle/macos/naxvoice.app /Applications/
codesign --force --deep --sign - /Applications/naxvoice.app
codesign --verify --deep --strict /Applications/naxvoice.app 2>/dev/null \
  && ok "installed and signed" \
  || die "the signature did not verify; permissions would silently fail"

# ------------------------------------------------------------------ what is left
cat <<'EOF'

Installed to /Applications/naxvoice.app

Two permissions remain, and a script cannot grant them — macOS requires a
human. Open the app from Finder first so it appears in the lists:

  open /Applications/naxvoice.app

Then System Settings > Privacy & Security, and add naxvoice to BOTH:

  Input Monitoring   decides whether the dictation key fires at all
  Accessibility      decides whether the text can be pasted

They are separate grants and both are needed. Quit naxvoice from its menu
bar icon and open it again afterwards: they are only read at launch.

Launch it from Finder, not a terminal. macOS attributes permissions to
whatever started the app, so a terminal launch asks about your terminal.

Then hold Right Command, speak, and let go.
EOF
