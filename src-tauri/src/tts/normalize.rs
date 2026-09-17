//! Text normalization before synthesis.
//!
//! This is the least glamorous file in the repo and the one that decides whether
//! read-aloud sounds like a person or like a screen reader. Speech models are
//! trained on spoken language; raw screen text is full of things that have a
//! written form and a spoken form, and the model guesses badly.
//!
//! Run order matters. Code and URL stripping happens first so later rules don't
//! mangle an identifier. User pronunciation rules run last so they always win.

use regex::Regex;

pub struct Normalizer {
    rules: Vec<(Regex, String)>,
    skip_code: bool,
    skip_urls: bool,
    expand_numbers: bool,
}

#[derive(Debug, Clone)]
pub struct PronunciationRule {
    pub matches: String,
    pub say: String,
}

impl Normalizer {
    pub fn new(
        user_rules: &[PronunciationRule],
        skip_code: bool,
        skip_urls: bool,
        expand_numbers: bool,
    ) -> anyhow::Result<Self> {
        let rules = user_rules
            .iter()
            .map(|r| {
                let pattern = format!(r"(?i)\b{}\b", regex::escape(&r.matches));
                Ok((Regex::new(&pattern)?, r.say.clone()))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        Ok(Self { rules, skip_code, skip_urls, expand_numbers })
    }

    pub fn run(&self, input: &str) -> String {
        let mut text = input.to_string();

        if self.skip_code {
            text = strip_code(&text);
        }
        if self.skip_urls {
            text = strip_urls(&text);
        }

        text = expand_abbreviations(&text);

        if self.expand_numbers {
            text = expand_currency(&text);
            text = expand_years(&text);
        }

        // User rules last so they override anything above.
        for (re, say) in &self.rules {
            text = re.replace_all(&text, say.as_str()).to_string();
        }

        collapse_whitespace(&text)
    }
}

/// Fenced blocks and inline code read as gibberish. Replace with a spoken marker
/// rather than deleting, so the listener knows something was skipped.
fn strip_code(text: &str) -> String {
    let fenced = Regex::new(r"(?s)```.*?```").unwrap();
    let inline = Regex::new(r"`[^`\n]+`").unwrap();
    let t = fenced.replace_all(text, " code block. ");
    inline.replace_all(&t, " code. ").to_string()
}

fn strip_urls(text: &str) -> String {
    let url = Regex::new(r"https?://\S+").unwrap();
    url.replace_all(text, " link. ").to_string()
}

/// Period-carrying abbreviations break sentence splitting downstream as well as
/// reading wrong, so expand them before chunking ever sees the text.
fn expand_abbreviations(text: &str) -> String {
    const PAIRS: &[(&str, &str)] = &[
        (r"\bDr\.", "Doctor"),
        (r"\bMr\.", "Mister"),
        (r"\bMrs\.", "Missus"),
        (r"\bSt\.", "Street"),
        (r"\bvs\.", "versus"),
        (r"\be\.g\.", "for example"),
        (r"\bi\.e\.", "that is"),
        (r"\betc\.", "et cetera"),
        (r"\bapprox\.", "approximately"),
    ];

    let mut out = text.to_string();
    for (pat, rep) in PAIRS {
        out = Regex::new(pat).unwrap().replace_all(&out, *rep).to_string();
    }
    out
}

/// "$1,200" reads as "dollar one comma two zero zero" without this.
/// Naira first — it's the common case here and the symbol is otherwise silent.
fn expand_currency(text: &str) -> String {
    let re = Regex::new(r"([₦$£€])\s?([\d,]+(?:\.\d{2})?)").unwrap();
    re.replace_all(text, |caps: &regex::Captures| {
        let unit = match &caps[1] {
            "₦" => "naira",
            "$" => "dollars",
            "£" => "pounds",
            _ => "euros",
        };
        let digits = caps[2].replace(',', "");
        format!("{} {}", spoken_number(&digits), unit)
    })
    .to_string()
}

/// Years are said in pairs: "twenty twenty-six", not "two thousand and twenty-six".
fn expand_years(text: &str) -> String {
    let re = Regex::new(r"\b(19|20)(\d{2})\b").unwrap();
    re.replace_all(text, |caps: &regex::Captures| {
        format!("{} {}", spoken_pair(&caps[1]), spoken_pair(&caps[2]))
    })
    .to_string()
}

fn spoken_pair(_two_digits: &str) -> String {
    // TODO: number-to-words. Use the `num2words` crate rather than hand-rolling;
    // the edge cases (teens, zero-padded pairs, "oh-five") are where this breaks.
    unimplemented!("wire up num2words")
}

fn spoken_number(_digits: &str) -> String {
    unimplemented!("wire up num2words")
}

fn collapse_whitespace(text: &str) -> String {
    Regex::new(r"\s{2,}").unwrap().replace_all(text.trim(), " ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_becomes_a_spoken_marker() {
        let out = strip_code("run `npm install` now");
        assert!(out.contains("code."));
        assert!(!out.contains("npm"));
    }

    #[test]
    fn abbreviations_lose_their_periods() {
        // Sentence splitting downstream depends on this.
        assert_eq!(expand_abbreviations("see Dr. Ada"), "see Doctor Ada");
        assert!(!expand_abbreviations("e.g. this").contains('.'));
    }

    #[test]
    fn user_rules_override_everything() {
        let rules = vec![PronunciationRule {
            matches: "Naxcrow".into(),
            say: "nax crow".into(),
        }];
        let n = Normalizer::new(&rules, false, false, false).unwrap();
        assert_eq!(n.run("deploy Naxcrow today"), "deploy nax crow today");
    }
}
