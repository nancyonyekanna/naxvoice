//! Splits text into synthesis units.
//!
//! Character-count splitting is the usual shortcut and it's what makes local TTS
//! sound wrong. Cutting mid-clause produces a falling intonation where the
//! sentence hasn't ended, then a fresh start on the next fragment. The listener
//! hears a machine restarting.
//!
//! Split on sentence boundaries. Keep the previous sentence as context so the
//! model gets the intonation right on the current one. Merge very short
//! sentences into their neighbour — one-word sentences render with odd emphasis
//! when synthesized alone.

/// Below this, a sentence gets merged forward rather than rendered alone.
const MIN_UNIT_CHARS: usize = 24;
/// Above this, split at the nearest clause boundary — long sentences blow past
/// the model's context and the tail degrades.
const MAX_UNIT_CHARS: usize = 280;

/// Above this, the opening unit is cut at its first clause boundary.
///
/// The opening is the only unit whose length the listener experiences as a
/// wait: every later one renders while the previous plays. First-word latency
/// is just the opening's speech duration times the render rate, so shortening
/// it is the only lever there is.
///
/// Measured on this machine, release build, three runs each on a quiet system:
/// the 80-character opening of the self-test passage rendered in 2884ms, and
/// the same sentence cut at its comma to 52 characters in 2046ms. Below this
/// threshold the saving does not justify an extra unit.
const FIRST_UNIT_SOFT_MAX: usize = 60;

/// Where the opening may be cut. Clause boundaries only.
///
/// Never a word boundary. Cutting mid-clause produces a falling intonation on
/// the first thing the listener hears, which is the failure this whole module
/// exists to prevent. A comma already implies continuation, so the voice
/// carries across it.
const CLAUSE_MARKS: [char; 3] = [',', ';', ':'];

#[derive(Debug, Clone)]
pub struct Unit {
    pub text: String,
    /// Preceding sentence, passed to the model as prosody context but not spoken.
    pub context: Option<String>,
}

pub fn split(text: &str) -> Vec<Unit> {
    let sentences = split_sentences(text);
    let merged = merge_short(sentences);

    let mut units = Vec::with_capacity(merged.len());
    for (i, s) in merged.iter().enumerate() {
        for piece in split_long(s) {
            units.push(Unit {
                text: piece,
                context: if i > 0 { Some(merged[i - 1].clone()) } else { None },
            });
        }
    }
    split_opening(units)
}

/// Cuts the opening unit at its first clause boundary, when that shortens the
/// wait before the first word without cutting mid-clause.
///
/// Only the opening, and only when there is something to gain. Three ways it
/// declines: a short opening is already fast, a head below `MIN_UNIT_CHARS`
/// renders with the odd emphasis `merge_short` exists to avoid, and a stub
/// remainder would have the same problem one unit later.
fn split_opening(mut units: Vec<Unit>) -> Vec<Unit> {
    let Some(first) = units.first() else {
        return units;
    };
    if first.text.chars().count() <= FIRST_UNIT_SOFT_MAX {
        return units;
    }

    let text = first.text.clone();
    let context = first.context.clone();

    // The earliest clause boundary leaving a head worth saying on its own,
    // because the earliest one is the shortest wait.
    let mut cut = None;
    for (i, c) in text.char_indices() {
        if CLAUSE_MARKS.contains(&c) && text[..=i].chars().count() >= MIN_UNIT_CHARS {
            cut = Some(i + c.len_utf8());
            break;
        }
    }

    let Some(cut) = cut else {
        return units;
    };
    let head = text[..cut].trim().to_string();
    let tail = text[cut..].trim().to_string();

    if tail.chars().count() < MIN_UNIT_CHARS {
        return units;
    }

    units[0] = Unit { text: head.clone(), context };
    units.insert(1, Unit { text: tail, context: Some(head) });
    units
}

/// Assumes abbreviations were already expanded by the normalizer. If they
/// weren't, "Dr. Ada" splits into two sentences and the read sounds broken.
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();

    for (i, &c) in chars.iter().enumerate() {
        current.push(c);
        if matches!(c, '.' | '!' | '?') {
            let next_is_break = chars
                .get(i + 1)
                .map(|n| n.is_whitespace())
                .unwrap_or(true);
            if next_is_break {
                let t = current.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                current.clear();
            }
        }
    }

    let tail = current.trim();
    if !tail.is_empty() {
        out.push(tail.to_string());
    }
    out
}

fn merge_short(sentences: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in sentences {
        if s.chars().count() < MIN_UNIT_CHARS {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(&s);
                continue;
            }
        }
        out.push(s);
    }
    out
}

/// Splits at the last clause boundary before the limit. Falls back to a word
/// boundary if there's no punctuation to work with.
fn split_long(sentence: &str) -> Vec<String> {
    if sentence.chars().count() <= MAX_UNIT_CHARS {
        return vec![sentence.to_string()];
    }

    let mut out = Vec::new();
    let mut rest = sentence;

    while rest.chars().count() > MAX_UNIT_CHARS {
        let window: String = rest.chars().take(MAX_UNIT_CHARS).collect();
        let cut = window
            .rfind([',', ';', ':'])
            .map(|i| i + 1)
            .or_else(|| window.rfind(' '))
            .unwrap_or(window.len());

        out.push(rest[..cut].trim().to_string());
        rest = rest[cut..].trim_start();
    }

    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sentences_merge_backward() {
        let units = split("The build passed. Good. Now ship it to production today.");
        assert_eq!(units.len(), 2);
        assert!(units[0].text.contains("Good."));
    }

    #[test]
    fn first_unit_has_no_context() {
        let units = split("One sentence here that is long enough to stand alone.");
        assert!(units[0].context.is_none());
    }

    #[test]
    fn later_units_carry_previous_sentence() {
        let units = split(
            "This is the first sentence and it is long enough. \
             This is the second sentence and it is also long enough.",
        );
        assert_eq!(units[1].context.as_deref(), Some("This is the first sentence and it is long enough."));
    }

    /// The whole point: a long opening is cut at its comma so the first word
    /// arrives sooner. Measured at 2884ms before this, 2046ms after.
    #[test]
    fn a_long_opening_is_cut_at_its_first_clause() {
        let units = split(
            "The paste target is captured when the key goes down, not when the text is ready.",
        );
        assert_eq!(units[0].text, "The paste target is captured when the key goes down,");
        assert_eq!(units[1].text, "not when the text is ready.");
        // The tail continues from the head, so the model gets the intonation right.
        assert_eq!(units[1].context.as_deref(), Some(units[0].text.as_str()));
    }

    /// A short opening is already fast, and an extra unit would cost more in
    /// prosody than it saves in time.
    #[test]
    fn a_short_opening_is_left_whole() {
        let units = split("One sentence here that is long enough to stand alone.");
        assert_eq!(units.len(), 1);
        assert!(units[0].text.ends_with("alone."));
    }

    /// No clause boundary means no cut. A word-boundary cut would produce the
    /// falling intonation this module exists to prevent, on the first thing
    /// the listener hears.
    #[test]
    fn an_opening_without_a_clause_boundary_is_never_cut_mid_clause() {
        let text = "The quick brown fox jumped over the extremely lazy dog again and again today.";
        let units = split(text);
        assert_eq!(units.len(), 1, "it should not have been cut");
        assert_eq!(units[0].text, text);
    }

    /// An early comma would leave a head too short to render well, so the cut
    /// moves to the next boundary that clears MIN_UNIT_CHARS.
    ///
    /// The tail is deliberately long here. An earlier version of this test
    /// ended it at "before anything else.", which is 21 characters, so the
    /// stub-remainder guard refused the cut and the test passed its first
    /// assertion while exercising a completely different branch.
    #[test]
    fn a_very_early_comma_does_not_produce_a_stub_head() {
        let units = split(
            "Yes, the paste target is captured when the key goes down, \
             before anything else happens on screen.",
        );
        assert_eq!(units.len(), 2);
        // Not "Yes,", which is four characters and would render with the odd
        // emphasis that merge_short exists to avoid.
        assert_eq!(
            units[0].text,
            "Yes, the paste target is captured when the key goes down,"
        );
        assert_eq!(units[1].text, "before anything else happens on screen.");
    }

    /// A cut that would leave a stub behind is refused: the problem would just
    /// land one unit later.
    #[test]
    fn a_cut_leaving_a_stub_remainder_is_refused() {
        let text = "The paste target is captured when the key finally goes down, quickly.";
        let units = split(text);
        assert_eq!(units.len(), 1, "the remainder would have been a stub");
        assert_eq!(units[0].text, text);
    }

    #[test]
    fn long_sentence_splits_at_a_clause() {
        let long = format!("{}, and then the rest continues here.", "word ".repeat(70));
        let pieces = split_long(&long);
        assert!(pieces.len() > 1);
        assert!(pieces[0].ends_with(',') || pieces[0].ends_with("word"));
    }
}
