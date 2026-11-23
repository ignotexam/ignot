use super::{Generated, Level};
use crate::grade::GradeKind;
use crate::rng::Rng;
use crate::{Result, invalid};

const DIGITS: [u32; 5] = [5, 6, 6, 7, 7];

/// What the numbers being counted must satisfy, besides never having two equal digits side by side.
#[derive(Debug, Clone, Copy)]
struct Rule {
    digits: u32,
    sum: u32,
    /// This digit must appear exactly twice.
    digit: u32,
    first_below_last: bool,
    needs_zero: bool,
}

/// Counts the numbers by walking the digits left to right and keeping, for every state a prefix can be in
/// (digit sum, times the digit was seen, last digit, first digit, whether a zero was seen), how many prefixes
/// are in it.
fn count(rule: &Rule) -> u64 {
    let index = |sum: u32, seen: u32, last: u32, first: u32, zero: u32| {
        ((((sum * 3 + seen) * 10 + last) * 10 + first) * 2 + zero) as usize
    };
    let states = index(rule.sum, 2, 9, 9, 1) + 1;
    let mut current = vec![0u64; states];
    for d in 1..=9.min(rule.sum) {
        current[index(d, u32::from(d == rule.digit), d, d, 0)] = 1;
    }
    for _ in 1..rule.digits {
        let mut next = vec![0u64; states];
        for sum in 0..=rule.sum {
            for seen in 0..3 {
                for last in 0..10 {
                    for first in 1..10 {
                        for zero in 0..2 {
                            let ways = current[index(sum, seen, last, first, zero)];
                            if ways == 0 {
                                continue;
                            }
                            for d in 0..10 {
                                let seen_now = seen + u32::from(d == rule.digit);
                                if d != last && sum + d <= rule.sum && seen_now <= 2 {
                                    next[index(sum + d, seen_now, d, first, zero | u32::from(d == 0))] += ways;
                                }
                            }
                        }
                    }
                }
            }
        }
        current = next;
    }
    let mut total = 0;
    for last in 0..10 {
        for first in 1..10 {
            for zero in 0..2 {
                if (!rule.first_below_last || first < last) && (!rule.needs_zero || zero == 1) {
                    total += current[index(rule.sum, 2, last, first, zero)];
                }
            }
        }
    }
    total
}

pub(super) fn counting(rng: &mut Rng, level: Level) -> Result<Generated> {
    let digits = level.pick(DIGITS);
    for _ in 0..1000 {
        let rule = Rule {
            digits,
            sum: rng.int(18, 9 * digits - 18),
            digit: rng.int(1, 9),
            first_below_last: level.get() >= 3,
            needs_zero: level.get() == 5,
        };
        let n = count(&rule);
        if n == 0 {
            continue;
        }
        let low = 10u32.pow(digits - 1);
        let more = [
            if rule.first_below_last { " and whose first digit is smaller than its last digit" } else { "" },
            if rule.needs_zero { " and that contain at least one 0" } else { "" },
        ]
        .concat();
        return Ok(Generated {
            prompt: format!(
                "How many {digits}-digit numbers ({low} to {}) have digit sum {}, contain the digit {} exactly 2 times, never have two equal digits next to each other{more}? Answer with the number only.",
                low * 10 - 1,
                rule.sum,
                rule.digit
            ),
            answer: n.to_string(),
            grade: GradeKind::Num,
        });
    }
    invalid("counting: no rule with at least one number")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts by looking at every number.
    fn count_one_by_one(rule: &Rule) -> u64 {
        let low = 10u32.pow(rule.digits - 1);
        (low..low * 10)
            .filter(|n| {
                let digits: Vec<u32> = n.to_string().bytes().map(|c| u32::from(c - b'0')).collect();
                digits.iter().sum::<u32>() == rule.sum
                    && digits.iter().filter(|&&d| d == rule.digit).count() == 2
                    && digits.windows(2).all(|pair| pair[0] != pair[1])
                    && (!rule.first_below_last || digits[0] < digits[digits.len() - 1])
                    && (!rule.needs_zero || digits.contains(&0))
            })
            .count() as u64
    }

    #[test]
    fn the_count_matches_looking_at_every_number() {
        let mut rng = Rng::from_text("counting rules");
        for digits in [1, 2, 3, 4, 5] {
            for _ in 0..60 {
                let rule = Rule {
                    digits,
                    sum: rng.int(0, 9 * digits),
                    digit: rng.int(0, 9),
                    first_below_last: rng.int(0, 1) == 1,
                    needs_zero: rng.int(0, 1) == 1,
                };
                assert_eq!(count(&rule), count_one_by_one(&rule), "{rule:?}");
            }
        }
        for (digits, sum, digit) in [(6, 27, 3), (6, 36, 9), (7, 31, 1), (7, 45, 5)] {
            let rule = Rule { digits, sum, digit, first_below_last: true, needs_zero: digits == 7 };
            assert_eq!(count(&rule), count_one_by_one(&rule), "{rule:?}");
        }
    }

    #[test]
    fn the_key_is_the_count_the_question_asks_for() {
        for level in Level::ALL {
            for i in 0..6 {
                let question = counting(&mut Rng::from_text(&format!("counting {i}")), level).unwrap();
                let number = |after: &str| -> u32 {
                    let rest = &question.prompt[question.prompt.find(after).unwrap() + after.len()..];
                    rest[..rest.find(|c: char| !c.is_ascii_digit()).unwrap()].parse().unwrap()
                };
                let rule = Rule {
                    digits: number("How many "),
                    sum: number("digit sum "),
                    digit: number("the digit "),
                    first_below_last: question.prompt.contains("first digit is smaller than its last digit"),
                    needs_zero: question.prompt.contains("at least one 0"),
                };
                assert_eq!(rule.first_below_last, level.get() >= 3);
                assert_eq!(rule.needs_zero, level.get() == 5);
                assert_eq!(question.answer, count_one_by_one(&rule).to_string(), "{}", question.prompt);
                assert_ne!(question.answer, "0");
            }
        }
    }
}
