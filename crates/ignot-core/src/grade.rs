//! Grading: pull the answer out of a model's reply and compare it with the key. No judgement, no model.

use serde::{Deserialize, Serialize};

/// How an answer is compared with the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GradeKind {
    /// A decimal number, right when it is less than 0.005 away from the key.
    Num,
    /// The same characters as the key.
    Exact,
    /// The same words as the key, split at spaces and commas, ignoring letter case.
    Tokens,
}

const MARKER: &str = "ANSWER:";

/// The answer in a reply: what follows the last `ANSWER:` up to the end of that line, without the quotes,
/// backticks, stars and spaces a model may wrap around it. Empty when the reply has no such line.
#[must_use]
pub fn extract_answer(raw: &str) -> &str {
    let Some(at) = raw.rfind(MARKER) else { return "" };
    let rest = raw[at + MARKER.len()..].trim_start();
    let line = rest.split(['\n', '\r', '\u{2028}', '\u{2029}']).next().unwrap_or("");
    line.trim_matches(|c: char| c.is_whitespace() || matches!(c, '`' | '*' | '"'))
}

/// Whether `got` is the answer `expected` under this kind of comparison.
#[must_use]
pub fn grade(kind: GradeKind, expected: &str, got: &str) -> bool {
    // A key with nothing in it would be matched by saying nothing.
    if expected.trim().is_empty() {
        return false;
    }
    match kind {
        GradeKind::Exact => got == expected,
        GradeKind::Tokens => {
            let mut a = tokens(got);
            let mut b = tokens(expected).peekable();
            // A key of separators alone has no words, and saying nothing would match it.
            if b.peek().is_none() {
                return false;
            }
            loop {
                match (a.next(), b.next()) {
                    (None, None) => return true,
                    (Some(x), Some(y)) if x.eq_ignore_ascii_case(y) => {}
                    _ => return false,
                }
            }
        }
        GradeKind::Num => {
            let cleaned: String = got.chars().filter(|c| !matches!(c, ',' | '$')).collect();
            match (Decimal::parse(expected), Decimal::parse(cleaned.trim())) {
                (Some(key), Some(answer)) => key.exact && answer.near(&key),
                _ => false,
            }
        }
    }
}

fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| c.is_whitespace() || c == ',').filter(|token| !token.is_empty())
}

/// Digits kept after the decimal point. Anything finer is remembered only as "there was more".
const SCALE: usize = 18;
/// The largest whole part a number may have: 18 digits.
const WHOLE_DIGITS: usize = 18;
/// 0.005 at this scale.
const TOLERANCE: i128 = 5_000_000_000_000_000;

/// A decimal number held exactly: no float ever takes part in a grade.
struct Decimal {
    negative: bool,
    /// The absolute value times 10^18, cut off (not rounded) after 18 decimals.
    units: i128,
    /// Whether nothing was cut off.
    exact: bool,
}

impl Decimal {
    /// Reads `12`, `-12.50`, `+.5` or `12.`: an optional sign, digits, an optional point, digits. Nothing else.
    fn parse(text: &str) -> Option<Self> {
        let (negative, body) = match text.as_bytes().first()? {
            b'-' => (true, &text[1..]),
            b'+' => (false, &text[1..]),
            _ => (false, text),
        };
        let (whole, fraction) = body.split_once('.').unwrap_or((body, ""));
        if (whole.is_empty() && fraction.is_empty())
            || !whole.bytes().chain(fraction.bytes()).all(|c| c.is_ascii_digit())
        {
            return None;
        }
        let whole = whole.trim_start_matches('0');
        if whole.len() > WHOLE_DIGITS {
            return None;
        }
        let mut units: i128 = 0;
        for c in whole.bytes().chain(fraction.bytes().chain(std::iter::repeat(b'0')).take(SCALE)) {
            units = units * 10 + i128::from(c - b'0');
        }
        let exact = fraction.bytes().skip(SCALE).all(|c| c == b'0');
        Some(Self { negative, units, exact })
    }

    /// Whether this number is less than 0.005 away from `key`, which must be exact.
    fn near(&self, key: &Self) -> bool {
        // Seen from this number's side of zero, it is `units` plus a fraction of one unit that is above zero
        // exactly when digits were cut off.
        let centre = if self.negative == key.negative { key.units } else { -key.units };
        let above_low = self.units > centre - TOLERANCE || (self.units == centre - TOLERANCE && !self.exact);
        above_low && self.units < centre + TOLERANCE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_last_answer_line() {
        assert_eq!(extract_answer("thinking...\nANSWER: 12\nmore\nANSWER: 42"), "42");
        assert_eq!(extract_answer("ANSWER: 42\n\nHope that helps."), "42");
        assert_eq!(extract_answer("I will end with ANSWER: <x>.\nANSWER:\n\n  7 8 9  \n"), "7 8 9");
        assert_eq!(extract_answer("ANSWER: 5 ANSWER: 6"), "6");
    }

    #[test]
    fn strips_what_models_wrap_around_an_answer() {
        assert_eq!(extract_answer("**ANSWER:** `3,4,N`"), "3,4,N");
        assert_eq!(extract_answer("ANSWER: \"silver Harbor\"  "), "silver Harbor");
        assert_eq!(extract_answer("ANSWER: **1 2 3**\r\n"), "1 2 3");
    }

    #[test]
    fn no_answer_line_means_no_answer() {
        assert_eq!(extract_answer("the result is 42"), "");
        assert_eq!(extract_answer("answer: 42"), "");
        assert_eq!(extract_answer("ANSWER:"), "");
        assert_eq!(extract_answer("ANSWER: 42\nANSWER:   "), "");
        assert_eq!(extract_answer(""), "");
    }

    #[test]
    fn exact_is_exact() {
        assert!(grade(GradeKind::Exact, "siLver", "siLver"));
        assert!(!grade(GradeKind::Exact, "siLver", "silver"));
        assert!(!grade(GradeKind::Exact, "12", "12 "));
    }

    #[test]
    fn tokens_ignore_spacing_commas_and_case() {
        assert!(grade(GradeKind::Tokens, "3,4,N", "3, 4, n"));
        assert!(grade(GradeKind::Tokens, "1 2 3", " 1  2\t3 "));
        assert!(!grade(GradeKind::Tokens, "1 2 3", "1 2"));
        assert!(!grade(GradeKind::Tokens, "1 2 3", "1 2 3 4"));
        assert!(!grade(GradeKind::Tokens, "1 2 3", "1 2 4"));
        assert!(!grade(GradeKind::Tokens, "1 2 3", ""));
    }

    #[test]
    fn numbers_within_half_a_cent() {
        for got in [
            "1234.50",
            "1234.5",
            "$1,234.50",
            " 1234.504 ",
            "+1234.4951",
            "1234.5049999999999999999999",
            "01234.50",
            "1234.495000000000000000000001",
        ] {
            assert!(grade(GradeKind::Num, "1234.50", got), "{got}");
        }
        for got in ["1234.505", "1234.495", "1234.51", "1234", "-1234.50", "1234.5050000000000000000000001", ""] {
            assert!(!grade(GradeKind::Num, "1234.50", got), "{got}");
        }
        assert!(grade(GradeKind::Num, "42", "42."));
        assert!(grade(GradeKind::Num, "42", "42.0000"));
        assert!(grade(GradeKind::Num, "0", "-0.004"));
        assert!(grade(GradeKind::Num, "0", ".004"));
        assert!(!grade(GradeKind::Num, "0", "-0.005"));
        assert!(grade(GradeKind::Num, "-3", "-3.001"));
        assert!(!grade(GradeKind::Num, "-3", "3"));
    }

    #[test]
    fn only_plain_decimals_are_numbers() {
        for got in [
            "4.2e1",
            "0x2a",
            "42 minutes",
            "forty-two",
            "4 2",
            "Infinity",
            "NaN",
            ".",
            "-",
            "+",
            "1.2.3",
            "--42",
            "\u{2212}42",
        ] {
            assert!(!grade(GradeKind::Num, "42", got), "{got}");
        }
    }

    #[test]
    fn huge_numbers_are_wrong_not_a_crash() {
        let huge = "9".repeat(400);
        assert!(!grade(GradeKind::Num, "42", &huge));
        assert!(!grade(GradeKind::Num, "42", &format!("42.{huge}")));
        assert!(grade(GradeKind::Num, "42", &format!("42.{}", "0".repeat(400))));
        assert!(grade(GradeKind::Num, "42", &format!("{}42", "0".repeat(400))));
        assert!(grade(GradeKind::Num, "999999999999999999", "999999999999999999.004"));
        assert!(!grade(GradeKind::Num, "1000000000000000000", "1000000000000000000"));
    }

    #[test]
    fn an_empty_key_is_matched_by_nothing() {
        for kind in [GradeKind::Exact, GradeKind::Tokens, GradeKind::Num] {
            assert!(!grade(kind, "", ""));
            assert!(!grade(kind, " , ", ""));
            assert!(!grade(kind, "", "anything"));
        }
    }

    #[test]
    fn a_key_that_is_not_a_plain_number_matches_nothing() {
        assert!(!grade(GradeKind::Num, "abc", "abc"));
        assert!(!grade(GradeKind::Num, "", ""));
        assert!(!grade(GradeKind::Num, "0.0000000000000000001", "0"));
    }
}
