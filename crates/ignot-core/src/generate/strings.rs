use super::{Generated, Level};
use crate::grade::GradeKind;
use crate::rng::Rng;

/// Words and steps, per level.
const SIZE: [(usize, usize); 5] = [(5, 5), (7, 7), (9, 9), (11, 11), (13, 13)];
const POOL: [&str; 14] = [
    "silver", "harbor", "quietly", "drifts", "amber", "lantern", "orbit", "meadow", "copper", "willow", "falcon",
    "ribbon", "summit", "pebble",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    ReverseWords,
    ReverseLetters,
    SwapEnds,
    Shift(u8),
    DropVowels,
    Rotate,
}

const STEPS: [Step; 6] =
    [Step::ReverseWords, Step::ReverseLetters, Step::SwapEnds, Step::Shift(0), Step::DropVowels, Step::Rotate];

impl Step {
    fn describe(self) -> String {
        match self {
            Self::ReverseWords => "Reverse the order of the words.".into(),
            Self::ReverseLetters => "Reverse the letters inside every word.".into(),
            Self::SwapEnds => "In every word, swap the first and last letters.".into(),
            Self::Shift(by) => format!("Shift every letter forward {by} places in the alphabet (wrapping z->a)."),
            Self::DropVowels => "In every word, delete each vowel (a, e, i, o, u) at an even position in that word (positions counted from 1 before deleting).".into(),
            Self::Rotate => "In every word, move the first letter to the end.".into(),
        }
    }

    /// Applies the step to words made of the letters a to z.
    fn apply(self, words: &mut [Vec<u8>]) {
        if self == Self::ReverseWords {
            words.reverse();
            return;
        }
        for word in words {
            match self {
                Self::ReverseWords => {}
                Self::ReverseLetters => word.reverse(),
                Self::SwapEnds => {
                    if let Some(last) = word.len().checked_sub(1) {
                        word.swap(0, last);
                    }
                }
                Self::Shift(by) => word.iter_mut().for_each(|c| *c = (*c - b'a' + by) % 26 + b'a'),
                Self::DropVowels => {
                    let mut position = 0;
                    word.retain(|c| {
                        position += 1;
                        position % 2 == 1 || !b"aeiou".contains(c)
                    });
                }
                Self::Rotate => {
                    if !word.is_empty() {
                        word.rotate_left(1);
                    }
                }
            }
        }
    }
}

/// Joins the words with spaces and makes every `every`th character uppercase, counting from 1 and counting spaces.
fn join_and_raise(words: &[Vec<u8>], every: usize) -> String {
    words
        .join(&b' ')
        .iter()
        .enumerate()
        .map(|(i, &c)| char::from(if (i + 1) % every == 0 { c.to_ascii_uppercase() } else { c }))
        .collect()
}

pub(super) fn strings(rng: &mut Rng, level: Level) -> Generated {
    let (word_count, step_count) = level.pick(SIZE);
    let chosen = rng.sample(&POOL, word_count);
    let mut steps = Vec::with_capacity(step_count);
    for _ in 1..step_count {
        steps.push(match *rng.choice(&STEPS) {
            Step::Shift(_) => Step::Shift(rng.int(1, 5) as u8),
            other => other,
        });
    }
    let every = rng.int(3, 5) as usize;

    let mut words: Vec<Vec<u8>> = chosen.iter().map(|word| word.as_bytes().to_vec()).collect();
    let mut lines = Vec::with_capacity(step_count);
    for step in &steps {
        step.apply(&mut words);
        lines.push(step.describe());
    }
    lines.push(format!("Join the words with single spaces, then make every {every}th character of the whole string uppercase (count from 1; spaces count as characters)."));
    let numbered: Vec<String> = lines.iter().enumerate().map(|(i, line)| format!("{}. {line}", i + 1)).collect();
    Generated {
        prompt: format!(
            "Start with: \"{}\"\nApply these steps in order:\n{}\nWhat is the final string? Answer with the string only.",
            chosen.join(" "),
            numbered.join("\n")
        ),
        answer: join_and_raise(&words, every),
        grade: GradeKind::Exact,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(step: Step, text: &str) -> String {
        let mut words: Vec<Vec<u8>> = text.split(' ').map(|word| word.as_bytes().to_vec()).collect();
        step.apply(&mut words);
        String::from_utf8(words.join(&b' ')).unwrap()
    }

    #[test]
    fn each_step_does_what_its_sentence_says() {
        assert_eq!(after(Step::ReverseWords, "silver harbor amber"), "amber harbor silver");
        assert_eq!(after(Step::ReverseLetters, "silver harbor"), "revlis robrah");
        assert_eq!(after(Step::SwapEnds, "silver harbor a"), "rilves rarboh a");
        assert_eq!(after(Step::Shift(3), "xyz abc"), "abc def");
        assert_eq!(after(Step::Shift(5), "quietly"), "vznjyqd");
        // silver: i(2) e(5) are vowels; only i is at an even position. harbor: a(2) and o(5): a goes.
        assert_eq!(after(Step::DropVowels, "silver harbor"), "slver hrbor");
        // quietly: u(2) i(3) e(4): u and e are at even positions.
        assert_eq!(after(Step::DropVowels, "quietly a"), "qitly a");
        assert_eq!(after(Step::Rotate, "silver a"), "ilvers a");
    }

    #[test]
    fn every_nth_character_counts_spaces() {
        let words: Vec<Vec<u8>> = ["ab", "cde"].iter().map(|w| w.as_bytes().to_vec()).collect();
        // a b _ c d e: characters 3 and 6 are the space and e.
        assert_eq!(join_and_raise(&words, 3), "ab cdE");
        assert_eq!(join_and_raise(&words, 4), "ab Cde");
        assert_eq!(join_and_raise(&words, 5), "ab cDe");
    }

    /// Follows the numbered sentences of a question with a second set of string functions, written on text
    /// instead of bytes.
    fn solve_from_text(prompt: &str) -> String {
        let start = prompt.split('"').nth(1).unwrap();
        let mut words: Vec<String> = start.split(' ').map(String::from).collect();
        let mut result = String::new();
        for line in prompt.lines().filter(|line| line.split_once(". ").is_some_and(|(n, _)| n.parse::<u32>().is_ok())) {
            let sentence = line.split_once(". ").unwrap().1;
            if sentence.starts_with("Reverse the order") {
                words = words.into_iter().rev().collect();
            } else if sentence.starts_with("Reverse the letters") {
                words = words.iter().map(|w| w.chars().rev().collect()).collect();
            } else if sentence.starts_with("In every word, swap") {
                words = words
                    .iter()
                    .map(|w| {
                        if w.len() < 2 {
                            w.clone()
                        } else {
                            format!("{}{}{}", &w[w.len() - 1..], &w[1..w.len() - 1], &w[..1])
                        }
                    })
                    .collect();
            } else if sentence.starts_with("Shift every letter") {
                let by: u32 = sentence.split(' ').nth(4).unwrap().parse().unwrap();
                words = words
                    .iter()
                    .map(|w| w.chars().map(|c| char::from_u32((c as u32 - 97 + by) % 26 + 97).unwrap()).collect())
                    .collect();
            } else if sentence.starts_with("In every word, delete") {
                words = words
                    .iter()
                    .map(|w| {
                        w.chars()
                            .enumerate()
                            .filter(|(i, c)| i % 2 == 0 || !"aeiou".contains(*c))
                            .map(|(_, c)| c)
                            .collect()
                    })
                    .collect();
            } else if sentence.starts_with("In every word, move") {
                words = words.iter().map(|w| format!("{}{}", &w[1..], &w[..1])).collect();
            } else if sentence.starts_with("Join the words") {
                let every: usize = sentence.split(' ').nth(9).unwrap().trim_end_matches("th").parse().unwrap();
                result = words
                    .join(" ")
                    .chars()
                    .enumerate()
                    .map(|(i, c)| if (i + 1) % every == 0 { c.to_ascii_uppercase() } else { c })
                    .collect();
            } else {
                panic!("a step nobody knows: {sentence}");
            }
        }
        result
    }

    #[test]
    fn the_key_is_what_the_steps_in_the_question_give() {
        for level in Level::ALL {
            for i in 0..40 {
                let question = strings(&mut Rng::from_text(&format!("strings {i}")), level);
                let (word_count, step_count) = level.pick(SIZE);
                assert_eq!(question.prompt.split('"').nth(1).unwrap().split(' ').count(), word_count);
                assert_eq!(question.prompt.lines().count(), step_count + 3);
                assert_eq!(question.answer, solve_from_text(&question.prompt), "{}", question.prompt);
            }
        }
    }

    #[test]
    fn no_word_ever_empties() {
        // The first letter of a word is at position 1, which is odd, so deleting vowels always leaves it.
        let mut words: Vec<Vec<u8>> = POOL.iter().map(|w| w.as_bytes().to_vec()).collect();
        for _ in 0..20 {
            Step::DropVowels.apply(&mut words);
            Step::Rotate.apply(&mut words);
            assert!(words.iter().all(|w| !w.is_empty()));
        }
    }
}
