//! Reassembles out-of-order chunk transcripts into one transcript.
//!
//! Chunks are dispatched in order but resolve out of order — chunk 2 can come
//! back before chunk 1. Each chunk carries `overlap_ms` of audio from the end of
//! the previous one, so adjacent transcripts usually share a word or two at the
//! seam. Naive concatenation duplicates them.
//!
//! The fix is a bounded suffix/prefix match on the seam: look at the last few
//! words of the accumulated text and the first few of the incoming chunk, and
//! drop the longest overlap. Bounded because an unbounded search will happily
//! "find" a false overlap in repetitive speech ("the the the").

use std::collections::BTreeMap;

/// Max words to consider at a seam. Overlap is 200ms — realistically one or two
/// words, so 4 is generous. Raising this causes false matches, not better ones.
const SEAM_WINDOW: usize = 4;

#[derive(Default)]
pub struct Stitcher {
    resolved: BTreeMap<usize, String>,
    next_index: usize,
    emitted: String,
}

impl Stitcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a resolved chunk. Returns any newly contiguous text, so a caller
    /// can update the overlay live rather than waiting for the whole utterance.
    pub fn resolve(&mut self, index: usize, text: String) -> Option<String> {
        self.resolved.insert(index, text);

        let mut appended = String::new();
        while let Some(next) = self.resolved.remove(&self.next_index) {
            let merged = merge_seam(&self.emitted, next.trim());
            appended.push_str(&merged);
            self.emitted.push_str(&merged);
            self.next_index += 1;
        }

        if appended.is_empty() { None } else { Some(appended) }
    }

    pub fn transcript(&self) -> &str {
        &self.emitted
    }

    /// True when every dispatched chunk has come back.
    pub fn is_complete(&self, dispatched: usize) -> bool {
        self.next_index >= dispatched && self.resolved.is_empty()
    }
}

/// Returns `incoming` with any duplicated seam words removed, prefixed by a space
/// when joining to non-empty text.
fn merge_seam(accumulated: &str, incoming: &str) -> String {
    if accumulated.is_empty() {
        return incoming.to_string();
    }
    if incoming.is_empty() {
        return String::new();
    }

    let tail: Vec<&str> = accumulated
        .split_whitespace()
        .rev()
        .take(SEAM_WINDOW)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let head: Vec<&str> = incoming.split_whitespace().collect();

    let max = tail.len().min(head.len()).min(SEAM_WINDOW);
    for n in (1..=max).rev() {
        let tail_slice = &tail[tail.len() - n..];
        let head_slice = &head[..n];
        if eq_ignoring_case_and_punct(tail_slice, head_slice) {
            let rest = head[n..].join(" ");
            return if rest.is_empty() { String::new() } else { format!(" {}", rest) };
        }
    }

    format!(" {}", incoming)
}

fn eq_ignoring_case_and_punct(a: &[&str], b: &[&str]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| normalize(x) == normalize(y))
}

fn normalize(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_duplicated_seam_word() {
        let out = merge_seam("add a route to web.php", "web.php calling NaxcrowController");
        assert_eq!(out, " calling NaxcrowController");
    }

    #[test]
    fn ignores_punctuation_at_seam() {
        let out = merge_seam("fix the handler,", "handler for webhooks");
        assert_eq!(out, " for webhooks");
    }

    #[test]
    fn no_overlap_joins_with_space() {
        let out = merge_seam("first part", "second part");
        assert_eq!(out, " second part");
    }

    #[test]
    fn out_of_order_chunks_emit_in_order() {
        let mut s = Stitcher::new();
        assert_eq!(s.resolve(1, "world".into()), None);
        assert_eq!(s.resolve(0, "hello".into()), Some("hello world".into()));
        assert_eq!(s.transcript(), "hello world");
    }
}
