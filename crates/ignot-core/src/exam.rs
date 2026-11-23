//! An exam: its questions with their keys, and the recipe that makes the same exam again.

use serde::{Deserialize, Serialize};

use crate::generate::{Kind, Level};
use crate::grade::GradeKind;
use crate::hash::{canonical, sha256_hex};
use crate::rng::Rng;
use crate::{GENERATOR_VERSION, NAME, Result, date, invalid};

/// The version of the file formats this crate reads and writes.
pub const FORMAT: u32 = 1;

/// The most questions an exam may have. A bound, so that re-making an exam someone else published always ends.
pub const MAX_QUESTIONS: usize = 500;

/// The fewest characters a seed may have.
///
/// A manifest publishes the hash of the sealed exam, and everything in an exam but its seed is public or easy
/// to guess. A short seed could be found by trying seeds until the hash fits, and the exam read before its
/// round. The default seed is 64 random hex digits.
pub const MIN_SEED_CHARS: usize = 32;

/// How many questions of one kind and level an exam has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MixItem {
    /// The kind of question.
    pub kind: Kind,
    /// Its level.
    pub level: Level,
    /// How many of them.
    pub count: u32,
}

/// One question of an exam, with its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// `q01`, `q02`, and so on.
    pub id: String,
    /// The kind of question.
    pub kind: Kind,
    /// Its level.
    pub level: Level,
    /// What the model is asked.
    pub prompt: String,
    /// The right answer.
    pub answer: String,
    /// How an answer is compared with the key.
    pub grade: GradeKind,
}

/// Which code made an exam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// The project's name.
    pub name: String,
    /// The generator version: exams of the same version regenerate with the same code.
    pub version: String,
    /// The commit the generator was built from.
    pub commit: String,
}

/// A whole exam. This is what gets sealed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Exam {
    /// The file format, [`FORMAT`].
    pub format: u32,
    /// A short name such as `2026-w48`.
    pub id: String,
    /// The name shown to people.
    pub title: String,
    /// When the exam was made.
    pub created_at: String,
    /// Which code made it.
    pub generator: Provenance,
    /// The secret every question was drawn from.
    pub seed: String,
    /// How many questions of each kind and level.
    pub mix: Vec<MixItem>,
    /// The questions, in order.
    pub questions: Vec<Question>,
}

/// Everything that decides an exam. The same recipe always makes the same exam, byte for byte, which is what
/// lets anyone check an opened exam against its seed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    /// A short name: lowercase letters, digits and dashes, at most 64 characters.
    pub id: String,
    /// The name shown to people.
    pub title: String,
    /// When the exam is made, such as `2026-11-24T14:00:00.000Z`.
    pub created_at: String,
    /// The commit the generator was built from.
    pub commit: String,
    /// The secret every question is drawn from.
    pub seed: String,
    /// How many questions of each kind and level.
    pub mix: Vec<MixItem>,
}

/// The default exam: one question of every kind at levels 3, 4 and 5.
#[must_use]
pub fn default_mix() -> Vec<MixItem> {
    let levels = [Level::ALL[2], Level::ALL[3], Level::ALL[4]];
    Kind::ALL.into_iter().flat_map(|kind| levels.map(|level| MixItem { kind, level, count: 1 })).collect()
}

/// Reads a mix such as `trace:3,graph:5:2`: kind, level and, optionally, how many.
pub fn parse_mix(spec: &str) -> Result<Vec<MixItem>> {
    spec.split(',')
        .map(|part| {
            let fields: Vec<&str> = part.trim().split(':').collect();
            let (kind, level, count) = match fields[..] {
                [kind, level] => (kind, level, "1"),
                [kind, level, count] => (kind, level, count),
                _ => ("", "", ""),
            };
            let digits = |text: &str| !text.is_empty() && text.bytes().all(|c| c.is_ascii_digit());
            match (
                Kind::parse(kind),
                level.parse().ok().filter(|_| digits(level)).and_then(|n: u8| Level::try_from(n).ok()),
                count.parse().ok().filter(|&n: &u32| digits(count) && n >= 1),
            ) {
                (Some(kind), Some(level), Some(count)) => Ok(MixItem { kind, level, count }),
                _ => invalid(format!(
                    "mix item \"{}\": want kind:level or kind:level:count, with kind one of {}",
                    part.trim(),
                    Kind::ALL.map(Kind::name).join(", ")
                )),
            }
        })
        .collect()
}

/// Whether a text can be an exam id: lowercase letters, digits and dashes, not starting with a dash, 1 to 64 long.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && !id.starts_with('-')
        && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// What [`valid_id`] asks of an id, as an error says it.
pub(crate) const ID_RULE: &str = "exam id: lowercase letters, digits and dashes, at most 64 characters";

/// Makes the exam a recipe describes.
pub fn make_exam(recipe: &Recipe) -> Result<Exam> {
    if !valid_id(&recipe.id) {
        return invalid(ID_RULE);
    }
    if recipe.seed.chars().count() < MIN_SEED_CHARS {
        return invalid(format!(
            "the exam seed is shorter than {MIN_SEED_CHARS} characters: it could be guessed from the manifest's hash"
        ));
    }
    if recipe.title.is_empty() || recipe.title.chars().count() > 200 || recipe.title.chars().any(char::is_control) {
        return invalid("exam title: 1 to 200 characters on one line");
    }
    if recipe.commit.is_empty() || recipe.commit.len() > 64 || !recipe.commit.bytes().all(|c| c.is_ascii_alphanumeric())
    {
        return invalid("exam commit: letters and digits, at most 64");
    }
    date::parse_rfc3339(&recipe.created_at)?;
    let total = recipe
        .mix
        .iter()
        .try_fold(0usize, |sum, item| sum.checked_add(item.count as usize).filter(|_| item.count >= 1));
    if !total.is_some_and(|n| (1..=MAX_QUESTIONS).contains(&n)) {
        return invalid(format!("exam mix: 1 to {MAX_QUESTIONS} questions, and no kind with a count of zero"));
    }

    let mut questions = Vec::new();
    for item in &recipe.mix {
        for _ in 0..item.count {
            let number = questions.len() + 1;
            // The number is part of the seed, so two questions of the same kind and level still differ.
            let mut rng =
                Rng::from_text(&format!("{}|{number}|{}|{}", recipe.seed, item.kind.name(), item.level.get()));
            let made = item.kind.generate(&mut rng, item.level)?;
            questions.push(Question {
                id: format!("q{number:02}"),
                kind: item.kind,
                level: item.level,
                prompt: made.prompt,
                answer: made.answer,
                grade: made.grade,
            });
        }
    }
    Ok(Exam {
        format: FORMAT,
        id: recipe.id.clone(),
        title: recipe.title.clone(),
        created_at: recipe.created_at.clone(),
        generator: Provenance { name: NAME.into(), version: GENERATOR_VERSION.into(), commit: recipe.commit.clone() },
        seed: recipe.seed.clone(),
        mix: recipe.mix.clone(),
        questions,
    })
}

impl Exam {
    /// Reads an exam from its JSON.
    pub fn parse(json: &str) -> Result<Self> {
        let exam: Self = serde_json::from_str(json)?;
        if exam.format != FORMAT {
            return invalid(format!("exam format {}: this version reads format {FORMAT}", exam.format));
        }
        // An id ends up in paths, on chain and on the screen, so one that make_exam would refuse is refused here.
        if !valid_id(&exam.id) {
            return invalid(ID_RULE);
        }
        Ok(exam)
    }

    /// The recipe this exam says it was made from.
    #[must_use]
    pub fn recipe(&self) -> Recipe {
        Recipe {
            id: self.id.clone(),
            title: self.title.clone(),
            created_at: self.created_at.clone(),
            commit: self.generator.commit.clone(),
            seed: self.seed.clone(),
            mix: self.mix.clone(),
        }
    }

    /// The exact bytes that are sealed and hashed.
    pub fn bytes(&self) -> Result<String> {
        canonical(self)
    }
}

/// The id an exam goes by on chain: the SHA-256 of its id.
#[must_use]
pub fn chain_exam_id(id: &str) -> String {
    sha256_hex(id.as_bytes())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const SEED: &str = "a seed for the tests of this crate, long enough to seal";

    pub(crate) fn recipe() -> Recipe {
        Recipe {
            id: "test-01".into(),
            title: "Test exam".into(),
            created_at: "2026-11-20T10:00:00.000Z".into(),
            commit: "0123abc".into(),
            seed: SEED.into(),
            mix: default_mix(),
        }
    }

    #[test]
    fn the_default_exam_has_thirty_questions() {
        let exam = make_exam(&recipe()).unwrap();
        assert_eq!(exam.questions.len(), 30);
        assert_eq!(exam.questions[0].id, "q01");
        assert_eq!(exam.questions[29].id, "q30");
        assert_eq!((exam.questions[0].kind, exam.questions[0].level.get()), (Kind::Trace, 3));
        assert_eq!((exam.questions[29].kind, exam.questions[29].level.get()), (Kind::Chain, 5));
        assert_eq!(exam.generator, Provenance { name: "ignot".into(), version: "1".into(), commit: "0123abc".into() });
    }

    #[test]
    fn the_same_recipe_gives_the_same_bytes() {
        let first = make_exam(&recipe()).unwrap();
        assert_eq!(first.bytes().unwrap(), make_exam(&recipe()).unwrap().bytes().unwrap());
        assert_eq!(make_exam(&first.recipe()).unwrap(), first);
        let other =
            make_exam(&Recipe { seed: "another seed, every bit as long as the first".into(), ..recipe() }).unwrap();
        assert!(first.questions.iter().zip(&other.questions).all(|(a, b)| a.prompt != b.prompt));
    }

    #[test]
    fn two_questions_of_the_same_kind_and_level_differ() {
        let mix = parse_mix("stack:3:3").unwrap();
        let exam = make_exam(&Recipe { mix, ..recipe() }).unwrap();
        assert_eq!(exam.questions.len(), 3);
        assert_ne!(exam.questions[0].prompt, exam.questions[1].prompt);
        assert_ne!(exam.questions[1].prompt, exam.questions[2].prompt);
    }

    #[test]
    fn the_bytes_parse_back_to_the_same_exam() {
        let exam = make_exam(&recipe()).unwrap();
        let bytes = exam.bytes().unwrap();
        assert!(bytes.starts_with("{\"createdAt\":\"2026-11-20T10:00:00.000Z\",\"format\":1,\"generator\":{\"commit\":\"0123abc\",\"name\":\"ignot\",\"version\":\"1\"},\"id\":\"test-01\",\"mix\":[{\"count\":1,\"kind\":\"trace\",\"level\":3},"));
        assert_eq!(Exam::parse(&bytes).unwrap(), exam);
    }

    #[test]
    fn an_exam_with_extra_or_missing_parts_does_not_read() {
        let bytes = make_exam(&recipe()).unwrap().bytes().unwrap();
        assert!(Exam::parse(&bytes.replacen("\"format\":1", "\"format\":2", 1)).is_err());
        assert!(Exam::parse(&bytes.replacen("{\"createdAt\"", "{\"extra\":1,\"createdAt\"", 1)).is_err());
        assert!(Exam::parse(&bytes.replacen(&format!("\"seed\":\"{SEED}\","), "", 1)).is_err());
        assert!(Exam::parse(&bytes.replacen("\"level\":3", "\"level\":9", 1)).is_err());
        assert!(Exam::parse(&bytes.replacen("\"kind\":\"trace\"", "\"kind\":\"essay\"", 1)).is_err());
        assert!(Exam::parse("").is_err());
        assert!(Exam::parse("[]").is_err());
        for id in ["test-01\\nPASS", "Test-01", "-test", "", "../x"] {
            let bad = bytes.replacen("\"id\":\"test-01\"", &format!("\"id\":\"{id}\""), 1);
            assert!(Exam::parse(&bad).unwrap_err().to_string().contains("exam id"), "{id}");
        }
    }

    #[test]
    fn mixes_parse() {
        assert_eq!(parse_mix("trace:3").unwrap(), [MixItem { kind: Kind::Trace, level: Level::ALL[2], count: 1 }]);
        assert_eq!(
            parse_mix(" graph:5:2 , bases:1 ").unwrap().iter().map(|item| item.count).collect::<Vec<_>>(),
            [2, 1]
        );
        for bad in [
            "",
            "trace",
            "trace:0",
            "trace:6",
            "trace:3:0",
            "essay:3",
            "trace:3:1:1",
            "trace:+3",
            "trace:3:-1",
            "trace:3,",
            "Trace:3",
        ] {
            assert!(parse_mix(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn recipes_that_cannot_be_an_exam_are_refused() {
        let refused = |change: fn(&mut Recipe)| {
            let mut bad = recipe();
            change(&mut bad);
            make_exam(&bad).is_err()
        };
        assert!(refused(|r| r.id = String::new()));
        assert!(refused(|r| r.id = "Has-Capitals".into()));
        assert!(refused(|r| r.id = "-leading-dash".into()));
        assert!(refused(|r| r.id = "a".repeat(65)));
        assert!(refused(|r| r.seed = String::new()));
        assert!(refused(|r| r.seed = "x".repeat(MIN_SEED_CHARS - 1)));
        assert!(!refused(|r| r.seed = "x".repeat(MIN_SEED_CHARS)));
        assert!(refused(|r| r.title = String::new()));
        assert!(refused(|r| r.title = "two\nlines".into()));
        assert!(refused(|r| r.commit = "not a commit".into()));
        assert!(refused(|r| r.created_at = "yesterday".into()));
        assert!(refused(|r| r.mix = Vec::new()));
        assert!(refused(|r| r.mix[0].count = 0));
        assert!(refused(|r| r.mix[0].count = 472));
        assert!(refused(|r| r.mix[0].count = u32::MAX));
        assert!(!refused(|r| r.mix[0].count = 471));
        assert!(!refused(|r| r.id = "a".repeat(64)));
    }

    #[test]
    fn the_chain_id_is_the_hash_of_the_id() {
        assert_eq!(chain_exam_id("abc"), "0xba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
