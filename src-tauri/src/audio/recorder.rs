//! Microphone capture, split into chunks at natural pauses.
//!
//! The device dictates the format, not us. The built-in mic on this machine
//! offers F32 only, at 44.1/48/88.2/96kHz — never 16kHz and never i16, which are
//! the two things `audio::vad` and the transcription path both want. So capture
//! runs at 48kHz mono F32 and converts on the way out: low-pass, decimate 3:1,
//! then f32 to i16. 48000/16000 is exactly 3, so there is no fractional
//! resampling and no resampler dependency.
//!
//! The low-pass is not optional. Dropping every third sample on its own folds
//! everything above 8kHz back down into the speech band as a metallic artifact,
//! which would degrade transcription in a way that is very hard to trace back to
//! this file. `attenuates_what_would_otherwise_alias` is the test that keeps it
//! honest.
//!
//! Chunks leave through a channel as they close, while the user is still
//! talking, so on key release only the tail is still in flight. Each chunk
//! carries `overlap` worth of the previous one's audio, which is what lets
//! `stt::stitch` drop words duplicated across a seam.
//!
//! Capture runs on its own thread. The Silero session is built there and never
//! leaves, so nothing depends on ort's `Session` or cpal's `Stream` being `Send`
//! — which matters because the Windows backend is still unverified.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SupportedStreamConfig};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use super::vad::{Boundary, ChunkDetector, ChunkPolicy, CloseReason, SileroSession, FRAME_SAMPLES};

/// What `audio::vad` and the transcription endpoint both expect.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// Offered by the device and an exact multiple of the target.
pub const CAPTURE_SAMPLE_RATE: u32 = 48_000;
/// CAPTURE_SAMPLE_RATE / TARGET_SAMPLE_RATE.
pub const DECIMATION: usize = 3;

/// FIR length. 63 taps at 48kHz gives roughly 50dB of stopband rejection, which
/// puts aliased energy far enough down to be inaudible and harmless to decoding.
const FILTER_TAPS: usize = 63;
/// Cutoff in Hz. Below the 8kHz Nyquist of the target rate, with room for the
/// filter's transition band to roll off before it gets there.
const CUTOFF_HZ: f32 = 7_000.0;

/// One segment of speech, ready to upload.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Dispatch order. `stt::stitch` reassembles by this, because chunks resolve
    /// out of order.
    pub index: usize,
    pub samples: Vec<i16>,
    pub reason: CloseReason,
}

impl Chunk {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / TARGET_SAMPLE_RATE as f64)
    }
}

/// What a finished dictation amounted to. The audio itself already left as
/// chunks; this is for logging and for playing the file back when something
/// sounds wrong.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub path: PathBuf,
    pub chunks: usize,
    pub duration: Duration,
    pub samples: usize,
}

/// Where recordings are written. Kept next to the app's other data so a
/// dictation survives the run that produced it and can be played back.
pub fn default_output_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("com", "naxvoice", "naxvoice")
        .context("locating the app data directory")?;
    Ok(dirs.data_dir().join("recordings"))
}

pub struct Recorder {
    output_dir: PathBuf,
    policy: ChunkPolicy,
    session: Mutex<Option<Session>>,
}

/// The live half of a recording: a way to ask the capture thread to stop, and a
/// way to hear back what it produced.
struct Session {
    stop: Sender<()>,
    done: Receiver<Result<SessionSummary>>,
}

impl Recorder {
    pub fn new(output_dir: PathBuf, policy: ChunkPolicy) -> Self {
        Self { output_dir, policy, session: Mutex::new(None) }
    }

    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    pub fn is_recording(&self) -> bool {
        self.session.lock().map(|s| s.is_some()).unwrap_or(false)
    }

    /// Opens the mic and starts chunking.
    ///
    /// Chunks arrive on the returned channel as they close. The channel ends
    /// when capture stops, which is how the consumer knows the last one has
    /// been sent without needing a separate signal.
    ///
    /// A second press while already recording is refused rather than restarting:
    /// global hotkeys can repeat, and a spurious repeat should not throw away
    /// the dictation in progress.
    pub fn start(&self) -> Result<UnboundedReceiver<Chunk>> {
        let mut slot = self.session.lock().map_err(|_| anyhow!("recorder lock poisoned"))?;
        if slot.is_some() {
            bail!("already recording");
        }

        std::fs::create_dir_all(&self.output_dir)
            .with_context(|| format!("creating {}", self.output_dir.display()))?;

        let path = self.output_dir.join(next_filename());
        let (stop_tx, stop_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let (chunk_tx, chunk_rx) = unbounded_channel();

        let policy = self.policy.clone();
        thread::Builder::new()
            .name("naxvoice-capture".into())
            .spawn(move || {
                let result = capture(&path, policy, stop_rx, chunk_tx);
                let _ = done_tx.send(result);
            })
            .context("spawning the capture thread")?;

        *slot = Some(Session { stop: stop_tx, done: done_rx });
        Ok(chunk_rx)
    }

    /// Stops capture and waits for the final chunk to be dispatched.
    pub fn stop(&self) -> Result<SessionSummary> {
        let mut slot = self.session.lock().map_err(|_| anyhow!("recorder lock poisoned"))?;
        let session = slot.take().context("not recording")?;

        // If the thread already died the send fails, and `done` carries the
        // reason — so the error the caller sees is the real one, not this.
        let _ = session.stop.send(());

        session
            .done
            .recv_timeout(Duration::from_secs(5))
            .context("capture thread did not report back")?
    }
}

/// Epoch seconds keep filenames sortable and unique without a date-formatting
/// dependency for what is a debugging artifact.
fn next_filename() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("dictation-{secs}.wav")
}

/// Picks 48kHz mono F32, the one config that is both offered by the device and
/// an exact multiple of the target rate.
fn pick_config(device: &cpal::Device) -> Result<SupportedStreamConfig> {
    let supported = device
        .supported_input_configs()
        .context("querying supported input configs")?;

    supported
        .filter(|c| c.channels() == 1 && c.sample_format() == SampleFormat::F32)
        .find_map(|c| c.try_with_sample_rate(CAPTURE_SAMPLE_RATE))
        .with_context(|| {
            format!(
                "no mono F32 input at {CAPTURE_SAMPLE_RATE}Hz. Run with \
                 NAXVOICE_LOG=naxvoice=debug to see what the device does offer"
            )
        })
}

/// Runs one dictation start to finish on the capture thread.
fn capture(
    path: &Path,
    policy: ChunkPolicy,
    stop: Receiver<()>,
    chunks: UnboundedSender<Chunk>,
) -> Result<SessionSummary> {
    let host = cpal::default_host();
    let device = host.default_input_device().context("no default input device")?;

    if let Ok(desc) = device.description() {
        tracing::debug!(device = desc.name(), "input device");
    }

    let supported = pick_config(&device)?;
    tracing::debug!(
        channels = supported.channels(),
        sample_rate = supported.sample_rate(),
        format = ?supported.sample_format(),
        "capture config"
    );

    // Built here so the session never crosses a thread boundary.
    let mut detector = ChunkDetector::new(policy, SileroSession::new(0.5)?);

    // Ask the detector rather than recomputing the formula here. Two copies of
    // "how much audio carries into the next chunk" is how the stitcher and the
    // recorder quietly stop agreeing.
    let overlap_samples = detector.overlap_samples(TARGET_SAMPLE_RATE);

    let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>();
    let stream = device
        .build_input_stream::<f32, _, _>(
            supported.config(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Copy and hand off immediately. This runs on a realtime audio
                // thread, where blocking or allocating heavily causes dropouts.
                let _ = audio_tx.send(data.to_vec());
            },
            move |err| {
                tracing::error!(error = %err, "input stream error");
            },
            None,
        )
        .context("building the input stream")?;

    stream.play().context("starting the input stream")?;

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    // The whole dictation is still written to disk. It is no longer uploaded —
    // chunks are — but being able to play back what the mic actually heard is
    // how audio problems get diagnosed at all.
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("creating {}", path.display()))?;

    let mut down = Downsampler::new();
    let mut converted = Vec::new();
    let mut pending: Vec<i16> = Vec::new();
    let mut frame_cursor = 0usize;
    let mut index = 0usize;
    let mut written = 0usize;

    let close = |pending: &mut Vec<i16>, cursor: &mut usize, index: &mut usize, reason: CloseReason| {
        if pending.is_empty() {
            return;
        }
        let samples = std::mem::take(pending);
        // Carry the tail into the next chunk so a word straddling the boundary
        // appears in both and the stitcher can dedupe it.
        let keep = overlap_samples.min(samples.len());
        *pending = samples[samples.len() - keep..].to_vec();
        *cursor = pending.len();

        tracing::debug!(index = *index, samples = samples.len(), ?reason, "chunk closed");
        let _ = chunks.send(Chunk { index: *index, samples, reason });
        *index += 1;
    };

    loop {
        match audio_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(block) => {
                down.push(&block, &mut converted);
                for s in converted.drain(..) {
                    writer.write_sample(s)?;
                    written += 1;
                    pending.push(s);
                }

                // Feed whole frames only: Silero rejects any other window size.
                while pending.len() - frame_cursor >= FRAME_SAMPLES {
                    let frame: Vec<i16> =
                        pending[frame_cursor..frame_cursor + FRAME_SAMPLES].to_vec();
                    frame_cursor += FRAME_SAMPLES;

                    if let Boundary::Close(reason) = detector.push_frame(&frame) {
                        detector.reset();
                        close(&mut pending, &mut frame_cursor, &mut index, reason);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        match stop.try_recv() {
            Ok(()) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
    }

    // Stop the mic first, then drain whatever the callback already queued, so
    // the tail of the utterance is not clipped off.
    drop(stream);
    while let Ok(block) = audio_rx.try_recv() {
        down.push(&block, &mut converted);
        for s in converted.drain(..) {
            writer.write_sample(s)?;
            written += 1;
            pending.push(s);
        }
    }

    // Whatever is left is the final chunk — unless there is no speech in it.
    // Holding the key for several seconds after the last word is normal, and
    // that silence would otherwise be uploaded and transcribed on the release
    // path, which is exactly the stretch the latency budget measures.
    if detector.saw_speech() {
        close(&mut pending, &mut frame_cursor, &mut index, CloseReason::Pause);
    } else if !pending.is_empty() {
        tracing::debug!(
            samples = pending.len(),
            "final chunk held no speech, skipping the upload"
        );
    }

    writer.finalize().context("finalizing the wav file")?;

    Ok(SessionSummary {
        path: path.to_path_buf(),
        chunks: index,
        duration: Duration::from_secs_f64(written as f64 / TARGET_SAMPLE_RATE as f64),
        samples: written,
    })
}

/// Wraps raw samples in a WAV container in memory, for upload.
///
/// The transcription endpoint decides the codec from the filename and content
/// type, so the bytes have to be a real container rather than bare PCM.
pub fn chunk_to_wav(samples: &[i16]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut buffer = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut buffer, spec).context("starting a wav chunk")?;
        for &s in samples {
            writer.write_sample(s)?;
        }
        writer.finalize().context("finalizing a wav chunk")?;
    }
    Ok(buffer.into_inner())
}

/// Low-pass then decimate by `DECIMATION`, emitting i16 at the target rate.
pub struct Downsampler {
    taps: Vec<f32>,
    history: Vec<f32>,
    pos: usize,
    phase: usize,
}

impl Downsampler {
    pub fn new() -> Self {
        let taps = design_lowpass(FILTER_TAPS, CUTOFF_HZ / CAPTURE_SAMPLE_RATE as f32);
        Self { history: vec![0.0; taps.len()], taps, pos: 0, phase: 0 }
    }

    /// Feeds captured samples in and appends converted ones to `out`.
    pub fn push(&mut self, input: &[f32], out: &mut Vec<i16>) {
        let n = self.history.len();
        for &x in input {
            self.history[self.pos] = x;
            self.pos = (self.pos + 1) % n;

            self.phase += 1;
            if self.phase == DECIMATION {
                self.phase = 0;
                out.push(to_i16(self.filtered()));
            }
        }
    }

    fn filtered(&self) -> f32 {
        let n = self.history.len();
        self.taps
            .iter()
            .enumerate()
            .map(|(k, &tap)| tap * self.history[(self.pos + n - 1 - k) % n])
            .sum()
    }
}

impl Default for Downsampler {
    fn default() -> Self {
        Self::new()
    }
}

/// Windowed-sinc low-pass, Hamming window, normalized to unity gain at DC so
/// the filter changes the spectrum without changing the loudness.
fn design_lowpass(taps: usize, cutoff_ratio: f32) -> Vec<f32> {
    use std::f32::consts::PI;

    let m = (taps - 1) as f32;
    let mut h = Vec::with_capacity(taps);
    let mut sum = 0.0;

    for i in 0..taps {
        let n = i as f32 - m / 2.0;
        let sinc = if n.abs() < f32::EPSILON {
            2.0 * cutoff_ratio
        } else {
            (2.0 * PI * cutoff_ratio * n).sin() / (PI * n)
        };
        let window = 0.54 - 0.46 * (2.0 * PI * i as f32 / m).cos();
        let v = sinc * window;
        h.push(v);
        sum += v;
    }

    for v in h.iter_mut() {
        *v /= sum;
    }
    h
}

/// Clamps rather than wrapping. An overdriven mic should sound clipped, not
/// inverted, and inversion is what a bare `as i16` cast produces.
fn to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: f32, n: usize, amplitude: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin() * amplitude)
            .collect()
    }

    /// Peak of the settled part, skipping the filter's warm-up.
    fn settled_peak(out: &[i16]) -> f32 {
        let skip = FILTER_TAPS;
        out[skip..].iter().map(|&s| (s as f32 / i16::MAX as f32).abs()).fold(0.0, f32::max)
    }

    fn run(freq: f32) -> Vec<i16> {
        let input = sine(freq, CAPTURE_SAMPLE_RATE as f32, 9_600, 0.9);
        let mut d = Downsampler::new();
        let mut out = Vec::new();
        d.push(&input, &mut out);
        out
    }

    #[test]
    fn speech_passes_through_at_its_original_level() {
        // 300Hz is well inside the passband and should survive intact.
        let peak = settled_peak(&run(300.0));
        assert!(peak > 0.8, "300Hz should pass, peak was {peak}");
    }

    #[test]
    fn attenuates_what_would_otherwise_alias() {
        // 12kHz is above the 8kHz Nyquist of the target rate. Undetected, it
        // would fold down to 4kHz and sit right on top of the speech.
        let peak = settled_peak(&run(12_000.0));
        assert!(peak < 0.05, "12kHz should be rejected, peak was {peak}");
    }

    #[test]
    fn emits_one_sample_per_three_captured() {
        let input = vec![0.0f32; 3_000];
        let mut d = Downsampler::new();
        let mut out = Vec::new();
        d.push(&input, &mut out);
        assert_eq!(out.len(), 1_000);
    }

    #[test]
    fn decimation_ratio_matches_the_two_rates() {
        assert_eq!(CAPTURE_SAMPLE_RATE as usize / TARGET_SAMPLE_RATE as usize, DECIMATION);
    }

    #[test]
    fn loud_input_clips_instead_of_inverting() {
        // A bare `as i16` cast wraps, turning a loud peak into a loud peak of
        // the opposite sign — an audible click exactly where it hurts.
        assert_eq!(to_i16(1.5), i16::MAX);
        assert_eq!(to_i16(-1.5), -i16::MAX);
        assert_eq!(to_i16(0.0), 0);
    }

    #[test]
    fn filter_has_unity_gain_at_dc() {
        let taps = design_lowpass(FILTER_TAPS, CUTOFF_HZ / CAPTURE_SAMPLE_RATE as f32);
        let sum: f32 = taps.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4, "DC gain was {sum}");
    }

    /// A chunk has to arrive as a real container, not bare PCM: the endpoint
    /// picks its decoder from the filename and content type.
    #[test]
    fn a_chunk_becomes_a_readable_wav() {
        let samples: Vec<i16> = (0..1_600).map(|i| (i % 100) as i16).collect();
        let bytes = chunk_to_wav(&samples).unwrap();

        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");

        let mut reader = hound::WavReader::new(Cursor::new(bytes)).unwrap();
        assert_eq!(reader.spec().sample_rate, TARGET_SAMPLE_RATE);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(reader.samples::<i16>().count(), samples.len());
    }

    #[test]
    fn chunk_duration_is_derived_from_the_target_rate() {
        let c = Chunk {
            index: 0,
            samples: vec![0; TARGET_SAMPLE_RATE as usize / 2],
            reason: CloseReason::Pause,
        };
        assert_eq!(c.duration(), Duration::from_millis(500));
    }
}
