//! `config.yaml` load and the typed view of it.
//!
//! This is the Rust half of the contract in `config.example.yaml`: every key
//! there has a field here, and every dashboard control will bind to one of them.
//!
//! Where a section already has a home in another module — chunk boundaries in
//! `audio::vad`, voices in `tts`, cleanup profiles in `cleanup` — the shape here
//! converts into that module's type rather than restating it. One definition per
//! concept, and the conversion is the only place that can drift.
//!
//! The OpenRouter key is deliberately not a field. It belongs in the OS keychain
//! once the dashboard can accept one; until then `dev_api_key` reads it from the
//! environment. A field here would be one careless `tracing::debug!` away from
//! the log file, and `config.yaml` is a file people paste into issues.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::audio::vad::ChunkPolicy;
use crate::cleanup::{DictionaryTerm, Profile};
use crate::platform::DictateKey;
use crate::tts::normalize::PronunciationRule;
use crate::tts::{Engine, VoiceConfig};

/// Holds the OpenRouter key in dev, until the dashboard can write to the keychain.
pub const API_KEY_ENV: &str = "NAXVOICE_OPENROUTER_KEY";

/// Points at a config file directly, overriding the usual search.
pub const CONFIG_PATH_ENV: &str = "NAXVOICE_CONFIG";

/// The profile key that applies when no pattern matches the focused app.
pub const DEFAULT_PROFILE_KEY: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub hotkeys: Hotkeys,
    pub transcription: Transcription,
    pub chunking: Chunking,
    pub audio: Audio,
    pub cleanup: Cleanup,
    pub tts: Tts,
    #[serde(default)]
    pub pronunciation: Vec<PronunciationEntry>,
    pub dictionary: Dictionary,
    /// Keyed by the pipe-separated pattern exactly as written in the file, with
    /// `default` among them. `from_yaml` copies each key into its `Profile`'s
    /// `pattern` field, which is `#[serde(skip)]` because in YAML it is the key
    /// rather than a value.
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub telemetry: Telemetry,
    #[serde(default)]
    pub history: History,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hotkeys {
    /// A key name rather than an accelerator: a bare modifier cannot be a
    /// registered global hotkey, so this key is watched instead.
    pub dictate_key: String,
    #[serde(default = "default_latch_ms")]
    pub latch_ms: u64,
    pub read_aloud: String,
    pub stop: String,
}

fn default_latch_ms() -> u64 {
    350
}

impl Hotkeys {
    pub fn dictate(&self) -> Result<DictateKey> {
        self.dictate_key.parse()
    }

    /// How long after a release a second tap still counts as a double-tap.
    pub fn latch_window(&self) -> Duration {
        Duration::from_millis(self.latch_ms)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcription {
    pub base_url: String,
    pub model: String,
    pub language: String,
    pub timeout_ms: u64,
}

impl Transcription {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunking {
    pub pause_threshold_ms: u64,
    pub overlap_ms: u64,
    pub min_chunk_ms: u64,
    pub max_chunk_ms: u64,
}

impl Chunking {
    /// The boundary policy `audio::vad::ChunkDetector` runs on.
    pub fn policy(&self) -> ChunkPolicy {
        ChunkPolicy {
            pause_threshold: Duration::from_millis(self.pause_threshold_ms),
            overlap: Duration::from_millis(self.overlap_ms),
            min_chunk: Duration::from_millis(self.min_chunk_ms),
            max_chunk: Duration::from_millis(self.max_chunk_ms),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audio {
    pub codec: String,
    pub bitrate: u32,
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cleanup {
    /// Turning this off pastes exactly what was heard. It replaced a second
    /// hotkey: one key is the whole interaction now, so the bypass lives here.
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub base_url: String,
    pub model: String,
    pub speculative: bool,
    pub max_tokens: u32,
    pub offline_behavior: OfflineBehavior,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineBehavior {
    /// Paste the unpolished transcript rather than losing the dictation.
    PasteRaw,
    /// Hold it until the network returns.
    Queue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tts {
    pub first_sentence: VoiceEntry,
    pub main: VoiceEntry,
    pub speed: f32,
    pub handoff_after_sentences: usize,
    pub skip_code_blocks: bool,
    pub skip_urls: bool,
    pub expand_numbers: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceEntry {
    pub engine: String,
    pub voice: String,
    /// Chatterbox only; Kokoro ignores it, so the first-sentence entry omits it.
    #[serde(default)]
    pub exaggeration: f32,
}

impl Tts {
    pub fn first_voice(&self) -> Result<VoiceConfig> {
        self.voice(&self.first_sentence)
    }

    pub fn main_voice(&self) -> Result<VoiceConfig> {
        self.voice(&self.main)
    }

    /// Speed is global rather than per-voice: the two engines hand off mid-passage
    /// and a speed change at the seam is as audible as a pitch change.
    fn voice(&self, entry: &VoiceEntry) -> Result<VoiceConfig> {
        Ok(VoiceConfig {
            engine: parse_engine(&entry.engine)?,
            voice: entry.voice.clone(),
            speed: self.speed,
            exaggeration: entry.exaggeration,
        })
    }
}

fn parse_engine(name: &str) -> Result<Engine> {
    match name {
        "kokoro" => Ok(Engine::Kokoro),
        "chatterbox-turbo" => Ok(Engine::ChatterboxTurbo),
        other => bail!("unknown tts engine {other:?}, expected \"kokoro\" or \"chatterbox-turbo\""),
    }
}

/// YAML spells this `match`, which is a Rust keyword, and
/// `tts::normalize::PronunciationRule` is not itself `Deserialize` — so this
/// mirrors it for serde and converts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PronunciationEntry {
    #[serde(rename = "match")]
    pub matches: String,
    pub say: String,
}

impl From<&PronunciationEntry> for PronunciationRule {
    fn from(e: &PronunciationEntry) -> Self {
        PronunciationRule { matches: e.matches.clone(), say: e.say.clone() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dictionary {
    /// Top N terms biased into the transcription prompt. The rest are left to the
    /// cleanup model.
    pub prompt_budget: usize,
    #[serde(default)]
    pub terms: Vec<DictionaryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DictionaryEntry {
    pub term: String,
    #[serde(default)]
    pub sounds_like: Vec<String>,
}

impl From<&DictionaryEntry> for DictionaryTerm {
    fn from(e: &DictionaryEntry) -> Self {
        DictionaryTerm { term: e.term.clone(), sounds_like: e.sounds_like.clone() }
    }
}

impl Dictionary {
    pub fn terms(&self) -> Vec<DictionaryTerm> {
        self.terms.iter().map(DictionaryTerm::from).collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Telemetry {
    #[serde(default)]
    pub enabled: bool,
}

/// Whether dictations are kept on disk so the History screen has something to
/// show. Defaulted throughout, so a config.yaml written before this existed
/// still loads — a key with no default is what broke startup once already.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct History {
    #[serde(default = "default_true")]
    pub keep: bool,
}

impl Default for History {
    fn default() -> Self {
        Self { keep: true }
    }
}

impl Config {
    /// Loads from the first location that exists. See `path`.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let config = Self::from_yaml(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        tracing::info!(path = %path.display(), "config loaded");
        Ok(config)
    }

    pub fn from_yaml(text: &str) -> Result<Self> {
        let mut config: Config = serde_norway::from_str(text)?;
        // In YAML the pattern is the map key, so serde skips the field and we
        // fill it here. `cleanup::match_profile` reads it to score matches.
        for (pattern, profile) in config.profiles.iter_mut() {
            profile.pattern = pattern.clone();
        }
        Ok(config)
    }

    /// Writes the config back to wherever `path` resolves.
    ///
    /// **Comments do not survive.** serde emits data, not formatting, and this
    /// file is about a quarter comments and blank lines. That is a deliberate
    /// choice rather than an oversight: `config.example.yaml` stays committed
    /// and fully annotated as the reference, so the explanations are never
    /// lost, only not duplicated into the live file.
    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path()?)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        let yaml = serde_norway::to_string(self).context("serialising the config")?;

        let dir = path.parent().context("the config path has no parent directory")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

        // Write beside the target and rename, so an interrupted save leaves the
        // previous config intact instead of a truncated one. The temporary file
        // shares a directory with the target, so the rename cannot cross a
        // filesystem boundary and degrade into a copy.
        let temp = path.with_extension("yaml.saving");
        std::fs::write(&temp, yaml.as_bytes())
            .with_context(|| format!("writing {}", temp.display()))?;
        std::fs::rename(&temp, path)
            .with_context(|| format!("replacing {}", path.display()))?;

        tracing::info!(path = %path.display(), "config saved");
        Ok(())
    }

    /// Serialises without writing. Split out so the round trip can be tested
    /// without touching the filesystem.
    pub fn to_yaml(&self) -> Result<String> {
        serde_norway::to_string(self).context("serialising the config")
    }

    /// Where `config.yaml` lives, in order of precedence:
    ///
    /// 1. `NAXVOICE_CONFIG`, for tests and for running two configs side by side
    /// 2. the per-user config directory, which is where an installed build reads
    /// 3. the repo root, which is where `cp config.example.yaml config.yaml` puts it
    ///
    /// `directories` resolves 2 per-OS, so this needs no `#[cfg]`.
    pub fn path() -> Result<PathBuf> {
        if let Some(explicit) = std::env::var_os(CONFIG_PATH_ENV) {
            return Ok(PathBuf::from(explicit));
        }

        if let Some(dirs) = directories::ProjectDirs::from("com", "naxvoice", "naxvoice") {
            let candidate = dirs.config_dir().join("config.yaml");
            if candidate.exists() {
                return Ok(candidate);
            }
        }

        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("locating repo root")?
            .join("config.yaml");
        if repo_root.exists() {
            return Ok(repo_root);
        }

        bail!(
            "no config.yaml found. Copy config.example.yaml to config.yaml in the \
             project root, or set {CONFIG_PATH_ENV}"
        )
    }

    /// The fallback profile, used when nothing matches the focused app.
    pub fn default_profile(&self) -> Result<&Profile> {
        self.profiles
            .get(DEFAULT_PROFILE_KEY)
            .with_context(|| format!("config.yaml has no {DEFAULT_PROFILE_KEY:?} profile"))
    }

    /// Profiles matched against the focused app, in the slice
    /// `cleanup::match_profile` takes. `default` is excluded deliberately: it is
    /// the fallback, not a pattern, and leaving it in would let it match an app
    /// that happens to be called "default".
    pub fn app_profiles(&self) -> Vec<Profile> {
        self.profiles
            .iter()
            .filter(|(key, _)| key.as_str() != DEFAULT_PROFILE_KEY)
            .map(|(_, profile)| profile.clone())
            .collect()
    }

    pub fn pronunciation_rules(&self) -> Vec<PronunciationRule> {
        self.pronunciation.iter().map(PronunciationRule::from).collect()
    }
}

/// The OpenRouter key for dev runs. Returns `None` rather than erroring, because
/// every caller so far has an offline path and a missing key is a normal state
/// before the dashboard exists.
///
/// Checks the environment first, then a `.env` beside `config.yaml`. The second
/// is not a convenience: the Tauri CLI does not read `.env` files, so without it
/// the dev path SETUP.md documents silently does nothing and the app reports a
/// missing key that is sitting right there on disk.
///
/// `.env` is gitignored, which is what keeps this compatible with the rule that
/// the key never enters a committed file.
///
/// Never log the return value.
pub fn dev_api_key() -> Option<String> {
    if let Some(key) = std::env::var(API_KEY_ENV).ok().filter(|k| !k.trim().is_empty()) {
        return Some(key);
    }

    let dotenv = repo_root()?.join(".env");
    let contents = std::fs::read_to_string(dotenv).ok()?;
    parse_dotenv(&contents, API_KEY_ENV)
}

/// The directory holding `config.yaml` and `.env`, one level above the crate.
fn repo_root() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf)
}

/// Pulls one value out of `.env` contents.
///
/// Deliberately minimal rather than a dotenv crate: this handles the shapes a
/// person actually writes by hand, and anything more elaborate belongs in the
/// keychain path instead.
fn parse_dotenv(contents: &str, key: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (name, value) = match line.split_once('=') {
            Some(pair) => pair,
            None => continue,
        };
        if name.trim() != key {
            continue;
        }
        let value = value.trim();
        // Strip one matched pair of quotes, so KEY="v" and KEY='v' both work.
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cleanup::match_profile;

    /// The committed example is the contract, so the tests parse that file rather
    /// than a fixture that could drift away from it.
    const EXAMPLE: &str = include_str!("../../config.example.yaml");

    fn example() -> Config {
        Config::from_yaml(EXAMPLE).expect("config.example.yaml should parse")
    }

    #[test]
    fn example_config_parses() {
        let c = example();
        assert_eq!(c.transcription.model, "deepgram/nova-3");
        assert_eq!(c.cleanup.offline_behavior, OfflineBehavior::PasteRaw);
        assert_eq!(c.dictionary.prompt_budget, 40);
        assert!(!c.telemetry.enabled);
    }

    #[test]
    fn the_dictate_key_resolves_to_a_real_key() {
        let h = example().hotkeys;
        assert_eq!(h.dictate().unwrap(), DictateKey::RightCommand);
        assert_eq!(h.latch_window(), Duration::from_millis(350));
    }

    #[test]
    fn an_unknown_dictate_key_is_an_error_not_a_default() {
        // Silently falling back would give a key that never fires, which is the
        // single hardest failure in this app to diagnose.
        let bad: Result<DictateKey> = "CapsLock".parse();
        assert!(bad.is_err());
    }

    #[test]
    fn cleanup_is_enabled_unless_turned_off() {
        assert!(example().cleanup.enabled);
        let off = Config::from_yaml(&EXAMPLE.replace("enabled: true", "enabled: false")).unwrap();
        assert!(!off.cleanup.enabled);
    }

    #[test]
    fn profile_patterns_come_from_the_map_keys() {
        // The pattern is the YAML key, so it survives only if from_yaml copies it
        // into the skipped field. Without that, every profile matches nothing.
        let c = example();
        let profiles = c.app_profiles();
        let hit = match_profile(&profiles, "Code.exe").expect("Code.exe should match a profile");
        assert!(hit.pattern.contains("Code"));
        assert!(hit.prompt.contains("Technical dictation"));
    }

    #[test]
    fn default_is_the_fallback_not_a_pattern() {
        let c = example();
        assert!(c.default_profile().is_ok());
        assert!(
            !c.app_profiles().iter().any(|p| p.pattern == DEFAULT_PROFILE_KEY),
            "default must not be matched against app identifiers"
        );
    }

    #[test]
    fn chunking_converts_to_the_vad_policy() {
        let p = example().chunking.policy();
        assert_eq!(p.pause_threshold, Duration::from_millis(450));
        assert_eq!(p.overlap, Duration::from_millis(200));
        assert_eq!(p.max_chunk, Duration::from_millis(25_000));
    }

    #[test]
    fn voices_resolve_to_engines_and_share_one_speed() {
        let tts = example().tts;
        let first = tts.first_voice().unwrap();
        let main = tts.main_voice().unwrap();
        assert_eq!(first.engine, Engine::Kokoro);
        // Both are Kokoro now. The second engine was measured at ~2.1s of
        // compute per spoken second and dropped, so the example no longer
        // configures one; see CLAUDE.md step 7.
        assert_eq!(main.engine, Engine::Kokoro);
        // Still worth pinning: both voices read `speed` from the same key, and
        // a silent divergence there would be audible.
        assert_eq!(first.speed, main.speed);
    }

    #[test]
    fn an_unknown_engine_is_an_error_not_a_default() {
        assert!(parse_engine("piper").is_err());
    }

    /// The highest-consequence path the dashboard opens up: if saving produces
    /// a file the app cannot read back, startup breaks on the next launch —
    /// which is exactly what happened when a config key gained no default.
    #[test]
    fn a_saved_config_can_be_loaded_again() {
        let original = example();
        let yaml = original.to_yaml().expect("serialising");
        let reloaded = Config::from_yaml(&yaml).expect("reparsing what we just wrote");

        assert_eq!(reloaded.hotkeys.dictate_key, original.hotkeys.dictate_key);
        assert_eq!(reloaded.hotkeys.latch_ms, original.hotkeys.latch_ms);
        assert_eq!(reloaded.transcription.model, original.transcription.model);
        assert_eq!(reloaded.cleanup.model, original.cleanup.model);
        assert_eq!(
            reloaded.chunking.pause_threshold_ms,
            original.chunking.pause_threshold_ms
        );
        assert_eq!(reloaded.dictionary.terms.len(), original.dictionary.terms.len());

        // Profiles are the likeliest thing to lose: the pattern is the YAML map
        // key and the struct field is `#[serde(skip)]`, so it is written by the
        // key and refilled on parse rather than round-tripping directly.
        assert_eq!(reloaded.profiles.len(), original.profiles.len());
        for (key, profile) in &reloaded.profiles {
            assert_eq!(
                &profile.pattern, key,
                "the pattern was not refilled from the map key"
            );
        }
    }

    /// Round-trips the config this machine actually runs on.
    ///
    /// The example is committed and known-good. The live file is the one the
    /// dashboard's Save button overwrites, and it can hold profiles,
    /// pronunciation rules and dictionary terms the example never had. A save
    /// that cannot be read back breaks startup.
    ///
    /// Ignored by default because it depends on the machine it runs on.
    ///
    ///     cargo test the_live_config -- --ignored --nocapture
    #[test]
    #[ignore]
    fn the_live_config_survives_a_save() {
        let Ok(path) = Config::path() else {
            eprintln!("  no config.yaml on this machine, nothing to check");
            return;
        };

        let original = Config::load_from(&path).expect("loading the live config");
        let yaml = original.to_yaml().expect("serialising");
        let reloaded = Config::from_yaml(&yaml).expect("reparsing the saved form");

        assert_eq!(reloaded.profiles.len(), original.profiles.len(), "profiles lost");
        assert_eq!(
            reloaded.pronunciation.len(),
            original.pronunciation.len(),
            "pronunciation rules lost"
        );
        assert_eq!(
            reloaded.dictionary.terms.len(),
            original.dictionary.terms.len(),
            "dictionary terms lost"
        );
        for (key, profile) in &reloaded.profiles {
            assert_eq!(&profile.pattern, key, "a profile pattern was not refilled");
        }

        eprintln!("  {} round-trips cleanly", path.display());
    }

    /// The pronunciation entry spells its field `match` in YAML, which is a
    /// Rust keyword. A save that emitted `matches` instead would parse as an
    /// empty rule set on the next launch, silently dropping every rule.
    #[test]
    fn saving_keeps_the_yaml_spelling_of_match() {
        let yaml = example().to_yaml().expect("serialising");
        assert!(
            yaml.contains("match:"),
            "expected the YAML key `match:`, got:\n{yaml}"
        );
        assert!(!yaml.contains("matches:"), "the Rust field name leaked into the file");
    }

    #[test]
    fn pronunciation_maps_the_match_key_onto_the_rule() {
        let c = example();
        let rules = c.pronunciation_rules();
        assert!(rules.iter().any(|r| r.matches == "₦" && r.say == "naira"));
    }

    #[test]
    fn dotenv_finds_the_key() {
        let env = "# a comment\nOTHER=1\nNAXVOICE_OPENROUTER_KEY=sk-or-v1-abc\n";
        assert_eq!(parse_dotenv(env, API_KEY_ENV).as_deref(), Some("sk-or-v1-abc"));
    }

    #[test]
    fn dotenv_handles_quotes_and_export() {
        assert_eq!(
            parse_dotenv("export NAXVOICE_OPENROUTER_KEY=\"sk-quoted\"\n", API_KEY_ENV).as_deref(),
            Some("sk-quoted")
        );
        assert_eq!(
            parse_dotenv("NAXVOICE_OPENROUTER_KEY='sk-single'\n", API_KEY_ENV).as_deref(),
            Some("sk-single")
        );
    }

    #[test]
    fn dotenv_ignores_comments_and_near_misses() {
        // A commented-out key must not be read, and a longer name that merely
        // starts with the real one must not match it.
        let env = "#NAXVOICE_OPENROUTER_KEY=commented\nNAXVOICE_OPENROUTER_KEY_OLD=stale\n";
        assert_eq!(parse_dotenv(env, API_KEY_ENV), None);
    }

    #[test]
    fn dotenv_treats_an_empty_value_as_absent() {
        assert_eq!(parse_dotenv("NAXVOICE_OPENROUTER_KEY=\n", API_KEY_ENV), None);
    }

    #[test]
    fn dictionary_terms_carry_their_mishearings() {
        let terms = example().dictionary.terms();
        let naxcrow = terms.iter().find(|t| t.term == "Naxcrow").unwrap();
        assert!(naxcrow.sounds_like.contains(&"nax crow".to_string()));
    }
}
