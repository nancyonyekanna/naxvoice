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

    #[test]
    fn long_sentence_splits_at_a_clause() {
        let long = format!("{}, and then the rest continues here.", "word ".repeat(70));
        let pieces = split_long(&long);
        assert!(pieces.len() > 1);
        assert!(pieces[0].ends_with(',') || pieces[0].ends_with("word"));
    }
}
