//! Interruptible playback for synthesised speech.
//!
//! Synthesis runs at roughly 585ms per spoken second, measured in release and
//! flat from 29 to 197 characters, so it renders about 1.7x faster than the
//! speech it produces. Segments are still played as they arrive rather than
//! after the whole passage, because the opening unit is pure waiting and there
//! is no reason to add every later unit to it.
//!
//! **An earlier version of this file said the opposite, and built a design on
//! it.** It recorded 1480ms per spoken second, a 1.44x deficit, and 19.2
//! seconds of inserted silence across a 66.2 second read, and concluded the
//! gaps were deliberate and unavoidable. The rate is wrong by about 2.5x, so
//! that conclusion does not follow: an engine which outpaces playback cannot
//! starve this queue on its own account.
//!
//! The observation was real even though the explanation was not. Long reads
//! have paused mid-passage, and `starved_frames` still counts it. The cause is
//! unresolved. Machine load is the leading suspect: the identical measurement
//! taken while the machine was 25GB into swap came back 20 to 40 times worse,
//! which is comfortably the scale that produces audible gaps.
//!
//! Two things still hold regardless of the rate. Silence lands between
//! sentences rather than mid-word, because whole units are queued at once. And
//! splitting the opening *mid-clause* remains forbidden: it produces a falling
//! intonation on the first thing the listener hears. `tts::chunk` cuts the
//! opening at a clause boundary instead, which costs nothing in prosody.
//!
//! Do not reintroduce a pre-buffer on the strength of the old numbers. If gaps
//! appear, measure the machine before changing the code.
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
    /// Frames of silence emitted *after* the first real sample, because
    /// synthesis had not kept up. Counted in frames rather than callbacks so it
    /// converts to milliseconds, and only after audio has begun: the silence
    /// before the first sample is the start delay, not a gap in the speech, and
    /// counting it made the first measurement of this useless.
    starved_frames: Arc<std::sync::atomic::AtomicUsize>,
    /// Whether any real sample has been played yet.
    begun: Arc<AtomicBool>,
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
            starved_frames: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            begun: Arc::new(AtomicBool::new(false)),
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
        self.starved_frames.store(0, Ordering::Relaxed);
        self.begun.store(false, Ordering::Relaxed);

        let buffer = Arc::clone(&self.buffer);
        let playing = Arc::clone(&self.playing);

        let cb_buffer = Arc::clone(&buffer);
        let cb_starved = Arc::clone(&self.starved_frames);
        let cb_begun = Arc::clone(&self.begun);
        let stream = match config.sample_format() {
            SampleFormat::F32 => device.build_output_stream::<f32, _, _>(
                config.config(),
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    fill(out, channels, &cb_buffer, &cb_begun, &cb_starved);
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

        // Reported rather than silently tolerated: this is how long the
        // listener heard nothing mid-passage because synthesis fell behind.
        let frames = self.starved_frames.load(Ordering::Relaxed);
        if frames > 0 {
            tracing::info!(
                ms = frames as u64 * 1000 / device_rate.max(1) as u64,
                frames,
                "silence inserted mid-read because synthesis fell behind"
            );
        }
        Ok(())
    }
}

/// Copies queued samples into the output, padding with silence when starved.
///
/// Counts the padded frames, but only once real audio has started: silence
/// before the first sample is the wait for the first unit to render, not a gap
/// in the speech, and counting both together measures nothing useful.
///
/// Mono synthesis is written to every channel: a voice that comes out of one
/// earpiece sounds broken.
fn fill(
    out: &mut [f32],
    channels: usize,
    buffer: &Arc<Mutex<Buffer>>,
    begun: &AtomicBool,
    starved_frames: &std::sync::atomic::AtomicUsize,
) {
    let Ok(mut b) = buffer.lock() else {
        out.fill(0.0);
        return;
    };

    let mut dry = 0usize;
    for frame in out.chunks_mut(channels.max(1)) {
        match b.samples.pop_front() {
            Some(sample) => {
                begun.store(true, Ordering::Relaxed);
                frame.fill(sample);
            }
            None => {
                dry += 1;
                frame.fill(0.0);
            }
        }
    }

    if dry > 0 && begun.load(Ordering::Relaxed) {
        starved_frames.fetch_add(dry, Ordering::Relaxed);
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
        fill(&mut out, 2, &buffer, &AtomicBool::new(false), &std::sync::atomic::AtomicUsize::new(0));
        // Stereo device: each mono sample fills both channels of its frame.
        assert_eq!(out, [0.5, 0.5, 0.25, 0.25]);
    }

    #[test]
    fn starved_output_is_silence_rather_than_noise() {
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        let mut out = [1.0f32; 4];
        fill(&mut out, 1, &buffer, &AtomicBool::new(false), &std::sync::atomic::AtomicUsize::new(0));
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
