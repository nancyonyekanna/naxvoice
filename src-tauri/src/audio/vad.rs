//! Voice activity detection and chunk boundary logic.
//!
//! This is where the perceived latency is won. Instead of buffering the whole
//! utterance and sending one request on key release, we watch for natural pauses
//! and close a chunk at each one. Each closed chunk uploads immediately while the
//! user is still speaking, so on release only the tail is in flight.
//!
//! Two rules that matter:
//!   - Cut on silence, never on a timer. A timer cut lands mid-word.
//!   - Carry `overlap_ms` of audio into the next chunk so a word straddling the
//!     boundary appears in both and the stitcher can dedupe it.

use std::time::Duration;

use anyhow::{Context, Result};

/// 512 samples at 16kHz. Not a free choice: Silero rejects any other window
/// size outright ("Supported values: 256 for 8000 sample rate, 512 for 16000"),
/// so the frame length is set by the model rather than by us.
pub const FRAME_SAMPLES: usize = 512;

/// 512 / 16000, rounded to the millisecond. Was 30ms before the VAD was wired
/// up, which would have been 480 samples and rejected on every call.
const FRAME_MS: usize = 32;

/// Silero keeps 64 samples of the previous window as context and prepends it,
/// so each inference actually sees 576 samples.
const CONTEXT_SAMPLES: usize = 64;

/// Shape of the recurrent state carried between calls: (2, batch, 128).
const STATE_LEN: usize = 2 * 128;

/// The 16kHz-only build of Silero v5. Verified to produce output identical to
/// the 2.3MB general model while being half the size, and embedded rather than
/// downloaded because a failed download here breaks dictation entirely.
const MODEL: &[u8] = include_bytes!("../../assets/silero_vad_16k_op15.onnx");

#[derive(Debug, Clone)]
pub struct ChunkPolicy {
    pub pause_threshold: Duration,
    pub overlap: Duration,
    pub min_chunk: Duration,
    pub max_chunk: Duration,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self {
            pause_threshold: Duration::from_millis(450),
            overlap: Duration::from_millis(200),
            min_chunk: Duration::from_millis(600),
            max_chunk: Duration::from_millis(25_000),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Boundary {
    /// Keep buffering.
    Continue,
    /// Close the chunk here and dispatch it. Carries the reason for logging.
    Close(CloseReason),
}

/// Copy because it rides along on every `Chunk`, which is cloned when a
/// dictation is dispatched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// Natural pause. The good case.
    Pause,
    /// Hit max_chunk. Forced cut, may land mid-word — the stitcher relies on
    /// overlap to recover. Only happens if someone talks for 25s without pausing.
    MaxLength,
}

pub struct ChunkDetector {
    policy: ChunkPolicy,
    /// Silero VAD session. Small ONNX model, ~1ms per frame on CPU.
    session: SileroSession,
    silence_run: Duration,
    chunk_len: Duration,
    saw_speech: bool,
}

impl ChunkDetector {
    pub fn new(policy: ChunkPolicy, session: SileroSession) -> Self {
        Self {
            policy,
            session,
            silence_run: Duration::ZERO,
            chunk_len: Duration::ZERO,
            saw_speech: false,
        }
    }

    /// Feed one 30ms frame of 16kHz mono PCM. Call this from the capture thread.
    pub fn push_frame(&mut self, frame: &[i16]) -> Boundary {
        let frame_dur = Duration::from_millis(FRAME_MS as u64);
        self.chunk_len += frame_dur;

        let is_speech = self.session.is_speech(frame);

        if is_speech {
            self.saw_speech = true;
            self.silence_run = Duration::ZERO;
        } else {
            self.silence_run += frame_dur;
        }

        // Never close a chunk that contains no speech — that's just the user
        // holding the key while thinking. Let it keep buffering.
        if !self.saw_speech {
            return Boundary::Continue;
        }

        if self.chunk_len >= self.policy.max_chunk {
            return Boundary::Close(CloseReason::MaxLength);
        }

        if self.silence_run >= self.policy.pause_threshold
            && self.chunk_len >= self.policy.min_chunk
        {
            return Boundary::Close(CloseReason::Pause);
        }

        Boundary::Continue
    }

    /// Call after dispatching a chunk. Resets counters but the caller is
    /// responsible for retaining `overlap` worth of samples in the new buffer.
    pub fn reset(&mut self) {
        self.silence_run = Duration::ZERO;
        self.chunk_len = Duration::ZERO;
        self.saw_speech = false;
    }

    /// Whether the chunk being built has any speech in it yet.
    ///
    /// The last chunk of a dictation is often pure silence: the key stays down
    /// for seconds after the final word. Measured on a real recording, 6.56s of
    /// the remainder was 0% speech. Uploading that spends a round trip on the
    /// release path — the one stretch the latency budget is about — to
    /// transcribe nothing, so the caller can look here and skip it.
    pub fn saw_speech(&self) -> bool {
        self.saw_speech
    }

    pub fn overlap_samples(&self, sample_rate: u32) -> usize {
        (self.policy.overlap.as_millis() as usize * sample_rate as usize) / 1000
    }

    /// True once a pause has been seen and a chunk is in flight. The cleanup
    /// pass uses this to fire speculatively rather than waiting for key release.
    pub fn ready_for_speculative_cleanup(&self) -> bool {
        self.saw_speech && self.silence_run >= self.policy.pause_threshold
    }
}

/// Thin wrapper over the Silero ONNX model.
///
/// The model is stateful in two ways, and both have to be carried between calls
/// or the probabilities are nonsense: a recurrent `state` tensor that comes back
/// out as `stateN`, and the trailing 64 samples of the previous window.
///
/// Build this on the thread that will feed it. Doing so means the session never
/// has to cross a thread boundary, so nothing here depends on ort's `Session`
/// being `Send`.
pub struct SileroSession {
    session: ort::session::Session,
    state: Vec<f32>,
    context: Vec<f32>,
    threshold: f32,
}

impl SileroSession {
    pub fn new(threshold: f32) -> Result<Self> {
        let session = ort::session::Session::builder()
            .and_then(|mut b| b.commit_from_memory(MODEL))
            .context("loading the embedded Silero VAD model")?;

        Ok(Self {
            session,
            state: vec![0.0; STATE_LEN],
            context: vec![0.0; CONTEXT_SAMPLES],
            threshold,
        })
    }

    /// True when the frame is speech.
    ///
    /// Fails **open** — a broken model reports speech rather than silence. That
    /// is deliberate: `ChunkDetector` never closes a chunk it has seen no speech
    /// in, so failing closed would mean chunks never close at all and the
    /// dictation vanishes. Failing open degrades to max-length chunks, which is
    /// slow but still produces a transcript.
    pub fn is_speech(&mut self, frame: &[i16]) -> bool {
        match self.probability(frame) {
            Ok(p) => p >= self.threshold,
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "VAD inference failed, assuming speech");
                true
            }
        }
    }

    fn probability(&mut self, frame: &[i16]) -> Result<f32> {
        use ort::value::Tensor;

        if frame.len() != FRAME_SAMPLES {
            anyhow::bail!("Silero needs {FRAME_SAMPLES} samples, got {}", frame.len());
        }

        // Context first, then the window: the model is fed 64 + 512 samples.
        let mut input = Vec::with_capacity(CONTEXT_SAMPLES + FRAME_SAMPLES);
        input.extend_from_slice(&self.context);
        input.extend(frame.iter().map(|&s| s as f32 / 32768.0));

        let audio = Tensor::from_array(([1usize, input.len()], input.clone()))?;
        let state = Tensor::from_array(([2usize, 1usize, 128usize], self.state.clone()))?;
        let rate = Tensor::from_array(([1usize], vec![16_000i64]))?;

        let outputs = self
            .session
            .run(ort::inputs!["input" => audio, "state" => state, "sr" => rate])
            .context("running Silero")?;

        let probability = {
            let value = outputs.get("output").context("Silero returned no output")?;
            let (_, data) = value.try_extract_tensor::<f32>()?;
            *data.first().context("empty probability tensor")?
        };

        // Carry the recurrent state forward, or every frame is judged in isolation.
        if let Some(next) = outputs.get("stateN") {
            if let Ok((_, data)) = next.try_extract_tensor::<f32>() {
                self.state.clear();
                self.state.extend_from_slice(data);
            }
        }
        self.context.clear();
        self.context.extend_from_slice(&input[input.len() - CONTEXT_SAMPLES..]);

        Ok(probability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detector() -> ChunkDetector {
        ChunkDetector::new(ChunkPolicy::default(), SileroSession::new(0.5).unwrap())
    }

    /// The embedded model has to load, or dictation has no way to find a pause.
    #[test]
    fn the_embedded_model_loads() {
        assert!(SileroSession::new(0.5).is_ok());
    }

    /// Silence really must read as silence, otherwise chunks only ever close on
    /// max length and the whole latency win disappears.
    #[test]
    fn silence_scores_below_the_threshold() {
        let mut s = SileroSession::new(0.5).unwrap();
        let quiet = vec![0i16; FRAME_SAMPLES];
        // A few frames, because the recurrent state needs to settle.
        for _ in 0..5 {
            assert!(!s.is_speech(&quiet));
        }
    }

    /// A wrong window size is rejected by the model rather than silently
    /// producing a meaningless probability.
    #[test]
    fn the_frame_size_matches_what_silero_demands() {
        assert_eq!(FRAME_SAMPLES, 512);
        assert_eq!(FRAME_SAMPLES * 1000 / 16_000, FRAME_MS);
        let mut s = SileroSession::new(0.5).unwrap();
        assert!(s.probability(&vec![0i16; 480]).is_err());
    }

    #[test]
    fn silence_before_any_speech_never_closes() {
        let mut d = detector();
        d.saw_speech = false;
        d.silence_run = Duration::from_secs(5);
        // Guard holds even well past the threshold.
        assert!(!d.ready_for_speculative_cleanup());
    }

    /// Silence alone must never look like speech, or the release path uploads a
    /// silent final chunk for nothing.
    #[test]
    fn silence_alone_never_marks_a_chunk_as_speech() {
        let mut d = detector();
        let quiet = vec![0i16; FRAME_SAMPLES];
        assert!(!d.saw_speech());
        for _ in 0..10 {
            d.push_frame(&quiet);
        }
        assert!(!d.saw_speech(), "silence should not arm the chunk");
    }

    #[test]
    fn overlap_samples_match_rate() {
        let d = detector();
        assert_eq!(d.overlap_samples(16_000), 3_200);
    }
}
