//! OpenRouter transcription client.
//!
//! POST /api/v1/audio/transcriptions — accepts base64 JSON via `input_audio`
//! or OpenAI-style multipart. We use multipart: base64 inflates the payload by
//! a third, which matters on a metered mobile connection.
//!
//! Known limits to design around:
//!   - 60s upstream timeout (chunking keeps us far under)
//!   - 25MB multipart cap (Opus at 24kbps means ~2.3 hours before that bites)
//!   - no audio URLs, no SRT/VTT output, no realtime websocket

use anyhow::{Context, Result};
use reqwest::multipart;
use serde::Deserialize;
use std::time::Duration;

const ENDPOINT: &str = "/audio/transcriptions";

/// How naxvoice identifies itself to OpenRouter.
///
/// Without these two headers every request is anonymous, which means an account
/// that shares one key between several tools cannot tell afterwards which of
/// them spent what. The local ledger in `spend.rs` is what the Status screen
/// reads, but these make the same split visible in OpenRouter's own activity
/// dashboard, which is a useful independent check on our arithmetic.
pub const APP_URL: &str = "https://github.com/nancyonyekanna/naxvoice";
pub const APP_TITLE: &str = "naxvoice";

/// How the uploaded audio is labelled.
///
/// This matters more than it looks. OpenRouter derives the codec from the
/// filename extension or the part's content type — not from the bytes — so
/// mislabelled audio is decoded as the wrong format rather than rejected, and
/// the failure surfaces as a poor transcript instead of an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    /// 16kHz mono PCM, what `audio::recorder` writes today.
    Wav,
    /// Opus in an Ogg container, for when chunked upload lands and the size of
    /// the payload starts to matter.
    Opus,
}

impl AudioFormat {
    fn file_name(self) -> &'static str {
        match self {
            Self::Wav => "chunk.wav",
            Self::Opus => "chunk.opus",
        }
    }

    fn mime(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Opus => "audio/ogg",
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TranscriptionResponse {
    pub text: String,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub seconds: Option<f64>,
    #[serde(default)]
    pub cost: Option<f64>,
}

impl TranscriptionResponse {
    /// What this chunk cost, when the provider priced it.
    ///
    /// `None` is not zero. A provider that returns no price leaves the ledger
    /// unable to account for this chunk, and `spend.rs` records that as a floor
    /// rather than pretending the chunk was free.
    pub fn cost(&self) -> Option<f64> {
        self.usage.as_ref().and_then(|u| u.cost)
    }
}

/// Checks a key against OpenRouter without spending anything.
///
/// `/key` is the same 656-byte endpoint `warm` uses, chosen there because
/// `/models` returns 737KB across 444 models.
///
/// The three outcomes are kept apart on purpose. A 401 means the key is wrong.
/// Anything else unsuccessful is reported as the status it was, and a transport
/// failure says so, because telling someone their key is invalid when their
/// network is down sends them to regenerate a key that was fine.
pub async fn verify(base_url: &str, api_key: &str) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("building the verification client")?;

    let res = http
        .get(format!("{base_url}/key"))
        .bearer_auth(api_key)
        .header("HTTP-Referer", APP_URL)
        .header("X-Title", APP_TITLE)
        .send()
        .await
        .context("could not reach OpenRouter")?;

    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        anyhow::bail!("OpenRouter rejected this key");
    }
    anyhow::bail!("OpenRouter answered {status}");
}

pub struct SttClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    language: String,
}

impl SttClient {
    /// Build once at app launch and keep it for the process lifetime.
    ///
    /// The pool settings are the point. A cold TLS handshake to OpenRouter from
    /// Lagos or Abuja costs 200-400ms, which would otherwise be paid on every
    /// single hotkey press. Keeping idle connections alive and warm means the
    /// first chunk of a dictation reuses an open socket.
    pub fn new(base_url: String, api_key: String, model: String, language: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .pool_idle_timeout(Duration::from_secs(300))
            .pool_max_idle_per_host(4)
            .tcp_keepalive(Duration::from_secs(30))
            .http2_keep_alive_interval(Duration::from_secs(20))
            .http2_keep_alive_while_idle(true)
            .timeout(Duration::from_secs(15))
            .build()
            .context("building stt http client")?;

        Ok(Self { http, base_url, api_key, model, language })
    }

    /// Call on app launch and after any network change. Opens the connection so
    /// the first real dictation doesn't pay handshake cost.
    ///
    /// Two details, both measured rather than assumed. It hits `/key` (656
    /// bytes) instead of `/models`, which returns 737KB across 444 models — a
    /// download that costs more than the handshake it is meant to save, on
    /// exactly the metered connections this file's header worries about. And it
    /// drains the body: an unread response keeps the connection out of the pool,
    /// which defeats the point of warming it.
    ///
    /// Worth roughly a second on the first dictation. Sustained reuse is where
    /// the real win is, which is what rolling chunks provide.
    pub async fn warm(&self) {
        if let Ok(response) = self
            .http
            .get(format!("{}/key", self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .await
        {
            let _ = response.bytes().await;
        }
    }

    /// Transcribe one chunk. `bias_terms` are the top-N dictionary entries; they
    /// steer decoding toward known vocabulary. Budget is limited, so pass the
    /// terms that have actually been corrected most often, not the whole list.
    pub async fn transcribe(
        &self,
        audio: Vec<u8>,
        format: AudioFormat,
        bias_terms: &[String],
    ) -> Result<TranscriptionResponse> {
        let part = multipart::Part::bytes(audio)
            .file_name(format.file_name())
            .mime_str(format.mime())?;

        let mut form = multipart::Form::new()
            .part("file", part)
            .text("model", self.model.clone())
            .text("language", self.language.clone());

        if !bias_terms.is_empty() {
            form = form.text("prompt", bias_terms.join(", "));
        }

        let res = self
            .http
            .post(format!("{}{}", self.base_url, ENDPOINT))
            .bearer_auth(&self.api_key)
            .header("HTTP-Referer", APP_URL)
            .header("X-Title", APP_TITLE)
            .multipart(form)
            .send()
            .await
            .context("transcription request failed")?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            anyhow::bail!("transcription {}: {}", status, body);
        }

        res.json::<TranscriptionResponse>()
            .await
            .context("decoding transcription response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extension and the content type have to agree, because the server
    /// trusts either one of them to pick a decoder.
    #[test]
    fn each_format_labels_itself_consistently() {
        assert_eq!(AudioFormat::Wav.file_name(), "chunk.wav");
        assert_eq!(AudioFormat::Wav.mime(), "audio/wav");
        assert_eq!(AudioFormat::Opus.file_name(), "chunk.opus");
        assert_eq!(AudioFormat::Opus.mime(), "audio/ogg");
    }

    #[test]
    fn wav_is_not_labelled_as_opus() {
        // The bug this whole enum exists to prevent.
        assert_ne!(AudioFormat::Wav.mime(), AudioFormat::Opus.mime());
        assert!(AudioFormat::Wav.file_name().ends_with(".wav"));
    }
}
