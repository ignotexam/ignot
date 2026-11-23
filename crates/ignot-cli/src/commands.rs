//! What each command does.

use std::collections::BTreeMap;
use std::fs::{self, TryLockError};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command as Process;

use anyhow::{Context, Result, bail};
use ignot_core::budget::{COST_DECIMALS, Track, format_amount, most_prompt_tokens, parse_amount};
use ignot_core::drand::{self, Beacon};
use ignot_core::exam::{Exam, Recipe, default_mix, make_exam, parse_mix};
use ignot_core::hash::{same_hex, sha256_hex, to_hex};
use ignot_core::results::{AnswerFile, AnswerRecord, Results, build_results, valid_model};
use ignot_core::seal::{Manifest, open_exam, seal_exam};
use ignot_core::verify::{Evidence, Receipts, Report, State, verify_exam};
use ignot_core::{chain, date, timelock};

use crate::args::{ChainArgs, Command, Tracks};
use crate::files::{self, paths};
use crate::net::{Node, World, fetch_beacon, peek_beacon};
use crate::one_line;
use crate::run::{
    Plan, SYSTEM, Terms, dollars, fetch_pricing, journal_answers, journal_start, merge_answers, run_exam,
    set_aside_unanswered,
};

/// Where lines for the person at the terminal go.
pub type Out<'a> = &'a mut (dyn Write + Send);

/// Runs a command. `Ok(false)` means it ran and the answer is no: an exam failed its checks, or a model that
/// was to be asked could not be reached.
///
/// `record` is the record contract the checks trust: the one in the engine's source, when there is one.
pub fn run(
    command: Command,
    world: &dyn World,
    api_key: Option<&str>,
    record: Option<chain::Record>,
    out: Out,
) -> Result<bool> {
    match command {
        Command::Make { id, title, seed, mix, commit, out: path } => {
            make(world, out, id, title, seed, mix, commit, path)?
        }
        Command::Seal { exam, open, dir, keep } => seal(world, out, &exam, &open, dir, keep)?,
        Command::Open { dir } => open(world, out, &dir)?,
        Command::Run { dir, models, panel, track, budget, effort, concurrency } => {
            let Some(api_key) = api_key.filter(|key| !key.is_empty()) else {
                bail!("set OPENROUTER_API_KEY to run an exam")
            };
            let models = match panel {
                Some(panel) => serde_json::from_str(&files::read_text(&panel)?)
                    .with_context(|| format!("{} is not a list of model names", panel.display()))?,
                None => models,
            };
            let tracks: &[Track] = match track {
                Tracks::Both => &Track::ALL,
                Tracks::SameBudget => &[Track::SameBudget],
                Tracks::Unlimited => &[Track::Unlimited],
            };
            let concurrency = usize::from(concurrency);
            return run_models(world, out, &dir, &models, tracks, &budget, effort.name(), concurrency, api_key);
        }
        Command::Verify { dir, chain } => {
            let (report, note) = verify_dir(world, &dir, &chain, record)?;
            print_report(out, &dir, &report, note.as_deref())?;
            return Ok(report.ok);
        }
        Command::VerifyAll { root, chain } => return verify_all(world, out, &root, &chain, record),
        Command::SealFile { file, open, out: path } => seal_file(world, out, &file, &open, path)?,
        Command::OpenFile { file, out: path } => open_file(world, out, &file, path)?,
    }
    Ok(true)
}

/// A moment given as a time (`2026-12-03T14:00:00Z`) or as a delay from now (`+90m`), in Unix milliseconds.
fn parse_when(text: &str, now_ms: u64) -> Result<u64> {
    let Some(delay) = text.strip_prefix('+') else {
        return Ok(date::parse_rfc3339(text)?);
    };
    let mut characters = delay.chars();
    let unit = match characters.next_back() {
        Some('s') => 1_000,
        Some('m') => 60_000,
        Some('h') => 3_600_000,
        Some('d') => 86_400_000,
        _ => 0,
    };
    let count = characters.as_str();
    let plain = unit > 0 && !count.is_empty() && count.bytes().all(|c| c.is_ascii_digit());
    let ms = count
        .parse::<u64>()
        .ok()
        .filter(|_| plain)
        .and_then(|n| n.checked_mul(unit))
        .and_then(|ms| now_ms.checked_add(ms));
    ms.with_context(|| format!("\"{text}\" is not a delay like +90m, +6h or +2d"))
}

/// The round a seal to `when` is locked to. The time must still be ahead.
fn round_for(world: &dyn World, when: &str) -> Result<u64> {
    let now = world.now_ms();
    let at = parse_when(when, now)?;
    if at <= now {
        bail!("the opening time has passed already: a seal to it would open at once");
    }
    // The first round at or after a time that is still ahead is itself still ahead, by this machine's clock.
    let round = drand::round_at_or_after(at);
    // That clock may be behind. A round drand has published already would make a seal that anyone can open.
    if peek_beacon(world, round).is_some() {
        bail!("drand has published round {round} already: the clock on this machine is behind");
    }
    Ok(round)
}

/// The commit checked out in the current folder, when it is a git checkout.
fn checked_out_commit() -> Option<String> {
    let output =
        Process::new("git").args(["rev-parse", "HEAD"]).output().ok().filter(|output| output.status.success())?;
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (commit.len() == 40 && commit.bytes().all(|c| c.is_ascii_hexdigit())).then_some(commit)
}

#[allow(clippy::too_many_arguments)]
fn make(
    world: &dyn World,
    out: Out,
    id: String,
    title: Option<String>,
    seed: Option<String>,
    mix: Option<String>,
    commit: Option<String>,
    path: Option<PathBuf>,
) -> Result<()> {
    let seed = match seed {
        Some(seed) => seed,
        None => {
            let mut bytes = [0u8; 32];
            getrandom::getrandom(&mut bytes).context("the system gave no randomness for a seed")?;
            to_hex(&bytes).split_off(2)
        }
    };
    let recipe = Recipe {
        title: title.unwrap_or_else(|| id.clone()),
        created_at: date::rfc3339(world.now_ms()),
        commit: commit.or_else(checked_out_commit).unwrap_or_else(|| "unknown".into()),
        seed,
        mix: mix.as_deref().map_or_else(|| Ok(default_mix()), parse_mix)?,
        id,
    };
    let exam = make_exam(&recipe)?;
    let path = path.unwrap_or_else(|| PathBuf::from(format!("{}.plain.json", exam.id)));
    files::write_new_private(&path, format!("{}\n", exam.bytes()?).as_bytes())?;
    writeln!(
        out,
        "wrote {}: {} questions. It holds the keys: seal it, never publish it.",
        path.display(),
        exam.questions.len()
    )?;
    Ok(())
}

fn seal(world: &dyn World, out: Out, source: &Path, when: &str, dir: Option<PathBuf>, keep: bool) -> Result<()> {
    let exam = Exam::parse(files::read_text(source)?.trim_end())?;
    // An exam that was edited by hand would seal without complaint and fail every check once it opens.
    match make_exam(&exam.recipe()) {
        Ok(again) if again == exam => {}
        Ok(_) => bail!("{} is not the exam its seed gives with this version; make it again", source.display()),
        Err(e) => bail!("{} cannot be sealed: {e}", source.display()),
    }
    let round = round_for(world, when)?;
    let dir = dir.unwrap_or_else(|| Path::new("exams").join(&exam.id));
    let at = paths(&dir);
    if [&at.manifest, &at.sealed, &at.exam, &at.answers, &at.results].iter().any(|path| path.exists()) {
        bail!("{} already holds an exam's files", dir.display());
    }
    let (sealed, manifest) = seal_exam(&exam, round)?;
    fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    files::write_new(&at.sealed, sealed.as_bytes())?;
    files::write_new(&at.manifest, files::pretty(&manifest)?.as_bytes())?;
    if !keep {
        fs::remove_file(source).with_context(|| format!("sealed, but cannot remove {}", source.display()))?;
    }
    writeln!(out, "sealed {} into {}; it opens at {} (drand round {round})", exam.id, dir.display(), manifest.open_at)?;
    if !keep {
        writeln!(out, "removed {}", source.display())?;
    }
    writeln!(
        out,
        "to record it on chain: ExamSeals.seal({}, {}, {}, {round})",
        manifest.exam_id, manifest.ciphertext_sha256, manifest.plaintext_sha256
    )?;
    Ok(())
}

/// The signature of a round, fetched and checked. Refuses while the round is still ahead.
fn beacon_for(world: &dyn World, round: u64) -> Result<Beacon> {
    let opens = drand::round_time(round)?;
    let answer = if world.now_ms() < opens {
        // The clock on this machine may be behind. A relay that has the signature already settles it.
        peek_beacon(world, round).with_context(|| format!("still sealed: it opens at {}", date::rfc3339(opens)))?
    } else {
        fetch_beacon(world, round)?
    };
    Ok(Beacon::parse(round, &answer)?)
}

fn open(world: &dyn World, out: Out, dir: &Path) -> Result<()> {
    let stored = files::load(dir)?;
    let manifest = Manifest::parse(&stored.manifest)?;
    if !same_hex(&sha256_hex(&stored.sealed), &manifest.ciphertext_sha256) {
        bail!("the sealed file in {} is not the one its manifest names", dir.display());
    }
    let (_, plaintext) = open_exam(&stored.sealed, &beacon_for(world, manifest.open_round)?, Some(&manifest))?;
    let path = paths(dir).exam;
    match stored.exam {
        Some(already) if already == plaintext => {
            writeln!(out, "{} is open already: {} is the sealed exam", manifest.id, path.display())?
        }
        Some(_) => bail!("{} exists and is not the sealed exam; move it away first", path.display()),
        None => {
            files::write_new(&path, plaintext.as_bytes())?;
            writeln!(out, "opened {}: wrote {}", manifest.id, path.display())?;
        }
    }
    Ok(())
}

/// Asks models an opened exam and saves their answers and the scores. Returns whether every model that was
/// to be asked could be reached.
///
/// An answer that was paid for is never asked for again: what the folder holds, and what the journal of a run
/// that was cut short holds, is taken over, and only the questions that are missing or whose request failed
/// are asked.
#[allow(clippy::too_many_arguments)]
fn run_models(
    world: &dyn World,
    out: Out,
    dir: &Path,
    models: &[String],
    tracks: &[Track],
    budget: &str,
    effort: &str,
    concurrency: usize,
    api_key: &str,
) -> Result<bool> {
    // One run at a time in a folder: two at once would each save over what the other saved. The lock is the
    // system's, so a run that is killed lets go of it, and it is on a file of its own: on some systems a locked
    // file cannot be read, even by the process that locked it.
    let at = paths(dir);
    if !at.manifest.is_file() {
        bail!("cannot read {}: no exam is in {}", at.manifest.display(), dir.display());
    }
    let held = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&at.lock)
        .with_context(|| format!("cannot create {}", at.lock.display()))?;
    match held.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => bail!("another run is under way in {}", dir.display()),
        Err(TryLockError::Error(e)) => writeln!(
            out,
            "{} cannot be locked ({e}): start no second run in it while this one is under way",
            dir.display()
        )?,
    }
    let stored = files::load(dir)?;
    let manifest = Manifest::parse(&stored.manifest)?;
    let Some(exam_text) = stored.exam else { bail!("open the exam first: {} has no exam.json", dir.display()) };
    if !same_hex(&sha256_hex(exam_text.trim_end().as_bytes()), &manifest.plaintext_sha256) {
        bail!("exam.json in {} is not the exam that was sealed", dir.display());
    }
    let exam = Exam::parse(exam_text.trim_end())?;
    let budget = parse_amount(budget, COST_DECIMALS).context("--budget")?;
    let before: Vec<AnswerFile> = stored
        .answers
        .unwrap_or_default()
        .iter()
        .map(|text| AnswerFile::parse(text))
        .collect::<ignot_core::Result<_>>()?;
    if let Some(earlier) = stored.results.as_deref().map(Results::parse).transpose()?
        && earlier.budget_nano_usd != budget
    {
        bail!(
            "this exam was run with a budget of ${} a question; one exam has one budget",
            format_amount(earlier.budget_nano_usd, COST_DECIMALS)
        );
    }
    let mut asked: Vec<String> = models.iter().map(|model| model.trim().to_string()).collect();
    asked.sort();
    asked.dedup();
    if asked.is_empty() || !asked.iter().all(|model| valid_model(model)) {
        bail!("model names are plain text without spaces, such as lab/model-1");
    }
    // Whatever could refuse this run is checked now, before a single request is paid for: the answers already
    // in the folder must stand on their own, and every model must get a file name of its own.
    if !before.is_empty() {
        build_results(&exam, &before, budget)
            .context("the answers already in this folder do not check out, so nothing was asked")?;
    }
    let mut everyone: Vec<&str> = before.iter().map(|file| file.model.as_str()).collect();
    everyone.extend(asked.iter().map(String::as_str));
    files::distinct_names(&everyone)?;
    let plan = Plan { exam: &exam, models: &asked, tracks, budget, effort, concurrency, api_key };
    // A run that was cut short left the answers it had in its journal.
    let journal_path = paths(dir).journal;
    let terms = Terms {
        exam_id: exam.id.clone(),
        plaintext_sha256: manifest.plaintext_sha256.clone(),
        budget_nano_usd: budget,
        effort: effort.to_string(),
    };
    let journaled = match files::read_journal(&journal_path)? {
        Some(text) => journal_answers(&text, &terms, &plan).with_context(|| {
            format!(
                "{} holds a run that was cut short, and this run does not fit it: run that one again to finish it, or remove the file to start over",
                journal_path.display()
            )
        })?,
        None => Vec::new(),
    };
    // Every answer of the pairs this run asks for that is in already, from the folder or from the journal.
    let key = |r: &AnswerRecord| (r.model.clone(), r.track, r.question_id.clone());
    let mut taken: BTreeMap<_, AnswerRecord> = BTreeMap::new();
    let kept = before.iter().flat_map(|file| &file.records);
    for record in kept.filter(|r| r.error.is_none() && asked.contains(&r.model) && tracks.contains(&r.track)) {
        taken.insert(key(record), record.clone());
    }
    let in_folder = taken.len();
    for record in &journaled {
        taken.insert(key(record), record.clone());
    }
    let done: Vec<AnswerRecord> = taken.into_values().collect();
    let longest = exam.questions.iter().map(|q| most_prompt_tokens(SYSTEM.len() + q.prompt.len())).max().unwrap_or(0);
    let pricing = fetch_pricing(world, &asked, longest)?;

    files::replace(&journal_path, journal_start(&terms, &journaled)?.as_bytes())?;
    let mut journal = files::Durable::append(&journal_path)?;
    if !journaled.is_empty() {
        writeln!(out, "taking up a run that was cut short, with {} of its answers in already", journaled.len())?;
    }
    let wanted = asked.len() * tracks.len() * exam.questions.len();
    if in_folder > 0 {
        writeln!(out, "{in_folder} of the {wanted} answers this run asks for are in the folder already and are kept")?;
    }
    if done.len() == wanted {
        writeln!(out, "every answer this run asks for is in already: nothing is asked")?;
    }
    let fresh = run_exam(world, &plan, &pricing, done, &mut journal, out);
    drop(journal);
    let (fresh, unanswered) = set_aside_unanswered(fresh);
    let records = || fresh.iter().flat_map(|file| &file.records);
    let failed = records().filter(|r| r.error.is_some() && r.max_tokens > 0).count();
    // Every price a model lists was reserved for on the same-budget track. An answer that cost more all the
    // same was billed a fee the list does not show, such as a web search on every request.
    let mut over: BTreeMap<String, usize> = BTreeMap::new();
    for record in records().filter(|r| r.track == Track::SameBudget && r.error.is_none() && r.cost_nano_usd > budget) {
        *over.entry(record.model.clone()).or_default() += 1;
    }
    let saved = if fresh.is_empty() {
        None
    } else {
        let answers = merge_answers(&exam.id, before, fresh);
        let results = build_results(&exam, &answers, budget)?;
        files::save_answers(dir, &answers)?;
        files::replace(&paths(dir).results, files::pretty(&results)?.as_bytes())?;
        Some(results)
    };
    // Whatever the journal held is saved now, or was no answer at all.
    match fs::remove_file(&journal_path) {
        // Gone already, which only someone else can have done: there is nothing left to keep.
        Err(e) if e.kind() != ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("saved, but cannot remove {}", journal_path.display()));
        }
        _ => {}
    }

    for (model, track, why) in &unanswered {
        writeln!(
            out,
            "no request to {model} on the {} track got through ({why}); nothing of it is saved",
            track.name()
        )?;
    }
    let Some(results) = saved else {
        writeln!(out, "nothing was saved")?;
        return Ok(false);
    };
    for scores in &results.tracks {
        let terms = match scores.track {
            Track::SameBudget => format!(" (${} a question for every model)", format_amount(budget, COST_DECIMALS)),
            Track::Unlimited => String::new(),
        };
        writeln!(out, "\n{}{terms}", scores.track.name())?;
        for score in &scores.models {
            writeln!(out, "  {:>3}/{}  {}  {}", score.correct, score.total, dollars(score.cost_nano_usd), score.model)?;
        }
    }
    match failed {
        0 => {}
        1 => writeln!(out, "\n1 request failed; it counts as not answered until the same command asks it again")?,
        n => writeln!(out, "\n{n} requests failed; each counts as not answered until the same command asks it again")?,
    }
    if !over.is_empty() {
        writeln!(out)?;
    }
    for (model, n) in &over {
        writeln!(
            out,
            "{n} same-budget answers of {model} cost more than the budget and do not count: it is billed fees its listed prices do not show"
        )?;
    }
    writeln!(out, "\nresults root {}", results.results_root)?;
    Ok(unanswered.is_empty())
}

/// Checks one exam's folder. Also returns why the round's signature could not be fetched, when it could not.
fn verify_dir(
    world: &dyn World,
    dir: &Path,
    chain: &ChainArgs,
    record: Option<chain::Record>,
) -> Result<(Report, Option<String>)> {
    let stored = files::load(dir)?;
    let manifest = Manifest::parse(&stored.manifest)
        .with_context(|| format!("{} is not a manifest", paths(dir).manifest.display()))?;
    // Past the round's time a missing signature is a failed check, so relays that cannot be reached show up
    // in the report instead of ending the run. Before it, by the clock on this machine, the relays are asked
    // once all the same: that clock may be behind, and an exam that has opened must not read as one that
    // leaked.
    let due = drand::round_time(manifest.open_round).is_ok_and(|opens| world.now_ms() >= opens);
    let (beacon, relay_problem) = if due {
        match fetch_beacon(world, manifest.open_round) {
            Ok(beacon) => (Some(beacon), None),
            Err(e) => (None, Some(e.to_string())),
        }
    } else {
        (peek_beacon(world, manifest.open_round), None)
    };
    // Receipts are read only from the chain of the record contract. A manifest that points anywhere else
    // fails its check without a node being asked.
    let (mut seal_receipt, mut publish_receipt) = (None, None);
    let reference = manifest.chain.as_ref().filter(|c| record.is_some_and(|record| record.chain_id == c.chain_id));
    let read_chain = !chain.no_chain && reference.is_some();
    if let Some(reference) = reference.filter(|_| read_chain) {
        let node = Node::connect(world, chain.rpc.as_deref(), reference.chain_id)?;
        seal_receipt = reference.seal_tx.as_deref().map(|tx| node.receipt(tx)).transpose()?;
        publish_receipt = reference.publish_tx.as_deref().map(|tx| node.receipt(tx)).transpose()?;
    }
    let evidence = Evidence {
        manifest: &stored.manifest,
        ciphertext: &stored.sealed,
        now_ms: world.now_ms(),
        beacon: beacon.as_deref(),
        exam: stored.exam.as_deref(),
        answers: stored.answers.as_ref().map(|files| files.iter().map(String::as_str).collect()),
        results: stored.results.as_deref(),
        chain: read_chain.then_some(Receipts { seal: seal_receipt.as_deref(), publish: publish_receipt.as_deref() }),
        record,
    };
    Ok((verify_exam(&evidence)?, relay_problem))
}

fn print_report(out: Out, dir: &Path, report: &Report, note: Option<&str>) -> Result<()> {
    let state = match report.state {
        State::Sealed => "sealed",
        State::Open => "open",
    };
    let skipped = report.checks.iter().filter(|check| check.skipped).count();
    let unchecked = match skipped {
        0 => String::new(),
        1 => "; 1 check could not be made".into(),
        n => format!("; {n} checks could not be made"),
    };
    // Names of folders, and the reasons taken from files and from the network, are someone else's words. Each
    // is kept on the line it is put in, so that none of them can add a line that reads like a verdict.
    let folder = one_line(&dir.display().to_string());
    writeln!(out, "{} {folder} ({state}{unchecked})", if report.ok { "PASS" } else { "FAIL" })?;
    for check in &report.checks {
        let mark = if check.skipped {
            "skip"
        } else if check.ok {
            " ok "
        } else {
            "FAIL"
        };
        writeln!(
            out,
            "  {mark}  {}{}",
            check.name,
            check.detail.as_ref().map(|detail| format!(": {}", one_line(detail))).unwrap_or_default()
        )?;
    }
    if let Some(note) = note {
        writeln!(out, "  note  {}", one_line(note))?;
    }
    Ok(())
}

fn verify_all(
    world: &dyn World,
    out: Out,
    root: &Path,
    chain: &ChainArgs,
    record: Option<chain::Record>,
) -> Result<bool> {
    let mut dirs = Vec::new();
    match fs::read_dir(root) {
        Ok(entries) => {
            for entry in entries {
                let dir = entry.with_context(|| format!("cannot list {}", root.display()))?.path();
                if paths(&dir).manifest.exists() {
                    dirs.push(dir);
                }
            }
        }
        // A folder that is not there is a mistyped path, not a set of exams that all pass.
        Err(e) => return Err(e).with_context(|| format!("cannot list {}", root.display())),
    }
    dirs.sort();
    let mut passed = 0;
    for dir in &dirs {
        match verify_dir(world, dir, chain, record) {
            Ok((report, note)) => {
                print_report(out, dir, &report, note.as_deref())?;
                passed += usize::from(report.ok);
            }
            Err(e) => {
                writeln!(out, "FAIL {}\n  {}", one_line(&dir.display().to_string()), one_line(&format!("{e:#}")))?
            }
        }
    }
    if dirs.is_empty() {
        writeln!(out, "no exams in {}", root.display())?;
    } else {
        writeln!(out, "{passed} of {} exams pass", dirs.len())?;
    }
    Ok(passed == dirs.len())
}

fn seal_file(world: &dyn World, out: Out, source: &Path, when: &str, path: Option<PathBuf>) -> Result<()> {
    let data = fs::read(source).with_context(|| format!("cannot read {}", source.display()))?;
    let round = round_for(world, when)?;
    let path = path.unwrap_or_else(|| {
        let mut name = source.as_os_str().to_owned();
        name.push(".age");
        PathBuf::from(name)
    });
    files::write_new(&path, timelock::seal_bytes(&data, round)?.as_bytes())?;
    writeln!(
        out,
        "wrote {}; it opens at {} (drand round {round})",
        path.display(),
        date::rfc3339(drand::round_time(round)?)
    )?;
    Ok(())
}

fn open_file(world: &dyn World, out: Out, source: &Path, path: Option<PathBuf>) -> Result<()> {
    let sealed = fs::read(source).with_context(|| format!("cannot read {}", source.display()))?;
    let beacon = beacon_for(world, timelock::sealed_round(&sealed)?)?;
    let path = path.unwrap_or_else(|| match source.extension() {
        Some(extension) if extension == "age" => source.with_extension(""),
        _ => {
            let mut name = source.as_os_str().to_owned();
            name.push(".opened");
            PathBuf::from(name)
        }
    });
    files::write_new(&path, &timelock::open_bytes(&sealed, &beacon)?)?;
    writeln!(out, "wrote {}", path.display())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use ignot_core::chain::{PUBLISHED, SEALED, topic};
    use serde_json::{Value, json};

    use super::*;
    use crate::args::Cli;
    use crate::files::tests::Scratch;
    use crate::net::tests::{ROUND_MS, Scripted};
    use crate::run::tests::{BUDGET, SEED, provider};

    const BEFORE: u64 = ROUND_MS - 3_600_000;
    const WHEN: &str = "2023-08-23T15:59:24Z";

    /// Runs a command line against a scripted outside. Returns whether it passed and what it printed.
    fn ignot(world: &Scripted, line: &[&str]) -> Result<(bool, String)> {
        ignot_trusting(None, world, line)
    }

    /// The same, with a record contract to trust. None is deployed yet, so the tests bring their own.
    fn ignot_trusting(record: Option<chain::Record>, world: &Scripted, line: &[&str]) -> Result<(bool, String)> {
        let cli = Cli::try_parse_from(std::iter::once("ignot").chain(line.iter().copied()))?;
        let mut printed = Vec::new();
        let passed = run(cli.command, world, Some("test-key"), record, &mut printed)?;
        Ok((passed, String::from_utf8(printed)?))
    }

    fn fails(world: &Scripted, line: &[&str]) -> String {
        format!("{:#}", ignot(world, line).expect_err("the command should fail"))
    }

    /// Makes and seals a small exam to round 1000, an hour before that round. Returns the exam's folder.
    fn sealed(scratch: &Scratch, world: &Scripted) -> String {
        let plain = scratch.path("pilot.plain.json");
        let dir = scratch.path("exams/pilot");
        let (plain, dir) = (plain.to_str().unwrap().to_string(), dir.to_str().unwrap().to_string());
        ignot(
            world,
            &[
                "make",
                "--id",
                "pilot",
                "--title",
                "Pilot",
                "--seed",
                SEED,
                "--mix",
                "stack:1:2,bases:1:2",
                "--commit",
                "abc1234",
                "--out",
                &plain,
            ],
        )
        .unwrap();
        ignot(world, &["seal", &plain, "--open", WHEN, "--dir", &dir]).unwrap();
        dir
    }

    #[test]
    fn times_and_delays_parse() {
        assert_eq!(parse_when("+90m", 0).unwrap(), 90 * 60_000);
        assert_eq!(parse_when("+2d", 1000).unwrap(), 1000 + 2 * 86_400_000);
        assert_eq!(parse_when("+30s", 5).unwrap(), 30_005);
        assert_eq!(parse_when("+6h", 0).unwrap(), 21_600_000);
        assert_eq!(parse_when("2026-12-03T14:00:00Z", 0).unwrap(), 1_796_306_400_000);
        for bad in [
            "soon",
            "+",
            "+m",
            "+5",
            "+5w",
            "+-5m",
            "+1.5h",
            "+ 5m",
            "+99999999999999999999d",
            "+18446744073709551615d",
            "90m",
            "",
            "+5\u{e9}",
            "+\u{e9}",
            "+\u{661}\u{662}m",
            "+5\u{ff4d}",
        ] {
            assert!(parse_when(bad, 0).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_exam_goes_from_seed_to_checked_results() {
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        world.set_now(BEFORE);
        let dir = sealed(&scratch, &world);
        assert!(!scratch.path("pilot.plain.json").exists(), "the unsealed exam is removed");

        // Sealed: it checks out, and nothing opens it.
        let (passed, printed) = ignot(&world, &["verify", &dir]).unwrap();
        assert!(passed, "{printed}");
        assert!(printed.starts_with(&format!("PASS {dir} (sealed; 2 checks could not be made)\n")), "{printed}");
        assert!(
            printed.contains("  skip  the round's signature was given: sealed until 2023-08-23T15:59:24.000Z"),
            "{printed}"
        );
        assert!(fails(&world, &["open", &dir]).contains("still sealed: it opens at 2023-08-23T15:59:24.000Z"));
        assert!(fails(&world, &["run", &dir, "--models", "a/good"]).contains("open the exam first"));

        // The round passes.
        world.publish_round_1000();
        world.set_now(ROUND_MS);
        let (_, printed) = ignot(&world, &["open", &dir]).unwrap();
        assert!(printed.starts_with("opened pilot: wrote "), "{printed}");
        assert!(ignot(&world, &["open", &dir]).unwrap().1.contains("is open already"));
        let manifest =
            Manifest::parse(&fs::read_to_string(scratch.path("exams/pilot/manifest.json")).unwrap()).unwrap();
        assert_eq!(sha256_hex(&fs::read(scratch.path("exams/pilot/exam.json")).unwrap()), manifest.plaintext_sha256);

        // Two models on both tracks.
        let (_, printed) = ignot(&world, &["run", &dir, "--models", "a/good,b/bad", "--concurrency", "2"]).unwrap();
        assert!(printed.contains("\nsame-budget ($0.02 a question for every model)\n    4/4  $0.004000  a/good\n    0/4  $0.004000  b/bad\n"), "{printed}");
        assert!(printed.contains("\nunlimited\n    4/4  $0.004000  a/good\n"), "{printed}");
        let root = printed.lines().last().unwrap().strip_prefix("results root ").unwrap().to_string();
        assert!(
            scratch.path("exams/pilot/answers/a_good.json").exists()
                && scratch.path("exams/pilot/answers/b_bad.json").exists()
        );

        let (passed, printed) = ignot(&world, &["verify", &dir]).unwrap();
        assert!(passed, "{printed}");
        assert!(printed.starts_with(&format!("PASS {dir} (open; 2 checks could not be made)\n")), "{printed}");
        assert!(printed.contains("   ok   the scores and the root recompute from the answers"), "{printed}");
        assert_eq!(printed.matches("  skip  ").count(), 2, "{printed}");

        // A later run adds a model; the first two stay as recorded, and the root moves.
        let (_, printed) = ignot(&world, &["run", &dir, "--models", "c/pricey", "--track", "unlimited"]).unwrap();
        assert!(
            printed.contains(
                "\nunlimited\n    4/4  $0.004000  a/good\n    0/4  $0.004000  b/bad\n    0/4  $0.004000  c/pricey\n"
            ),
            "{printed}"
        );
        assert!(!printed.contains(&root));
        assert!(ignot(&world, &["verify", &dir]).unwrap().0);
        assert!(
            fails(&world, &["run", &dir, "--models", "a/good", "--budget", "0.05"]).contains("one exam has one budget")
        );

        let (passed, printed) = ignot(&world, &["verify-all", scratch.path("exams").to_str().unwrap()]).unwrap();
        assert!(passed && printed.ends_with("1 of 1 exams pass\n"), "{printed}");
    }

    #[test]
    fn a_changed_answer_fails_the_check() {
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        world.set_now(BEFORE);
        let dir = sealed(&scratch, &world);
        world.publish_round_1000();
        world.set_now(ROUND_MS);
        ignot(&world, &["open", &dir]).unwrap();
        ignot(&world, &["run", &dir, "--models", "b/bad", "--track", "unlimited"]).unwrap();
        let path = scratch.path("exams/pilot/answers/b_bad.json");
        let edited = fs::read_to_string(&path).unwrap().replacen("\"correct\": false", "\"correct\": true", 1);
        fs::write(&path, edited).unwrap();
        let (passed, printed) = ignot(&world, &["verify", &dir]).unwrap();
        assert!(!passed);
        assert!(printed.starts_with(&format!("FAIL {dir} (open; 2 checks could not be made)\n")), "{printed}");
        assert!(
            printed
                .contains("  FAIL  every reply grades to its stored mark: 1 of 4 do not, the first being b/bad on q01"),
            "{printed}"
        );
        let (passed, printed) = ignot(&world, &["verify-all", scratch.path("exams").to_str().unwrap()]).unwrap();
        assert!(!passed && printed.ends_with("0 of 1 exams pass\n"), "{printed}");
    }

    #[test]
    fn sealing_refuses_what_would_not_hold() {
        let scratch = Scratch::new();
        let world = Scripted::at(BEFORE);
        let dir = sealed(&scratch, &world);
        let plain = scratch.path("again.plain.json");
        let plain = plain.to_str().unwrap();
        let make = ["make", "--id", "pilot", "--seed", SEED, "--mix", "stack:1", "--commit", "abc1234", "--out", plain];
        ignot(&world, &make).unwrap();
        assert!(fails(&world, &make).contains("already exists"));
        assert!(
            fails(&world, &["seal", plain, "--open", WHEN, "--dir", &dir]).contains("already holds an exam's files")
        );
        // Leftovers of an earlier run would be published next to a sealed exam they do not belong to.
        let leftovers = scratch.path("exams/leftovers");
        fs::create_dir_all(&leftovers).unwrap();
        fs::write(leftovers.join("results.json"), "{}").unwrap();
        let into = leftovers.to_str().unwrap();
        assert!(
            fails(&world, &["seal", plain, "--open", WHEN, "--dir", into]).contains("already holds an exam's files")
        );
        // A time that has passed, or that is passing.
        let other = scratch.path("exams/other");
        let other = other.to_str().unwrap();
        assert!(
            fails(&world, &["seal", plain, "--open", "2023-08-23T14:00:00Z", "--dir", other])
                .contains("has passed already")
        );
        assert!(fails(&world, &["seal", plain, "--open", "+0s", "--dir", other]).contains("has passed already"));
        assert!(fails(&world, &["seal", plain, "--open", "next week", "--dir", other]).contains("is not a time"));
        // An exam edited by hand.
        let text = fs::read_to_string(plain).unwrap();
        let key = Exam::parse(text.trim_end()).unwrap().questions[0].answer.clone();
        fs::write(plain, text.replacen(&format!("\"answer\":\"{key}\""), "\"answer\":\"1\"", 1)).unwrap();
        assert!(
            fails(&world, &["seal", plain, "--open", WHEN, "--dir", other]).contains("is not the exam its seed gives")
        );
        assert!(
            !Path::new(other).exists() && Path::new(plain).exists(),
            "nothing is written and nothing removed when sealing fails"
        );
        // With --keep the unsealed exam stays.
        fs::write(plain, text).unwrap();
        let (_, printed) = ignot(&world, &["seal", plain, "--open", "+1h", "--dir", other, "--keep"]).unwrap();
        assert!(
            Path::new(plain).exists()
                && printed.contains("it opens at 2023-08-23T15:59:24.000Z (drand round 1000)")
                && !printed.contains("removed"),
            "{printed}"
        );
        assert!(printed.contains("to record it on chain: ExamSeals.seal(0x"), "{printed}");
    }

    #[test]
    fn sealing_refuses_a_round_that_drand_has_published() {
        // The clock here says an hour before round 1000. The relays say round 1000 is out.
        let scratch = Scratch::new();
        let world = Scripted::at(BEFORE).with_round_1000();
        let (plain, dir) = (scratch.path("late.plain.json"), scratch.path("exams/late"));
        let (plain, dir) = (plain.to_str().unwrap(), dir.to_str().unwrap());
        ignot(&world, &["make", "--id", "late", "--commit", "abc1234", "--out", plain]).unwrap();
        let error = fails(&world, &["seal", plain, "--open", WHEN, "--dir", dir]);
        assert!(
            error.contains("drand has published round 1000 already: the clock on this machine is behind"),
            "{error}"
        );
        assert!(Path::new(plain).exists() && !Path::new(dir).exists());
        let note = scratch.path("note.txt");
        fs::write(&note, "x").unwrap();
        assert!(fails(&world, &["seal-file", note.to_str().unwrap(), "--open", "+1h"]).contains("drand has published"));
        assert!(!scratch.path("note.txt.age").exists());
        // A later round, which no relay has, seals as usual.
        assert!(ignot(&world, &["seal-file", note.to_str().unwrap(), "--open", "+2h"]).is_ok());
    }

    #[test]
    fn make_fills_in_what_is_not_given() {
        let scratch = Scratch::new();
        let world = Scripted::at(1_795_528_800_000);
        let path = scratch.path("w48.plain.json");
        let (_, printed) =
            ignot(&world, &["make", "--id", "w48", "--commit", "abc1234", "--out", path.to_str().unwrap()]).unwrap();
        assert!(printed.contains("30 questions"), "{printed}");
        let exam = Exam::parse(fs::read_to_string(&path).unwrap().trim_end()).unwrap();
        assert_eq!(
            (exam.title.as_str(), exam.created_at.as_str(), exam.seed.len()),
            ("w48", "2026-11-24T14:00:00.000Z", 64)
        );
        assert!(fails(&world, &["make", "--id", "Bad Id", "--out", path.to_str().unwrap()]).contains("exam id"));
        let short = ["make", "--id", "x", "--seed", "guessable", "--out", path.to_str().unwrap()];
        assert!(fails(&world, &short).contains("shorter than 32 characters"));
        assert!(
            fails(&world, &["make", "--id", "x", "--mix", "essay:3", "--out", path.to_str().unwrap()])
                .contains("mix item")
        );
    }

    #[test]
    fn opening_and_running_refuse_swapped_files() {
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        world.set_now(BEFORE);
        let dir = sealed(&scratch, &world);
        world.publish_round_1000();
        world.set_now(ROUND_MS);
        let sealed_path = scratch.path("exams/pilot/exam.age");
        let real = fs::read(&sealed_path).unwrap();
        fs::write(&sealed_path, timelock::seal_bytes(b"something else", 1000).unwrap()).unwrap();
        assert!(fails(&world, &["open", &dir]).contains("is not the one its manifest names"));
        fs::write(&sealed_path, real).unwrap();
        fs::write(scratch.path("exams/pilot/exam.json"), "{}").unwrap();
        assert!(fails(&world, &["open", &dir]).contains("is not the sealed exam"));
        assert!(fails(&world, &["run", &dir, "--models", "a/good"]).contains("is not the exam that was sealed"));
    }

    #[test]
    fn run_refuses_before_spending_anything() {
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        world.set_now(BEFORE);
        let dir = sealed(&scratch, &world);
        world.publish_round_1000();
        world.set_now(ROUND_MS);
        ignot(&world, &["open", &dir]).unwrap();
        assert!(fails(&world, &["run", &dir, "--models", "nobody/knows"]).contains("no model named nobody/knows"));
        assert!(fails(&world, &["run", &dir, "--models", "a/good", "--budget", "two cents"]).contains("--budget"));
        assert!(fails(&world, &["run", &dir, "--models", "a good"]).contains("model names are plain text"));
        let panel = scratch.path("panel.json");
        fs::write(&panel, "{\"models\":[]}").unwrap();
        assert!(
            fails(&world, &["run", &dir, "--panel", panel.to_str().unwrap()]).contains("is not a list of model names")
        );
        assert_eq!(world.posts.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(!scratch.path("exams/pilot/answers").exists() && !scratch.path("exams/pilot/results.json").exists());
        let cli = Cli::try_parse_from(["ignot", "run", &dir, "--models", "a/good"]).unwrap();
        let error = run(cli.command, &world, None, None, &mut Vec::new()).unwrap_err().to_string();
        assert!(error.contains("OPENROUTER_API_KEY"), "{error}");
        // A panel file works like --models.
        fs::write(&panel, "[\"b/bad\"]").unwrap();
        assert!(
            ignot(&world, &["run", &dir, "--panel", panel.to_str().unwrap(), "--track", "unlimited"])
                .unwrap()
                .1
                .contains("0/4")
        );
        ignot(&world, &["run", &dir, "--models", "a/good", "--track", "same-budget"]).unwrap();

        // From here on nothing may be asked: what is already in the folder is checked first.
        let asked = world.posts.load(std::sync::atomic::Ordering::SeqCst);
        let answers = scratch.path("exams/pilot/answers");
        let run_again = ["run", &dir, "--models", "b/bad", "--track", "unlimited"];
        // A stray copy of an answers file.
        fs::copy(answers.join("b_bad.json"), answers.join("b_bad copy.json")).unwrap();
        let error = fails(&world, &run_again);
        assert!(error.contains("do not check out, so nothing was asked") && error.contains("twice"), "{error}");
        fs::remove_file(answers.join("b_bad copy.json")).unwrap();
        // Answers that belong to another exam.
        let theirs = fs::read_to_string(answers.join("b_bad.json")).unwrap();
        let theirs = theirs.replace("\"examId\": \"pilot\"", "\"examId\": \"another\"").replace("b/bad", "x/other");
        fs::write(answers.join("x_other.json"), theirs).unwrap();
        let error = fails(&world, &run_again);
        assert!(error.contains("nothing was asked") && error.contains("are for exam another"), "{error}");
        fs::remove_file(answers.join("x_other.json")).unwrap();
        // Two names that would share one file.
        let error = fails(&world, &["run", &dir, "--models", "a_good"]);
        assert!(error.contains("a/good and a_good would both be saved as a_good.json"), "{error}");
        // Marks that were given under another budget, with the results file gone.
        fs::remove_file(scratch.path("exams/pilot/results.json")).unwrap();
        let error = fails(&world, &["run", &dir, "--models", "b/bad", "--budget", "0.0001"]);
        assert!(error.contains("nothing was asked") && error.contains("stored mark of a/good"), "{error}");
        assert_eq!(world.posts.load(std::sync::atomic::Ordering::SeqCst), asked);
        // With the budget they were marked under, the run goes ahead and the results file is back.
        assert!(ignot(&world, &run_again).is_ok());
        assert!(ignot(&world, &["verify", &dir]).unwrap().0);
    }

    /// Seals a small exam, lets its round pass and opens it. Returns the exam's folder.
    fn opened(scratch: &Scratch, world: &Scripted) -> String {
        world.set_now(BEFORE);
        let dir = sealed(scratch, world);
        world.publish_round_1000();
        world.set_now(ROUND_MS);
        ignot(world, &["open", &dir]).unwrap();
        dir
    }

    fn asked(world: &Scripted) -> usize {
        world.posts.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn a_clock_that_is_behind_does_not_make_an_opened_exam_read_as_leaked() {
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        let dir = opened(&scratch, &world);
        ignot(&world, &["run", &dir, "--models", "a/good", "--track", "unlimited"]).unwrap();
        // The clock of this machine says an hour before the round. The relays have the round's signature,
        // and that settles it.
        world.set_now(BEFORE);
        let (passed, printed) = ignot(&world, &["verify", &dir]).unwrap();
        assert!(passed, "{printed}");
        assert!(printed.starts_with(&format!("PASS {dir} (open; 2 checks could not be made)\n")), "{printed}");
        assert!(ignot(&world, &["open", &dir]).unwrap().1.contains("is open already"));
        // With no relay to ask, that clock is all there is, and by it nothing may be published yet.
        let alone = Scripted::at(BEFORE);
        let (passed, printed) = ignot(&alone, &["verify", &dir]).unwrap();
        assert!(!passed, "{printed}");
        assert!(
            printed.contains(
                "  FAIL  nothing of the exam is published before its round: the folder already holds exam.json, answers, results.json\n"
            ),
            "{printed}"
        );
        assert!(fails(&alone, &["open", &dir]).contains("still sealed"));
    }

    #[test]
    fn a_run_that_was_cut_short_goes_on_from_its_journal() {
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        let dir = opened(&scratch, &world);
        let run = ["run", &dir, "--models", "b/bad", "--track", "unlimited"];
        let (_, whole) = ignot(&world, &run).unwrap();
        let journal = scratch.path("exams/pilot/run.partial.jsonl");
        assert!(!journal.exists(), "a run that finished leaves no journal");
        let answers = scratch.path("exams/pilot/answers/b_bad.json");
        let saved = fs::read_to_string(&answers).unwrap();

        // The same run again, cut short with two answers in and a third half written.
        let manifest =
            Manifest::parse(&fs::read_to_string(scratch.path("exams/pilot/manifest.json")).unwrap()).unwrap();
        let terms = Terms {
            exam_id: "pilot".into(),
            plaintext_sha256: manifest.plaintext_sha256,
            budget_nano_usd: BUDGET,
            effort: "high".into(),
        };
        let cut = journal_start(&terms, &AnswerFile::parse(&saved).unwrap().records[..2]).unwrap()
            + "{\"model\":\"b/bad\",\"tra";
        fs::write(&journal, &cut).unwrap();
        fs::remove_dir_all(scratch.path("exams/pilot/answers")).unwrap();
        fs::remove_file(scratch.path("exams/pilot/results.json")).unwrap();
        let before = asked(&world);
        let (passed, printed) = ignot(&world, &run).unwrap();
        assert!(passed, "{printed}");
        assert!(
            printed.starts_with("taking up a run that was cut short, with 2 of its answers in already\n"),
            "{printed}"
        );
        assert_eq!(asked(&world), before + 2, "only the two answers that were missing are asked for");
        assert_eq!(fs::read_to_string(&answers).unwrap(), saved);
        assert_eq!(printed.lines().last(), whole.lines().last(), "the same answers give the same root");
        assert!(!journal.exists());
        assert!(ignot(&world, &["verify", &dir]).unwrap().0);

        // A journal that does not fit the run is refused before anything is asked, and is left where it is.
        fs::write(&journal, &cut).unwrap();
        let before = asked(&world);
        let error = fails(&world, &["run", &dir, "--models", "b/bad", "--track", "unlimited", "--effort", "low"]);
        assert!(
            error.contains(
                "run.partial.jsonl holds a run that was cut short, and this run does not fit it: run that one"
            ) && error.contains("it was started with $0.02 a question at high effort"),
            "{error}"
        );
        let error = fails(&world, &["run", &dir, "--models", "a/good", "--track", "unlimited"]);
        assert!(error.contains("answers of b/bad on the unlimited track, which this run does not ask for"), "{error}");
        assert_eq!(asked(&world), before);
        assert_eq!(fs::read_to_string(&journal).unwrap(), cut);
        // A run that asks for more takes it over all the same.
        let (passed, printed) =
            ignot(&world, &["run", &dir, "--models", "a/good,b/bad", "--track", "unlimited"]).unwrap();
        assert!(passed && printed.contains("with 2 of its answers in already"), "{printed}");
        assert!(printed.contains("4 of the 8 answers this run asks for are in the folder already and are kept\n"));
        // Four answers of a/good, whose first request the provider fails once. Those of b/bad are all in.
        assert_eq!(asked(&world), before + 4 + 1);
        assert!(!journal.exists());
    }

    #[test]
    fn the_same_command_twice_asks_nothing_the_second_time() {
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        let dir = opened(&scratch, &world);
        let run = ["run", &dir, "--models", "a/good,b/bad"];
        let (_, first) = ignot(&world, &run).unwrap();
        let paid = asked(&world);
        let (passed, again) = ignot(&world, &run).unwrap();
        assert!(passed, "{again}");
        assert!(
            again.starts_with(
                "16 of the 16 answers this run asks for are in the folder already and are kept\nevery answer this run asks for is in already: nothing is asked\n"
            ),
            "{again}"
        );
        assert_eq!(asked(&world), paid);
        assert_eq!(first.lines().last(), again.lines().last(), "the same answers, the same root");
        // To ask a model again from scratch, its file goes first.
        fs::remove_file(scratch.path("exams/pilot/answers/b_bad.json")).unwrap();
        ignot(&world, &run).unwrap();
        assert_eq!(asked(&world), paid + 8);
        assert!(ignot(&world, &["verify", &dir]).unwrap().0);
    }

    #[test]
    fn a_second_run_in_the_same_folder_is_refused() {
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        let dir = opened(&scratch, &world);
        // A run under way holds the lock, and the files stay readable meanwhile.
        let held = fs::File::create(scratch.path("exams/pilot/run.partial.lock")).unwrap();
        held.lock().unwrap();
        assert!(ignot(&world, &["verify", &dir]).unwrap().0);
        let error = fails(&world, &["run", &dir, "--models", "b/bad", "--track", "unlimited"]);
        assert!(error.contains("another run is under way in "), "{error}");
        assert_eq!(asked(&world), 0);
        assert!(!scratch.path("exams/pilot/run.partial.jsonl").exists());
        // Once it is over, the next run goes ahead.
        drop(held);
        assert!(ignot(&world, &["run", &dir, "--models", "b/bad", "--track", "unlimited"]).unwrap().0);
    }

    #[test]
    fn a_model_that_cannot_be_reached_is_not_saved_as_all_wrong() {
        let scratch = Scratch::new();
        let mut world = provider(&crate::run::tests::exam());
        let dir = opened(&scratch, &world);
        assert!(ignot(&world, &["run", &dir, "--models", "b/bad", "--track", "unlimited"]).unwrap().0);
        let answers = scratch.path("exams/pilot/answers/b_bad.json");
        let saved = fs::read(&answers).unwrap();

        // From here on the provider turns every request to b/bad away.
        let answering = std::mem::replace(&mut world.on_post, Box::new(|_, _, _| bail!("not used")));
        world.on_post = Box::new(move |url, bearer, body| {
            if body.contains("\"b/bad\"") {
                Ok((404, r#"{"error":{"message":"No endpoints found"}}"#.into()))
            } else {
                answering(url, bearer, body)
            }
        });
        let (passed, printed) = ignot(&world, &["run", &dir, "--models", "a/good,b/bad"]).unwrap();
        assert!(!passed, "{printed}");
        assert!(
            printed.contains(
                "no request to b/bad on the same-budget track got through (HTTP 404: No endpoints found, with the price cap raised as far as 3.375 times the listed price); nothing of it is saved\n"
            ),
            "{printed}"
        );
        // What a/good answered is saved, and what b/bad answered before stays as it was.
        assert!(
            printed.contains(
                "\nsame-budget ($0.02 a question for every model)\n    4/4  $0.004000  a/good\n\nunlimited\n    4/4  $0.004000  a/good\n    0/4  $0.004000  b/bad\n"
            ),
            "{printed}"
        );
        assert_eq!(fs::read(&answers).unwrap(), saved);
        assert!(ignot(&world, &["verify", &dir]).unwrap().0);
        let results = fs::read(scratch.path("exams/pilot/results.json")).unwrap();

        // Asked alone, it changes nothing at all.
        let (passed, printed) = ignot(&world, &["run", &dir, "--models", "b/bad", "--track", "same-budget"]).unwrap();
        assert!(!passed && printed.ends_with("nothing was saved\n"), "{printed}");
        assert_eq!(fs::read(scratch.path("exams/pilot/results.json")).unwrap(), results);
        assert_eq!(fs::read(&answers).unwrap(), saved);
        assert!(!scratch.path("exams/pilot/run.partial.jsonl").exists());
    }

    #[test]
    fn requests_that_failed_are_asked_again_by_the_same_command() {
        let scratch = Scratch::new();
        let mut world = provider(&crate::run::tests::exam());
        let dir = opened(&scratch, &world);
        // The provider loses the first, third and fifth requests it gets.
        let answering = std::mem::replace(&mut world.on_post, Box::new(|_, _, _| bail!("not used")));
        let turn = std::sync::atomic::AtomicUsize::new(0);
        world.on_post = Box::new(move |url, bearer, body| {
            if [0, 2, 4].contains(&turn.fetch_add(1, std::sync::atomic::Ordering::SeqCst)) {
                Ok((400, r#"{"error":{"message":"lost"}}"#.into()))
            } else {
                answering(url, bearer, body)
            }
        });
        let run = ["run", &dir, "--models", "b/bad", "--track", "unlimited", "--concurrency", "1"];
        let (passed, printed) = ignot(&world, &run).unwrap();
        assert!(passed, "{printed}");
        assert!(
            printed.contains(
                "\n2 requests failed; each counts as not answered until the same command asks it again\n\nresults root 0x"
            ),
            "{printed}"
        );
        assert!(ignot(&world, &["verify", &dir]).unwrap().0);
        // The same command asks the two again, and only them; one of them fails again.
        let (_, printed) = ignot(&world, &run).unwrap();
        assert_eq!(asked(&world), 4 + 2);
        assert!(
            printed.contains("\n1 request failed; it counts as not answered until the same command asks it again\n")
        );
        // And then the last one.
        let (_, printed) = ignot(&world, &run).unwrap();
        assert_eq!(asked(&world), 4 + 2 + 1);
        assert!(!printed.contains("failed"), "{printed}");
        let saved = fs::read_to_string(scratch.path("exams/pilot/answers/b_bad.json")).unwrap();
        assert!(!saved.contains("\"error\""), "{saved}");
        assert!(ignot(&world, &["verify", &dir]).unwrap().0);
    }

    #[test]
    fn answers_billed_above_the_budget_are_named() {
        let scratch = Scratch::new();
        let mut world = provider(&crate::run::tests::exam());
        let dir = opened(&scratch, &world);
        // The provider bills every same-budget answer two and a half cents, as a search fee on top would.
        let answering = std::mem::replace(&mut world.on_post, Box::new(|_, _, _| bail!("not used")));
        world.on_post = Box::new(move |url, bearer, body| {
            let (status, reply) = answering(url, bearer, body)?;
            Ok((
                status,
                if body.contains("max_price") { reply.replace("\"cost\":0.001", "\"cost\":0.025") } else { reply },
            ))
        });
        let (passed, printed) = ignot(&world, &["run", &dir, "--models", "a/good"]).unwrap();
        assert!(passed, "{printed}");
        assert!(
            printed.contains("\nsame-budget ($0.02 a question for every model)\n    0/4  $0.100000  a/good\n"),
            "{printed}"
        );
        assert!(
            printed.contains(
                "\n4 same-budget answers of a/good cost more than the budget and do not count: it is billed fees its listed prices do not show\n"
            ),
            "{printed}"
        );
    }

    #[test]
    fn words_from_files_cannot_add_lines_to_a_report() {
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        let dir = opened(&scratch, &world);
        ignot(&world, &["run", &dir, "--models", "b/bad", "--track", "unlimited"]).unwrap();
        // An answers file with a field nobody defined, whose name carries a line of its own.
        let path = scratch.path("exams/pilot/answers/b_bad.json");
        let edited = fs::read_to_string(&path).unwrap().replacen(
            "\"format\": 1,",
            "\"format\": 1, \"x\\nPASS exams/pilot (open)\\u202e\": 1,",
            1,
        );
        fs::write(&path, edited).unwrap();
        let (passed, printed) = ignot(&world, &["verify", &dir]).unwrap();
        assert!(!passed, "{printed}");
        assert!(
            printed.contains("  FAIL  the answers and results read: json: unknown field `x PASS exams/pilot (open)?`"),
            "{printed}"
        );
        assert_eq!(printed.lines().filter(|line| line.starts_with("PASS")).count(), 0, "{printed}");
    }

    #[cfg(unix)]
    #[test]
    fn the_name_of_a_folder_cannot_add_lines_to_a_report() {
        let scratch = Scratch::new();
        let world = Scripted::at(BEFORE);
        let odd = scratch.path("exams/odd\nPASS fine (open)");
        fs::create_dir_all(&odd).unwrap();
        fs::write(odd.join("manifest.json"), "{}").unwrap();
        fs::write(odd.join("exam.age"), "").unwrap();
        let (passed, printed) = ignot(&world, &["verify-all", scratch.path("exams").to_str().unwrap()]).unwrap();
        assert!(!passed, "{printed}");
        assert!(printed.contains("exams/odd PASS fine (open)\n"), "{printed}");
        assert_eq!(printed.lines().filter(|line| line.starts_with("PASS")).count(), 0, "{printed}");
    }

    /// A node that serves chain 4663 and one receipt per transaction hash.
    fn with_node(mut world: Scripted, receipts: Vec<(String, Value)>) -> Scripted {
        world.on_post = Box::new(move |url, _, body| {
            assert_eq!(url, "https://rpc.mainnet.chain.robinhood.com");
            let request: Value = serde_json::from_str(body).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "eth_chainId" => json!("0x1237"),
                _ => receipts
                    .iter()
                    .find(|(tx, _)| request["params"][0] == tx.as_str())
                    .map_or(Value::Null, |(_, receipt)| receipt.clone()),
            };
            Ok((200, json!({"jsonrpc": "2.0", "id": 1, "result": result}).to_string()))
        });
        world
    }

    #[test]
    fn the_record_on_chain_is_read_and_compared() {
        const CONTRACT: &str = "0x5fbdb2315678afecb367f032d93f642f64180aa3";
        let (seal_tx, publish_tx) = (format!("0x{}", "5e".repeat(32)), format!("0x{}", "9b".repeat(32)));
        let scratch = Scratch::new();
        let world = provider(&crate::run::tests::exam());
        world.set_now(BEFORE);
        let dir = sealed(&scratch, &world);
        world.publish_round_1000();
        world.set_now(ROUND_MS);
        ignot(&world, &["open", &dir]).unwrap();
        ignot(&world, &["run", &dir, "--models", "a/good", "--track", "unlimited"]).unwrap();

        let manifest_path = scratch.path("exams/pilot/manifest.json");
        let mut manifest: Value = serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap()).unwrap();
        let results: Value =
            serde_json::from_str(&fs::read_to_string(scratch.path("exams/pilot/results.json")).unwrap()).unwrap();
        manifest["chain"] = json!({"chainId": 4663, "contract": CONTRACT, "sealTx": seal_tx, "publishTx": publish_tx});
        fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        let hex = |value: &Value| value.as_str().unwrap().trim_start_matches("0x").to_string();
        let log = |event: &str, data: String| json!({"address": CONTRACT, "topics": [topic(event), manifest["examId"]], "data": format!("0x{data}")});
        let receipt = |tx: &str, log: Value| json!({"status": "0x1", "transactionHash": tx, "logs": [log]});
        let seal_data =
            format!("{}{}{:064x}", hex(&manifest["ciphertextSha256"]), hex(&manifest["plaintextSha256"]), 1000);
        let seal = receipt(&seal_tx, log(SEALED, seal_data));
        let publish = receipt(&publish_tx, log(PUBLISHED, hex(&results["resultsRoot"])));

        let record = Some(chain::Record { chain_id: 4663, contract: CONTRACT });
        let world = with_node(
            Scripted::at(ROUND_MS).with_round_1000(),
            vec![(seal_tx.clone(), seal.clone()), (publish_tx.clone(), publish)],
        );
        let (passed, printed) = ignot_trusting(record, &world, &["verify", &dir]).unwrap();
        assert!(passed && !printed.contains("skip"), "{printed}");
        assert!(printed.starts_with(&format!("PASS {dir} (open)\n")), "{printed}");
        assert!(
            printed.contains("   ok   the chain recorded this seal before the round")
                && printed.contains("   ok   the chain recorded this results root"),
            "{printed}"
        );

        // Without reading the chain those two checks are skipped, and say so.
        let (passed, printed) = ignot_trusting(record, &world, &["verify", &dir, "--no-chain"]).unwrap();
        assert!(
            passed && printed.matches("  skip  ").count() == 2 && printed.contains("the chain was not read"),
            "{printed}"
        );

        // With no record contract to trust, the chain is not read at all, and the report says why.
        let asked = world.posts.load(std::sync::atomic::Ordering::SeqCst);
        let (passed, printed) = ignot(&world, &["verify", &dir]).unwrap();
        assert!(
            passed && printed.matches("this version of ignot trusts no record contract yet").count() == 2,
            "{printed}"
        );
        assert_eq!(world.posts.load(std::sync::atomic::Ordering::SeqCst), asked);

        // A manifest that points at some other contract fails without a node being asked.
        let elsewhere = Some(chain::Record { chain_id: 4663, contract: "0x00000000000000000000000000000000000000bb" });
        let (passed, printed) = ignot_trusting(elsewhere, &world, &["verify", &dir, "--no-chain"]).unwrap();
        assert!(!passed && printed.matches("is not the record contract").count() == 2, "{printed}");

        // A node that has the seal but no publication.
        let world = with_node(Scripted::at(ROUND_MS).with_round_1000(), vec![(seal_tx, seal)]);
        let (passed, printed) = ignot_trusting(record, &world, &["verify", &dir]).unwrap();
        assert!(
            !passed
                && printed.contains(
                    "  FAIL  the chain recorded this results root: that transaction holds no Published event"
                ),
            "{printed}"
        );
    }

    #[test]
    fn past_the_round_an_unreachable_relay_fails_the_check_with_its_reason() {
        let scratch = Scratch::new();
        let world = Scripted::at(BEFORE);
        let dir = sealed(&scratch, &world);
        world.set_now(ROUND_MS + 3_600_000);
        let (passed, printed) = ignot(&world, &["verify", &dir]).unwrap();
        assert!(!passed);
        assert!(
            printed.contains(
                "  FAIL  the round's signature was given: the round has passed, and its signature was not given\n"
            ),
            "{printed}"
        );
        assert!(printed.contains("  note  no drand relay gave round 1000: "), "{printed}");
    }

    #[test]
    fn any_file_seals_and_opens() {
        let scratch = Scratch::new();
        let world = Scripted::at(BEFORE);
        let source = scratch.path("notes.bin");
        fs::write(&source, [0u8, 159, 146, 150, 255]).unwrap();
        let source = source.to_str().unwrap();
        let (_, printed) = ignot(&world, &["seal-file", source, "--open", WHEN]).unwrap();
        assert!(
            printed.contains("notes.bin.age; it opens at 2023-08-23T15:59:24.000Z (drand round 1000)"),
            "{printed}"
        );
        let sealed_path = format!("{source}.age");
        assert!(fails(&world, &["seal-file", source, "--open", WHEN]).contains("already exists"));
        assert!(
            fails(&world, &["open-file", &sealed_path]).contains("still sealed: it opens at 2023-08-23T15:59:24.000Z")
        );
        world.publish_round_1000();
        world.set_now(ROUND_MS);
        assert!(
            fails(&world, &["open-file", &sealed_path]).contains("already exists"),
            "the original is never overwritten"
        );
        let opened = scratch.path("opened.bin");
        ignot(&world, &["open-file", &sealed_path, "--out", opened.to_str().unwrap()]).unwrap();
        assert_eq!(fs::read(&opened).unwrap(), [0u8, 159, 146, 150, 255]);
        // Without .age in the name, the opened file gets .opened.
        let odd = scratch.path("sealed-thing");
        fs::rename(&sealed_path, &odd).unwrap();
        ignot(&world, &["open-file", odd.to_str().unwrap()]).unwrap();
        assert!(scratch.path("sealed-thing.opened").exists());
        assert!(fails(&world, &["open-file", source]).contains("not an age file"));
    }

    #[test]
    fn a_folder_without_exams_passes_and_a_broken_one_fails() {
        let scratch = Scratch::new();
        let world = Scripted::at(BEFORE);
        let root = scratch.path("exams");
        // A folder that is not there is an error: a mistyped path must not read as "everything passes".
        assert!(fails(&world, &["verify-all", root.to_str().unwrap()]).contains("cannot list"));
        fs::create_dir_all(&root).unwrap();
        let (passed, printed) = ignot(&world, &["verify-all", root.to_str().unwrap()]).unwrap();
        assert!(passed && printed.starts_with("no exams in "), "{printed}");
        fs::create_dir_all(root.join("broken")).unwrap();
        fs::write(root.join("broken/manifest.json"), "{}").unwrap();
        fs::write(root.join("broken/exam.age"), "").unwrap();
        let (passed, printed) = ignot(&world, &["verify-all", root.to_str().unwrap()]).unwrap();
        assert!(!passed && printed.contains("FAIL ") && printed.ends_with("0 of 1 exams pass\n"), "{printed}");
        assert!(fails(&world, &["verify", root.join("missing").to_str().unwrap()]).contains("cannot read"));
    }
}
