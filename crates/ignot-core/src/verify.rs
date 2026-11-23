//! Checking a published exam from its files alone.
//!
//! Nothing here trusts whoever published the files. Each check recomputes one thing the publication claims:
//! that the sealed file is the one promised, that it could not be opened before its round, that the opened
//! exam is the one that was sealed and comes from its seed and nothing else, and that the scores follow from
//! the answers.

use serde::Serialize;

use crate::drand::{self, Beacon};
use crate::exam::{Exam, chain_exam_id, make_exam};
use crate::hash::{same_hex, sha256_hex};
use crate::results::{AnswerFile, Results, build_results, ordered_records, regrade};
use crate::seal::{ChainRef, Manifest};
use crate::{GENERATOR_VERSION, NAME, Result, chain, date, timelock};

/// Receipts of the transactions a manifest names, as a node returned them (`eth_getTransactionReceipt`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Receipts<'a> {
    /// The receipt of the seal transaction.
    pub seal: Option<&'a str>,
    /// The receipt of the transaction that published the results.
    pub publish: Option<&'a str>,
}

/// Everything a check can look at: the published files, and what the outside world says.
#[derive(Debug, Clone, Default)]
pub struct Evidence<'a> {
    /// `manifest.json`.
    pub manifest: &'a str,
    /// The sealed file, byte for byte.
    pub ciphertext: &'a [u8],
    /// The time now, in Unix milliseconds. Only used when no signature is given, to tell "still sealed" from
    /// "should have opened": a signature that verifies settles the matter on its own.
    pub now_ms: u64,
    /// What a drand relay answers for the manifest's round, or the bare signature in hex.
    pub beacon: Option<&'a str>,
    /// The published `exam.json`.
    pub exam: Option<&'a str>,
    /// Every published answer file.
    pub answers: Option<Vec<&'a str>>,
    /// The published `results.json`.
    pub results: Option<&'a str>,
    /// Receipts from the chain, when it was read.
    pub chain: Option<Receipts<'a>>,
    /// The record contract to trust: [`chain::RECORD`], unless the caller runs a record of its own.
    pub record: Option<chain::Record>,
}

/// One thing that was checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// What was checked.
    pub name: &'static str,
    /// Whether it holds. A check that could not be made has not failed, so it holds too; `skipped` tells
    /// the two apart, and a careful reader counts the skipped ones.
    pub ok: bool,
    /// Whether it could not be run, for the reason in `detail`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub skipped: bool,
    /// Why it failed or was skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Whether the exam can be read yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// No valid signature of the round was given: the exam was not opened.
    Sealed,
    /// The round's signature exists, so anyone can open the exam.
    Open,
}

/// The outcome of checking an exam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// Whether no check failed. Checks that could not be made are marked `skipped`, one by one.
    pub ok: bool,
    /// Whether the exam can be read yet.
    pub state: State,
    /// Every check, in the order it was made.
    pub checks: Vec<Check>,
    /// The opened exam, when it opened and reads as an exam.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exam: Option<Exam>,
}

#[derive(Default)]
struct Checks(Vec<Check>);

impl Checks {
    fn test(&mut self, name: &'static str, ok: bool) -> bool {
        self.0.push(Check { name, ok, skipped: false, detail: None });
        ok
    }

    fn add(&mut self, name: &'static str, outcome: std::result::Result<(), String>) -> bool {
        let ok = outcome.is_ok();
        self.0.push(Check { name, ok, skipped: false, detail: outcome.err() });
        ok
    }

    fn skip(&mut self, name: &'static str, why: impl Into<String>) {
        self.0.push(Check { name, ok: true, skipped: true, detail: Some(why.into()) });
    }

    fn report(self, state: State, exam: Option<Exam>) -> Report {
        Report { ok: self.0.iter().all(|check| check.ok), state, checks: self.0, exam }
    }
}

const OPENED: &str = "the round's signature was given";
const NOTHING_EARLY: &str = "nothing of the exam is published before its round";
const SEAL_ON_CHAIN: &str = "the chain recorded this seal before the round";
const ROOT_ON_CHAIN: &str = "the chain recorded this results root";
const NO_RECORD: &str = "this version of ignot trusts no record contract yet";

/// Checks a published exam. An error means there is no manifest to check against; everything else is a
/// check in the report.
pub fn verify_exam(evidence: &Evidence) -> Result<Report> {
    let manifest = Manifest::parse(evidence.manifest)?;
    let mut checks = Checks::default();
    checks.test(
        "the sealed file has the hash in the manifest",
        same_hex(&sha256_hex(evidence.ciphertext), &manifest.ciphertext_sha256),
    );
    checks
        .test("the on-chain id is the hash of the exam id", same_hex(&chain_exam_id(&manifest.id), &manifest.exam_id));
    checks.test("the drand chain is quicknet", manifest.drand_chain == drand::CHAIN_HASH);
    let opens_at = drand::round_time(manifest.open_round);
    checks.test(
        "the opening time is the round's time",
        opens_at.as_ref().is_ok_and(|&ms| date::rfc3339(ms) == manifest.open_at),
    );
    checks.add(
        "the sealed file is locked to the manifest's round",
        match timelock::sealed_round(evidence.ciphertext) {
            Ok(round) if round == manifest.open_round => Ok(()),
            Ok(round) => Err(format!("it is locked to round {round}")),
            Err(e) => Err(e.to_string()),
        },
    );
    // The round's signature is the clock that counts: once it exists the round has passed, whatever the
    // caller's own clock says. Only without it does that clock tell "still sealed" from "overdue".
    let beacon = evidence.beacon.map(|answer| Beacon::parse(manifest.open_round, answer));
    let due = matches!(beacon, Some(Ok(_))) || opens_at.is_ok_and(|ms| evidence.now_ms >= ms);
    seal_on_chain(&mut checks, &manifest, evidence, due);

    let beacon = match beacon {
        Some(Ok(beacon)) => beacon,
        Some(Err(e)) => {
            checks.add(OPENED, Err(e.to_string()));
            return Ok(checks.report(State::Sealed, None));
        }
        None if due => {
            checks.add(OPENED, Err("the round has passed, and its signature was not given".into()));
            return Ok(checks.report(State::Sealed, None));
        }
        None => {
            checks.skip(OPENED, format!("sealed until {}", manifest.open_at));
            // A sealed exam's questions, answers or scores cannot exist yet. If something is there, either
            // the exam leaked or the numbers are made up.
            let early: Vec<&str> = [
                ("exam.json", evidence.exam.is_some()),
                ("answers", evidence.answers.is_some()),
                ("results.json", evidence.results.is_some()),
            ]
            .iter()
            .filter_map(|&(name, there)| there.then_some(name))
            .collect();
            checks.add(
                NOTHING_EARLY,
                if early.is_empty() { Ok(()) } else { Err(format!("the folder already holds {}", early.join(", "))) },
            );
            return Ok(checks.report(State::Sealed, None));
        }
    };
    checks.test(OPENED, true);

    let opened = timelock::open_bytes(evidence.ciphertext, &beacon).and_then(|bytes| {
        String::from_utf8(bytes).map_err(|_| crate::Error::Sealed("the opened file is not text".into()))
    });
    let plaintext = match opened {
        Ok(plaintext) => plaintext,
        Err(e) => {
            checks.add("the sealed file opens with that signature", Err(e.to_string()));
            return Ok(checks.report(State::Open, None));
        }
    };
    checks.test("the sealed file opens with that signature", true);
    checks.test(
        "the opened exam has the hash in the manifest",
        same_hex(&sha256_hex(plaintext.as_bytes()), &manifest.plaintext_sha256),
    );
    if let Some(published) = evidence.exam {
        checks.test("the published exam.json is the opened exam", published.trim() == plaintext.trim());
    }
    let exam = match Exam::parse(&plaintext) {
        Ok(exam) => exam,
        Err(e) => {
            checks.add("the opened file is an exam", Err(e.to_string()));
            return Ok(checks.report(State::Open, None));
        }
    };
    checks.test("the opened file is an exam", true);
    checks.test(
        "the exam's id, title, size and generator are the manifest's",
        exam.id == manifest.id
            && exam.title == manifest.title
            && u32::try_from(exam.questions.len()).is_ok_and(|n| n == manifest.question_count)
            && exam.generator == manifest.generator,
    );

    const REGENERATES: &str = "the questions and keys regenerate from the revealed seed";
    if exam.generator.name == NAME && exam.generator.version == GENERATOR_VERSION {
        checks.add(
            REGENERATES,
            match make_exam(&exam.recipe()).and_then(|again| again.bytes()) {
                Ok(again) if again == plaintext => Ok(()),
                Ok(_) => Err("the seed gives a different exam".into()),
                Err(e) => Err(e.to_string()),
            },
        );
    } else {
        // This build cannot make that exam again, so it cannot vouch for its questions or its keys.
        checks.add(
            REGENERATES,
            Err(format!(
                "it was made by {} version {}, and this build makes {NAME} version {GENERATOR_VERSION}: check it with the release that made it",
                exam.generator.name, exam.generator.version
            )),
        );
    }

    match (&evidence.answers, evidence.results) {
        (None, None) => {}
        (Some(answers), Some(results)) => scores(&mut checks, &manifest, &exam, answers, results, evidence),
        _ => {
            checks.add("answers and results are published together", Err("one is there without the other".into()));
        }
    }
    Ok(checks.report(State::Open, Some(exam)))
}

/// Whether a manifest's chain reference points at the record contract.
fn is_record(reference: &ChainRef, record: &chain::Record) -> std::result::Result<(), String> {
    if reference.chain_id == record.chain_id && same_hex(&reference.contract, record.contract) {
        Ok(())
    } else {
        Err("the manifest points at a contract that is not the record contract".into())
    }
}

/// `due` says whether the round's time has come: from then on a seal that was never recorded cannot be shown
/// to be older than its round.
fn seal_on_chain(checks: &mut Checks, manifest: &Manifest, evidence: &Evidence, due: bool) {
    let Some(record) = evidence.record else {
        return checks.skip(SEAL_ON_CHAIN, NO_RECORD);
    };
    let Some((reference, tx)) = manifest.chain.as_ref().and_then(|c| Some((c, c.seal_tx.as_deref()?))) else {
        if due {
            checks.add(SEAL_ON_CHAIN, Err("the manifest names no seal transaction".into()));
        } else {
            checks.skip(SEAL_ON_CHAIN, "the manifest names no seal transaction yet");
        }
        return;
    };
    if let Err(other) = is_record(reference, &record) {
        checks.add(SEAL_ON_CHAIN, Err(other));
        return;
    }
    let Some(receipts) = evidence.chain else {
        return checks.skip(SEAL_ON_CHAIN, "the chain was not read");
    };
    let outcome = match receipts.seal.map(|receipt| chain::seal_event(receipt, tx, record.contract)) {
        None => Err("no receipt of the seal transaction was given".into()),
        Some(Err(e)) => Err(e.to_string()),
        Some(Ok(None)) => Err("that transaction holds no Sealed event from the contract".into()),
        Some(Ok(Some(event))) => {
            let same = same_hex(&event.exam_id, &manifest.exam_id)
                && same_hex(&event.ciphertext_hash, &manifest.ciphertext_sha256)
                && same_hex(&event.plaintext_hash, &manifest.plaintext_sha256)
                && event.drand_round == manifest.open_round;
            if same { Ok(()) } else { Err("the chain recorded other hashes or another round".into()) }
        }
    };
    checks.add(SEAL_ON_CHAIN, outcome);
}

fn scores(checks: &mut Checks, manifest: &Manifest, exam: &Exam, answers: &[&str], results: &str, evidence: &Evidence) {
    let read = answers
        .iter()
        .map(|text| AnswerFile::parse(text))
        .collect::<Result<Vec<_>>>()
        .and_then(|files| Ok((files, Results::parse(results)?)));
    let (files, results) = match read {
        Ok(read) => read,
        Err(e) => {
            checks.add("the answers and results read", Err(e.to_string()));
            return;
        }
    };
    checks.test("the answers and results read", true);
    checks.test("the results are for this exam", results.exam_id == exam.id);
    let records = match ordered_records(exam, &files) {
        Ok(records) => records,
        Err(e) => {
            checks.add("every model answered every question once", Err(e.to_string()));
            return;
        }
    };
    checks.test("every model answered every question once", true);
    let off = regrade(exam, &records, results.budget_nano_usd);
    checks.add(
        "every reply grades to its stored mark",
        match off.first() {
            None => Ok(()),
            Some(first) => Err(format!(
                "{} of {} do not, the first being {} on {}",
                off.len(),
                records.len(),
                first.model,
                first.question_id
            )),
        },
    );
    let again = build_results(exam, &files, results.budget_nano_usd);
    checks.add(
        "the scores and the root recompute from the answers",
        match &again {
            Ok(again) if *again == results => Ok(()),
            Ok(again) => Err(format!("recomputing gives root {}", again.results_root)),
            Err(e) => Err(e.to_string()),
        },
    );

    let Some(record) = evidence.record else {
        return checks.skip(ROOT_ON_CHAIN, NO_RECORD);
    };
    let Some((reference, tx)) = manifest.chain.as_ref().and_then(|c| Some((c, c.publish_tx.as_deref()?))) else {
        return checks.skip(ROOT_ON_CHAIN, "the manifest names no publish transaction");
    };
    if let Err(other) = is_record(reference, &record) {
        checks.add(ROOT_ON_CHAIN, Err(other));
        return;
    }
    let Some(receipts) = evidence.chain else {
        return checks.skip(ROOT_ON_CHAIN, "the chain was not read");
    };
    let outcome = match receipts.publish.map(|receipt| chain::publish_event(receipt, tx, record.contract)) {
        None => Err("no receipt of the publish transaction was given".into()),
        Some(Err(e)) => Err(e.to_string()),
        Some(Ok(None)) => Err("that transaction holds no Published event from the contract".into()),
        Some(Ok(Some(event))) => {
            // Compared with the root computed here from the answers, not with the one the results file states.
            let same = same_hex(&event.exam_id, &manifest.exam_id)
                && again.is_ok_and(|again| same_hex(&event.results_root, &again.results_root));
            if same { Ok(()) } else { Err("the chain recorded another root".into()) }
        }
    };
    checks.add(ROOT_ON_CHAIN, outcome);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Track;
    use crate::chain::tests::{CONTRACT, TX, publish_receipt, seal_receipt};
    use crate::drand::tests::{ROUND, SIGNATURE};
    use crate::results::tests::{BUDGET, answers, small_exam};
    use crate::seal::{ChainRef, seal_exam};

    /// A published exam, as text files.
    struct Published {
        exam: Exam,
        manifest: Manifest,
        manifest_text: String,
        ciphertext: String,
        answers: Vec<String>,
        results: String,
    }

    fn published() -> Published {
        let exam = small_exam();
        let (ciphertext, manifest) = seal_exam(&exam, ROUND).unwrap();
        let files = [
            answers(&exam, "lab/strong", Track::SameBudget, 3, 900),
            answers(&exam, "lab/weak", Track::SameBudget, 1, 100),
        ];
        let results = serde_json::to_string(&build_results(&exam, &files, BUDGET).unwrap()).unwrap();
        let manifest_text = serde_json::to_string_pretty(&manifest).unwrap();
        Published {
            exam,
            manifest,
            manifest_text,
            ciphertext,
            answers: files.iter().map(|f| serde_json::to_string(f).unwrap()).collect(),
            results,
        }
    }

    impl Published {
        /// Changes the manifest, as a publisher who misstates something would.
        fn edit(&mut self, change: impl FnOnce(&mut Manifest)) {
            change(&mut self.manifest);
            self.manifest_text = serde_json::to_string_pretty(&self.manifest).unwrap();
        }

        /// Everything published and the signature, with the chain not read.
        fn check<'a>(&'a self, change: impl FnOnce(&mut Evidence<'a>)) -> Report {
            let mut evidence = Evidence {
                manifest: &self.manifest_text,
                ciphertext: self.ciphertext.as_bytes(),
                now_ms: u64::MAX,
                beacon: Some(SIGNATURE),
                answers: Some(self.answers.iter().map(String::as_str).collect()),
                results: Some(&self.results),
                ..Evidence::default()
            };
            change(&mut evidence);
            verify_exam(&evidence).unwrap()
        }
    }

    fn failed(report: &Report) -> Vec<&'static str> {
        report.checks.iter().filter(|c| !c.ok).map(|c| c.name).collect()
    }

    fn skipped(report: &Report) -> Vec<&'static str> {
        report.checks.iter().filter(|c| c.skipped).map(|c| c.name).collect()
    }

    #[test]
    fn a_sealed_exam_checks_out_without_being_opened() {
        let p = published();
        let report = p.check(|e| {
            (e.beacon, e.answers, e.results) = (None, None, None);
            e.now_ms = drand::round_time(ROUND).unwrap() - 1;
        });
        assert!(report.ok);
        assert_eq!(report.state, State::Sealed);
        assert_eq!(report.exam, None);
        assert_eq!(skipped(&report), [SEAL_ON_CHAIN, OPENED]);
        assert_eq!(report.checks.len(), 8);
        assert_eq!(report.checks[7].name, NOTHING_EARLY);
        assert_eq!(report.checks[6].detail.as_deref(), Some("sealed until 2023-08-23T15:59:24.000Z"));
    }

    #[test]
    fn a_sealed_exam_with_questions_or_scores_beside_it_fails() {
        // Before the round nobody can hold the exam, an answer to it or a score: these are leaked or made up.
        let p = published();
        let sealed = drand::round_time(ROUND).unwrap() - 1;
        let exam_text = p.exam.bytes().unwrap();
        let report = p.check(|e| {
            (e.beacon, e.now_ms, e.exam) = (None, sealed, Some(exam_text.as_str()));
        });
        assert_eq!((report.ok, report.state), (false, State::Sealed));
        assert_eq!(failed(&report), [NOTHING_EARLY]);
        assert_eq!(
            report.checks[7].detail.as_deref(),
            Some("the folder already holds exam.json, answers, results.json")
        );
        let report = p.check(|e| (e.beacon, e.now_ms, e.answers) = (None, sealed, None));
        assert_eq!(report.checks[7].detail.as_deref(), Some("the folder already holds results.json"));
        let report = p.check(|e| (e.beacon, e.now_ms, e.results) = (None, sealed, Some("made up")));
        assert_eq!(failed(&report), [NOTHING_EARLY]);
    }

    #[test]
    fn a_published_exam_checks_out_on_a_machine_whose_clock_is_behind() {
        // The clock says the round is still ahead; the signature says otherwise, and the signature wins.
        let p = published();
        let exam_text = p.exam.bytes().unwrap();
        let report =
            p.check(|e| (e.now_ms, e.exam) = (drand::round_time(ROUND).unwrap() - 3_600_000, Some(exam_text.as_str())));
        assert!(report.ok, "{:?}", failed(&report));
        assert_eq!(report.state, State::Open);
        assert!(report.checks.iter().all(|check| check.name != NOTHING_EARLY));
    }

    #[test]
    fn past_the_round_a_missing_signature_is_a_failure() {
        let p = published();
        let report = p.check(|e| {
            e.beacon = None;
            e.now_ms = drand::round_time(ROUND).unwrap();
        });
        assert_eq!((report.ok, report.state), (false, State::Sealed));
        assert_eq!(failed(&report), [OPENED]);
    }

    #[test]
    fn an_open_exam_with_its_answers_checks_out() {
        let p = published();
        let exam_text = p.exam.bytes().unwrap();
        let report = p.check(|e| e.exam = Some(&exam_text));
        assert!(report.ok, "{:?}", failed(&report));
        assert_eq!(report.state, State::Open);
        assert_eq!(report.exam.as_ref(), Some(&p.exam));
        assert_eq!(skipped(&report), [SEAL_ON_CHAIN, ROOT_ON_CHAIN]);
        assert_eq!(report.checks.len(), 19);
        // The clock of whoever checks does not matter once the signature exists.
        assert!(p.check(|e| e.now_ms = 0).ok);
    }

    #[test]
    fn a_signature_that_is_not_the_rounds_opens_nothing() {
        let p = published();
        for wrong in ["", "00", "{}", &SIGNATURE.replace('b', "c")] {
            let report = p.check(|e| e.beacon = Some(wrong));
            assert_eq!((report.ok, report.state), (false, State::Sealed));
            assert_eq!(failed(&report), [OPENED], "{wrong}");
            assert_eq!(report.exam, None);
        }
    }

    #[test]
    fn a_swapped_sealed_file_is_caught() {
        let p = published();
        let (other, _) = seal_exam(&p.exam, ROUND).unwrap();
        let report = p.check(|e| e.ciphertext = other.as_bytes());
        assert_eq!(failed(&report), ["the sealed file has the hash in the manifest"]);
        let report = p.check(|e| e.ciphertext = b"not a sealed file");
        assert_eq!(
            failed(&report),
            [
                "the sealed file has the hash in the manifest",
                "the sealed file is locked to the manifest's round",
                "the sealed file opens with that signature"
            ]
        );
        assert_eq!(report.exam, None);
    }

    #[test]
    fn a_manifest_that_misstates_the_exam_is_caught() {
        let mut p = published();
        p.edit(|m| m.title = "Another title".into());
        assert_eq!(failed(&p.check(|_| {})), ["the exam's id, title, size and generator are the manifest's"]);
        let mut p = published();
        p.edit(|m| m.question_count = 4);
        assert_eq!(failed(&p.check(|_| {})), ["the exam's id, title, size and generator are the manifest's"]);
        let mut p = published();
        p.edit(|m| m.open_at = "2023-08-23T15:59:25.000Z".into());
        assert_eq!(failed(&p.check(|_| {})), ["the opening time is the round's time"]);
        let mut p = published();
        p.edit(|m| m.exam_id = chain_exam_id("another"));
        assert_eq!(failed(&p.check(|_| {})), ["the on-chain id is the hash of the exam id"]);
        let mut p = published();
        p.edit(|m| m.drand_chain = "dbd506d6ef76e5f386f41c651dcb808c5bcbd75471cc4eafa3f4df7ad4e4c493".into());
        assert_eq!(failed(&p.check(|_| {})), ["the drand chain is quicknet"]);
        let mut p = published();
        p.edit(|m| m.plaintext_sha256 = sha256_hex(b"another exam"));
        assert_eq!(failed(&p.check(|_| {})), ["the opened exam has the hash in the manifest"]);
    }

    #[test]
    fn a_manifest_that_names_another_round_is_caught() {
        let mut p = published();
        p.edit(|m| (m.open_round, m.open_at) = (ROUND + 1, date::rfc3339(drand::round_time(ROUND + 1).unwrap())));
        let report = p.check(|_| {});
        assert_eq!(failed(&report), ["the sealed file is locked to the manifest's round", OPENED]);
        assert_eq!(report.state, State::Sealed);
    }

    /// Seals arbitrary text under a manifest whose hashes match it, as a dishonest publisher could.
    fn forged(p: &Published, plaintext: &str) -> Published {
        let ciphertext = timelock::seal_bytes(plaintext.as_bytes(), ROUND).unwrap();
        let manifest = Manifest {
            ciphertext_sha256: sha256_hex(ciphertext.as_bytes()),
            plaintext_sha256: sha256_hex(plaintext.as_bytes()),
            ..p.manifest.clone()
        };
        let mut forgery = Published {
            exam: p.exam.clone(),
            manifest,
            manifest_text: String::new(),
            ciphertext,
            answers: p.answers.clone(),
            results: p.results.clone(),
        };
        forgery.edit(|_| {});
        forgery
    }

    #[test]
    fn a_hand_edited_exam_does_not_regenerate() {
        let p = published();
        let honest = p.exam.bytes().unwrap();
        // An easier key for the first question, sealed with hashes that match: only the seed gives it away.
        let mut edited = p.exam.clone();
        edited.questions[0].answer = "1".into();
        let report = forged(&p, &edited.bytes().unwrap()).check(|e| (e.answers, e.results) = (None, None));
        assert_eq!(failed(&report), ["the questions and keys regenerate from the revealed seed"]);
        // The same exam written with different spacing is not the sealed bytes either.
        let report = forged(&p, &honest.replacen(':', ": ", 1)).check(|e| (e.answers, e.results) = (None, None));
        assert_eq!(failed(&report), ["the questions and keys regenerate from the revealed seed"]);
        assert!(forged(&p, &honest).check(|_| {}).ok);
    }

    #[test]
    fn sealed_text_that_is_not_an_exam_is_caught() {
        let p = published();
        let report = forged(&p, "{\"hello\":\"world\"}").check(|_| {});
        assert_eq!(failed(&report), ["the opened file is an exam"]);
        assert_eq!((report.state, report.exam), (State::Open, None));
    }

    #[test]
    fn an_exam_that_names_another_generator_cannot_hide_hand_written_keys() {
        // Naming a version this build does not make must not turn "regenerate" into "trust".
        let p = published();
        for change in [
            |g: &mut crate::exam::Provenance| g.version = "2".into(),
            |g: &mut crate::exam::Provenance| {
                g.name = "another".into();
            },
        ] {
            let mut other = p.exam.clone();
            change(&mut other.generator);
            other.questions[0].answer = "1".into();
            let mut forgery = forged(&p, &other.bytes().unwrap());
            forgery.edit(|m| m.generator = other.generator.clone());
            let report = forgery.check(|e| (e.answers, e.results) = (None, None));
            assert_eq!(failed(&report), ["the questions and keys regenerate from the revealed seed"]);
            assert_eq!(skipped(&report), [SEAL_ON_CHAIN]);
        }
    }

    #[test]
    fn a_published_exam_json_must_be_the_sealed_one() {
        let p = published();
        let text = p.exam.bytes().unwrap();
        assert!(p.check(|e| e.exam = Some(&text)).ok);
        let with_newline = format!("{text}\n");
        assert!(p.check(|e| e.exam = Some(&with_newline)).ok);
        let edited = text.replacen("q01", "q00", 1);
        assert_eq!(failed(&p.check(|e| e.exam = Some(&edited))), ["the published exam.json is the opened exam"]);
    }

    #[test]
    fn changed_answers_or_scores_are_caught() {
        let p = published();
        let change = |text: &str, from: &str, to: &str| {
            assert!(text.contains(from), "{from}");
            text.replacen(from, to, 1)
        };
        // A wrong answer marked right.
        let mut edited = p.answers.clone();
        edited[1] = change(&edited[1], "\"correct\":false", "\"correct\":true");
        let report = p.check(|e| e.answers = Some(edited.iter().map(String::as_str).collect()));
        assert_eq!(
            failed(&report),
            ["every reply grades to its stored mark", "the scores and the root recompute from the answers"]
        );
        // A cheaper bill: the marks still grade, the root does not recompute.
        let mut edited = p.answers.clone();
        edited[0] = change(&edited[0], "\"costNanoUsd\":900", "\"costNanoUsd\":9");
        let report = p.check(|e| e.answers = Some(edited.iter().map(String::as_str).collect()));
        assert_eq!(failed(&report), ["the scores and the root recompute from the answers"]);
        // A better score in the results file.
        let edited = change(&p.results, "\"correct\":1", "\"correct\":2");
        assert_eq!(
            failed(&p.check(|e| e.results = Some(&edited))),
            ["the scores and the root recompute from the answers"]
        );
        // A model left out.
        let report = p.check(|e| e.answers = Some(vec![&p.answers[0]]));
        assert_eq!(failed(&report), ["the scores and the root recompute from the answers"]);
        // A wrong answer left out.
        let mut edited = p.answers.clone();
        let mut file = AnswerFile::parse(&edited[1]).unwrap();
        file.records.pop();
        edited[1] = serde_json::to_string(&file).unwrap();
        let report = p.check(|e| e.answers = Some(edited.iter().map(String::as_str).collect()));
        assert_eq!(failed(&report), ["every model answered every question once"]);
        // A smaller budget than the answers were marked against.
        let edited = change(&p.results, "\"budgetNanoUsd\":20000000", "\"budgetNanoUsd\":500");
        let report = p.check(|e| e.results = Some(&edited));
        assert_eq!(
            failed(&report),
            ["every reply grades to its stored mark", "the scores and the root recompute from the answers"]
        );
    }

    #[test]
    fn half_a_publication_is_caught() {
        let p = published();
        assert_eq!(failed(&p.check(|e| e.results = None)), ["answers and results are published together"]);
        assert_eq!(failed(&p.check(|e| e.answers = None)), ["answers and results are published together"]);
        assert_eq!(failed(&p.check(|e| e.results = Some("{}"))), ["the answers and results read"]);
        let other = change_exam_id(&p.results);
        assert_eq!(
            failed(&p.check(|e| e.results = Some(&other))),
            ["the results are for this exam", "the scores and the root recompute from the answers"]
        );
    }

    fn change_exam_id(results: &str) -> String {
        results.replacen("\"examId\":\"test-01\"", "\"examId\":\"test-02\"", 1)
    }

    const RECORD: chain::Record = chain::Record { chain_id: 4663, contract: CONTRACT };

    fn on_chain(p: &mut Published) -> (String, String) {
        p.edit(|m| {
            m.chain = Some(ChainRef {
                chain_id: 4663,
                contract: CONTRACT.into(),
                seal_tx: Some(TX.into()),
                publish_tx: Some(TX.into()),
            })
        });
        let root = Results::parse(&p.results).unwrap().results_root;
        (
            seal_receipt(&p.manifest.exam_id, &p.manifest.ciphertext_sha256, &p.manifest.plaintext_sha256, ROUND),
            publish_receipt(&p.manifest.exam_id, &root),
        )
    }

    #[test]
    fn the_record_on_chain_is_compared_with_the_files() {
        let mut p = published();
        let (seal, publish) = on_chain(&mut p);
        let report = p.check(|e| {
            (e.record, e.chain) = (Some(RECORD), Some(Receipts { seal: Some(&seal), publish: Some(&publish) }));
        });
        assert!(report.ok, "{:?}", failed(&report));
        assert_eq!(skipped(&report), [""; 0]);
        // Not reading the chain is a skip, and says so.
        let report = p.check(|e| e.record = Some(RECORD));
        assert_eq!(skipped(&report), [SEAL_ON_CHAIN, ROOT_ON_CHAIN]);
        assert!(
            report.checks.iter().filter(|c| c.skipped).all(|c| c.detail.as_deref() == Some("the chain was not read"))
        );
        // Reading it and finding nothing is a failure.
        let report =
            p.check(|e| (e.record, e.chain) = (Some(RECORD), Some(Receipts { seal: Some("null"), publish: None })));
        assert_eq!(failed(&report), [SEAL_ON_CHAIN, ROOT_ON_CHAIN]);
        let report = p.check(|e| (e.record, e.chain) = (Some(RECORD), Some(Receipts::default())));
        assert_eq!(failed(&report), [SEAL_ON_CHAIN, ROOT_ON_CHAIN]);
    }

    #[test]
    fn a_record_in_any_other_contract_proves_nothing() {
        // The manifest may name any contract, and anyone can deploy one that says whatever they like.
        let mut p = published();
        let (seal, publish) = on_chain(&mut p);
        let receipts = Receipts { seal: Some(&seal), publish: Some(&publish) };
        let elsewhere = chain::Record { chain_id: 4663, contract: "0x00000000000000000000000000000000000000bb" };
        let report = p.check(|e| (e.record, e.chain) = (Some(elsewhere), Some(receipts)));
        assert_eq!(failed(&report), [SEAL_ON_CHAIN, ROOT_ON_CHAIN]);
        let other_chain = chain::Record { chain_id: 1, contract: CONTRACT };
        let report = p.check(|e| (e.record, e.chain) = (Some(other_chain), Some(receipts)));
        assert_eq!(failed(&report), [SEAL_ON_CHAIN, ROOT_ON_CHAIN]);
        // With no record contract to trust, what the manifest names is not looked at: skipped, not passed.
        let report = p.check(|e| e.chain = Some(receipts));
        assert_eq!(skipped(&report), [SEAL_ON_CHAIN, ROOT_ON_CHAIN]);
        assert!(report.checks.iter().filter(|c| c.skipped).all(|c| c.detail.as_deref() == Some(NO_RECORD)));
    }

    #[test]
    fn once_a_record_contract_exists_an_opened_exam_must_be_in_it() {
        let p = published();
        // No seal transaction in the manifest: before the round it may still come, after it nothing can
        // show that the exam was sealed in time.
        let report = p.check(|e| {
            (e.record, e.beacon, e.answers, e.results) = (Some(RECORD), None, None, None);
            e.now_ms = drand::round_time(ROUND).unwrap() - 1;
        });
        assert!(report.ok);
        assert_eq!(skipped(&report), [SEAL_ON_CHAIN, OPENED]);
        let report = p.check(|e| e.record = Some(RECORD));
        assert_eq!(failed(&report), [SEAL_ON_CHAIN]);
        assert_eq!(skipped(&report), [ROOT_ON_CHAIN]);
        // A clock set back does not bring the grace back: the signature shows that the round has passed.
        let report = p.check(|e| (e.record, e.now_ms) = (Some(RECORD), 0));
        assert_eq!((failed(&report), report.state), (vec![SEAL_ON_CHAIN], State::Open));
    }

    #[test]
    fn a_record_on_chain_that_differs_is_caught() {
        let mut p = published();
        let (seal, publish) = on_chain(&mut p);
        let other_round =
            seal_receipt(&p.manifest.exam_id, &p.manifest.ciphertext_sha256, &p.manifest.plaintext_sha256, ROUND + 1);
        let other_hash = seal_receipt(&p.manifest.exam_id, &p.manifest.ciphertext_sha256, &sha256_hex(b"x"), ROUND);
        let other_exam =
            seal_receipt(&chain_exam_id("another"), &p.manifest.ciphertext_sha256, &p.manifest.plaintext_sha256, ROUND);
        for wrong in [&other_round, &other_hash, &other_exam, &publish] {
            let report = p.check(|e| {
                (e.record, e.chain) = (Some(RECORD), Some(Receipts { seal: Some(wrong), publish: Some(&publish) }));
            });
            assert_eq!(failed(&report), [SEAL_ON_CHAIN]);
        }
        let other_root = publish_receipt(&p.manifest.exam_id, &sha256_hex(b"another root"));
        for wrong in [
            &other_root,
            &seal,
            &publish_receipt(&chain_exam_id("another"), &Results::parse(&p.results).unwrap().results_root),
        ] {
            let report = p.check(|e| {
                (e.record, e.chain) = (Some(RECORD), Some(Receipts { seal: Some(&seal), publish: Some(wrong) }));
            });
            assert_eq!(failed(&report), [ROOT_ON_CHAIN]);
        }
    }

    #[test]
    fn the_root_on_chain_must_be_the_recomputed_one_not_the_stated_one() {
        let mut p = published();
        let (seal, _) = on_chain(&mut p);
        // The results file and the chain agree on a root that the answers do not give.
        let stated = sha256_hex(b"made up");
        let real = Results::parse(&p.results).unwrap().results_root;
        let results = p.results.replacen(&real, &stated, 1);
        let publish = publish_receipt(&p.manifest.exam_id, &stated);
        let report = p.check(|e| {
            e.results = Some(&results);
            (e.record, e.chain) = (Some(RECORD), Some(Receipts { seal: Some(&seal), publish: Some(&publish) }));
        });
        assert_eq!(failed(&report), ["the scores and the root recompute from the answers", ROOT_ON_CHAIN]);
    }

    #[test]
    fn without_a_manifest_there_is_nothing_to_check() {
        let p = published();
        for bad in ["", "{}", "[]", "{\"format\":2}"] {
            assert!(
                verify_exam(&Evidence { manifest: bad, ciphertext: p.ciphertext.as_bytes(), ..Evidence::default() })
                    .is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_report_writes_as_json() {
        let p = published();
        let report = p.check(|e| (e.beacon, e.answers, e.results, e.now_ms) = (None, None, None, 0));
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.starts_with("{\"ok\":true,\"state\":\"sealed\",\"checks\":[{\"name\":\"the sealed file has the hash in the manifest\",\"ok\":true},"));
        assert!(json.ends_with(concat!(
            r#"{"name":"the round's signature was given","ok":true,"skipped":true,"#,
            r#""detail":"sealed until 2023-08-23T15:59:24.000Z"},"#,
            r#"{"name":"nothing of the exam is published before its round","ok":true}]}"#
        )));
    }
}
