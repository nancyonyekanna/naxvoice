//! Kokoro speech synthesis, driven directly through `ort`.
//!
//! The inference is written here rather than taken from a crate, and that is a
//! deliberate choice made after trying both of the published ones:
//!
//! - `sayd-kokoro` enables ort's `load-dynamic` feature, which sets
//!   `ort-sys/disable-linking` and switches ort from linking the ONNX runtime to
//!   locating a dylib at startup. Cargo features are additive across the whole
//!   graph and cannot be opted out of, so pulling it in would flip the entire
//!   app to dynamic loading and break the voice detection that dictation needs.
//! - `kokoro-en` avoids that but turns on `cuda` and `directml`, dragging GPU
//!   backends into a build that has never run on Windows, and chains back to an
//!   espeak version whose build step crashes on this machine.
//!
//! What is left is about thirty lines: look the phonemes up in a vocabulary,
//! slice a row out of the voice's style pack, run one session. `audio::vad`
//! already drives ort the same way for Silero.
//!
//! Text reaches the model as phonemes, never as text — "hello" has to become
//! "həlˈoʊ" first, which is what espeak does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, bail, Context, Result};
use ort::session::Session;
use ort::value::Tensor;

use super::{AudioSegment, Synthesizer, VoiceConfig};

/// Kokoro emits 24kHz mono.
pub const SAMPLE_RATE: u32 = 24_000;

/// A voice pack is `STYLE_ROWS * STYLE_DIM` little-endian f32, flat on disk.
/// Confirmed against af_heart.bin: 510 * 256 * 4 == 522_240 bytes exactly.
const STYLE_ROWS: usize = 510;
const STYLE_DIM: usize = 256;

/// Points at the directory holding `espeak-ng-data` and `tokenizer.json`.
pub const RESOURCES_ENV: &str = "NAXVOICE_RESOURCES";
/// Points at the directory holding the model and voice packs.
pub const MODELS_ENV: &str = "NAXVOICE_MODELS";

/// espeak initialises once per process and reads its data directory at that
/// moment, so the variable has to be set before the first call rather than per
/// synthesis.
static ESPEAK: OnceLock<Result<(), String>> = OnceLock::new();

/// espeak-ng is a C library from the 1990s with global mutable state and no
/// thread-safety guarantees. `espeak-rs` guards *initialisation* with a
/// `OnceLock` but leaves the calls themselves unprotected, and two threads
/// phonemising at once segfaults — reproduced as `signal: 11, SIGSEGV` in the
/// test binary, passing cleanly under `--test-threads=1`.
///
/// Serialising costs nothing worth measuring: phonemisation is around 22ms
/// against roughly 585ms of synthesis per spoken second.
///
/// That rate was measured in a release build by two independent harnesses,
/// `length_curve` and `first_unit_candidates`, and is flat from 29 to 197
/// characters at 543 to 648ms per spoken second. Synthesis therefore runs about
/// 1.7x *faster* than the speech it produces.
///
/// **It replaces an earlier figure of 1480ms per spoken second and a claim that
/// synthesis ran 1.44x slower than speech.** Those came from a machine under
/// heavy load and do not reproduce. The scale of that error is the most useful
/// thing to know before trusting any timing in this file: the identical
/// measurement, taken while this Mac was 25GB into swap with a load average of
/// 140, returned about 15000ms per spoken second, which is 20 to 40x worse than
/// a quiet run. Measure the machine before you believe a regression here.
///
/// CoreML was registered as an execution provider and measured across four
/// lengths: 1519, 1559, 1633 and 1594ms per spoken second, against 1464, 1423,
/// 1434 and 1441 on the default CPU provider. Consistently about 10% slower, so
/// the code for it was removed rather than left as a switch nobody should flip.
/// The same happened with Chatterbox's decoder, where CoreML cost 105ms per
/// token against 65ms on CPU. Those absolute values carry the same load as the
/// 1480 figure and have not been re-measured, so trust the ranking and not the
/// numbers.
///
/// What the listener waits for is the opening unit alone, because every later
/// unit renders while the previous one plays. On the self-test passage that is
/// 2884ms for the whole 80-character first sentence, or 2046ms once
/// `chunk::split_opening` cuts it at the first comma. Long reads have still
/// been observed to pause mid-passage; the cause is unresolved, and machine
/// load is the leading suspect, because an engine this far ahead of playback
/// cannot starve the queue on its own account.
static ESPEAK_CALLS: Mutex<()> = Mutex::new(());

pub struct Kokoro {
    model: PathBuf,
    voices_dir: PathBuf,
    resources: PathBuf,
    loaded: Mutex<Option<Loaded>>,
}

struct Loaded {
    session: Session,
    /// The model names its token input; read it rather than assuming.
    input_name: String,
    vocab: HashMap<char, i64>,
    voices: HashMap<String, Vec<f32>>,
}

/// Punctuation the model has tokens for, and acts on.
///
/// Measured: the same phonemes read flat take 1.73s, and 2.10s once these marks
/// are present. The model really does pause; it is not merely tolerating them.
const SPOKEN_MARKS: &[char] = &['.', ',', '!', '?', ';', ':', '—', '…'];

/// Splits text into fragments, each with the mark that ended it.
///
/// espeak discards punctuation entirely — `"Wait, really?"` comes back as
/// `wˈeɪtɹˈiəli`, which is both fused and flat. So the marks are carved out
/// here, the fragments are phonemised separately, and the marks are put back
/// afterwards. Splitting is free: a fragment phonemises to exactly the
/// characters it contributed to the whole, verified across four sentences.
fn segments(text: &str) -> Vec<(&str, Option<char>)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut start = 0;

    for (i, (idx, c)) in chars.iter().enumerate() {
        if !SPOKEN_MARKS.contains(c) {
            continue;
        }

        // A mark hugged by digits belongs to the number, not to the prosody:
        // "3.14", "1,000" and "12:30" are each one spoken thing. Normalisation
        // usually expands these first, but it does not always run.
        let prev = chars[..i].last().map(|(_, p)| *p);
        let next = chars.get(i + 1).map(|(_, n)| *n);
        if matches!(c, '.' | ',' | ':')
            && prev.is_some_and(|p| p.is_ascii_digit())
            && next.is_some_and(|n| n.is_ascii_digit())
        {
            continue;
        }

        out.push((&text[start..*idx], Some(*c)));
        start = idx + c.len_utf8();
    }

    if start < text.len() {
        out.push((&text[start..], None));
    }
    out
}

impl Kokoro {
    /// `resources` holds espeak-ng-data and tokenizer.json and is committed.
    /// `models` holds the 88MB model and the voice packs, which are downloaded
    /// rather than committed.
    pub fn new(resources: PathBuf, models: PathBuf, model_file: &str) -> Self {
        Self {
            model: models.join(model_file),
            voices_dir: models,
            resources,
            loaded: Mutex::new(None),
        }
    }

    /// Default layout: resources/ beside the crate, assets/ for the weights.
    pub fn from_project_layout() -> Self {
        let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let resources = std::env::var_os(RESOURCES_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| crate_dir.join("resources"));
        let models = std::env::var_os(MODELS_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| crate_dir.join("assets"));
        Self::new(resources, models, "kokoro-v1.0.quantized.onnx")
    }

    pub fn model_path(&self) -> &Path {
        &self.model
    }

    /// Turns text into phonemes. Separate from synthesis so a caller can see
    /// what the model will actually be asked to say.
    pub fn phonemes(&self, text: &str) -> Result<String> {
        // Held across both the init and the call: see ESPEAK_CALLS.
        let _guard = ESPEAK_CALLS
            .lock()
            .map_err(|_| anyhow!("espeak lock poisoned"))?;

        self.init_espeak()?;

        // One espeak call per fragment rather than one for the whole text, so
        // the marks between them survive to the model. See `segments`.
        let mut out = String::new();
        for (fragment, mark) in segments(text) {
            let fragment = fragment.trim();
            if !fragment.is_empty() {
                let parts = espeak_rs::text_to_phonemes(fragment, "en-us", None)
                    .map_err(|e| anyhow!("espeak failed: {e}"))?;
                let spoken = parts.join(" ");
                let spoken = spoken.trim();
                if !spoken.is_empty() {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(spoken);
                }
            }
            // A mark with nothing before it would open the utterance on a
            // pause, which the model renders as a stumble.
            if let Some(mark) = mark {
                if !out.is_empty() {
                    out.push(mark);
                }
            }
        }

        Ok(out)
    }

    /// Callers must hold `ESPEAK_CALLS`.
    fn init_espeak(&self) -> Result<()> {
        let resources = self.resources.clone();
        let outcome = ESPEAK.get_or_init(move || {
            // espeak looks for a directory *containing* espeak-ng-data.
            std::env::set_var(
                "PIPER_ESPEAKNG_DATA_DIRECTORY",
                resources.as_os_str(),
            );
            match espeak_rs::text_to_phonemes("ok", "en-us", None) {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("{e}")),
            }
        });

        match outcome {
            Ok(()) => Ok(()),
            Err(e) => bail!(
                "espeak could not initialise from {}: {e}. It needs an \
                 espeak-ng-data directory there.",
                self.resources.display()
            ),
        }
    }

    fn load(&self) -> Result<Loaded> {
        if !self.model.exists() {
            bail!(
                "Kokoro model missing at {}. It is downloaded rather than \
                 committed; see SETUP.md.",
                self.model.display()
            );
        }

        // Default CPU, deliberately. CoreML was tried and measured slower —
        // see the note at the top of this file.
        //
        // Thread count is left to ONNX Runtime, which was measured and is
        // right. `with_intra_threads` was swept over 1, 2, 4, 6, 8 and 10 on a
        // 10-core M1 Pro. A first pass appeared to show 8 threads winning by
        // 17%, on the theory that the default spills work onto the two slower
        // efficiency cores — but that pass ran each setting once, in order,
        // while the machine was cooling from a build, so every later setting
        // scored better for being later. Re-run alternating across three
        // rounds, the default was fastest or tied every time and the whole
        // effect vanished. Do not re-add it without alternating the runs.
        let session = Session::builder()
            .and_then(|mut b| b.commit_from_file(&self.model))
            .with_context(|| format!("loading {}", self.model.display()))?;

        let input_name = session
            .inputs()
            .first()
            .context("the model declares no inputs")?
            .name()
            .to_string();

        let vocab = load_vocab(&self.resources.join("tokenizer.json"))?;

        Ok(Loaded { session, input_name, vocab, voices: HashMap::new() })
    }

    fn voice_pack(&self, loaded: &mut Loaded, name: &str) -> Result<Vec<f32>> {
        if let Some(v) = loaded.voices.get(name) {
            return Ok(v.clone());
        }
        let path = self.voices_dir.join(format!("{name}.bin"));
        let raw = std::fs::read(&path)
            .with_context(|| format!("reading voice pack {}", path.display()))?;
        let pack = decode_pack(&raw, name)?;
        loaded.voices.insert(name.to_string(), pack.clone());
        Ok(pack)
    }
}

impl Kokoro {
    /// Synthesises already-phonemised input.
    ///
    /// Split out from `synthesize` so the phonemiser and the model can be
    /// exercised separately — and because punctuation has to be re-inserted
    /// into the phoneme string before it reaches the model.
    pub async fn synthesize_phonemes(
        &self,
        phonemes: &str,
        cfg: &VoiceConfig,
    ) -> Result<AudioSegment> {
        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| anyhow!("kokoro lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(self.load()?);
        }
        let loaded = guard.as_mut().expect("just loaded");

        // A Chatterbox clone target is meaningless here; Kokoro uses presets.
        let voice = cfg.voice.split(':').next_back().unwrap_or("af_heart");
        let pack = self.voice_pack(loaded, voice)?;

        let ids: Vec<i64> = phonemes
            .chars()
            .filter_map(|c| loaded.vocab.get(&c).copied())
            .take(STYLE_ROWS - 1)
            .collect();
        if ids.is_empty() {
            bail!("no phoneme in {phonemes:?} is in the model's vocabulary");
        }

        // The style row is chosen by token count: longer phrases get different
        // prosody from the same voice.
        let row = ids.len().min(STYLE_ROWS - 1);
        let style = pack[row * STYLE_DIM..(row + 1) * STYLE_DIM].to_vec();

        let mut tokens = Vec::with_capacity(ids.len() + 2);
        tokens.push(0i64);
        tokens.extend_from_slice(&ids);
        tokens.push(0i64);

        let token_count = tokens.len();
        let t_tokens = Tensor::from_array(([1usize, token_count], tokens))?;
        let t_style = Tensor::from_array(([1usize, STYLE_DIM], style))?;
        let t_speed = Tensor::from_array(([1usize], vec![cfg.speed]))?;

        let outputs = loaded.session.run(ort::inputs![
            loaded.input_name.as_str() => t_tokens,
            "style" => t_style,
            "speed" => t_speed,
        ])?;

        let value = outputs.values().next().context("the model returned nothing")?;
        let (_, pcm) = value.try_extract_tensor::<f32>()?;

        Ok(AudioSegment { index: 0, pcm: pcm.to_vec(), sample_rate: SAMPLE_RATE })
    }
}

#[async_trait::async_trait]
impl Synthesizer for Kokoro {
    async fn synthesize(&self, text: &str, cfg: &VoiceConfig) -> Result<AudioSegment> {
        let phonemes = self.phonemes(text)?;
        self.synthesize_phonemes(&phonemes, cfg).await
    }

    async fn warm(&self) -> Result<()> {
        {
            let _guard = ESPEAK_CALLS
                .lock()
                .map_err(|_| anyhow!("espeak lock poisoned"))?;
            self.init_espeak()?;
        }
        let mut guard = self
            .loaded
            .lock()
            .map_err(|_| anyhow!("kokoro lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(self.load()?);
        }
        Ok(())
    }

    fn is_loaded(&self) -> bool {
        self.loaded.lock().map(|g| g.is_some()).unwrap_or(false)
    }
}

/// Kokoro's vocabulary maps single characters to ids, so espeak's IPA feeds it
/// directly. Verified: every character of `həlˈoʊ ðˈɛɹ` is present.
fn load_vocab(path: &Path) -> Result<HashMap<char, i64>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let object = json["model"]["vocab"]
        .as_object()
        .context("tokenizer.json has no model.vocab object")?;

    let vocab: HashMap<char, i64> = object
        .iter()
        .filter_map(|(key, value)| {
            let mut chars = key.chars();
            let c = chars.next()?;
            // Multi-character keys are not phonemes we can index by char.
            if chars.next().is_some() {
                return None;
            }
            Some((c, value.as_i64()?))
        })
        .collect();

    if vocab.is_empty() {
        bail!("tokenizer.json produced an empty vocabulary");
    }
    Ok(vocab)
}

/// Little-endian f32, validated up front so slicing a style row later cannot
/// index out of bounds.
fn decode_pack(raw: &[u8], name: &str) -> Result<Vec<f32>> {
    let expected = STYLE_ROWS * STYLE_DIM;
    if raw.len() != expected * 4 {
        bail!(
            "voice pack {name} is {} bytes, expected {} ({STYLE_ROWS}x{STYLE_DIM} f32)",
            raw.len(),
            expected * 4
        );
    }
    Ok(raw
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kokoro() -> Kokoro {
        Kokoro::from_project_layout()
    }

    #[test]
    fn a_voice_pack_must_be_exactly_the_expected_geometry() {
        // Short, long, and not a multiple of four all have to be refused, or a
        // style row could be sliced out of bounds at synthesis time.
        assert!(decode_pack(&[0u8; 16], "short").is_err());
        assert!(decode_pack(&[0u8; STYLE_ROWS * STYLE_DIM * 4 + 4], "long").is_err());
        let ok = decode_pack(&vec![0u8; STYLE_ROWS * STYLE_DIM * 4], "af_heart").unwrap();
        assert_eq!(ok.len(), STYLE_ROWS * STYLE_DIM);
    }

    /// This segfaulted before `ESPEAK_CALLS` existed. Rust runs tests in
    /// parallel, which is how it surfaced at all — and it would have reached
    /// the app the moment two reads overlapped.
    #[test]
    fn phonemising_from_several_threads_at_once_does_not_crash() {
        let k = std::sync::Arc::new(kokoro());
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let k = std::sync::Arc::clone(&k);
                std::thread::spawn(move || k.phonemes(&format!("thread {i} is speaking now")))
            })
            .collect();

        for t in threads {
            // A panic here is a crashed thread, which is the failure this guards.
            let _ = t.join().expect("no phonemiser thread should crash");
        }
    }

    #[test]
    fn the_committed_vocabulary_parses_and_is_single_characters() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/tokenizer.json");
        let vocab = load_vocab(&path).expect("tokenizer.json should parse");
        assert!(vocab.len() > 50, "got {} entries", vocab.len());
    }

    /// The two halves only fit together if every phoneme espeak emits has an id.
    /// If this fails, synthesis silently drops sounds.
    #[test]
    fn espeak_output_maps_cleanly_into_the_vocabulary() {
        let k = kokoro();
        let Ok(phonemes) = k.phonemes("Hello there, this is a test of naxvoice.") else {
            eprintln!("espeak unavailable, skipping");
            return;
        };
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/tokenizer.json");
        let vocab = load_vocab(&path).unwrap();

        let unmapped: Vec<char> = phonemes
            .chars()
            .filter(|c| !vocab.contains_key(c) && !c.is_whitespace())
            .collect();
        assert!(unmapped.is_empty(), "unmapped phonemes {unmapped:?} in {phonemes:?}");
    }

    /// The number CLAUDE.md's budget is actually about: not a whole passage,
    /// but how long before the *first* sentence can start playing. The two
    /// engine design exists because that figure is what the listener feels.
    ///
    ///     cargo test --release first_sentence -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn first_sentence_latency() {
        let k = kokoro();
        if !k.model_path().exists() {
            eprintln!("model absent, skipping");
            return;
        }
        let cfg = VoiceConfig {
            engine: super::super::Engine::Kokoro,
            voice: "af_heart".into(),
            speed: 1.0,
            exaggeration: 0.0,
        };

        // Warm first: a cold model load is a launch cost, not a per-read one.
        k.warm().await.expect("warm");

        let passage = "The build passed. Now ship it to production, and tell \
                       the team it is ready for review.";
        let units = super::super::chunk::split(passage);
        let first = &units.first().expect("at least one unit").text;

        let started = std::time::Instant::now();
        let segment = k.synthesize(first, &cfg).await.expect("synthesis");
        let elapsed = started.elapsed();

        let spoken = segment.pcm.len() as f32 / segment.sample_rate as f32;
        eprintln!(
            "first sentence {first:?}\n  {} chars -> {spoken:.2}s of speech in {elapsed:?} \
             ({:.1}x realtime), budget is ~200ms",
            first.len(),
            spoken / elapsed.as_secs_f32()
        );
    }

    /// Is the cost fixed per call, or does it scale with output length?
    ///
    /// The answer decides the design. Fixed cost means short sentences are
    /// permanently expensive and splitting finer buys nothing; scaling cost
    /// means the first-audio budget is reachable by saying less at a time.
    ///
    ///     cargo test --release latency_shape -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn latency_shape() {
        let k = kokoro();
        if !k.model_path().exists() {
            eprintln!("model absent, skipping");
            return;
        }
        let cfg = VoiceConfig {
            engine: super::super::Engine::Kokoro,
            voice: "af_heart".into(),
            speed: 1.0,
            exaggeration: 0.0,
        };
        k.warm().await.expect("warm");

        for text in [
            "Yes.",
            "The build passed.",
            "The build passed and the tests are green.",
            "The build passed and the tests are green, so it is ready to ship today.",
        ] {
            // Two runs: the first of a given shape can carry allocation noise.
            let mut best = std::time::Duration::MAX;
            let mut spoken = 0.0;
            for _ in 0..2 {
                let t = std::time::Instant::now();
                let seg = k.synthesize(text, &cfg).await.expect("synthesis");
                best = best.min(t.elapsed());
                spoken = seg.pcm.len() as f32 / seg.sample_rate as f32;
            }
            eprintln!(
                "  {:>3} chars -> {spoken:>5.2}s speech in {:>7.0}ms  ({:.0}ms per spoken second)",
                text.len(),
                best.as_secs_f64() * 1000.0,
                best.as_secs_f64() * 1000.0 / spoken.max(0.01) as f64
            );
        }
    }

    /// What each candidate opening costs, measured back to back.
    ///
    /// First-word latency is just the speech duration of unit 0 times the
    /// render rate, so the only lever is how much is said before playback can
    /// start. Three options, in increasing order of what they cost in prosody:
    ///
    ///   full    the whole first sentence, which is what `chunk::split` does
    ///   clause  cut at the first comma, where intonation already continues
    ///   head    cut mid-clause, which `chunk.rs` and `player.rs` forbid
    ///
    /// Measured three times each and reported in full, because run to run
    /// variance on this machine has reached 2.4x on identical text and a single
    /// sample would be meaningless.
    ///
    ///     cargo test --release first_unit_candidates -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn first_unit_candidates() {
        let k = kokoro();
        if !k.model_path().exists() {
            eprintln!("model absent, skipping");
            return;
        }
        let cfg = VoiceConfig {
            engine: super::super::Engine::Kokoro,
            voice: "af_heart".into(),
            speed: 1.0,
            exaggeration: 0.0,
        };
        k.warm().await.expect("warm");

        let candidates = [
            (
                "full  ",
                "The paste target is captured when the key goes down, not when the text is ready.",
            ),
            ("clause", "The paste target is captured when the key goes down,"),
            ("head  ", "The paste target is captured."),
        ];

        for (label, text) in candidates {
            let mut runs = Vec::new();
            let mut spoken = 0.0f64;
            for _ in 0..3 {
                let started = std::time::Instant::now();
                let segment = k.synthesize(text, &cfg).await.expect("synthesis");
                runs.push(started.elapsed().as_secs_f64() * 1000.0);
                spoken = segment.pcm.len() as f64 / segment.sample_rate as f64;
            }
            let best = runs.iter().cloned().fold(f64::MAX, f64::min);
            let worst = runs.iter().cloned().fold(0.0f64, f64::max);
            eprintln!(
                "  {label} {:>3} chars  {spoken:>5.2}s speech   best {best:>6.0}ms  \
                 worst {worst:>6.0}ms  ({:>4.0}ms/spoken s at best)   runs {:?}",
                text.len(),
                best / spoken.max(0.01),
                runs.iter().map(|r| r.round() as i64).collect::<Vec<_>>()
            );
        }
        eprintln!("\n  target for the first word is under 1500ms");
    }

    /// How cost scales with unit length.
    ///
    /// `latency_shape` stops at 71 characters and reports a flat rate. The real
    /// passage has 63 to 122 character units and costs far more per spoken
    /// second, so the flat region is not where the app actually runs. If the
    /// curve rises, shorter units are cheaper in total as well as sooner, and
    /// `chunk::MAX_UNIT_CHARS` is the lever worth pulling.
    ///
    /// Swept ascending, then descending, in one run. Thermal drift penalises
    /// whichever end is measured last, so only a curve that survives both
    /// directions is real. This project has already been fooled once by running
    /// a sweep in a single direction; see the thread-count note above.
    ///
    ///     cargo test --release length_curve -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn length_curve() {
        let k = kokoro();
        if !k.model_path().exists() {
            eprintln!("model absent, skipping");
            return;
        }
        let cfg = VoiceConfig {
            engine: super::super::Engine::Kokoro,
            voice: "af_heart".into(),
            speed: 1.0,
            exaggeration: 0.0,
        };
        k.warm().await.expect("warm");

        /// Whole words up to `chars`, closed with a full stop so the model is
        /// given a complete utterance at every length.
        fn prefix(text: &str, chars: usize) -> String {
            let mut out = String::new();
            for word in text.split_whitespace() {
                let word = word.trim_end_matches(['.', ',', ';', ':']);
                if out.chars().count() + word.chars().count() + 1 > chars {
                    break;
                }
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(word);
            }
            out.push('.');
            out
        }

        let passage = super::super::read_aloud::SELFTEST_PASSAGE;
        let mut lengths = vec![30usize, 60, 90, 120, 160, 200];

        for direction in ["ascending", "descending"] {
            eprintln!("  {direction}:");
            for &n in &lengths {
                let text = prefix(passage, n);
                let mut best = f64::MAX;
                let mut spoken = 0.0f64;
                // Best of two: the first call of a given shape carries
                // allocation noise, as latency_shape already found.
                for _ in 0..2 {
                    let started = std::time::Instant::now();
                    let segment = k.synthesize(&text, &cfg).await.expect("synthesis");
                    best = best.min(started.elapsed().as_secs_f64() * 1000.0);
                    spoken = segment.pcm.len() as f64 / segment.sample_rate as f64;
                }
                eprintln!(
                    "    {:>3} chars -> {spoken:>5.2}s speech in {best:>7.0}ms  \
                     ({:>4.0}ms/spoken s)",
                    text.len(),
                    best / spoken.max(0.01)
                );
            }
            lengths.reverse();
        }
    }

    /// What a real passage costs, unit by unit, with nothing playing.
    ///
    /// Deliberately isolates synthesis from playback. The figures in
    /// `player.rs` come from a live read, where a cpal callback runs at
    /// real-time priority on the same cores as the model. If this harness is
    /// much faster than the live read, contention is the cost and the ratio in
    /// the docs describes the app rather than the engine.
    ///
    /// Reports cumulative render against cumulative speech, which is what
    /// decides whether the queue can starve at all.
    ///
    ///     cargo test --release passage_latency -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn passage_latency() {
        let k = kokoro();
        if !k.model_path().exists() {
            eprintln!("model absent, skipping");
            return;
        }
        let cfg = VoiceConfig {
            engine: super::super::Engine::Kokoro,
            voice: "af_heart".into(),
            speed: 1.0,
            exaggeration: 0.0,
        };
        k.warm().await.expect("warm");

        let passage = super::super::read_aloud::SELFTEST_PASSAGE;
        let units = super::super::chunk::split(passage);
        eprintln!(
            "passage {} chars -> {} units (the first unit is the first word latency)",
            passage.len(),
            units.len()
        );

        let mut render_s = 0.0f64;
        let mut speech_s = 0.0f64;
        let mut first_ms = 0.0f64;

        for (i, unit) in units.iter().enumerate() {
            // Rendered twice, on the same text, so length and punctuation are
            // held constant and the only difference is whether the session has
            // seen this shape before. The app only ever pays the first call;
            // the gap between the two is what repetition buys, and
            // `length_curve` reports that cheaper number by taking best of two.
            let mut cold = 0.0f64;
            let mut repeat = 0.0f64;
            let mut spoken = 0.0f64;
            for pass in 0..2 {
                let started = std::time::Instant::now();
                let segment = k.synthesize(&unit.text, &cfg).await.expect("synthesis");
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                spoken = segment.pcm.len() as f64 / segment.sample_rate as f64;
                if pass == 0 {
                    cold = ms;
                } else {
                    repeat = ms;
                }
            }

            // Cumulative figures use the cold timing, because that is the only
            // one a real read ever pays.
            render_s += cold / 1000.0;
            speech_s += spoken;
            if i == 0 {
                first_ms = cold;
            }

            // A positive "behind" means rendering has fallen behind the speech
            // already queued, which is the only way the player can run dry.
            eprintln!(
                "  unit {i:>2} {:>4} chars {spoken:>5.2}s speech   cold {cold:>6.0}ms \
                 ({:>4.0}ms/s)   repeat {repeat:>6.0}ms ({:>4.0}ms/s)   behind {:+.2}s",
                unit.text.len(),
                cold / spoken.max(0.01),
                repeat / spoken.max(0.01),
                render_s - speech_s
            );
        }

        // Unit 0 again, after every other shape has passed through the session.
        // Fast means allocations for a seen shape survive, and bucketing token
        // lengths would make real reads pay the repeat price. Slow means they
        // are evicted and every unit of a real read pays the cold price.
        {
            let started = std::time::Instant::now();
            let segment = k.synthesize(&units[0].text, &cfg).await.expect("synthesis");
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            let spoken = segment.pcm.len() as f64 / segment.sample_rate as f64;
            eprintln!(
                "\n  unit  0 revisited after the others: {ms:.0}ms ({:.0}ms/spoken s)",
                ms / spoken.max(0.01)
            );
        }

        eprintln!(
            "\n  first unit:     {first_ms:.0}ms   (target is under 1500ms)\n  \
             whole passage:  {render_s:.1}s render for {speech_s:.1}s speech  \
             ({:.2}x realtime)\n  queue starves:  {}",
            speech_s / render_s.max(0.001),
            if render_s > speech_s { "yes" } else { "no, rendering outpaces playback" }
        );
    }

    /// espeak throws punctuation away, so the marks have to be re-inserted or
    /// the voice reads everything as one flat run-on. This is the regression
    /// test for that: the user heard it as "doesn't follow punctuation".
    #[test]
    fn punctuation_survives_into_the_phonemes() {
        let k = kokoro();
        for (text, expected) in [
            ("Hello. This is a test.", vec!['.']),
            ("Wait, really? Yes!", vec![',', '?', '!']),
            ("First item; second item: third.", vec![';', ':', '.']),
        ] {
            let phonemes = k.phonemes(text).expect("phonemisation");
            for mark in expected {
                assert!(
                    phonemes.contains(mark),
                    "{mark:?} was lost from {text:?}: got {phonemes:?}"
                );
            }
        }
    }

    /// The old bug had a second symptom: with the punctuation gone, the words
    /// either side of it fused into one. "Hello." + "This" became `həlˈoʊðɪs`.
    #[test]
    fn words_across_a_boundary_are_no_longer_fused() {
        let k = kokoro();
        let phonemes = k.phonemes("Hello. This is a test.").expect("phonemisation");
        assert!(
            !phonemes.contains("həlˈoʊðɪs"),
            "the words are still fused: {phonemes:?}"
        );
    }

    /// Splitting on every mark would read "3.14" as two numbers with a pause
    /// between them. A mark between digits is part of the number.
    #[test]
    fn a_mark_between_digits_is_not_a_pause() {
        for text in ["3.14", "1,000", "12:30"] {
            let parts = segments(text);
            assert_eq!(
                parts.len(),
                1,
                "{text:?} should be one fragment, got {parts:?}"
            );
            assert_eq!(parts[0], (text, None));
        }
    }

    #[test]
    fn a_mark_between_words_is_a_boundary() {
        assert_eq!(
            segments("Wait, really?"),
            vec![("Wait", Some(',')), (" really", Some('?'))]
        );
    }

    /// Re-inserting a mark the model has no token for would be silently
    /// dropped by the tokenizer, which would look like the fix not working.
    #[test]
    fn every_mark_we_re_insert_is_in_the_vocabulary() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/tokenizer.json");
        let vocab = load_vocab(&path).unwrap();
        let missing: Vec<char> = SPOKEN_MARKS
            .iter()
            .copied()
            .filter(|c| !vocab.contains_key(c))
            .collect();
        assert!(missing.is_empty(), "not in the vocabulary: {missing:?}");
    }

    /// The whole chain, on real text: does what `phonemes` now emits actually
    /// come out longer than the same words read flat?
    ///
    ///     cargo test --release end_to_end_punctuation -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn end_to_end_punctuation_lengthens_real_text() {
        let k = kokoro();
        if !k.model_path().exists() {
            eprintln!("model absent, skipping");
            return;
        }
        let cfg = VoiceConfig {
            engine: super::super::Engine::Kokoro,
            voice: "af_heart".into(),
            speed: 1.0,
            exaggeration: 0.0,
        };
        k.warm().await.expect("warm");

        for text in [
            "Wait, really? Yes!",
            "Hello. This is a test.",
            "First item; second item: third.",
        ] {
            let with = k.phonemes(text).expect("phonemes");
            let without: String = with.chars().filter(|c| !SPOKEN_MARKS.contains(c)).collect();

            let a = k.synthesize_phonemes(&without, &cfg).await.expect("flat");
            let b = k.synthesize_phonemes(&with, &cfg).await.expect("punctuated");
            let (a, b) = (
                a.pcm.len() as f32 / a.sample_rate as f32,
                b.pcm.len() as f32 / b.sample_rate as f32,
            );

            eprintln!("  {text:?}");
            eprintln!("    -> {with}");
            eprintln!("    flat {a:.2}s   punctuated {b:.2}s   ({:+.0}%)", (b / a - 1.0) * 100.0);
            assert!(b > a, "punctuation did not lengthen {text:?}");
        }
    }

    /// Does the model actually act on punctuation tokens, or merely accept them?
    ///
    /// The vocabulary containing `.` and `,` proves nothing about whether they
    /// change the audio. Real pauses make the output measurably longer, so the
    /// same phonemes with and without punctuation should differ in duration.
    ///
    ///     cargo test --release punctuation_changes -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn punctuation_changes_the_audio() {
        let k = kokoro();
        if !k.model_path().exists() {
            eprintln!("model absent, skipping");
            return;
        }
        let cfg = VoiceConfig {
            engine: super::super::Engine::Kokoro,
            voice: "af_heart".into(),
            speed: 1.0,
            exaggeration: 0.0,
        };
        k.warm().await.expect("warm");

        // Phonemes taken straight from espeak, then the same with punctuation
        // inserted where the original text had it.
        let flat = "wˈeɪtɹˈiəlijˈɛs";
        let punctuated = "wˈeɪt, ɹˈiəli? jˈɛs!";

        for (label, phonemes) in [("without punctuation", flat), ("with punctuation", punctuated)] {
            let seg = k
                .synthesize_phonemes(phonemes, &cfg)
                .await
                .expect("synthesis");
            let secs = seg.pcm.len() as f32 / seg.sample_rate as f32;
            eprintln!("  {label:<22} {phonemes:<28} -> {secs:.2}s");
        }
        eprintln!("  (longer with punctuation means the model is pausing)");
    }

    /// Writes a sample out so it can actually be listened to. Numbers only tell
    /// you the model ran; whether it sounds like speech is a different question,
    /// and the dictation audio taught us that lesson already.
    ///
    /// Ignored by default because it writes a file:
    ///     cargo test write_a_sample -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn write_a_sample_for_listening() {
        let k = kokoro();
        if !k.model_path().exists() {
            eprintln!("model absent, skipping");
            return;
        }
        let cfg = VoiceConfig {
            engine: super::super::Engine::Kokoro,
            voice: "af_heart".into(),
            speed: 1.0,
            exaggeration: 0.0,
        };
        let text = "Hello. This is naxvoice reading your selection aloud, \
                    including Naxcrow, Paystack and idempotency.";

        let started = std::time::Instant::now();
        let segment = k.synthesize(text, &cfg).await.expect("synthesis");
        let elapsed = started.elapsed();

        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: segment.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let path = "/tmp/naxvoice-kokoro.wav";
        let mut w = hound::WavWriter::create(path, spec).expect("create wav");
        for s in &segment.pcm {
            w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).unwrap();
        }
        w.finalize().expect("finalize");

        let seconds = segment.pcm.len() as f32 / segment.sample_rate as f32;
        eprintln!(
            "wrote {path}: {seconds:.2}s of audio, synthesised in {elapsed:?} \
             ({:.1}x realtime)",
            seconds / elapsed.as_secs_f32()
        );
    }

    /// The weights are downloaded rather than committed, so this skips on a
    /// fresh clone instead of failing.
    #[tokio::test]
    async fn synthesises_real_audio_when_the_model_is_present() {
        let k = kokoro();
        if !k.model_path().exists() {
            eprintln!("model absent at {}, skipping", k.model_path().display());
            return;
        }

        let cfg = VoiceConfig {
            engine: super::super::Engine::Kokoro,
            voice: "af_heart".into(),
            speed: 1.0,
            exaggeration: 0.0,
        };

        let segment = k
            .synthesize("Hello. This is naxvoice reading aloud.", &cfg)
            .await
            .expect("synthesis should succeed");

        assert_eq!(segment.sample_rate, SAMPLE_RATE);
        assert!(
            segment.pcm.len() > SAMPLE_RATE as usize / 4,
            "expected more than a quarter second, got {} samples",
            segment.pcm.len()
        );
        // Silence would mean the model ran but produced nothing audible.
        let peak = segment.pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.01, "output is silent, peak {peak}");
    }
}
