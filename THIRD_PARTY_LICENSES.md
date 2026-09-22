# Third-party licenses

naxvoice's own source code is MIT (see [LICENSE](LICENSE)). It ships and builds
against components that are **not** MIT, and this file says which, because the
MIT file alone would imply terms this project cannot grant.

This is a plain summary of what is in the tree, not legal advice.

## Redistributed in this repository

| Component | Path | License |
|---|---|---|
| eSpeak NG data (phoneme tables, English dictionary, `lang/gmw`, voice variants) | `src-tauri/resources/espeak-ng-data/**` | **GPL-3.0-or-later** |
| Silero VAD model | `src-tauri/assets/silero_vad_16k_op15.onnx` | MIT |
| Kokoro tokenizer | `src-tauri/resources/tokenizer.json` | Apache-2.0 |

The eSpeak NG data is a trimmed copy of the upstream `espeak-ng-data`
directory, committed because the build runs with `default-features = false` and
therefore never generates it (SETUP.md explains why). It is
GPL-3.0-or-later, copyright the eSpeak NG contributors:
<https://github.com/espeak-ng/espeak-ng>.

The Silero VAD model is compiled into the binary with `include_bytes!` in
`src-tauri/src/audio/vad.rs`. MIT, copyright Silero Team:
<https://github.com/snakers4/silero-vad>.

## Downloaded at install time, not committed

| Component | License |
|---|---|
| Kokoro-82M v1.0 ONNX weights and the `af_heart` voice pack | Apache-2.0 |

`install.sh` fetches these from
<https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX>.

## What this means if you build it

`src-tauri/Cargo.toml` depends on `espeak-rs`, which compiles eSpeak NG's
vendored C source. **A compiled naxvoice binary therefore links GPL-3.0-or-later
code.** Building it for yourself is unencumbered, but if you distribute a built
binary (a release, a `.app`, a package), you are distributing a combined work
that carries GPL-3.0-or-later obligations, including offering corresponding
source under those terms.

The MIT grant in `LICENSE` covers the code written for this project: everything
under `src-tauri/src/`, `src/`, the build scripts and the documentation. It does
not, and cannot, relicense the components above.

## Where the licence texts are

Describing a licence is not the same as conveying it, and GPL-3.0 requires the
text to travel with the binary. The full texts live in
`src-tauri/resources/licenses/` and are bundled into the app, so a release
carries them rather than pointing at a URL that may move:

| File | Covers |
|---|---|
| `GPL-3.0.txt` | eSpeak NG, and therefore any distributed naxvoice binary |
| `Apache-2.0.txt` | Kokoro weights, the `af_heart` voice pack, the tokenizer |
| `Silero-VAD-MIT.txt` | the embedded Silero VAD model |

Inside an installed app they sit at
`naxvoice.app/Contents/Resources/resources/licenses/`.

## Corresponding source

GPL-3.0 asks that source for the covered work be available to whoever receives
the binary. eSpeak NG's own source is at
<https://github.com/espeak-ng/espeak-ng>, and the exact revision of the data
files redistributed here is the copy committed under
`src-tauri/resources/espeak-ng-data/`. naxvoice's own source, including the
build configuration that links it, is the git tag the release was cut from.
