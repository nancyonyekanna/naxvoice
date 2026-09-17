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
/// against roughly 1480ms of synthesis per spoken second.
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
        let parts = espeak_rs::text_to_phonemes(text, "en-us", None)
            .map_err(|e| anyhow!("espeak failed: {e}"))?;
        Ok(parts.join(" "))
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

#[async_trait::async_trait]
impl Synthesizer for Kokoro {
    async fn synthesize(&self, text: &str, cfg: &VoiceConfig) -> Result<AudioSegment> {
        let phonemes = self.phonemes(text)?;

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
