//! Interruptible playback for synthesised speech.
//!
//! Synthesis runs at roughly 1480ms per spoken second — measured, and flat
//! across sentence lengths — so waiting for a whole passage before making a
//! sound would leave seconds of silence. Segments are therefore played as they
//! arrive, while later ones are still being rendered.
//!
//! They arrive out of order, because renders are dispatched concurrently, so
//! the player reorders by index exactly as `stt::stitch` does for transcripts.
//!
//! Stopping has to be immediate. A read-aloud you cannot interrupt is worse
//! than one that never started, so `stop` empties the queue rather than letting
//! the buffered audio drain.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::tts::AudioSegment;

/// Shared between the feeder and the audio callback.
#[derive(Default)]
struct Buffer {
    samples: std::collections::VecDeque<f32>,
}

pub struct Player {
    playing: Arc<AtomicBool>,
    buffer: Arc<Mutex<Buffer>>,
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Player {
    pub fn new() -> Self {
        Self {
            playing: Arc::new(AtomicBool::new(false)),
            buffer: Arc::new(Mutex::new(Buffer::default())),
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    /// Silences playback immediately and discards anything queued.
    pub fn stop(&self) {
        self.playing.store(false, Ordering::Relaxed);
        if let Ok(mut b) = self.buffer.lock() {
            b.samples.clear();
        }
    }

    /// Plays segments as they arrive, returning once the audio has drained or
    /// `stop` was called.
    ///
    /// Runs on its own thread: the cpal stream is built there and never crosses
    /// a thread boundary, the same discipline `recorder.rs` uses.
    pub fn play(&self, mut segments: UnboundedReceiver<AudioSegment>) -> Result<()> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .context("no default output device")?;
        let config = device
            .default_output_config()
            .context("querying the default output config")?;

        let device_rate = config.sample_rate();
        let channels = config.channels() as usize;

        {
            let mut b = self.buffer.lock().map_err(|_| anyhow!("player lock poisoned"))?;
            b.samples.clear();
        }
        self.playing.store(true, Ordering::Relaxed);

        let buffer = Arc::clone(&self.buffer);
        let playing = Arc::clone(&self.playing);

        let cb_buffer = Arc::clone(&buffer);
        let stream = match config.sample_format() {
            SampleFormat::F32 => device.build_output_stream::<f32, _, _>(
                config.config(),
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    fill(out, channels, &cb_buffer);
                },
                move |e| tracing::error!(error = %e, "output stream error"),
                None,
            ),
            other => {
                anyhow::bail!("unsupported output format {other:?}; only f32 is handled")
            }
        }
        .context("building the output stream")?;

        stream.play().context("starting playback")?;

        // Feed the buffer in index order. Segments render concurrently and can
        // arrive out of order; a gap must not let a later one jump the queue.
        let mut pending: BTreeMap<usize, Vec<f32>> = BTreeMap::new();
        let mut next = 0usize;

        while playing.load(Ordering::Relaxed) {
            let Some(segment) = segments.blocking_recv() else { break };

            let resampled = resample(&segment.pcm, segment.sample_rate, device_rate);
            pending.insert(segment.index, resampled);

            while let Some(ready) = pending.remove(&next) {
                if let Ok(mut b) = buffer.lock() {
                    b.samples.extend(ready);
                }
                next += 1;
            }
        }

        // Whatever arrived out of order and never became contiguous still gets
        // played rather than dropped silently.
        for (_, leftover) in std::mem::take(&mut pending) {
            if let Ok(mut b) = buffer.lock() {
                b.samples.extend(leftover);
            }
        }

        // Wait for the queue to drain, unless stopped.
        while playing.load(Ordering::Relaxed) {
            let remaining = buffer.lock().map(|b| b.samples.len()).unwrap_or(0);
            if remaining == 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        drop(stream);
        self.playing.store(false, Ordering::Relaxed);
        Ok(())
    }
}

/// Copies queued samples into the output, padding with silence when starved.
///
/// Mono synthesis is written to every channel: a voice that comes out of one
/// earpiece sounds broken.
fn fill(out: &mut [f32], channels: usize, buffer: &Arc<Mutex<Buffer>>) {
    let Ok(mut b) = buffer.lock() else {
        out.fill(0.0);
        return;
    };
    for frame in out.chunks_mut(channels.max(1)) {
        let sample = b.samples.pop_front().unwrap_or(0.0);
        frame.fill(sample);
    }
}

/// Linear resampling from the synthesiser's rate to the device's.
///
/// Kokoro emits 24kHz and output devices usually run at 48kHz. That happens to
/// be an exact 2:1 ratio, but nothing guarantees it — a device at 44.1kHz is
/// perfectly ordinary — so this interpolates rather than assuming a whole
/// number. Unlike the capture path there is no anti-aliasing concern, because
/// upsampling adds no frequencies above the original Nyquist.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }

    let ratio = to as f64 / from as f64;
    let out_len = (input.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let pos = i as f64 / ratio;
        let left = pos.floor() as usize;
        let frac = (pos - left as f64) as f32;
        let a = input.get(left).copied().unwrap_or(0.0);
        let b = input.get(left + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_rates_are_passed_through_untouched() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample(&input, 24_000, 24_000), input);
    }

    #[test]
    fn doubling_the_rate_doubles_the_sample_count() {
        let input = vec![0.0, 1.0, 0.0, 1.0];
        let out = resample(&input, 24_000, 48_000);
        assert_eq!(out.len(), 8);
    }

    #[test]
    fn a_non_integer_ratio_still_works() {
        // 24k -> 44.1k is not a whole number, and is an ordinary device rate.
        let input = vec![0.5; 240];
        let out = resample(&input, 24_000, 44_100);
        assert_eq!(out.len(), 441);
        // A constant signal must stay constant through interpolation.
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6), "interpolation distorted a constant");
    }

    #[test]
    fn interpolation_lands_between_neighbours() {
        let out = resample(&[0.0, 1.0], 1_000, 2_000);
        // Midpoints must interpolate, not repeat the previous sample.
        assert!(out.iter().any(|s| *s > 0.1 && *s < 0.9), "got {out:?}");
    }

    #[test]
    fn empty_input_does_not_panic() {
        assert!(resample(&[], 24_000, 48_000).is_empty());
    }

    #[test]
    fn mono_is_written_to_every_channel() {
        let buffer = Arc::new(Mutex::new(Buffer {
            samples: vec![0.5, 0.25].into(),
        }));
        let mut out = [0.0f32; 4];
        fill(&mut out, 2, &buffer);
        // Stereo device: each mono sample fills both channels of its frame.
        assert_eq!(out, [0.5, 0.5, 0.25, 0.25]);
    }

    #[test]
    fn starved_output_is_silence_rather_than_noise() {
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        let mut out = [1.0f32; 4];
        fill(&mut out, 1, &buffer);
        assert_eq!(out, [0.0; 4], "an empty queue must produce silence");
    }

    #[test]
    fn stop_discards_queued_audio() {
        let p = Player::new();
        if let Ok(mut b) = p.buffer.lock() {
            b.samples.extend([0.1, 0.2, 0.3]);
        }
        p.playing.store(true, Ordering::Relaxed);
        p.stop();
        assert!(!p.is_playing());
        assert_eq!(p.buffer.lock().unwrap().samples.len(), 0, "stop must not let audio drain");
    }
}
