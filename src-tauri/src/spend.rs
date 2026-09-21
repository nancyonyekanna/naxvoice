//! What naxvoice has cost, counted by naxvoice.
//!
//! Deliberately a separate file from `history.rs`. History holds the text of
//! what you dictated, it can be switched off, and it has a button that deletes
//! everything. None of that should erase the record of what the app has spent:
//! what you said and what you paid are two different decisions, and storing
//! them together would silently tie one to the other.
//!
//! **Only this app's own requests are counted.** OpenRouter's account-wide
//! figure is the wrong number to show here, because one key is usually shared
//! with other tools: putting it on naxvoice's dashboard would report spending
//! naxvoice never did. `/api/v1/activity`, which could break usage down by app,
//! needs a management key (the kind that can create and delete API keys), and
//! shipping an app that wants one of those would be indefensible. So the ledger
//! is local, and it counts what this process actually spent.
//!
//! Same JSONL discipline as `history.rs`: one object per line, appended, and a
//! line that does not parse is skipped, so a crash mid-append costs one entry
//! rather than the file.

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const DAY_SECONDS: u64 = 24 * 60 * 60;
const MONTH_SECONDS: u64 = 30 * DAY_SECONDS;

/// Entries are about 120 bytes. At fifty dictations a day this is years away,
/// which is the point: the file is bounded without the bound ever mattering.
const COMPACT_OVER_BYTES: u64 = 8 * 1024 * 1024;
const KEEP: usize = 20_000;

/// A carried-forward total is stamped with this, so it can never land inside a
/// rolling window. It counts toward the all-time figure and nothing else.
/// Without it, compaction would quietly reduce the lifetime total.
const CARRIED: u64 = 0;

/// Overrides where the ledger is kept, mirroring `NAXVOICE_HISTORY`, and for
/// the same reason: tests must never write into a real spending record.
pub const SPEND_PATH_ENV: &str = "NAXVOICE_SPEND";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Unix seconds. `0` means a carried-forward total from a compaction, in
    /// which case the split between `stt` and `cleanup` is not meaningful.
    pub at: u64,
    /// Transcription, summed across every chunk of the dictation.
    #[serde(default)]
    pub stt: f64,
    /// Cleanup, one call per dictation.
    #[serde(default)]
    pub cleanup: f64,
    /// At least one successful call reported no price, so this entry is a floor
    /// rather than an exact figure. Recorded rather than hidden: treating a
    /// missing price as zero would make the total quietly wrong, and a total
    /// that is quietly wrong is worse than one that admits its own limit.
    #[serde(default)]
    pub partial: bool,
}

impl Entry {
    pub fn total(&self) -> f64 {
        self.stt + self.cleanup
    }
}

/// What the Status screen shows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub day: f64,
    pub month: f64,
    pub all: f64,
    pub dictations_day: u64,
    /// Any entry inside the windows was missing a price.
    pub partial: bool,
}

pub fn path() -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os(SPEND_PATH_ENV) {
        return Ok(PathBuf::from(explicit));
    }

    let dirs = directories::ProjectDirs::from("com", "naxvoice", "naxvoice")
        .context("locating the app data directory")?;
    Ok(dirs.data_dir().join("spend.jsonl"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Records what one dictation cost.
///
/// Failures are logged rather than propagated, exactly as in `history.rs`: the
/// text has already been pasted by this point, and turning a failed ledger
/// write into a failed dictation would be the worse outcome.
pub fn record(stt: f64, cleanup: f64, partial: bool) {
    let entry = Entry { at: now().max(1), stt, cleanup, partial };
    if let Err(e) = write(&entry) {
        tracing::warn!(error = format!("{e:#}"), "could not record what the dictation cost");
    }
}

fn write(entry: &Entry) -> Result<()> {
    let path = path()?;
    let dir = path.parent().context("spend path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let line = serde_json::to_string(entry).context("serialising the entry")?;
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

/// Keeps the most recent `KEEP` entries, folding everything older into a single
/// carried total so the lifetime figure does not fall when the file is trimmed.
fn compact(path: &std::path::Path) -> Result<()> {
    let all = read_all(path);
    if all.len() <= KEEP {
        return Ok(());
    }

    let split = all.len() - KEEP;
    let dropped: f64 = all[..split].iter().map(Entry::total).sum();
    let dropped_partial = all[..split].iter().any(|e| e.partial);

    let mut body = String::new();
    if dropped > 0.0 {
        let carry = Entry { at: CARRIED, stt: dropped, cleanup: 0.0, partial: dropped_partial };
        body.push_str(&serde_json::to_string(&carry)?);
        body.push('\n');
    }
    for entry in &all[split..] {
        body.push_str(&serde_json::to_string(entry)?);
        body.push('\n');
    }

    // Written beside the target and renamed, so an interrupted compaction
    // cannot leave the ledger truncated.
    let temp = path.with_extension("jsonl.compacting");
    std::fs::write(&temp, body)?;
    std::fs::rename(&temp, path)?;
    tracing::debug!(kept = KEEP, "spend ledger compacted");
    Ok(())
}

fn read_all(path: &std::path::Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
        .collect()
}

pub fn totals() -> Totals {
    let Ok(path) = path() else { return Totals::default() };
    let entries = read_all(&path);

    let now = now();
    let day_cut = now.saturating_sub(DAY_SECONDS);
    let month_cut = now.saturating_sub(MONTH_SECONDS);

    let mut totals = Totals::default();
    for entry in &entries {
        let value = entry.total();
        totals.all += value;

        // A carried total belongs to no window, however far back the cutoffs
        // happen to reach.
        if entry.at == CARRIED {
            continue;
        }
        if entry.at >= month_cut {
            totals.month += value;
            totals.partial |= entry.partial;
        }
        if entry.at >= day_cut {
            totals.day += value;
            totals.dictations_day += 1;
        }
    }
    totals
}

/// Removes the ledger. Separate from clearing history on purpose.
pub fn clear() -> Result<()> {
    let path = path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SPEND_PATH_ENV` is process wide and cargo runs tests in parallel, so
    /// every test that redirects the ledger has to take this first. Without it
    /// they overwrite each other's path and fail at random.
    static LEDGER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_ledger(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("naxvoice-spend-{}-{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("spend.jsonl")
    }

    #[test]
    fn an_entry_totals_both_halves() {
        let e = Entry { at: 1, stt: 0.004, cleanup: 0.001, partial: false };
        assert!((e.total() - 0.005).abs() < 1e-9);
    }

    /// The ledger end to end, on a real file: written, read back, and summed
    /// into the three figures the Status tile shows.
    #[test]
    fn spending_round_trips_and_lands_in_every_window() {
        let _guard = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
        let path = temp_ledger("roundtrip");
        std::env::set_var(SPEND_PATH_ENV, &path);

        clear().expect("starting from empty");
        record(0.0040, 0.0010, false);
        record(0.0020, 0.0005, false);

        let t = totals();
        assert_eq!(t.dictations_day, 2);
        assert!((t.day - 0.0075).abs() < 1e-9, "day was {}", t.day);
        assert!((t.month - 0.0075).abs() < 1e-9);
        assert!((t.all - 0.0075).abs() < 1e-9);
        assert!(!t.partial, "nothing was unpriced");

        clear().expect("cleaning up");
        assert_eq!(totals().all, 0.0, "clear left something behind");

        std::env::remove_var(SPEND_PATH_ENV);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// A missing price must surface as "at least this much", never as zero.
    #[test]
    fn an_unpriced_call_marks_the_total_as_a_floor() {
        let _guard = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
        let path = temp_ledger("partial");
        std::env::set_var(SPEND_PATH_ENV, &path);

        clear().expect("starting from empty");
        record(0.003, 0.0, true);

        assert!(totals().partial, "the floor was not flagged");

        clear().ok();
        std::env::remove_var(SPEND_PATH_ENV);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// The whole reason carried entries exist: trimming the file must not
    /// reduce the lifetime figure, and must not add to a rolling window.
    #[test]
    fn a_carried_total_counts_for_all_time_but_no_window() {
        let _guard = LEDGER.lock().unwrap_or_else(|e| e.into_inner());
        let path = temp_ledger("carried");
        std::env::set_var(SPEND_PATH_ENV, &path);
        clear().expect("starting from empty");

        let carried = Entry { at: CARRIED, stt: 5.0, cleanup: 0.0, partial: false };
        let mut body = serde_json::to_string(&carried).unwrap();
        body.push('\n');
        let recent = Entry { at: now(), stt: 0.25, cleanup: 0.0, partial: false };
        body.push_str(&serde_json::to_string(&recent).unwrap());
        body.push('\n');
        std::fs::write(&path, body).unwrap();

        let t = totals();
        assert!((t.all - 5.25).abs() < 1e-9, "lifetime lost the carried total");
        assert!((t.day - 0.25).abs() < 1e-9, "carried total leaked into the day");
        assert!((t.month - 0.25).abs() < 1e-9, "carried total leaked into the month");
        assert_eq!(t.dictations_day, 1, "the carry is not a dictation");

        clear().ok();
        std::env::remove_var(SPEND_PATH_ENV);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// A crash mid-append leaves a partial final line, and losing the whole
    /// ledger to it would be absurd.
    #[test]
    fn a_truncated_last_line_does_not_lose_the_ledger() {
        let path = temp_ledger("truncated");

        let good = Entry { at: 1, stt: 0.01, cleanup: 0.002, partial: false };
        let mut body = serde_json::to_string(&good).unwrap();
        body.push('\n');
        body.push_str("{\"at\":2,\"stt\":0.0");
        std::fs::write(&path, body).unwrap();

        let all = read_all(&path);
        assert_eq!(all.len(), 1, "the good entry should survive");

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
