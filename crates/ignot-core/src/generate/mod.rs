//! The ten kinds of question.
//!
//! Each kind is a pure function of a random stream and a level: the same seed always gives the same question
//! and the same key. Every key is computed here, by code, at the moment the question is made; no model and no
//! person ever writes one.

mod bases;
mod chain;
mod counting;
mod graph;
mod queue;
mod robot;
mod stack;
mod strings;
mod table;
mod trace;

use serde::{Deserialize, Serialize};

use crate::grade::GradeKind;
use crate::rng::Rng;
use crate::{Error, Result};

/// How hard a question is, from 1 to 5. A higher level means a longer instance of the same task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Level(u8);

impl Level {
    /// Every level, easiest first.
    pub const ALL: [Self; 5] = [Self(1), Self(2), Self(3), Self(4), Self(5)];

    /// The level as a number from 1 to 5.
    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }

    /// This level's entry in a table that has one entry per level.
    fn pick<T: Copy>(self, table: [T; 5]) -> T {
        table[usize::from(self.0 - 1)]
    }
}

impl TryFrom<u8> for Level {
    type Error = Error;

    fn try_from(n: u8) -> Result<Self> {
        if (1..=5).contains(&n) {
            Ok(Self(n))
        } else {
            Err(Error::Invalid(format!("level {n}: levels run from 1 to 5")))
        }
    }
}

impl From<Level> for u8 {
    fn from(level: Level) -> Self {
        level.0
    }
}

/// A question with its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generated {
    /// What the model is asked.
    pub prompt: String,
    /// The right answer.
    pub answer: String,
    /// How an answer is compared with the key.
    pub grade: GradeKind,
}

/// The kinds of question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Say what a short program prints.
    Trace,
    /// Add up the rows of a table that pass several filters.
    Table,
    /// Find the cheapest route through a graph within a limit of steps.
    Graph,
    /// Apply a list of rewriting steps to a sentence.
    Strings,
    /// Work out how long customers wait in a shop.
    Queue,
    /// Do arithmetic across four number bases.
    Bases,
    /// Run a program on a stack machine.
    Stack,
    /// Follow a robot across a grid with walls.
    Robot,
    /// Count the numbers that meet several conditions at once.
    Counting,
    /// Two tasks in a row, where the second starts from the answer to the first.
    Chain,
}

impl Kind {
    /// Every kind, in the order of the default exam.
    pub const ALL: [Self; 10] = [
        Self::Trace,
        Self::Table,
        Self::Graph,
        Self::Strings,
        Self::Queue,
        Self::Bases,
        Self::Stack,
        Self::Robot,
        Self::Counting,
        Self::Chain,
    ];

    /// The name used in files and on the command line.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Table => "table",
            Self::Graph => "graph",
            Self::Strings => "strings",
            Self::Queue => "queue",
            Self::Bases => "bases",
            Self::Stack => "stack",
            Self::Robot => "robot",
            Self::Counting => "counting",
            Self::Chain => "chain",
        }
    }

    /// The kind with this name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// Makes one question of this kind.
    pub fn generate(self, rng: &mut Rng, level: Level) -> Result<Generated> {
        match self {
            Self::Trace => Ok(trace::trace(rng, level)),
            Self::Table => Ok(table::table(rng, level)),
            Self::Graph => graph::graph(rng, level),
            Self::Strings => Ok(strings::strings(rng, level)),
            Self::Queue => Ok(queue::queue(rng, level)),
            Self::Bases => bases::bases(rng, level),
            Self::Stack => Ok(stack::stack(rng, level)),
            Self::Robot => Ok(robot::robot(rng, level)),
            Self::Counting => counting::counting(rng, level),
            Self::Chain => chain::chain(rng, level),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_match_the_files() {
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.name()), Some(kind));
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{}\"", kind.name()));
        }
        assert_eq!(Kind::parse("Trace"), None);
        assert_eq!(Kind::parse(""), None);
    }

    #[test]
    fn levels_run_from_one_to_five() {
        assert!(Level::try_from(0).is_err());
        assert!(Level::try_from(6).is_err());
        assert_eq!(Level::ALL.map(Level::get), [1, 2, 3, 4, 5]);
        assert!(serde_json::from_str::<Level>("3").is_ok());
        assert!(serde_json::from_str::<Level>("0").is_err());
        assert!(serde_json::from_str::<Level>("3.0").is_err());
        assert!(serde_json::from_str::<Level>("\"3\"").is_err());
    }

    #[test]
    fn the_same_seed_gives_the_same_question_and_another_seed_another() {
        for kind in Kind::ALL {
            for level in Level::ALL {
                let make = |seed: &str| kind.generate(&mut Rng::from_text(seed), level).unwrap();
                let first = make("one");
                assert_eq!(first, make("one"), "{} {}", kind.name(), level.get());
                assert_ne!(first.prompt, make("two").prompt, "{} {}", kind.name(), level.get());
                assert!(!first.answer.is_empty());
                assert!(first.prompt.is_ascii() && first.answer.is_ascii());
            }
        }
    }

    /// Two thousand questions, forty of every kind at every level, hashed together. This is the output that
    /// was compared, prompt for prompt and key for key, with the implementation these generators were first
    /// ported from. A generator that changes what it makes for a seed would leave published exams unable to
    /// regenerate: raise `GENERATOR_VERSION` and keep the old generator instead.
    #[test]
    fn the_generators_still_make_what_version_1_made() {
        let mut lines = String::new();
        for seed in 0..40 {
            for (n, (kind, level)) in
                Kind::ALL.iter().flat_map(|kind| Level::ALL.map(|level| (kind, level))).enumerate()
            {
                let text = format!("differential-{seed}|{}|{}|{}", n + 1, kind.name(), level.get());
                let made = kind.generate(&mut Rng::from_text(&text), level).unwrap();
                let grade = serde_json::to_string(&made.grade).unwrap();
                let prompt = crate::hash::sha256_hex(made.prompt.as_bytes());
                lines.push_str(&format!("{text} {prompt} {} {}\n", made.answer, grade.trim_matches('"')));
            }
        }
        assert_eq!(
            crate::hash::sha256_hex(lines.as_bytes()),
            "0x05673d1861ef6a4cf0bd4820c55b61aec15ce1ab018423c23fb8d20a529643fe",
            "a generator changed what it makes for a seed"
        );
    }

    #[test]
    fn a_higher_level_is_a_longer_question() {
        for kind in Kind::ALL {
            let length = |level: Level| -> usize {
                (0..8)
                    .map(|i| kind.generate(&mut Rng::from_text(&format!("len {i}")), level).unwrap().prompt.len())
                    .sum()
            };
            let lengths = Level::ALL.map(length);
            assert!(lengths.is_sorted(), "{}: {lengths:?}", kind.name());
            assert!(lengths[0] < lengths[4], "{}", kind.name());
        }
    }
}
