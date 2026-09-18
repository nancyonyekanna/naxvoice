//! Speech synthesis. One engine, because the second one was too slow.
//!
//! The design was two: Kokoro speaks sentence one almost instantly while
//! Chatterbox-Turbo renders the rest behind it with better quality and voice
//! cloning, then hands over. It does not work, and the reason is arithmetic
//! rather than engineering. Measured on Apple silicon, Chatterbox-Turbo costs
//! about 2.1 seconds of compute per spoken second — 60ms per token at 25 tokens
//! per second of audio, plus 644ms per spoken second in its decoder. An engine
//! that renders slower than it speaks can never catch up to playback, so there
//! is nothing for the first sentence to hand off to.
//!
//! Every configuration was measured before giving up: fp16 60ms/token, q4f16
//! 65ms, int8 798ms, and CoreML *worse* than CPU at 105ms. Revisit only on
//! hardware with a usable GPU. See CLAUDE.md step 7.
//!
//! `TtsManager` below is the handoff that was built for that plan. Nothing
//! constructs it — `read_aloud` drives Kokoro directly — and it is kept only
//! because it is the shape a second engine would slot into.

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
