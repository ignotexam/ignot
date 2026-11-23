//! Answers, scores, and the Merkle root that pins them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::budget::Track;
use crate::exam::{Exam, FORMAT, Question};
use crate::grade::{extract_answer, grade};
use crate::hash::{canonical, sha256_hex, to_hex};
use crate::merkle::{leaf_hash, merkle_root};
use crate::{Result, invalid};

/// One model's answer to one question, with what it cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnswerRecord {
    /// The model's name at the provider.
    pub model: String,
    /// The track the answer was given on.
    pub track: Track,
    /// The question answered.
    pub question_id: String,
    /// The model's whole reply.
    pub raw: String,
    /// The answer taken from the reply.
    pub extracted: String,
    /// Whether the answer counts.
    pub correct: bool,
    /// What the provider charged, in billionths of a US dollar.
    pub cost_nano_usd: u64,
    /// Tokens in the question as the provider counted them.
    pub prompt_tokens: u64,
    /// Tokens in the reply, reasoning included.
    pub completion_tokens: u64,
    /// The most reply tokens the model was allowed.
    pub max_tokens: u32,
    /// Milliseconds from request to reply.
    pub latency_ms: u64,
    /// Why the model stopped, in the provider's words.
    pub finish_reason: Option<String>,
    /// Which of the model's hosts served the reply, as the provider named it. Absent when it named none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// What went wrong, when the model gave no reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// All of one model's answers to an exam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnswerFile {
    /// The file format.
    pub format: u32,
    /// The exam answered.
    pub exam_id: String,
    /// The model.
    pub model: String,
    /// One record per question and track.
    pub records: Vec<AnswerRecord>,
}

/// Whether a text can be a model's name: 1 to 200 printable ASCII characters, without spaces, as the provider
/// names its models (`lab/model-1`, `lab/model:free`).
#[must_use]
pub fn valid_model(name: &str) -> bool {
    (1..=200).contains(&name.len()) && name.bytes().all(|c| c.is_ascii_graphic())
}

/// Whether a text can be a host's name: 1 to 100 printable ASCII characters, spaces allowed (`Amazon Bedrock`).
#[must_use]
pub fn valid_host(name: &str) -> bool {
    (1..=100).contains(&name.len()) && name.bytes().all(|c| c == b' ' || c.is_ascii_graphic())
}

impl AnswerFile {
    /// Reads an answer file from its JSON.
    pub fn parse(json: &str) -> Result<Self> {
        let file: Self = serde_json::from_str(json)?;
        if file.format != FORMAT {
            return invalid(format!("answers format {}: this version reads format {FORMAT}", file.format));
        }
        // A name ends up in file names, in the root and on the screen, so it is held to the provider's form.
        if let Some(bad) =
            std::iter::once(&file.model).chain(file.records.iter().map(|r| &r.model)).find(|m| !valid_model(m))
        {
            return invalid(format!("{bad:?} is not a model name"));
        }
        if let Some(bad) = file.records.iter().filter_map(|r| r.host.as_ref()).find(|h| !valid_host(h)) {
            return invalid(format!("{bad:?} is not a host's name"));
        }
        Ok(file)
    }
}

/// One model's score on one track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ModelScore {
    /// The model.
    pub model: String,
    /// Questions answered right.
    pub correct: u32,
    /// Questions asked.
    pub total: u32,
    /// What all its answers cost, in billionths of a US dollar.
    pub cost_nano_usd: u64,
    /// The cost divided by the right answers, rounded to the nearest billionth; absent with no right answer.
    pub cost_per_correct_nano_usd: Option<u64>,
}

/// The scores of one track, best first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackScores {
    /// The track.
    pub track: Track,
    /// More right answers first; then the cheaper; then by name.
    pub models: Vec<ModelScore>,
}

/// An exam's results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Results {
    /// The file format.
    pub format: u32,
    /// The exam.
    pub exam_id: String,
    /// The budget per question on the same-budget track, in billionths of a US dollar.
    pub budget_nano_usd: u64,
    /// The scores, track by track.
    pub tracks: Vec<TrackScores>,
    /// How many answers the root covers.
    pub leaf_count: u32,
    /// The Merkle root over every answer.
    pub results_root: String,
}

impl Results {
    /// Reads results from their JSON.
    pub fn parse(json: &str) -> Result<Self> {
        let results: Self = serde_json::from_str(json)?;
        if results.format != FORMAT {
            return invalid(format!("results format {}: this version reads format {FORMAT}", results.format));
        }
        Ok(results)
    }
}

/// Grades a reply. One rule for every model: the right answer, no error, and on the same-budget track a cost
/// within the budget. Returns the answer taken from the reply and whether it counts.
#[must_use]
pub fn score(
    question: &Question,
    track: Track,
    raw: &str,
    cost_nano_usd: u64,
    failed: bool,
    budget_nano_usd: u64,
) -> (String, bool) {
    let extracted = extract_answer(raw);
    let within_budget = track != Track::SameBudget || cost_nano_usd <= budget_nano_usd;
    (extracted.to_string(), !failed && within_budget && grade(question.grade, &question.answer, extracted))
}

/// An answer as it goes into the Merkle tree: every field of the record, with the reply replaced by its hash.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Leaf<'a> {
    exam_id: &'a str,
    track: Track,
    model: &'a str,
    question_id: &'a str,
    raw_sha256: String,
    extracted: &'a str,
    correct: bool,
    cost_nano_usd: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    max_tokens: u32,
    latency_ms: u64,
    finish_reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

/// The text of an answer's leaf in the Merkle tree.
pub fn leaf_of(exam_id: &str, record: &AnswerRecord) -> Result<String> {
    canonical(&Leaf {
        exam_id,
        track: record.track,
        model: &record.model,
        question_id: &record.question_id,
        raw_sha256: sha256_hex(record.raw.as_bytes()),
        extracted: &record.extracted,
        correct: record.correct,
        cost_nano_usd: record.cost_nano_usd,
        prompt_tokens: record.prompt_tokens,
        completion_tokens: record.completion_tokens,
        max_tokens: record.max_tokens,
        latency_ms: record.latency_ms,
        finish_reason: record.finish_reason.as_deref(),
        host: record.host.as_deref(),
        error: record.error.as_deref(),
    })
}

/// Every record of every file, in the one order the tree is built in: by track, then model, then question.
///
/// Refuses files that are not about this exam, a record filed under another model, a question the exam does
/// not have, an answer given twice, and a model that did not answer every question: left-out answers would
/// make a model look cheaper than it was.
pub fn ordered_records<'a>(exam: &Exam, files: &'a [AnswerFile]) -> Result<Vec<&'a AnswerRecord>> {
    let mut records = Vec::new();
    for file in files {
        if file.exam_id != exam.id {
            return invalid(format!("the answers of {} are for exam {}", file.model, file.exam_id));
        }
        for record in &file.records {
            if record.model != file.model {
                return invalid(format!("an answer by {} is filed under {}", record.model, file.model));
            }
            if !exam.questions.iter().any(|q| q.id == record.question_id) {
                return invalid(format!("{} answered {}, which is not in the exam", record.model, record.question_id));
            }
            records.push(record);
        }
    }
    if records.is_empty() {
        return invalid("there are no answers");
    }
    let key = |r: &'a AnswerRecord| (r.track, r.model.as_str(), r.question_id.as_str());
    records.sort_by_key(|r| key(r));
    if let Some(pair) = records.windows(2).find(|pair| key(pair[0]) == key(pair[1])) {
        return invalid(format!(
            "{} answered {} twice on the {} track",
            pair[0].model,
            pair[0].question_id,
            pair[0].track.name()
        ));
    }
    // No answer is doubled and every one is to a question of the exam, so a full set has exactly this many.
    for group in records.chunk_by(|a, b| (a.track, &a.model) == (b.track, &b.model)) {
        if group.len() != exam.questions.len() {
            return invalid(format!(
                "{} answered {} of {} questions on the {} track",
                group[0].model,
                group.len(),
                exam.questions.len(),
                group[0].track.name()
            ));
        }
    }
    Ok(records)
}

/// Records whose stored answer or mark is not what grading the reply gives.
pub fn regrade<'a>(exam: &Exam, records: &[&'a AnswerRecord], budget_nano_usd: u64) -> Vec<&'a AnswerRecord> {
    let questions: BTreeMap<&str, &Question> = exam.questions.iter().map(|q| (q.id.as_str(), q)).collect();
    records
        .iter()
        .filter(|r| {
            questions.get(r.question_id.as_str()).is_none_or(|q| {
                score(q, r.track, &r.raw, r.cost_nano_usd, r.error.is_some(), budget_nano_usd)
                    != (r.extracted.clone(), r.correct)
            })
        })
        .copied()
        .collect()
}

/// Scores an exam from its answer files and builds the root over every answer.
pub fn build_results(exam: &Exam, files: &[AnswerFile], budget_nano_usd: u64) -> Result<Results> {
    let records = ordered_records(exam, files)?;
    if let Some(record) = regrade(exam, &records, budget_nano_usd).first() {
        return invalid(format!(
            "the stored mark of {} on {} is not what its reply grades to",
            record.model, record.question_id
        ));
    }
    let mut tracks: BTreeMap<Track, BTreeMap<&str, ModelScore>> = BTreeMap::new();
    let mut leaves = Vec::with_capacity(records.len());
    for record in &records {
        let entry = tracks.entry(record.track).or_default().entry(&record.model).or_insert_with(|| ModelScore {
            model: record.model.clone(),
            correct: 0,
            total: 0,
            cost_nano_usd: 0,
            cost_per_correct_nano_usd: None,
        });
        entry.total += 1;
        entry.correct += u32::from(record.correct);
        let Some(cost) = entry.cost_nano_usd.checked_add(record.cost_nano_usd) else {
            return invalid("the costs add up to more than can be counted");
        };
        entry.cost_nano_usd = cost;
        leaves.push(leaf_hash(leaf_of(&exam.id, record)?.as_bytes()));
    }
    let tracks = tracks
        .into_iter()
        .map(|(track, models)| {
            let mut models: Vec<ModelScore> = models.into_values().collect();
            for score in &mut models {
                let right = u64::from(score.correct);
                score.cost_per_correct_nano_usd = (right > 0)
                    .then(|| score.cost_nano_usd / right + u64::from(score.cost_nano_usd % right >= right.div_ceil(2)));
            }
            models.sort_by(|a, b| {
                b.correct
                    .cmp(&a.correct)
                    .then(a.cost_nano_usd.cmp(&b.cost_nano_usd))
                    .then_with(|| a.model.cmp(&b.model))
            });
            TrackScores { track, models }
        })
        .collect();
    let Ok(leaf_count) = u32::try_from(leaves.len()) else {
        return invalid("there are too many answers");
    };
    Ok(Results {
        format: FORMAT,
        exam_id: exam.id.clone(),
        budget_nano_usd,
        tracks,
        leaf_count,
        results_root: to_hex(&merkle_root(&leaves)?),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::exam::Recipe;
    use crate::exam::{make_exam, parse_mix, tests::recipe};

    pub(crate) const BUDGET: u64 = 20_000_000;

    pub(crate) fn small_exam() -> Exam {
        make_exam(&Recipe { mix: parse_mix("stack:1,queue:1,strings:1").unwrap(), ..recipe() }).unwrap()
    }

    /// A model that answers the first `right` questions correctly and the rest wrongly, at `cost` each.
    pub(crate) fn answers(exam: &Exam, model: &str, track: Track, right: usize, cost: u64) -> AnswerFile {
        let records = exam
            .questions
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let raw = if i < right {
                    format!("Let me work this out.\nANSWER: {}", q.answer)
                } else {
                    "ANSWER: no idea".to_string()
                };
                let (extracted, correct) = score(q, track, &raw, cost, false, BUDGET);
                AnswerRecord {
                    model: model.into(),
                    track,
                    question_id: q.id.clone(),
                    raw,
                    extracted,
                    correct,
                    cost_nano_usd: cost,
                    prompt_tokens: 100,
                    completion_tokens: 50,
                    max_tokens: 1000,
                    latency_ms: 1200,
                    finish_reason: Some("stop".into()),
                    host: None,
                    error: None,
                }
            })
            .collect();
        AnswerFile { format: FORMAT, exam_id: exam.id.clone(), model: model.into(), records }
    }

    fn merged(mut a: AnswerFile, b: AnswerFile) -> AnswerFile {
        a.records.extend(b.records);
        a
    }

    #[test]
    fn scores_count_right_answers_and_cost() {
        let exam = small_exam();
        let files = [
            answers(&exam, "lab/strong", Track::SameBudget, 3, 9_000_000),
            answers(&exam, "lab/weak", Track::SameBudget, 1, 1_000_000),
        ];
        let results = build_results(&exam, &files, BUDGET).unwrap();
        assert_eq!(results.leaf_count, 6);
        assert_eq!(results.tracks.len(), 1);
        let models = &results.tracks[0].models;
        assert_eq!(
            models[0],
            ModelScore {
                model: "lab/strong".into(),
                correct: 3,
                total: 3,
                cost_nano_usd: 27_000_000,
                cost_per_correct_nano_usd: Some(9_000_000)
            }
        );
        assert_eq!(
            models[1],
            ModelScore {
                model: "lab/weak".into(),
                correct: 1,
                total: 3,
                cost_nano_usd: 3_000_000,
                cost_per_correct_nano_usd: Some(3_000_000)
            }
        );
    }

    #[test]
    fn ties_go_to_the_cheaper_model_then_to_the_name() {
        let exam = small_exam();
        let files = [
            answers(&exam, "b", Track::Unlimited, 2, 500),
            answers(&exam, "a", Track::Unlimited, 2, 500),
            answers(&exam, "c", Track::Unlimited, 2, 100),
            answers(&exam, "d", Track::Unlimited, 0, 1),
        ];
        let results = build_results(&exam, &files, BUDGET).unwrap();
        let order: Vec<&str> = results.tracks[0].models.iter().map(|m| m.model.as_str()).collect();
        assert_eq!(order, ["c", "a", "b", "d"]);
        assert_eq!(results.tracks[0].models[3].cost_per_correct_nano_usd, None);
        // 300 over 2 right answers is 150; 1500 over 2 is 750.
        assert_eq!(results.tracks[0].models[0].cost_per_correct_nano_usd, Some(150));
    }

    #[test]
    fn cost_per_right_answer_rounds_to_the_nearest() {
        let exam = small_exam();
        let per_right = |right: usize, cost: u64| {
            build_results(&exam, &[answers(&exam, "m", Track::Unlimited, right, cost)], BUDGET).unwrap().tracks[0]
                .models[0]
                .cost_per_correct_nano_usd
        };
        // Three answers at 5 cost 15: over 2 right that is 7.5, which rounds up; at 1 they cost 3, and 1.5 rounds up.
        assert_eq!(per_right(2, 5), Some(8));
        assert_eq!(per_right(2, 1), Some(2));
        // 3 * 7 = 21 over 3 right is 7 exactly; 3 * 3 = 9 over 2 is 4.5, up to 5; 3 * 11 = 33 over 2 is 16.5, up to 17.
        assert_eq!(per_right(3, 7), Some(7));
        assert_eq!(per_right(2, 3), Some(5));
        assert_eq!(per_right(2, 11), Some(17));
        // 3 * 4 = 12 over 2 is 6; 3 * 10 = 30 over 1 is 30.
        assert_eq!(per_right(2, 4), Some(6));
        assert_eq!(per_right(1, 10), Some(30));
    }

    #[test]
    fn over_budget_answers_do_not_count_on_the_same_budget_track_only() {
        let exam = small_exam();
        let q = &exam.questions[0];
        let raw = format!("ANSWER: {}", q.answer);
        assert_eq!(score(q, Track::SameBudget, &raw, BUDGET, false, BUDGET), (q.answer.clone(), true));
        assert_eq!(score(q, Track::SameBudget, &raw, BUDGET + 1, false, BUDGET), (q.answer.clone(), false));
        assert_eq!(score(q, Track::Unlimited, &raw, BUDGET * 50, false, BUDGET), (q.answer.clone(), true));
        assert_eq!(score(q, Track::Unlimited, &raw, 0, true, BUDGET), (q.answer.clone(), false));
        assert_eq!(score(q, Track::Unlimited, "I do not know", 0, false, BUDGET), (String::new(), false));
    }

    #[test]
    fn both_tracks_are_scored_apart() {
        let exam = small_exam();
        let file = merged(answers(&exam, "m", Track::SameBudget, 1, 10), answers(&exam, "m", Track::Unlimited, 3, 900));
        let results = build_results(&exam, &[file], BUDGET).unwrap();
        assert_eq!(
            results.tracks.iter().map(|t| (t.track, t.models[0].correct)).collect::<Vec<_>>(),
            [(Track::SameBudget, 1), (Track::Unlimited, 3)]
        );
        assert_eq!(results.leaf_count, 6);
    }

    #[test]
    fn the_root_does_not_depend_on_the_order_of_files_or_records() {
        let exam = small_exam();
        let (a, b) = (answers(&exam, "a", Track::SameBudget, 2, 10), answers(&exam, "b", Track::SameBudget, 1, 20));
        let forward = build_results(&exam, &[a.clone(), b.clone()], BUDGET).unwrap();
        let mut reversed = a;
        reversed.records.reverse();
        assert_eq!(build_results(&exam, &[b, reversed], BUDGET).unwrap(), forward);
    }

    #[test]
    fn the_root_changes_with_any_field_of_any_answer() {
        let exam = small_exam();
        let file = answers(&exam, "m", Track::Unlimited, 2, 10);
        let root = |file: &AnswerFile| build_results(&exam, std::slice::from_ref(file), BUDGET).map(|r| r.results_root);
        let original = root(&file).unwrap();
        let changes: [fn(&mut AnswerRecord); 8] = [
            |r| r.cost_nano_usd += 1,
            |r| r.prompt_tokens += 1,
            |r| r.completion_tokens += 1,
            |r| r.max_tokens += 1,
            |r| r.latency_ms += 1,
            |r| r.finish_reason = None,
            |r| r.host = Some("Another host".into()),
            |r| r.raw.push(' '),
        ];
        for change in changes {
            let mut changed = file.clone();
            change(&mut changed.records[2]);
            assert_ne!(root(&changed).unwrap(), original);
        }
    }

    #[test]
    fn a_mark_that_the_reply_does_not_earn_is_refused() {
        let exam = small_exam();
        let mut file = answers(&exam, "m", Track::Unlimited, 1, 10);
        file.records[2].correct = true;
        assert!(build_results(&exam, &[file.clone()], BUDGET).unwrap_err().to_string().contains("q03"));
        file.records[2].correct = false;
        file.records[2].extracted = "something else".into();
        assert!(build_results(&exam, &[file.clone()], BUDGET).is_err());
        // An answer with an error never counts, even when the reply holds the key.
        let mut failed = answers(&exam, "m", Track::Unlimited, 3, 10);
        failed.records[0].error = Some("timeout".into());
        assert!(build_results(&exam, &[failed.clone()], BUDGET).is_err());
        failed.records[0].correct = false;
        assert_eq!(build_results(&exam, &[failed], BUDGET).unwrap().tracks[0].models[0].correct, 2);
    }

    #[test]
    fn incomplete_doubled_or_misfiled_answers_are_refused() {
        let exam = small_exam();
        let good = answers(&exam, "m", Track::Unlimited, 1, 10);
        let refused = |file: AnswerFile| build_results(&exam, &[file], BUDGET).is_err();
        assert!(!refused(good.clone()));
        let mut missing = good.clone();
        missing.records.pop();
        assert!(refused(missing));
        let mut doubled = good.clone();
        doubled.records.push(good.records[0].clone());
        assert!(refused(doubled));
        let mut misfiled = good.clone();
        misfiled.records[0].model = "another".into();
        assert!(refused(misfiled));
        let mut unknown = good.clone();
        unknown.records[0].question_id = "q99".into();
        assert!(refused(unknown));
        assert!(refused(AnswerFile { exam_id: "another-exam".into(), ..good.clone() }));
        assert!(refused(AnswerFile { records: Vec::new(), ..good.clone() }));
        assert!(build_results(&exam, &[good.clone(), good], BUDGET).is_err());
        assert!(build_results(&exam, &[], BUDGET).is_err());
    }

    #[test]
    fn files_read_strictly() {
        let exam = small_exam();
        let file = answers(&exam, "m", Track::Unlimited, 1, 10);
        let json = serde_json::to_string(&file).unwrap();
        assert_eq!(AnswerFile::parse(&json).unwrap(), file);
        assert!(
            json.contains("\"costNanoUsd\":10")
                && json.contains("\"finishReason\":\"stop\"")
                && !json.contains("\"error\"")
        );
        assert!(AnswerFile::parse(&json.replacen("\"costNanoUsd\":10", "\"costNanoUsd\":10.5", 1)).is_err());
        assert!(AnswerFile::parse(&json.replacen("\"costNanoUsd\":10", "\"costNanoUsd\":-1", 1)).is_err());
        assert!(AnswerFile::parse(&json.replacen("\"track\":\"unlimited\"", "\"track\":\"open\"", 1)).is_err());
        assert!(AnswerFile::parse(&json.replacen("\"format\":1", "\"format\":2", 1)).is_err());
        assert!(AnswerFile::parse(&json.replacen("\"correct\":true", "\"correct\":true,\"note\":1", 1)).is_err());
        for name in ["m\\nPASS exams/x (open)", "m n", "", "caf\u{e9}"] {
            let renamed = json.replace("\"model\":\"m\"", &format!("\"model\":\"{name}\""));
            assert!(AnswerFile::parse(&renamed).unwrap_err().to_string().contains("is not a model name"), "{name}");
        }
        assert!(valid_model("lab/model:free") && valid_model(&"x".repeat(200)) && !valid_model(&"x".repeat(201)));
        // the host that served a reply: kept when it is a plain name, refused when it carries a line of its own
        let hosted =
            json.replacen("\"finishReason\":\"stop\"", "\"finishReason\":\"stop\",\"host\":\"Amazon Bedrock\"", 1);
        assert_eq!(AnswerFile::parse(&hosted).unwrap().records[0].host.as_deref(), Some("Amazon Bedrock"));
        let forged = hosted.replacen("Amazon Bedrock", "Amazon\\nPASS", 1);
        assert!(AnswerFile::parse(&forged).unwrap_err().to_string().contains("is not a host's name"));
        assert!(valid_host("Baidu") && !valid_host("") && !valid_host(&"x".repeat(101)) && !valid_host("caf\u{e9}"));
        let results = build_results(&exam, &[file], BUDGET).unwrap();
        let json = serde_json::to_string(&results).unwrap();
        assert_eq!(Results::parse(&json).unwrap(), results);
        assert!(json.contains("\"budgetNanoUsd\":20000000") && json.contains("\"costPerCorrectNanoUsd\":30"));
        assert!(Results::parse(&json.replacen("\"leafCount\":3", "\"leafCount\":3,\"x\":0", 1)).is_err());
    }

    #[test]
    fn a_leaf_names_everything_but_the_reply_itself() {
        let exam = small_exam();
        let file = answers(&exam, "m", Track::SameBudget, 1, 10);
        let leaf = leaf_of(&exam.id, &file.records[0]).unwrap();
        assert!(leaf.starts_with(
            "{\"completionTokens\":50,\"correct\":true,\"costNanoUsd\":10,\"examId\":\"test-01\",\"extracted\":"
        ));
        assert!(leaf.contains(&format!("\"rawSha256\":\"{}\"", sha256_hex(file.records[0].raw.as_bytes()))));
        assert!(!leaf.contains("Let me work this out") && !leaf.contains("\"error\""));
        let mut failed = file.records[0].clone();
        failed.error = Some("timeout".into());
        assert!(leaf_of(&exam.id, &failed).unwrap().contains("\"error\":\"timeout\""));
    }
}
