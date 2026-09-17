//! Speech synthesis. Two engines, one perceived voice.
//!
//! Chatterbox-Turbo is autoregressive, so a long passage has real lag before the
//! first sample exists. Kokoro is tiny and near-instant but less expressive.
//! We speak sentence one with Kokoro while Chatterbox renders the rest behind it,
//! then hand over. The listener hears continuous speech starting in ~200ms.
//!
//! The handoff is audible if the two voices are far apart in pitch or pace, so
//! the Kokoro preset should be chosen to sit close to the cloned Chatterbox voice.
//! That pairing is a setup-time decision, not something to fix at runtime.

pub mod chunk;
pub mod kokoro;
pub mod normalize;
pub mod read_aloud;

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Engine {
    Kokoro,
    ChatterboxTurbo,
}

#[derive(Debug, Clone)]
pub struct VoiceConfig {
    pub engine: Engine,
    /// Preset name, or `clone:<path>` for a Chatterbox voice sample.
    pub voice: String,
    pub speed: f32,
    /// Chatterbox only. Default is flat for long reading; 0.4-0.5 is a good start.
    pub exaggeration: f32,
}

/// A rendered piece of audio, tagged with its position so the player keeps order
/// even though renders resolve out of order.
pub struct AudioSegment {
    pub index: usize,
    pub pcm: Vec<f32>,
    pub sample_rate: u32,
}

pub struct TtsManager {
    kokoro: Arc<dyn Synthesizer>,
    chatterbox: Arc<dyn Synthesizer>,
    first: VoiceConfig,
    main: VoiceConfig,
    handoff_after: usize,
}

#[async_trait::async_trait]
pub trait Synthesizer: Send + Sync {
    async fn synthesize(&self, text: &str, cfg: &VoiceConfig) -> Result<AudioSegment>;
    /// Load weights into memory. Call at launch, not on first use — a cold load
    /// on the first hotkey press costs seconds and ruins the impression.
    async fn warm(&self) -> Result<()>;
    fn is_loaded(&self) -> bool;
}

impl TtsManager {
    pub fn new(
        kokoro: Arc<dyn Synthesizer>,
        chatterbox: Arc<dyn Synthesizer>,
        first: VoiceConfig,
        main: VoiceConfig,
        handoff_after: usize,
    ) -> Self {
        Self { kokoro, chatterbox, first, main, handoff_after }
    }

    pub async fn warm_all(&self) {
        let _ = tokio::join!(self.kokoro.warm(), self.chatterbox.warm());
    }

    /// Render `sentences` and push segments to `out` as they complete.
    ///
    /// The first `handoff_after` sentences go to Kokoro sequentially so playback
    /// can start immediately. Everything after is dispatched to Chatterbox
    /// concurrently — the player reorders by index, so completion order is free.
    pub async fn speak(
        &self,
        sentences: Vec<String>,
        out: mpsc::Sender<AudioSegment>,
    ) -> Result<()> {
        let split = self.handoff_after.min(sentences.len());

        for (i, s) in sentences[..split].iter().enumerate() {
            let seg = self.kokoro.synthesize(s, &self.first).await?;
            out.send(AudioSegment { index: i, ..seg }).await.ok();
        }

        let mut tasks = Vec::new();
        for (offset, s) in sentences[split..].iter().enumerate() {
            let engine = Arc::clone(&self.chatterbox);
            let cfg = self.main.clone();
            let tx = out.clone();
            let text = s.clone();
            let index = split + offset;

            tasks.push(tokio::spawn(async move {
                match engine.synthesize(&text, &cfg).await {
                    Ok(seg) => {
                        tx.send(AudioSegment { index, ..seg }).await.ok();
                    }
                    Err(e) => {
                        // One failed sentence must not kill the whole read.
                        // The player treats a missing index as silence and moves on.
                        tracing::warn!(index, error = %e, "synthesis failed, skipping");
                    }
                }
            }));
        }

        for t in tasks {
            let _ = t.await;
        }
        Ok(())
    }
}
