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

/// Half of a year, spoken as people say it.
///
/// This receives halves — `expand_years` splits "2026" into "20" and "26" — so
/// it must not use num2words' `year()` mode, which is built for whole years and
/// would read "19" as a date rather than a number.
///
/// The zero-padded case is the one the original TODO warned about: "05" is
/// "oh five", not "five", and certainly not "zero five".
fn spoken_pair(two_digits: &str) -> String {
    let n: i64 = match two_digits.trim().parse() {
        Ok(n) => n,
        // Unparseable input is left exactly as it was rather than guessed at.
        Err(_) => return two_digits.to_string(),
    };

    if two_digits.len() == 2 && two_digits.starts_with('0') {
        if n == 0 {
            return "hundred".to_string();
        }
        return format!("oh {}", cardinal(n));
    }
    cardinal(n)
}

fn spoken_number(digits: &str) -> String {
    match digits.trim().parse::<f64>() {
        Ok(n) if n.fract() == 0.0 => cardinal(n as i64),
        // Decimals keep their fractional part spoken digit by digit, which is
        // how amounts are actually read: "one point five zero".
        Ok(_) => {
            let (whole, frac) = digits.split_once('.').unwrap_or((digits, ""));
            let whole = whole.parse::<i64>().map(cardinal).unwrap_or_default();
            let spoken: Vec<String> = frac
                .chars()
                .filter(|c| c.is_ascii_digit())
                .map(|c| cardinal(c.to_digit(10).unwrap_or(0) as i64))
                .collect();
            if spoken.is_empty() {
                whole
            } else {
                format!("{whole} point {}", spoken.join(" "))
            }
        }
        Err(_) => digits.to_string(),
    }
}

/// Falls back to the digits themselves rather than panicking: a number read
/// wrongly is a nuisance, a crash mid-sentence ends the whole read-aloud.
fn cardinal(n: i64) -> String {
    num2words::Num2Words::new(n)
        .cardinal()
        .to_words()
        .unwrap_or_else(|_| n.to_string())
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
    fn years_are_spoken_in_pairs() {
        // "twenty twenty-six", not "two thousand and twenty-six".
        let out = expand_years("shipped in 2026");
        assert!(out.contains("twenty"), "got {out:?}");
        assert!(!out.contains("thousand"), "read as a whole number: {out:?}");
    }

    /// The case the original TODO singled out as where hand-rolling breaks.
    #[test]
    fn zero_padded_pairs_are_spoken_as_oh() {
        assert_eq!(spoken_pair("05"), "oh five");
        assert_eq!(spoken_pair("19"), "nineteen");
        assert_eq!(spoken_pair("00"), "hundred");
    }

    #[test]
    fn amounts_keep_their_decimals() {
        assert_eq!(spoken_number("1200"), "one thousand two hundred");
        assert!(spoken_number("1.50").contains("point"), "{}", spoken_number("1.50"));
    }

    /// Never panic mid-sentence: unreadable input passes through untouched.
    #[test]
    fn unparseable_numbers_pass_through_rather_than_crashing() {
        assert_eq!(spoken_pair("xx"), "xx");
        assert_eq!(spoken_number("not-a-number"), "not-a-number");
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
