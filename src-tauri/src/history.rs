//! What was dictated, kept so it can be looked at again.
//!
//! **This writes your dictation to disk in plain text.** That is what a history
//! screen is, and DESIGN.md asks for one, but it is a real change in what the
//! app retains — so it is switchable, and `keep` is the only thing that decides
//! whether anything is written at all.
//!
//! One JSON object per line, appended. The format is chosen so a partly written
//! final line — a crash mid-append — costs one record rather than the file:
//! every line parses on its own and unparseable ones are skipped.
//!
//! The file is bounded. Without a cap it grows for the life of the install, and
//! a status screen that reads it would get slower every day.

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Records kept after a compaction. At a few dozen dictations a day this is
/// weeks of history and a file measured in hundreds of kilobytes.
const KEEP: usize = 1_000;

/// Compact once the file passes this, so the check costs a stat rather than a
/// parse of the whole file on every dictation.
const COMPACT_OVER_BYTES: u64 = 2 * 1024 * 1024;

const DAY_SECONDS: u64 = 24 * 60 * 60;

/// How many recent dictations the round-trip figures cover.
///
/// A time window is the wrong shape for this one. During development a morning
/// of slow runs drags a 24-hour median all day, so the screen keeps reporting a
/// build that no longer exists. A fixed count of the most recent dictations
/// tracks what the app is doing now, and recovers within an afternoon.
const ROUND_TRIP_SAMPLE: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// Unix seconds. Stored as a number rather than a formatted date so it can
    /// be compared without parsing, and rendered in the viewer's own locale.
    pub at: u64,
    /// The app the text was pasted into, captured when the key went down.
    pub app: String,
    /// What came back from transcription, before cleanup.
    pub raw: String,
    /// What was actually pasted.
    pub text: String,
    /// Release to pasted, in milliseconds — the number CLAUDE.md budgets.
    pub ms: u64,
    pub chunks: usize,
}

/// Aggregates for the Status screen.
///
/// Two different windows, deliberately. Words and dictations cover the last 24
/// hours, because "how much did I dictate today" is a question about time.
/// Calling a rolling window "today" would need a timezone, and a figure that
/// quietly means something other than its label is worse than a longer label.
///
/// Round trip covers the last `ROUND_TRIP_SAMPLE` dictations instead, because
/// "how fast is it" is a question about the current build rather than about
/// time.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    /// Last 24 hours.
    pub dictations: u64,
    /// Last 24 hours.
    pub words: u64,
    /// Median rather than mean: one slow outlier should not move it.
    pub median_ms: Option<u64>,
    /// The tail, shown beside the median. A median on its own hides the
    /// dictation that took five seconds, and that is the one the user remembers.
    pub p95_ms: Option<u64>,
    /// How many dictations the round-trip figures actually cover, so the screen
    /// can say "last 12" rather than claim a 50 it does not have.
    pub round_trip_sample: u64,
}

/// Overrides where history is kept. Mirrors `NAXVOICE_CONFIG`, and exists for
/// the same two reasons: running two profiles side by side, and being able to
/// test the store without writing into somebody's real dictation history.
pub const HISTORY_PATH_ENV: &str = "NAXVOICE_HISTORY";

pub fn path() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os(HISTORY_PATH_ENV) {
        return Ok(PathBuf::from(explicit));
    }

    let dirs = directories::ProjectDirs::from("com", "naxvoice", "naxvoice")
        .context("locating the app data directory")?;
    Ok(dirs.data_dir().join("history.jsonl"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Appends one dictation.
///
/// Failures are logged rather than propagated: the dictation has already been
/// pasted by this point, and turning a failed write of the history into a
/// failed dictation would be the worse outcome.
pub fn append(app: &str, raw: &str, text: &str, ms: u64, chunks: usize) {
    let record = Record {
        at: now(),
        app: app.to_string(),
        raw: raw.to_string(),
        text: text.to_string(),
        ms,
        chunks,
    };

    if let Err(e) = write(&record) {
        tracing::warn!(error = format!("{e:#}"), "could not record the dictation");
    }
}

fn write(record: &Record) -> Result<()> {
    let path = path()?;
    let dir = path.parent().context("history path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let line = serde_json::to_string(record).context("serialising the record")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))?;
    drop(file);

    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > COMPACT_OVER_BYTES {
        compact(&path)?;
    }
    Ok(())
}

/// Rewrites the file keeping only the most recent `KEEP` records.
fn compact(path: &std::path::Path) -> Result<()> {
    let all = read_all(path);
    if all.len() <= KEEP {
        return Ok(());
    }

    let kept = &all[all.len() - KEEP..];
    let mut body = String::new();
    for record in kept {
        body.push_str(&serde_json::to_string(record)?);
        body.push('\n');
    }

    // Written beside the target and renamed, so an interrupted compaction
    // cannot leave the history truncated.
    let temp = path.with_extension("jsonl.compacting");
    std::fs::write(&temp, body)?;
    std::fs::rename(&temp, path)?;
    tracing::debug!(kept = KEEP, "history compacted");
    Ok(())
}

fn read_all(path: &std::path::Path) -> Vec<Record> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        // A line that does not parse is skipped rather than fatal: a crash
        // mid-append leaves exactly one such line, and losing the whole
        // history to it would be absurd.
        .filter_map(|l| serde_json::from_str::<Record>(l).ok())
        .collect()
}

/// The most recent records, newest first.
pub fn recent(limit: usize) -> Vec<Record> {
    let Ok(path) = path() else { return Vec::new() };
    let mut all = read_all(&path);
    all.reverse();
    all.truncate(limit);
    all
}

/// Removes everything.
pub fn clear() -> Result<()> {
    let path = path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

pub fn stats() -> Stats {
    let Ok(path) = path() else { return Stats::default() };
    let all = read_all(&path);

    // Records are appended, so the file is already in chronological order and
    // the tail is the most recent run of dictations.
    let cutoff = now().saturating_sub(DAY_SECONDS);
    let day: Vec<&Record> = all.iter().filter(|r| r.at >= cutoff).collect();

    let words = day
        .iter()
        .map(|r| r.text.split_whitespace().count() as u64)
        .sum();

    let recent = &all[all.len().saturating_sub(ROUND_TRIP_SAMPLE)..];
    let latencies: Vec<u64> = recent.iter().map(|r| r.ms).collect();

    Stats {
        dictations: day.len() as u64,
        words,
        median_ms: median(latencies.clone()),
        p95_ms: percentile(latencies, 95),
        round_trip_sample: recent.len() as u64,
    }
}

/// Nearest-rank percentile.
///
/// No interpolation: with a handful of samples an interpolated p95 invents a
/// number that never happened, and every value here is a dictation that really
/// took that long. Below roughly twenty samples the p95 is simply the worst or
/// second-worst run, which is why the screen never shows it alone.
fn percentile(mut values: Vec<u64>, p: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    // Ceiling division without div_ceil, which would raise the MSRV.
    let rank = ((p * values.len()) + 99) / 100;
    Some(values[rank.max(1) - 1])
}

fn median(mut values: Vec<u64>) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2
    } else {
        values[middle]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_nothing_is_absent_rather_than_zero() {
        assert_eq!(median(vec![]), None);
    }

    #[test]
    fn an_odd_count_takes_the_middle() {
        assert_eq!(median(vec![300, 100, 200]), Some(200));
    }

    #[test]
    fn an_even_count_averages_the_two_middles() {
        assert_eq!(median(vec![100, 200, 300, 400]), Some(250));
    }

    /// One slow dictation should not move the median, which is the whole
    /// reason Status shows a median rather than a mean.
    #[test]
    fn an_outlier_does_not_drag_the_median() {
        assert_eq!(median(vec![600, 620, 610, 90_000]), Some(615));
    }

    /// The p95 exists precisely to show the outlier the median hides.
    #[test]
    fn the_p95_surfaces_what_the_median_hides() {
        let runs = vec![600, 620, 610, 90_000];
        assert_eq!(median(runs.clone()), Some(615));
        assert_eq!(percentile(runs, 95), Some(90_000));
    }

    #[test]
    fn percentile_of_nothing_is_absent_rather_than_zero() {
        assert_eq!(percentile(vec![], 95), None);
    }

    /// Nearest rank, never interpolated: every figure shown was a real run.
    #[test]
    fn percentile_returns_a_value_that_actually_occurred() {
        let runs: Vec<u64> = (1..=100).collect();
        let p95 = percentile(runs.clone(), 95).unwrap();
        assert_eq!(p95, 95);
        assert!(runs.contains(&p95));
    }

    /// Round trip is the last N dictations, not a time window, so an old slow
    /// build stops being reported as soon as N newer runs exist.
    #[test]
    fn the_round_trip_window_is_the_most_recent_dictations() {
        let mut all: Vec<Record> = Vec::new();
        // Fifty slow ones, then fifty fast ones.
        for i in 0..100u64 {
            all.push(Record {
                at: 1000 + i,
                app: "com.example".into(),
                raw: "x".into(),
                text: "x".into(),
                ms: if i < 50 { 9_000 } else { 500 },
                chunks: 1,
            });
        }
        let recent = &all[all.len().saturating_sub(ROUND_TRIP_SAMPLE)..];
        assert_eq!(recent.len(), 50);
        assert_eq!(
            median(recent.iter().map(|r| r.ms).collect()),
            Some(500),
            "the slow build should have fallen out of the window"
        );
    }

    /// The store end to end, on a real file.
    ///
    /// Everything else here tests a pure function. This is the only thing that
    /// proves a dictation can actually be written and read back, which is what
    /// the History screen and three tiles on Status depend on. It runs against
    /// a temporary path so it never touches real dictation history.
    #[test]
    fn a_record_round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("naxvoice-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var(HISTORY_PATH_ENV, dir.join("history.jsonl"));

        clear().expect("starting from empty");
        append("com.example.editor", "raw words here", "Raw words here.", 612, 2);

        let found = recent(10);
        assert_eq!(found.len(), 1, "the record was not written");
        assert_eq!(found[0].app, "com.example.editor");
        assert_eq!(found[0].ms, 612);
        assert_eq!(found[0].chunks, 2);
        assert_eq!(found[0].raw, "raw words here");

        let stats = stats();
        assert_eq!(stats.dictations, 1);
        assert_eq!(stats.words, 3, "three words in the pasted text");
        assert_eq!(stats.median_ms, Some(612));

        clear().expect("cleaning up");
        assert!(recent(10).is_empty(), "clear left something behind");

        std::env::remove_var(HISTORY_PATH_ENV);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A crash mid-append leaves a partial final line. Losing the whole
    /// history to it would be absurd, so it is skipped.
    #[test]
    fn a_truncated_last_line_does_not_lose_the_file() {
        let dir = std::env::temp_dir().join(format!("naxvoice-history-{}", now()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.jsonl");

        let good = Record {
            at: 1,
            app: "com.example".into(),
            raw: "raw".into(),
            text: "hello there".into(),
            ms: 500,
            chunks: 1,
        };
        let mut body = serde_json::to_string(&good).unwrap();
        body.push('\n');
        body.push_str("{\"at\":2,\"app\":\"com.exa");
        std::fs::write(&path, body).unwrap();

        let all = read_all(&path);
        assert_eq!(all.len(), 1, "the good record should survive");
        assert_eq!(all[0].text, "hello there");

        std::fs::remove_dir_all(&dir).ok();
    }
}
