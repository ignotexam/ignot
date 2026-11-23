//! Putting an exam to models, through OpenRouter.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use ignot_core::budget::{
    COST_DECIMALS, PRICE_DECIMALS, Pricing, Track, format_amount, parse_amount, same_budget_max_tokens,
    unlimited_max_tokens,
};
use ignot_core::exam::{Exam, FORMAT, Question};
use ignot_core::results::{AnswerFile, AnswerRecord, score, valid_host};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::net::World;

/// What every model is told before the question.
pub const SYSTEM: &str =
    "Solve the problem carefully. End your reply with one final line in exactly this form:\nANSWER: <your answer>";
pub const PROVIDER: &str = "https://openrouter.ai/api/v1";
/// A model on the unlimited track may think for a long time.
const PATIENCE: Duration = Duration::from_secs(15 * 60);
/// How long to wait before each new try of a request the provider could not take: it was busy (429) or failed
/// on its side (5xx). Such an answer is not billed.
const RETRY_AFTER: [Duration; 2] = [Duration::from_secs(3), Duration::from_secs(15)];
/// How a same-budget request goes on when no host takes it at its price cap, because the cheapest host is busy
/// or gone: the cap is raised by half, at most this many times, and the model is given the tokens the budget
/// buys at the raised price. The budget holds at every step, and a busy cheapest host costs a model some of its
/// thinking, not the question.
const PRICE_STEPS: usize = 3;
/// The price at each step, as a multiple of the listed price.
const STEP_FACTORS: [&str; PRICE_STEPS + 1] = ["1", "1.5", "2.25", "3.375"];

/// One run: who is asked what, and on which terms.
pub struct Plan<'a> {
    pub exam: &'a Exam,
    pub models: &'a [String],
    pub tracks: &'a [Track],
    /// Billionths of a dollar per question on the same-budget track.
    pub budget: u64,
    pub effort: &'a str,
    pub concurrency: usize,
    pub api_key: &'a str,
}

/// One price in a provider's list, in trillionths of a dollar. `None` when the list does not name it. A price
/// that is named must be a plain amount; the price of a router that picks the model itself is not.
///
/// A few prices have more decimals than a trillionth (a cache price of a twelfth of a cent per million tokens,
/// for one). They are rounded up, which keeps every reserve and every cap at or above what a host bills.
fn listed(prices: &Value, field: &str) -> Result<Option<u64>> {
    let text = match prices.get(field) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(text)) => text,
        Some(other) => bail!("{other} is not an amount"),
    };
    let decimals = PRICE_DECIMALS as usize;
    match text.split_once('.') {
        Some((whole, fraction)) if fraction.len() > decimals && fraction.bytes().all(|c| c.is_ascii_digit()) => {
            let (kept, rest) = fraction.split_at(decimals);
            let units = parse_amount(&format!("{whole}.{kept}"), PRICE_DECIMALS)?;
            let up = u64::from(rest.bytes().any(|c| c != b'0'));
            Ok(Some(units.checked_add(up).context("the price is too large")?))
        }
        _ => Ok(Some(parse_amount(text, PRICE_DECIMALS)?)),
    }
}

/// What each model of a run charges, from the provider's public list.
///
/// A model may list more than one price: a dearer one for some hours of the day, for long prompts, for the
/// tokens it thinks with, or for prompt tokens that a host writes to its cache (some do that unasked and bill
/// it). The price taken is the dearest that can apply to a prompt of at most `longest` tokens, so that a budget
/// worked out from it holds at any hour and at any host the request is capped to.
pub fn fetch_pricing(world: &dyn World, models: &[String], longest: u64) -> Result<BTreeMap<String, Pricing>> {
    let (status, body) = world.get(&format!("{PROVIDER}/models")).context("the list of models could not be fetched")?;
    if status != 200 {
        bail!("the list of models could not be fetched: the provider answered {status}");
    }
    let list: Value = serde_json::from_str(&body).context("the list of models is not JSON")?;
    let entries = list.get("data").and_then(Value::as_array).context("the list of models has no data")?;
    let mut pricing = BTreeMap::new();
    for model in models {
        let Some(entry) = entries.iter().find(|entry| entry.get("id").and_then(Value::as_str) == Some(model)) else {
            bail!("the provider lists no model named {model}");
        };
        let price = |prices: &Value, field: &str| {
            listed(prices, field).with_context(|| format!("{model} has no fixed {field} price"))
        };
        let prices = entry.get("pricing").unwrap_or(&Value::Null);
        let needed = |field: &str| price(prices, field)?.with_context(|| format!("{model} lists no {field} price"));
        let mut prompt = needed("prompt")?;
        let mut cache_write = price(prices, "input_cache_write")?.unwrap_or(0);
        let mut completion = needed("completion")?.max(price(prices, "internal_reasoning")?.unwrap_or(0));
        for other in prices.get("overrides").and_then(Value::as_array).into_iter().flatten() {
            // A price for long prompts only says nothing about prompts that are never that long.
            if other.get("min_prompt_tokens").and_then(Value::as_u64).is_some_and(|least| least > longest) {
                continue;
            }
            prompt = prompt.max(price(other, "prompt")?.unwrap_or(0));
            cache_write = cache_write.max(price(other, "input_cache_write")?.unwrap_or(0));
            completion = completion.max(price(other, "completion")?.unwrap_or(0));
        }
        let most = entry.pointer("/top_provider/max_completion_tokens").and_then(Value::as_u64);
        pricing.insert(
            model.clone(),
            Pricing {
                prompt,
                cache_write,
                completion,
                request: price(prices, "request")?.unwrap_or(0),
                max_completion: most.map(|n| u32::try_from(n).unwrap_or(u32::MAX)),
            },
        );
    }
    Ok(pricing)
}

/// Dollars, as a provider reports them, in billionths.
fn nano(usd: f64) -> Option<u64> {
    let nano = (usd * 1e9).round();
    // The cast cannot lose anything: the value is whole and inside the range.
    (nano.is_finite() && (0.0..=1e18).contains(&nano)).then_some(nano as u64)
}

/// What a request costs by the price list, in billionths of a dollar, rounded up.
fn list_cost(prompt_tokens: u64, completion_tokens: u64, price: &Pricing) -> u64 {
    let trillionths = (u128::from(prompt_tokens) * u128::from(price.prompt))
        .saturating_add(u128::from(completion_tokens) * u128::from(price.completion))
        .saturating_add(u128::from(price.request));
    u64::try_from(trillionths.div_ceil(1000)).unwrap_or(u64::MAX)
}

/// A whole number of units with `decimals` decimals as a number with a decimal point, for the provider's price
/// cap. Formatted as a decimal first, so that $0.10 goes out as 0.1 and not as 0.09999999999999999, which
/// would be under the provider's own price.
fn number(units: u64, decimals: u32) -> f64 {
    format_amount(units, decimals).parse().unwrap_or(0.0)
}

/// A price per token, in trillionths of a dollar, as dollars per million tokens: the unit the cap is in.
fn dollars_per_million(price: u64) -> f64 {
    number(price, PRICE_DECIMALS - 6)
}

/// A reason as a record keeps it: one line, and short.
fn clip(message: &str) -> String {
    crate::one_line(message).chars().take(200).collect()
}

/// The prices raised by half `step` times, rounded up.
fn raised(price: &Pricing, step: usize) -> Pricing {
    let (times, over) = (3u128.pow(u32::try_from(step).unwrap_or(0)), 2u128.pow(u32::try_from(step).unwrap_or(0)));
    let up = |amount: u64| u64::try_from((u128::from(amount) * times).div_ceil(over)).unwrap_or(u64::MAX);
    Pricing {
        prompt: up(price.prompt),
        cache_write: up(price.cache_write),
        completion: up(price.completion),
        request: up(price.request),
        max_completion: price.max_completion,
    }
}

/// What came of one request, tried a few times.
enum Outcome {
    Answered(AnswerRecord),
    /// No host took it at this price: the cheapest is busy (429), failing (5xx) or not there at all (404).
    NoHost(String),
    /// Refused for another reason, or no answer came at all.
    Failed(String),
}

/// Asks one model one question. Whatever happens, the outcome is a record: a failed request is an answer that
/// does not count, with the reason kept.
fn ask(
    world: &dyn World,
    plan: &Plan,
    model: &str,
    track: Track,
    question: &Question,
    price: &Pricing,
) -> AnswerRecord {
    let started = world.now_ms();
    let failed = |max_tokens: u32, error: String| AnswerRecord {
        model: model.to_string(),
        track,
        question_id: question.id.clone(),
        raw: String::new(),
        extracted: String::new(),
        correct: false,
        cost_nano_usd: 0,
        prompt_tokens: 0,
        completion_tokens: 0,
        max_tokens,
        latency_ms: world.now_ms().saturating_sub(started),
        finish_reason: None,
        host: None,
        error: Some(error),
    };
    let steps = if track == Track::SameBudget { PRICE_STEPS } else { 0 };
    let (mut last, mut before) = (None, None);
    for step in 0..=steps {
        let price = raised(price, step);
        // A free model costs nothing at any step: raising its prices changes nothing worth a new try.
        if before.replace(price) == Some(price) {
            break;
        }
        let max_tokens = match track {
            Track::SameBudget => same_budget_max_tokens(plan.budget, SYSTEM.len() + question.prompt.len(), &price),
            Track::Unlimited => unlimited_max_tokens(&price),
        };
        if max_tokens == 0 {
            break;
        }
        match send(world, plan, model, track, question, &price, max_tokens, started) {
            Outcome::Answered(record) => return record,
            Outcome::NoHost(problem) => last = Some((max_tokens, problem, step)),
            Outcome::Failed(problem) => return failed(max_tokens, problem),
        }
    }
    match last {
        None => failed(0, "the budget does not buy a useful answer from this model".into()),
        Some((max_tokens, problem, 0)) => failed(max_tokens, problem),
        Some((max_tokens, problem, step)) => failed(
            max_tokens,
            clip(&format!(
                "{problem}, with the price cap raised as far as {} times the listed price",
                STEP_FACTORS[step]
            )),
        ),
    }
}

/// Sends one request, at one price, and tries it again while the provider is busy.
#[allow(clippy::too_many_arguments)]
fn send(
    world: &dyn World,
    plan: &Plan,
    model: &str,
    track: Track,
    question: &Question,
    price: &Pricing,
    max_tokens: u32,
    started: u64,
) -> Outcome {
    let mut request = json!({
        "model": model,
        "messages": [{"role": "system", "content": SYSTEM}, {"role": "user", "content": question.prompt}],
        "max_tokens": max_tokens,
        "reasoning": {"effort": plan.effort},
        "usage": {"include": true},
        // Of the hosts that serve a model, the cheapest that is up. Left to itself the provider spreads
        // requests over hosts that charge several times the listed price, and what an answer cost would be
        // a matter of luck.
        "provider": {"sort": "price"},
    });
    if track == Track::SameBudget {
        // The token allowance was worked out at this price. Should the request still end up at a dearer host,
        // a right answer would cost more than the budget through no fault of the model. With this cap such a
        // request is refused instead of being billed.
        request["provider"]["max_price"] = json!({
            "prompt": dollars_per_million(price.prompt),
            "completion": dollars_per_million(price.completion),
            "request": number(price.request, PRICE_DECIMALS),
        });
    }
    let request = request.to_string();

    let mut problem = String::new();
    for attempt in 0..=RETRY_AFTER.len() {
        if let Some(&wait) = attempt.checked_sub(1).and_then(|before| RETRY_AFTER.get(before)) {
            world.pause(wait);
        }
        let (status, body) =
            match world.post(&format!("{PROVIDER}/chat/completions"), Some(plan.api_key), &request, PATIENCE) {
                Ok(answer) => answer,
                Err(e) => {
                    problem = clip(&format!("the request failed: {e}"));
                    // A request that got no answer may have been carried out and billed all the same, so it is
                    // tried once more, not again and again.
                    if attempt == 0 {
                        continue;
                    }
                    return Outcome::Failed(problem);
                }
            };
        let reply: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let refusal = reply.get("error").filter(|error| !error.is_null());
        if !(200..300).contains(&status) || refusal.is_some() {
            let said =
                refusal.and_then(|error| error.get("message")).and_then(Value::as_str).unwrap_or("no reason given");
            problem = clip(&format!("HTTP {status}: {said}"));
            // Under a 200 the error carries its own code, which says what happened.
            let code = refusal
                .and_then(|error| error.get("code"))
                .and_then(Value::as_u64)
                .and_then(|code| u16::try_from(code).ok())
                .filter(|_| (200..300).contains(&status))
                .unwrap_or(status);
            match code {
                429 | 500.. => continue,
                404 => return Outcome::NoHost(problem),
                _ => return Outcome::Failed(problem),
            }
        }
        let Some(choice) = reply.pointer("/choices/0") else {
            return Outcome::Failed("the provider sent no reply".into());
        };
        let raw = choice.pointer("/message/content").and_then(Value::as_str).unwrap_or("").to_string();
        let count = |field: &str| reply.pointer(&format!("/usage/{field}")).and_then(Value::as_u64).unwrap_or(0);
        let (prompt_tokens, completion_tokens) = (count("prompt_tokens"), count("completion_tokens"));
        let billed = reply.pointer("/usage/cost").and_then(Value::as_f64).and_then(nano);
        let cost_nano_usd = billed.unwrap_or_else(|| list_cost(prompt_tokens, completion_tokens, price));
        let finish_reason = choice.get("finish_reason").and_then(Value::as_str).map(String::from);
        // A host can fail in the middle of a reply. The provider then answers 200 and puts the error inside the
        // choice: that is no answer, though what it cost is kept.
        let broke = choice.get("error").filter(|error| !error.is_null());
        let error = (broke.is_some() || finish_reason.as_deref() == Some("error")).then(|| {
            let said =
                broke.and_then(|error| error.get("message")).and_then(Value::as_str).unwrap_or("no reason given");
            clip(&format!("the reply broke off: {said}"))
        });
        let (extracted, correct) = score(question, track, &raw, cost_nano_usd, error.is_some(), plan.budget);
        return Outcome::Answered(AnswerRecord {
            model: model.to_string(),
            track,
            question_id: question.id.clone(),
            raw,
            extracted,
            correct,
            cost_nano_usd,
            prompt_tokens,
            completion_tokens,
            max_tokens,
            latency_ms: world.now_ms().saturating_sub(started),
            finish_reason,
            // the host the provider routed the request to, kept so that a reader can tell hosts apart
            host: reply.get("provider").and_then(Value::as_str).filter(|name| valid_host(name)).map(String::from),
            error,
        });
    }
    Outcome::NoHost(problem)
}

/// Dollars with six decimals, for a line of progress.
pub fn dollars(nano_usd: u64) -> String {
    format!("${}.{:06}", nano_usd / 1_000_000_000, nano_usd % 1_000_000_000 / 1000)
}

fn files_by_model(exam_id: &str, records: Vec<AnswerRecord>) -> Vec<AnswerFile> {
    let mut by_model: BTreeMap<String, Vec<AnswerRecord>> = BTreeMap::new();
    for record in records {
        by_model.entry(record.model.clone()).or_default().push(record);
    }
    by_model
        .into_iter()
        .map(|(model, mut records)| {
            records.sort_by(|a, b| (a.track, &a.question_id).cmp(&(b.track, &b.question_id)));
            AnswerFile { format: FORMAT, exam_id: exam_id.to_string(), model, records }
        })
        .collect()
}

/// The terms a run was started on. A run that was cut short is taken up again on the same terms only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Terms {
    pub exam_id: String,
    pub plaintext_sha256: String,
    pub budget_nano_usd: u64,
    pub effort: String,
}

/// How the journal of a run begins: its terms on the first line, then the answers taken over from the run it
/// continues, one on each line. The run adds a line for every answer that comes in.
pub fn journal_start(terms: &Terms, done: &[AnswerRecord]) -> Result<String> {
    let mut text = serde_json::to_string(terms)? + "\n";
    for record in done {
        text += &serde_json::to_string(record)?;
        text.push('\n');
    }
    Ok(text)
}

/// The answers in the journal of a run that was cut short which need not be asked for again.
///
/// A request that failed is asked again, so its line is passed over. Everything else has to fit this run: the
/// same exam and terms, a model and a track that are asked for now, a question of the exam, and a mark the
/// reply grades to. What does not fit is refused here, before any new answer is paid for.
pub fn journal_answers(text: &str, terms: &Terms, plan: &Plan) -> Result<Vec<AnswerRecord>> {
    // Only whole lines count: the run may have been cut short in the middle of writing one.
    let mut lines = text.rfind('\n').map_or("", |end| &text[..end]).split('\n');
    let Some(first) = lines.next().filter(|first| !first.is_empty()) else {
        return Ok(Vec::new());
    };
    let started: Terms = serde_json::from_str(first).context("its first line is not the terms of a run")?;
    if (&started.exam_id, &started.plaintext_sha256) != (&terms.exam_id, &terms.plaintext_sha256) {
        bail!("it belongs to another exam, {}", started.exam_id);
    }
    if started != *terms {
        bail!(
            "it was started with ${} a question at {} effort, and can only be finished that way",
            format_amount(started.budget_nano_usd, COST_DECIMALS),
            started.effort
        );
    }
    let mut done: Vec<AnswerRecord> = Vec::new();
    let mut seen = BTreeSet::new();
    for line in lines {
        let record: AnswerRecord = serde_json::from_str(line).context("one of its lines is not an answer")?;
        if record.error.is_some() {
            continue;
        }
        let (model, track) = (&record.model, record.track);
        if !plan.models.contains(model) || !plan.tracks.contains(&track) {
            bail!("it holds answers of {model} on the {} track, which this run does not ask for", track.name());
        }
        let Some(question) = plan.exam.questions.iter().find(|question| question.id == record.question_id) else {
            bail!("it holds an answer to {}, which is not a question of the exam", record.question_id);
        };
        if score(question, track, &record.raw, record.cost_nano_usd, false, plan.budget)
            != (record.extracted.clone(), record.correct)
        {
            bail!("the mark it gives {model} on {} is not what the reply grades to", question.id);
        }
        if !seen.insert((model.clone(), track, question.id.clone())) {
            bail!("it holds two answers of {model} to {} on the {} track", question.id, track.name());
        }
        done.push(record);
    }
    Ok(done)
}

/// What the workers of a run share: where progress is shown, where each answer is written down as it comes
/// in, and the answers themselves.
struct Desk<'a> {
    log: &'a mut (dyn Write + Send),
    journal: Option<&'a mut (dyn Write + Send)>,
    records: Vec<AnswerRecord>,
}

impl Desk<'_> {
    fn take(&mut self, line: &str, record: AnswerRecord) {
        // A reader that went away must not stop a run that is being paid for.
        writeln!(self.log, "{}", crate::one_line(line)).ok();
        // One write for each answer, so a run that is cut short loses at most the answer it was writing.
        let written = serde_json::to_string(&record).map_err(io::Error::other).and_then(|json| {
            self.journal
                .as_mut()
                .map_or(Ok(()), |journal| journal.write_all((json + "\n").as_bytes()).and_then(|()| journal.flush()))
        });
        if let Err(e) = written {
            self.journal = None;
            writeln!(self.log, "the journal could not be written ({e}): cut short, this run would ask again").ok();
        }
        self.records.push(record);
    }
}

/// Asks every model every question on every track of the plan, a few at a time, and files the answers by
/// model. The answers in `done` are taken as given and not asked for again; every new one is added to
/// `journal` the moment it comes in.
pub fn run_exam(
    world: &dyn World,
    plan: &Plan,
    pricing: &BTreeMap<String, Pricing>,
    done: Vec<AnswerRecord>,
    journal: &mut (dyn Write + Send),
    log: &mut (dyn Write + Send),
) -> Vec<AnswerFile> {
    let mut jobs = Vec::new();
    {
        let have: BTreeSet<_> = done.iter().map(|r| (r.model.as_str(), r.track, r.question_id.as_str())).collect();
        for model in plan.models {
            let Some(price) = pricing.get(model) else { continue };
            for &track in plan.tracks {
                let open =
                    plan.exam.questions.iter().filter(|q| !have.contains(&(model.as_str(), track, q.id.as_str())));
                jobs.extend(open.map(|question| (model.as_str(), track, question, price)));
            }
        }
    }
    let next = AtomicUsize::new(0);
    let desk = Mutex::new(Desk { log, journal: Some(journal), records: done });
    std::thread::scope(|scope| {
        for _ in 0..plan.concurrency.clamp(1, jobs.len().max(1)) {
            scope.spawn(|| {
                while let Some(&(model, track, question, price)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let record = ask(world, plan, model, track, question, price);
                    let mark = if record.correct {
                        "ok "
                    } else if record.error.is_some() {
                        "err"
                    } else {
                        "x  "
                    };
                    let line = format!(
                        "{mark} {:<11} {model} {} {}{}",
                        track.name(),
                        question.id,
                        dollars(record.cost_nano_usd),
                        record.error.as_ref().map(|e| format!("  {e}")).unwrap_or_default()
                    );
                    desk.lock().unwrap_or_else(PoisonError::into_inner).take(&line, record);
                }
            });
        }
    });
    files_by_model(&plan.exam.id, desk.into_inner().unwrap_or_else(PoisonError::into_inner).records)
}

/// Takes out of a run every pair of model and track that no request got through to, and returns those pairs
/// with the first reason given: a wrong key, a provider that is down, a model that was withdrawn. Saved, such
/// a pair would read as a model that got every question wrong.
///
/// A pair that was never sent a request, because the budget buys nothing from that model, stays: that is its
/// answer.
pub fn set_aside_unanswered(files: Vec<AnswerFile>) -> (Vec<AnswerFile>, Vec<(String, Track, String)>) {
    let mut sent: BTreeMap<(String, Track), (bool, Option<String>)> = BTreeMap::new();
    for record in files.iter().flat_map(|file| &file.records).filter(|record| record.max_tokens > 0) {
        let (through, why) = sent.entry((record.model.clone(), record.track)).or_default();
        match &record.error {
            None => *through = true,
            Some(error) => drop(why.get_or_insert_with(|| error.clone())),
        }
    }
    let lost: Vec<(String, Track, String)> = sent
        .into_iter()
        .filter(|(_, (through, _))| !through)
        .map(|((model, track), (_, why))| (model, track, why.unwrap_or_default()))
        .collect();
    let is_lost =
        |record: &AnswerRecord| lost.iter().any(|(model, track, _)| (model, *track) == (&record.model, record.track));
    let kept = files
        .into_iter()
        .filter_map(|mut file| {
            file.records.retain(|record| !is_lost(record));
            (!file.records.is_empty()).then_some(file)
        })
        .collect();
    (kept, lost)
}

/// Adds a run to what was recorded before. Pairs of model and track that the new run answered replace the old
/// ones; everything else stays as it was.
pub fn merge_answers(exam_id: &str, before: Vec<AnswerFile>, after: Vec<AnswerFile>) -> Vec<AnswerFile> {
    let again: BTreeSet<(&str, Track)> =
        after.iter().flat_map(|file| &file.records).map(|record| (record.model.as_str(), record.track)).collect();
    let kept: Vec<AnswerRecord> = before
        .iter()
        .flat_map(|file| &file.records)
        .filter(|record| !again.contains(&(record.model.as_str(), record.track)))
        .cloned()
        .collect();
    let fresh = after.iter().flat_map(|file| file.records.iter().cloned());
    files_by_model(exam_id, kept.into_iter().chain(fresh).collect())
}

#[cfg(test)]
pub mod tests {
    use ignot_core::budget::most_prompt_tokens;
    use ignot_core::exam::{Recipe, make_exam, parse_mix};
    use ignot_core::results::build_results;

    use super::*;
    use crate::net::tests::Scripted;

    pub const BUDGET: u64 = 20_000_000;
    pub const SEED: &str = "a seed for the tests of the runner, long enough to seal";

    pub fn exam() -> Exam {
        make_exam(&Recipe {
            id: "runner-test".into(),
            title: "Runner test".into(),
            created_at: "2026-11-22T00:00:00.000Z".into(),
            commit: "abc1234".into(),
            seed: SEED.into(),
            mix: parse_mix("stack:1:2,bases:1:2").unwrap(),
        })
        .unwrap()
    }

    /// The provider's list: two cheap models, one dear one, and one without a fixed price.
    pub const MODELS: &str = r#"{"data":[
        {"id":"a/good","pricing":{"prompt":"0.0000001","completion":"0.0000004"},"top_provider":{"max_completion_tokens":8192}},
        {"id":"b/bad","pricing":{"prompt":"0.0000001","completion":"0.0000004"},"top_provider":{"max_completion_tokens":null}},
        {"id":"c/pricey","pricing":{"prompt":"0.001","completion":"0.001"}},
        {"id":"d/auto","pricing":{"prompt":"-1","completion":"-1"}},
        {"id":"e/peak","pricing":{"prompt":"0.0000001","completion":"0.0000004","internal_reasoning":"0.0000005",
            "request":"0.001","input_cache_write":"0.000000125","input_cache_read":"0.00000001","overrides":[
                {"utc_start":100,"utc_end":1500,"prompt":"0.0000002","completion":"0.0000003","input_cache_write":"0.000000375"},
                {"min_prompt_tokens":272000,"prompt":"0.000001","completion":"0.000002"},
                {"min_prompt_tokens":1000,"prompt":"0.0000003","completion":"0.0000001"}]}},
        {"id":"f/odd","pricing":{"prompt":"0.0000001","completion":"0.0000004","request":0.01}},
        {"id":"g/bare","pricing":{"prompt":"0.0000001"}},
        {"id":"h/fine","pricing":{"prompt":"0.000000100000000000000","completion":"0.0000004",
            "input_cache_write":"0.0000000833333333333333"}},
        {"id":"i/junk","pricing":{"prompt":"0.0000001","completion":"0.0000004",
            "input_cache_write":"0.00000008333333333333x3"}}
    ]}"#;

    /// A provider where a/good knows every key and fails its first request, and everyone else answers 12345.
    pub fn provider(exam: &Exam) -> Scripted {
        let keys: BTreeMap<String, String> =
            exam.questions.iter().map(|q| (q.prompt.clone(), q.answer.clone())).collect();
        let failed_once = std::sync::atomic::AtomicBool::new(false);
        let mut world = Scripted::at(1_800_000_000_000);
        world.page("/models", 200, MODELS);
        world.on_post = Box::new(move |url, bearer, body| {
            assert!(url.ends_with("/chat/completions"), "{url}");
            assert_eq!(bearer, Some("test-key"));
            let request: Value = serde_json::from_str(body).unwrap();
            assert_eq!(request["messages"][0]["content"], SYSTEM);
            assert_eq!(request["reasoning"]["effort"], "high");
            assert!(request["max_tokens"].as_u64().unwrap() >= 256);
            let model = request["model"].as_str().unwrap();
            if model == "a/good" && !failed_once.swap(true, Ordering::SeqCst) {
                return Ok((500, r#"{"error":{"message":"upstream"}}"#.into()));
            }
            let key = &keys[request["messages"][1]["content"].as_str().unwrap()];
            let content = if model == "a/good" { format!("thinking\nANSWER: {key}") } else { "ANSWER: 12345".into() };
            let reply = json!({
                "provider": "Host A",
                "choices": [{"message": {"content": content}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 100, "completion_tokens": 50, "cost": 0.001},
            });
            Ok((200, reply.to_string()))
        });
        world
    }

    fn names(models: &[&str]) -> Vec<String> {
        models.iter().map(|m| (*m).to_string()).collect()
    }

    #[test]
    fn the_runner_grades_retries_once_and_skips_what_the_budget_cannot_buy() {
        let exam = exam();
        let world = provider(&exam);
        let models = names(&["a/good", "b/bad", "c/pricey"]);
        let pricing = fetch_pricing(&world, &models, 0).unwrap();
        let plan = Plan {
            exam: &exam,
            models: &models,
            tracks: &Track::ALL,
            budget: BUDGET,
            effort: "high",
            concurrency: 3,
            api_key: "test-key",
        };
        let (mut journal, mut log) = (Vec::new(), Vec::new());
        let files = run_exam(&world, &plan, &pricing, Vec::new(), &mut journal, &mut log);
        // Every answer went into the journal as it came in, the skipped ones too.
        let journal = String::from_utf8(journal).unwrap();
        assert!(journal.ends_with('\n') && journal.lines().count() == 24, "{journal}");
        assert!(journal.lines().all(|line| serde_json::from_str::<AnswerRecord>(line).is_ok()), "{journal}");
        let results = build_results(&exam, &files, BUDGET).unwrap();

        let score = |track: Track, model: &str| {
            let scores = results.tracks.iter().find(|t| t.track == track).unwrap();
            scores.models.iter().find(|m| m.model == model).unwrap().clone()
        };
        assert_eq!(score(Track::Unlimited, "a/good").correct, 4);
        assert_eq!(score(Track::SameBudget, "a/good").correct, 4);
        assert_eq!(score(Track::Unlimited, "b/bad").correct, 0);
        assert_eq!(score(Track::SameBudget, "c/pricey").correct, 0);
        assert_eq!(score(Track::Unlimited, "c/pricey").cost_nano_usd, 4_000_000);
        let pricey = files.iter().find(|f| f.model == "c/pricey").unwrap();
        let skipped: Vec<_> = pricey.records.iter().filter(|r| r.track == Track::SameBudget).collect();
        assert!(
            skipped.len() == 4
                && skipped.iter().all(|r| r.error.is_some() && r.cost_nano_usd == 0 && r.max_tokens == 0)
        );
        // Three models, two tracks, four questions; less the dear model's four same-budget ones; plus one retry.
        assert_eq!(world.posts.load(Ordering::SeqCst), 3 * 2 * 4 - 4 + 1);
        let log = String::from_utf8(log).unwrap();
        assert_eq!(log.lines().count(), 24);
        assert!(log.contains("ok  unlimited   a/good q01 $0.001000"), "{log}");
        assert!(log.contains("err same-budget c/pricey q04 $0.000000  the budget does not buy"), "{log}");
        // The files are in the one order everything is hashed in.
        assert_eq!(files.iter().map(|f| f.model.as_str()).collect::<Vec<_>>(), ["a/good", "b/bad", "c/pricey"]);
        let first = &files[0].records;
        assert_eq!(
            (first[0].track, first[0].question_id.as_str(), first[4].track),
            (Track::SameBudget, "q01", Track::Unlimited)
        );
        assert_eq!((first[0].max_tokens, first[4].max_tokens), (8192, 8192));
        assert_eq!(
            (first[0].prompt_tokens, first[0].completion_tokens, first[0].finish_reason.as_deref()),
            (100, 50, Some("stop"))
        );
        assert_eq!(first[0].host.as_deref(), Some("Host A"));
        // a host's name that does not fit is left out, not kept
        let odd = one(&answering(200, r#"{"provider":"Host\nB","choices":[{"message":{"content":"ANSWER: 1"}}]}"#));
        assert_eq!((odd.host, odd.error), (None, None));
    }

    #[test]
    fn prices_come_from_the_list_and_must_be_fixed() {
        let world = provider(&exam());
        let pricing = fetch_pricing(&world, &names(&["a/good", "b/bad"]), 0).unwrap();
        assert_eq!(
            pricing["a/good"],
            Pricing { prompt: 100_000, cache_write: 0, completion: 400_000, request: 0, max_completion: Some(8192) }
        );
        assert_eq!(pricing["b/bad"].max_completion, None);
        let refused = |model: &str| format!("{:#}", fetch_pricing(&world, &names(&[model]), 0).unwrap_err());
        assert!(refused("nobody/knows").contains("no model named nobody/knows"));
        assert!(refused("d/auto").contains("d/auto has no fixed prompt price"));
        assert!(refused("f/odd").contains("f/odd has no fixed request price"));
        assert!(refused("g/bare").contains("g/bare lists no completion price"));
        // More decimals than a trillionth: rounded up when they are digits, refused when they are not.
        let fine = fetch_pricing(&world, &names(&["h/fine"]), 0).unwrap()["h/fine"];
        assert_eq!((fine.prompt, fine.cache_write), (100_000, 83_334));
        assert!(refused("i/junk").contains("i/junk has no fixed input_cache_write price"));
        world.page("/models", 503, "");
        assert!(fetch_pricing(&world, &names(&["a/good"]), 0).is_err());
        world.page("/models", 200, "<html>");
        assert!(fetch_pricing(&world, &names(&["a/good"]), 0).is_err());
    }

    #[test]
    fn a_model_with_several_prices_is_charged_at_the_dearest_that_can_apply() {
        let world = provider(&exam());
        let peak = |longest: u64| fetch_pricing(&world, &names(&["e/peak"]), longest).unwrap()["e/peak"];
        // The dearer hours count at any hour, thinking tokens are dearer than the rest, writing the prompt to a
        // cache is dearer still, and each request has a fee. Of the two prices for long prompts, only the one a
        // prompt of 5000 tokens can reach counts.
        assert_eq!(
            peak(5000),
            Pricing {
                prompt: 300_000,
                cache_write: 375_000,
                completion: 500_000,
                request: 1_000_000_000,
                max_completion: None
            }
        );
        assert_eq!((peak(999).prompt, peak(1000).prompt), (200_000, 300_000));
        assert_eq!(peak(272_000).prompt, 1_000_000);
    }

    fn one(world: &Scripted) -> AnswerRecord {
        let exam = exam();
        let models = names(&["a/good"]);
        let plan = Plan {
            exam: &exam,
            models: &models,
            tracks: &[Track::Unlimited],
            budget: BUDGET,
            effort: "low",
            concurrency: 1,
            api_key: "k",
        };
        let price = Pricing { prompt: 100_000, cache_write: 0, completion: 400_000, request: 0, max_completion: None };
        ask(world, &plan, "a/good", Track::Unlimited, &exam.questions[0], &price)
    }

    fn answering(status: u16, body: &'static str) -> Scripted {
        Scripted { on_post: Box::new(move |_, _, _| Ok((status, body.to_string()))), ..Scripted::at(0) }
    }

    #[test]
    fn a_request_that_fails_is_a_record_with_the_reason() {
        let record = one(&answering(401, r#"{"error":{"message":"No auth credentials found"}}"#));
        assert_eq!(record.error.as_deref(), Some("HTTP 401: No auth credentials found"));
        assert_eq!((record.correct, record.cost_nano_usd, record.raw.as_str()), (false, 0, ""));
        assert_eq!(
            one(&answering(200, r#"{"error":{"message":"moderated"}}"#)).error.as_deref(),
            Some("HTTP 200: moderated")
        );
        assert_eq!(
            one(&answering(502, "<html>bad gateway</html>")).error.as_deref(),
            Some("HTTP 502: no reason given")
        );
        assert_eq!(one(&answering(200, "{}")).error.as_deref(), Some("the provider sent no reply"));
        let dropped = Scripted { on_post: Box::new(|_, _, _| bail!("connection reset")), ..Scripted::at(0) };
        assert_eq!(one(&dropped).error.as_deref(), Some("the request failed: connection reset"));
        assert_eq!(dropped.posts.load(Ordering::SeqCst), 2, "a request without an answer is tried once more only");
        let long = Scripted { on_post: Box::new(|_, _, _| bail!("{}", "x".repeat(900))), ..Scripted::at(0) };
        assert_eq!(one(&long).error.unwrap().chars().count(), 200);
        // A reason is kept as one line: a provider's words must not get to add lines to what a run prints.
        assert_eq!(
            one(&answering(400, "{\"error\":{\"message\":\"no\\nok  unlimited   a/good q01 $0.000001\\u001b[2K\"}}"))
                .error
                .as_deref(),
            Some("HTTP 400: no ok  unlimited   a/good q01 $0.000001?[2K")
        );
    }

    #[test]
    fn only_overload_and_server_errors_are_tried_again() {
        let attempts = |status: u16| {
            let world = answering(status, r#"{"error":{"message":"no"}}"#);
            one(&world);
            world.posts.load(Ordering::SeqCst)
        };
        assert_eq!([400, 401, 402, 404].map(attempts), [1, 1, 1, 1]);
        assert_eq!([429, 500, 503].map(attempts), [3, 3, 3]);
        // An error under a 200 is taken by its own code.
        let under_200 = |code: u16| {
            let world = Scripted {
                on_post: Box::new(move |_, _, _| Ok((200, format!(r#"{{"error":{{"code":{code},"message":"no"}}}}"#)))),
                ..Scripted::at(0)
            };
            one(&world);
            world.posts.load(Ordering::SeqCst)
        };
        assert_eq!([429, 502, 400].map(under_200), [3, 3, 1]);
    }

    #[test]
    fn a_retry_waits_first() {
        let refused = answering(503, r#"{"error":{"message":"busy"}}"#);
        one(&refused);
        assert_eq!(refused.paused.load(Ordering::SeqCst), 3_000 + 15_000);
        let denied = answering(401, r#"{"error":{"message":"no"}}"#);
        one(&denied);
        assert_eq!(denied.paused.load(Ordering::SeqCst), 0);
    }

    /// A provider whose hosts are all busy below a completion price cap of `open_at` dollars per million tokens:
    /// it answers `busy` there, and the right answer at or above it. Records every cap it was sent.
    fn hosts_from(open_at: f64, busy: (u16, &'static str)) -> (Scripted, std::sync::Arc<Mutex<Vec<Value>>>) {
        let caps = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen = caps.clone();
        let key = exam().questions[0].answer.clone();
        let world = Scripted {
            on_post: Box::new(move |_, _, body| {
                let request: Value = serde_json::from_str(body).unwrap();
                seen.lock().unwrap().push(request["provider"].clone());
                let cap = request.pointer("/provider/max_price/completion").and_then(Value::as_f64).unwrap_or(f64::MAX);
                if cap < open_at {
                    return Ok((busy.0, busy.1.to_string()));
                }
                let reply = json!({
                    "choices": [{"message": {"content": format!("ANSWER: {key}")}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 100, "completion_tokens": 50, "cost": 0.001},
                });
                Ok((200, reply.to_string()))
            }),
            ..Scripted::at(0)
        };
        (world, caps)
    }

    fn same_budget(world: &Scripted, price: &Pricing) -> AnswerRecord {
        let exam = exam();
        let models = names(&["a/good"]);
        let plan = Plan {
            exam: &exam,
            models: &models,
            tracks: &[Track::SameBudget],
            budget: BUDGET,
            effort: "high",
            concurrency: 1,
            api_key: "k",
        };
        ask(world, &plan, "a/good", Track::SameBudget, &exam.questions[0], price)
    }

    #[test]
    fn a_busy_cheapest_host_raises_the_cap_and_never_the_bill() {
        // $0.10 and $0.40 per million tokens; the only hosts that are free charge $0.90 for output.
        let price = Pricing { prompt: 100_000, cache_write: 0, completion: 400_000, request: 0, max_completion: None };
        let (world, caps) = hosts_from(0.9, (429, r#"{"error":{"message":"Provider returned error"}}"#));
        let record = same_budget(&world, &price);
        assert!(record.correct && record.error.is_none(), "{record:?}");
        // Answered at the third price, 2.25 times the list, with the tokens the budget buys there.
        let bytes = SYSTEM.len() + exam().questions[0].prompt.len();
        assert_eq!(record.max_tokens, same_budget_max_tokens(BUDGET, bytes, &raised(&price, 2)));
        assert!(record.max_tokens < same_budget_max_tokens(BUDGET, bytes, &price));
        let completions: Vec<f64> =
            caps.lock().unwrap().iter().map(|p| p["max_price"]["completion"].as_f64().unwrap()).collect();
        assert_eq!(completions, [0.4, 0.4, 0.4, 0.6, 0.6, 0.6, 0.9], "three tries at each price that was too low");
        assert_eq!(world.paused.load(Ordering::SeqCst), 2 * (3_000 + 15_000));
        // At every price, the tokens it allows and the prompt together stay inside the budget.
        for step in 0..=PRICE_STEPS {
            let at = raised(&price, step);
            let allowed = u128::from(same_budget_max_tokens(BUDGET, bytes, &at));
            let bill =
                u128::from(most_prompt_tokens(bytes)) * u128::from(at.prompt) + allowed * u128::from(at.completion);
            assert!(bill <= u128::from(BUDGET) * 1000, "step {step}");
        }
    }

    #[test]
    fn a_cap_no_host_meets_is_raised_at_once() {
        let price = Pricing { prompt: 100_000, cache_write: 0, completion: 400_000, request: 0, max_completion: None };
        let no_host = r#"{"error":{"message":"No endpoints found that satisfy the max price for this request"}}"#;
        let (world, caps) = hosts_from(0.9, (404, no_host));
        assert!(same_budget(&world, &price).correct);
        assert_eq!(caps.lock().unwrap().len(), 3, "one try at each price");
        assert_eq!(world.paused.load(Ordering::SeqCst), 0);
        // With no host at any price, the reason says how far the cap went.
        let (world, caps) = hosts_from(f64::MAX, (404, no_host));
        let record = same_budget(&world, &price);
        assert_eq!(caps.lock().unwrap().len(), PRICE_STEPS + 1);
        assert_eq!(
            record.error.as_deref(),
            Some(
                "HTTP 404: No endpoints found that satisfy the max price for this request, with the price cap raised as far as 3.375 times the listed price"
            )
        );
        assert_eq!((record.correct, record.cost_nano_usd), (false, 0));
        // A price the budget cannot pay is not tried: here the second step would leave too few tokens.
        let dear = Pricing { completion: 60_000_000, ..price };
        let (world, caps) = hosts_from(f64::MAX, (404, no_host));
        let record = same_budget(&world, &dear);
        assert_eq!(caps.lock().unwrap().len(), 1);
        assert_eq!(
            record.error.as_deref(),
            Some("HTTP 404: No endpoints found that satisfy the max price for this request")
        );
        // Other refusals are not a matter of price.
        let (world, caps) = hosts_from(f64::MAX, (402, r#"{"error":{"message":"Insufficient credits"}}"#));
        assert_eq!(same_budget(&world, &price).error.as_deref(), Some("HTTP 402: Insufficient credits"));
        assert_eq!(caps.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_free_model_is_not_stepped_for_nothing() {
        let free = Pricing { prompt: 0, cache_write: 0, completion: 0, request: 0, max_completion: Some(8192) };
        let (world, caps) = hosts_from(f64::MAX, (429, r#"{"error":{"message":"Provider returned error"}}"#));
        let record = same_budget(&world, &free);
        assert_eq!(caps.lock().unwrap().len(), 3, "the tries at its one price, and no more");
        assert_eq!(record.error.as_deref(), Some("HTTP 429: Provider returned error"));
        assert_eq!(record.max_tokens, 8192);
    }

    #[test]
    fn prices_are_raised_by_half_and_rounded_up() {
        let price = Pricing { prompt: 3, cache_write: 0, completion: 400_000, request: 1, max_completion: Some(7) };
        assert_eq!(raised(&price, 0), price);
        assert_eq!(
            raised(&price, 3),
            Pricing { prompt: 11, cache_write: 0, completion: 1_350_000, request: 4, max_completion: Some(7) }
        );
        assert_eq!(raised(&Pricing { prompt: u64::MAX, ..price }, 3).prompt, u64::MAX);
        assert_eq!(STEP_FACTORS.len(), PRICE_STEPS + 1);
    }

    #[test]
    fn the_same_budget_track_caps_the_price_it_was_worked_out_at() {
        let exam = exam();
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        let world = Scripted {
            on_post: Box::new(move |_, _, body| {
                kept.lock().unwrap().push(serde_json::from_str::<Value>(body).unwrap());
                Ok((200, r#"{"choices":[{"message":{"content":"ANSWER: 1"}}]}"#.into()))
            }),
            ..Scripted::at(0)
        };
        let models = names(&["a/good"]);
        let plan = Plan {
            exam: &exam,
            models: &models,
            tracks: &Track::ALL,
            budget: BUDGET,
            effort: "high",
            concurrency: 1,
            api_key: "k",
        };
        // $0.10 and $0.40 per million tokens, given per token in trillionths of a dollar, and no fee.
        let price = Pricing { prompt: 100_000, cache_write: 0, completion: 400_000, request: 0, max_completion: None };
        for track in Track::ALL {
            ask(&world, &plan, "a/good", track, &exam.questions[0], &price);
        }
        let seen = seen.lock().unwrap();
        let cap = json!({"prompt": 0.1, "completion": 0.4, "request": 0.0});
        assert_eq!(seen[0]["provider"], json!({"sort": "price", "max_price": cap}));
        assert!(
            seen[0].to_string().contains(r#""max_price":{"completion":0.4,"prompt":0.1,"request":0.0}"#),
            "{}",
            seen[0]
        );
        // The unlimited track has no budget to protect, and asks for the cheapest host all the same.
        assert_eq!(seen[1]["provider"], json!({"sort": "price"}));
        // A fee of a tenth of a cent a request goes out in dollars.
        assert_eq!(number(1_000_000_000, PRICE_DECIMALS), 0.001);
        assert_eq!(dollars_per_million(15_000_000), 15.0);
        assert_eq!(dollars_per_million(37_500), 0.0375);
        assert_eq!(dollars_per_million(0), 0.0);
    }

    #[test]
    fn an_empty_reply_is_a_wrong_answer_not_an_error() {
        let record = one(&answering(
            200,
            r#"{"choices":[{"message":{"content":null},"finish_reason":"length"}],"usage":{"prompt_tokens":90,"completion_tokens":2000}}"#,
        ));
        assert_eq!((record.error, record.correct, record.finish_reason.as_deref()), (None, false, Some("length")));
        // No bill in the reply: the price list is used. 90 * 100000 + 2000 * 400000 trillionths.
        assert_eq!(record.cost_nano_usd, 809_000);
    }

    #[test]
    fn a_reply_that_broke_off_is_an_error_with_what_it_cost() {
        let record = one(&answering(
            200,
            r#"{"choices":[{"message":{"content":"ANSWER: 4"},"finish_reason":"error","error":{"code":502,"message":"upstream died"}}],"usage":{"prompt_tokens":90,"completion_tokens":20,"cost":0.00001}}"#,
        ));
        assert_eq!(record.error.as_deref(), Some("the reply broke off: upstream died"));
        assert_eq!((record.correct, record.cost_nano_usd, record.completion_tokens), (false, 10_000, 20));
        assert_eq!((record.extracted.as_str(), record.finish_reason.as_deref()), ("4", Some("error")));
        let bare = one(&answering(200, r#"{"choices":[{"message":{"content":"x"},"finish_reason":"error"}]}"#));
        assert_eq!(bare.error.as_deref(), Some("the reply broke off: no reason given"));
        // Such a record grades the way the engine grades it again: no answer.
        let question = &exam().questions[0];
        assert_eq!(
            score(question, Track::Unlimited, &record.raw, record.cost_nano_usd, true, BUDGET),
            ("4".into(), false)
        );
    }

    #[test]
    fn money_converts_without_drifting() {
        assert_eq!(nano(0.001), Some(1_000_000));
        assert_eq!(nano(0.0000123456), Some(12_346));
        assert_eq!(nano(0.0), Some(0));
        assert_eq!([nano(-0.01), nano(f64::NAN), nano(f64::INFINITY), nano(1e12)], [None; 4]);
        let price =
            |prompt, completion, request| Pricing { prompt, cache_write: 0, completion, request, max_completion: None };
        assert_eq!(list_cost(1, 0, &price(1, 0, 0)), 1);
        assert_eq!(list_cost(1000, 1000, &price(1, 1, 0)), 2);
        assert_eq!(list_cost(1000, 1000, &price(1, 1, 5000)), 7);
        assert_eq!(list_cost(u64::MAX, u64::MAX, &price(u64::MAX, u64::MAX, u64::MAX)), u64::MAX);
        assert_eq!(dollars(1_234_567_890), "$1.234567");
        assert_eq!(dollars(999), "$0.000000");
    }

    fn record(model: &str, track: Track, raw: &str) -> AnswerRecord {
        AnswerRecord {
            model: model.into(),
            track,
            question_id: "q01".into(),
            raw: raw.into(),
            extracted: raw.into(),
            correct: true,
            cost_nano_usd: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            max_tokens: 1,
            latency_ms: 0,
            finish_reason: None,
            host: None,
            error: None,
        }
    }

    #[test]
    fn a_later_run_replaces_only_the_pairs_it_covers() {
        let file = |model: &str, records: Vec<AnswerRecord>| AnswerFile {
            format: FORMAT,
            exam_id: "e".into(),
            model: model.into(),
            records,
        };
        let before = vec![
            file("a", vec![record("a", Track::SameBudget, "old"), record("a", Track::Unlimited, "old")]),
            file("b", vec![record("b", Track::SameBudget, "old"), record("b", Track::Unlimited, "old")]),
        ];
        let after = vec![
            file("b", vec![record("b", Track::SameBudget, "new")]),
            file("c", vec![record("c", Track::SameBudget, "new")]),
        ];
        let merged = merge_answers("e", before, after);
        let seen: Vec<(String, Vec<String>)> = merged
            .iter()
            .map(|f| (f.model.clone(), f.records.iter().map(|r| format!("{}:{}", r.track.name(), r.raw)).collect()))
            .collect();
        assert_eq!(
            seen,
            [
                ("a".to_string(), vec!["same-budget:old".to_string(), "unlimited:old".to_string()]),
                ("b".to_string(), vec!["same-budget:new".to_string(), "unlimited:old".to_string()]),
                ("c".to_string(), vec!["same-budget:new".to_string()]),
            ]
        );
    }

    #[test]
    fn a_model_that_could_not_be_reached_is_set_aside_not_marked_wrong() {
        let failed = |model: &str, track: Track, max_tokens: u32| AnswerRecord {
            correct: false,
            max_tokens,
            error: Some(format!("HTTP 401: no key for {model}")),
            ..record(model, track, "")
        };
        let file = |model: &str, records: Vec<AnswerRecord>| AnswerFile {
            format: FORMAT,
            exam_id: "e".into(),
            model: model.into(),
            records,
        };
        let (same, free) = (Track::SameBudget, Track::Unlimited);
        let run = vec![
            // Reached on one track only.
            file("a", vec![failed("a", same, 900), failed("a", same, 900), record("a", free, "1")]),
            // One answer among failures is an answer.
            file("b", vec![failed("b", free, 900), record("b", free, "1")]),
            // Never asked, because the budget buys nothing from it: that stands as its result.
            file("c", vec![failed("c", same, 0), failed("c", same, 0)]),
            // Not reached at all.
            file("d", vec![failed("d", same, 0), failed("d", same, 900), failed("d", free, 900)]),
        ];
        let (kept, lost) = set_aside_unanswered(run);
        let shape: Vec<(&str, Vec<Track>)> =
            kept.iter().map(|f| (f.model.as_str(), f.records.iter().map(|r| r.track).collect())).collect();
        assert_eq!(shape, [("a", vec![free]), ("b", vec![free, free]), ("c", vec![same, same])]);
        assert_eq!(
            lost,
            [
                ("a".to_string(), same, "HTTP 401: no key for a".to_string()),
                ("d".to_string(), same, "HTTP 401: no key for d".to_string()),
                ("d".to_string(), free, "HTTP 401: no key for d".to_string()),
            ]
        );
        // What was recorded before for a pair that was set aside stays as it was.
        let before = vec![file("a", vec![record("a", same, "old"), record("a", free, "old")])];
        let merged = merge_answers("e", before, kept);
        let a: Vec<String> = merged[0].records.iter().map(|r| format!("{}:{}", r.track.name(), r.raw)).collect();
        assert_eq!(a, ["same-budget:old", "unlimited:1"]);
    }

    fn terms(exam: &Exam) -> Terms {
        Terms {
            exam_id: exam.id.clone(),
            plaintext_sha256: "0xabc".into(),
            budget_nano_usd: BUDGET,
            effort: "high".into(),
        }
    }

    #[test]
    fn a_run_that_was_cut_short_is_taken_up_where_it_stopped() {
        let exam = exam();
        let world = provider(&exam);
        let models = names(&["a/good", "b/bad"]);
        let pricing = fetch_pricing(&world, &models, 0).unwrap();
        let plan = Plan {
            exam: &exam,
            models: &models,
            tracks: &[Track::Unlimited],
            budget: BUDGET,
            effort: "high",
            concurrency: 1,
            api_key: "test-key",
        };
        let terms = terms(&exam);
        assert!(journal_answers("", &terms, &plan).unwrap().is_empty());
        assert!(journal_answers("{\"examId\":\"runner-te", &terms, &plan).unwrap().is_empty(), "half a first line");

        let (mut journal, mut log) = (journal_start(&terms, &[]).unwrap().into_bytes(), Vec::new());
        let whole = run_exam(&world, &plan, &pricing, Vec::new(), &mut journal, &mut log);
        let journal = String::from_utf8(journal).unwrap();
        let lines: Vec<&str> = journal.lines().collect();
        assert_eq!(lines.len(), 1 + 8);

        // Cut short with three answers in and half the line of a fourth. A request that failed is in it too.
        let failed = serde_json::to_string(&one(&answering(503, "{}"))).unwrap();
        let cut = format!("{}\n{failed}\n{}", lines[..4].join("\n"), &lines[4][..lines[4].len() / 2]);
        let done = journal_answers(&cut, &terms, &plan).unwrap();
        assert_eq!(done.iter().map(|r| r.question_id.as_str()).collect::<Vec<_>>(), ["q01", "q02", "q03"]);

        let asked = world.posts.load(Ordering::SeqCst);
        let (mut journal, mut log) = (journal_start(&terms, &done).unwrap().into_bytes(), Vec::new());
        let again = run_exam(&world, &plan, &pricing, done, &mut journal, &mut log);
        assert_eq!(world.posts.load(Ordering::SeqCst), asked + 5, "only what was missing is asked for");
        assert_eq!(String::from_utf8(log).unwrap().lines().count(), 5);
        assert_eq!(again, whole);
        let journal = String::from_utf8(journal).unwrap();
        assert_eq!(journal_answers(&journal, &terms, &plan).unwrap().len(), 8);

        // With every answer in, nothing is asked at all.
        let done = journal_answers(&journal, &terms, &plan).unwrap();
        assert_eq!(run_exam(&world, &plan, &pricing, done, &mut Vec::new(), &mut Vec::new()), whole);
        assert_eq!(world.posts.load(Ordering::SeqCst), asked + 5);
    }

    #[test]
    fn a_journal_that_does_not_fit_the_run_is_refused() {
        let exam = exam();
        let models = names(&["a/good"]);
        let plan = Plan {
            exam: &exam,
            models: &models,
            tracks: &[Track::Unlimited],
            budget: BUDGET,
            effort: "high",
            concurrency: 1,
            api_key: "k",
        };
        let terms = terms(&exam);
        let right = AnswerRecord {
            model: "a/good".into(),
            raw: format!("ANSWER: {}", exam.questions[0].answer),
            extracted: exam.questions[0].answer.clone(),
            ..record("a/good", Track::Unlimited, "")
        };
        let refused = |terms_line: &Terms, records: &[AnswerRecord]| {
            let text = journal_start(terms_line, records).unwrap();
            format!("{:#}", journal_answers(&text, &terms, &plan).unwrap_err())
        };
        assert_eq!(
            journal_answers(&journal_start(&terms, std::slice::from_ref(&right)).unwrap(), &terms, &plan)
                .unwrap()
                .len(),
            1
        );

        let other = Terms { plaintext_sha256: "0xdef".into(), ..terms.clone() };
        assert!(refused(&other, &[]).contains("it belongs to another exam, runner-test"));
        let dearer = Terms { budget_nano_usd: 50_000_000, effort: "low".into(), ..terms.clone() };
        assert!(refused(&dearer, &[]).contains("it was started with $0.05 a question at low effort"));
        let elsewhere = AnswerRecord { model: "b/bad".into(), ..right.clone() };
        assert!(refused(&terms, &[elsewhere]).contains("answers of b/bad on the unlimited track, which this run does"));
        let other_track = AnswerRecord { track: Track::SameBudget, ..right.clone() };
        assert!(refused(&terms, &[other_track]).contains("answers of a/good on the same-budget track"));
        let unknown = AnswerRecord { question_id: "q99".into(), ..right.clone() };
        assert!(refused(&terms, &[unknown]).contains("an answer to q99, which is not a question of the exam"));
        let remarked = AnswerRecord { correct: false, ..right.clone() };
        assert!(
            refused(&terms, &[remarked]).contains("the mark it gives a/good on q01 is not what the reply grades to")
        );
        assert!(refused(&terms, &[right.clone(), right.clone()]).contains("two answers of a/good to q01"));

        let start = journal_start(&terms, &[]).unwrap();
        let problem = |text: &str| format!("{:#}", journal_answers(text, &terms, &plan).unwrap_err());
        assert!(problem(&format!("{start}not json\n")).contains("one of its lines is not an answer"));
        assert!(problem("not json\n").contains("its first line is not the terms of a run"));
        assert!(problem(&format!("{{\"examId\":\"x\"}}\n{start}")).contains("its first line is not the terms"));
    }
}
