//! The ignot engine for browsers.
//!
//! Everything here runs in the visitor's own browser: nothing is sent to a server to be sealed, opened or
//! checked. The page fetches what the engine needs from the outside (a drand relay's answer, a transaction
//! receipt) and hands it over as text; the engine trusts none of it and verifies all of it.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use ignot_core::drand::{self, Beacon};
use ignot_core::seal::Manifest;
use ignot_core::verify::{Evidence, Receipts};
use ignot_core::{chain, seal, timelock, verify};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

fn error(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// A JavaScript number as a whole number. JavaScript counts exactly only up to 2^53.
fn whole(n: f64, what: &str) -> Result<u64, JsError> {
    if n.is_finite() && n.fract() == 0.0 && (0.0..=9_007_199_254_740_992.0).contains(&n) {
        // The cast cannot lose anything: the value is whole and inside the range.
        Ok(n as u64)
    } else {
        Err(JsError::new(&format!("{what} must be a whole number")))
    }
}

/// The first drand round published at or after a time in Unix milliseconds.
#[wasm_bindgen(js_name = roundAtOrAfter)]
pub fn round_at_or_after(ms: f64) -> Result<f64, JsError> {
    Ok(drand::round_at_or_after(whole(ms, "the time")?) as f64)
}

/// Unix milliseconds at which a drand round's signature is published.
#[wasm_bindgen(js_name = roundTime)]
pub fn round_time(round: f64) -> Result<f64, JsError> {
    Ok(drand::round_time(whole(round, "the round")?).map_err(error)? as f64)
}

/// Where to fetch a round's signature: one address per public relay, in the order to try them.
#[wasm_bindgen(js_name = relayUrls)]
pub fn relay_urls(round: f64) -> Result<Vec<String>, JsError> {
    let path = drand::round_path(whole(round, "the round")?);
    Ok(drand::RELAYS.iter().map(|relay| format!("{relay}{path}")).collect())
}

/// Whether a relay's answer carries the true signature of a round. Relays are not trusted: the page asks them
/// in turn and keeps the first answer that passes.
#[wasm_bindgen(js_name = beaconVerifies)]
#[must_use]
pub fn beacon_verifies(round: f64, beacon: &str) -> bool {
    whole(round, "the round").is_ok_and(|round| Beacon::parse(round, beacon).is_ok())
}

/// A public node of a chain, to read transaction receipts from; nothing for a chain this engine does not know.
#[wasm_bindgen(js_name = publicNode)]
#[must_use]
pub fn public_node(chain_id: f64) -> Option<String> {
    chain::public_node(whole(chain_id, "the chain id").ok()?).map(String::from)
}

/// Seals bytes to a drand round. Returns the sealed file as text.
#[wasm_bindgen]
pub fn seal(data: &[u8], round: f64) -> Result<String, JsError> {
    timelock::seal_bytes(data, whole(round, "the round")?).map_err(error)
}

/// The drand round a sealed file is locked to.
#[wasm_bindgen(js_name = sealedRound)]
pub fn sealed_round(sealed: &[u8]) -> Result<f64, JsError> {
    Ok(timelock::sealed_round(sealed).map_err(error)? as f64)
}

/// Opens a sealed file. `beacon` is what a relay answered for the file's round, or the bare signature in hex;
/// it is checked against the chain's public key before anything is opened.
#[wasm_bindgen]
pub fn open(sealed: &[u8], beacon: &str) -> Result<Vec<u8>, JsError> {
    let round = timelock::sealed_round(sealed).map_err(error)?;
    let beacon = Beacon::parse(round, beacon).map_err(error)?;
    timelock::open_bytes(sealed, &beacon).map_err(error)
}

/// Opens a sealed exam and returns its exact text. The text must have the hash the manifest promised.
#[wasm_bindgen(js_name = openExam)]
pub fn open_exam(sealed: &[u8], beacon: &str, manifest: &str) -> Result<String, JsError> {
    let manifest = Manifest::parse(manifest).map_err(error)?;
    let beacon = Beacon::parse(manifest.open_round, beacon).map_err(error)?;
    Ok(seal::open_exam(sealed, &beacon, Some(&manifest)).map_err(error)?.1)
}

/// What the page gathered about an exam, apart from the sealed file itself.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Gathered {
    /// `manifest.json`, as text.
    manifest: String,
    /// The time now, in Unix milliseconds.
    now_ms: u64,
    /// What a relay answered for the manifest's round.
    beacon: Option<String>,
    /// `exam.json`, as text.
    exam: Option<String>,
    /// Every answer file, as text.
    answers: Option<Vec<String>>,
    /// `results.json`, as text.
    results: Option<String>,
    /// Receipts from a chain node, as text, when the chain was read.
    chain: Option<Chain>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chain {
    seal: Option<String>,
    publish: Option<String>,
}

/// Checks a published exam. `gathered` is JSON: `{manifest, nowMs, beacon?, exam?, answers?, results?, chain?}`,
/// every file given as its text. Returns the report as JSON.
#[wasm_bindgen]
pub fn verify(gathered: &str, sealed: &[u8]) -> Result<String, JsError> {
    report(gathered, sealed).map_err(error)
}

fn report(gathered: &str, sealed: &[u8]) -> ignot_core::Result<String> {
    let gathered: Gathered = serde_json::from_str(gathered)?;
    let evidence = Evidence {
        manifest: &gathered.manifest,
        ciphertext: sealed,
        now_ms: gathered.now_ms,
        beacon: gathered.beacon.as_deref(),
        exam: gathered.exam.as_deref(),
        answers: gathered.answers.as_ref().map(|files| files.iter().map(String::as_str).collect()),
        results: gathered.results.as_deref(),
        chain: gathered
            .chain
            .as_ref()
            .map(|chain| Receipts { seal: chain.seal.as_deref(), publish: chain.publish.as_deref() }),
        // The record contract comes from the engine's source, never from the page.
        record: chain::RECORD,
    };
    Ok(serde_json::to_string(&verify::verify_exam(&evidence)?)?)
}

#[cfg(test)]
mod tests {
    use ignot_core::exam::{Recipe, make_exam, parse_mix};
    use serde_json::{Value, json};

    use super::*;

    const SIGNATURE: &str =
        "b44679b9a59af2ec876b1a6b1ad52ea9b1615fc3982b19576350f93447cb1125e342b73a8dd2bacbe47e4b6b63ed5e39";

    fn sealed_exam() -> (String, String, String) {
        let recipe = Recipe {
            id: "page-test".into(),
            title: "Page test".into(),
            created_at: "2026-11-22T00:00:00.000Z".into(),
            commit: "abc1234".into(),
            seed: "a seed for the tests of the page's engine".into(),
            mix: parse_mix("queue:1,robot:1").unwrap(),
        };
        let exam = make_exam(&recipe).unwrap();
        let (sealed, manifest) = seal::seal_exam(&exam, 1000).unwrap();
        (sealed, serde_json::to_string(&manifest).unwrap(), exam.bytes().unwrap())
    }

    #[test]
    fn a_report_comes_back_as_json() {
        let (sealed, manifest, exam) = sealed_exam();
        let closed: Value = serde_json::from_str(
            &report(&json!({"manifest": manifest, "nowMs": 0}).to_string(), sealed.as_bytes()).unwrap(),
        )
        .unwrap();
        assert_eq!((closed["ok"].as_bool(), closed["state"].as_str()), (Some(true), Some("sealed")));
        let gathered = json!({"manifest": manifest, "nowMs": 0, "beacon": SIGNATURE, "exam": exam, "chain": {}});
        let open: Value = serde_json::from_str(&report(&gathered.to_string(), sealed.as_bytes()).unwrap()).unwrap();
        assert_eq!((open["ok"].as_bool(), open["state"].as_str()), (Some(true), Some("open")));
        assert_eq!(open["exam"]["questions"].as_array().map(Vec::len), Some(2));
        assert!(open["checks"].as_array().unwrap().iter().all(|check| check["name"].is_string()));
    }

    #[test]
    fn what_the_page_gathers_is_read_strictly() {
        let (sealed, manifest, _) = sealed_exam();
        for bad in [
            json!({"manifest": manifest}),
            json!({"manifest": manifest, "nowMs": -1}),
            json!({"manifest": manifest, "nowMs": 0, "extra": 1}),
            json!({"manifest": 5, "nowMs": 0}),
            json!({"manifest": "{}", "nowMs": 0}),
        ] {
            assert!(report(&bad.to_string(), sealed.as_bytes()).is_err(), "{bad}");
        }
        assert!(report("", sealed.as_bytes()).is_err());
    }
}
