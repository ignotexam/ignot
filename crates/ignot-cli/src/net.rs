//! Everything outside this process: the clock, drand's relays, a chain node, the model provider.
//!
//! Commands reach the outside only through [`World`], so the tests can run them against a scripted one.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use ignot_core::chain::public_node;
use ignot_core::drand::{self, Beacon};
use serde_json::{Value, json};

/// What a command can ask of the outside.
pub trait World: Sync {
    /// The time now, in Unix milliseconds.
    fn now_ms(&self) -> u64;
    /// Waits.
    fn pause(&self, time: Duration);
    /// Fetches a URL. Returns the status and the body.
    fn get(&self, url: &str) -> Result<(u16, String)>;
    /// Posts JSON, with a bearer token when given. Returns the status and the body.
    fn post(&self, url: &str, bearer: Option<&str>, body: &str, timeout: Duration) -> Result<(u16, String)>;
}

/// The real outside.
pub struct Internet {
    agent: ureq::Agent,
}

impl Internet {
    pub fn new() -> Self {
        let config = ureq::Agent::config_builder()
            // Statuses are read here, with their bodies; a redirect is never followed, so a token is never
            // sent anywhere but the address it was meant for.
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(concat!("ignot/", env!("CARGO_PKG_VERSION")))
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        Self { agent: config.into() }
    }
}

impl World for Internet {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
    }

    fn pause(&self, time: Duration) {
        std::thread::sleep(time);
    }

    fn get(&self, url: &str) -> Result<(u16, String)> {
        let mut response = self.agent.get(url).call()?;
        Ok((response.status().as_u16(), response.body_mut().read_to_string()?))
    }

    fn post(&self, url: &str, bearer: Option<&str>, body: &str, timeout: Duration) -> Result<(u16, String)> {
        let mut request =
            self.agent.post(url).config().timeout_global(Some(timeout)).build().content_type("application/json");
        if let Some(token) = bearer {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let mut response = request.send(body)?;
        Ok((response.status().as_u16(), response.body_mut().read_to_string()?))
    }
}

/// A relay's answer for a round that carries the round's true signature.
///
/// Relays are asked in turn and none is trusted: an answer counts only if its signature verifies against the
/// chain's public key. Right after a round's time the relays may not have it yet, so this waits a little.
pub fn fetch_beacon(world: &dyn World, round: u64) -> Result<String> {
    let published = drand::round_time(round)?;
    let mut problems = Vec::new();
    for attempt in 0..5 {
        problems.clear();
        for relay in drand::RELAYS {
            match world.get(&format!("{relay}{}", drand::round_path(round))) {
                Ok((200, body)) if Beacon::parse(round, &body).is_ok() => return Ok(body),
                Ok((200, _)) => {
                    problems.push(format!("{relay} answered with something that is not the round's signature"))
                }
                Ok((status, _)) => problems.push(format!("{relay} answered {status}")),
                Err(e) => problems.push(format!("{relay}: {e}")),
            }
        }
        if attempt == 4 || world.now_ms() > published.saturating_add(60_000) {
            break;
        }
        world.pause(Duration::from_secs(3));
    }
    bail!("no drand relay gave round {round}: {}", problems.join("; "))
}

/// A relay's answer with the round's true signature, if one has it now: each relay is asked once.
///
/// This is how a command learns that drand has published a round whatever the clock on this machine says. An
/// answer that carries the true signature is proof that it has. Anything else, a relay that cannot be reached
/// included, proves nothing.
pub fn peek_beacon(world: &dyn World, round: u64) -> Option<String> {
    drand::RELAYS.iter().find_map(|relay| {
        let (status, body) = world.get(&format!("{relay}{}", drand::round_path(round))).ok()?;
        (status == 200 && Beacon::parse(round, &body).is_ok()).then_some(body)
    })
}

/// One call to a chain node. Returns the `result` of the answer.
fn rpc_call(world: &dyn World, rpc: &str, method: &str, params: Value) -> Result<Value> {
    let request = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let (status, body) = world
        .post(rpc, None, &request.to_string(), Duration::from_secs(20))
        .with_context(|| format!("the node at {rpc} did not answer"))?;
    if status != 200 {
        bail!("the node at {rpc} answered {status}");
    }
    let mut answer: Value =
        serde_json::from_str(&body).with_context(|| format!("the node at {rpc} did not answer with JSON"))?;
    if let Some(error) = answer.get("error").filter(|e| !e.is_null()) {
        bail!("the node at {rpc} refused {method}: {error}");
    }
    answer
        .get_mut("result")
        .map(Value::take)
        .ok_or_else(|| anyhow!("the node at {rpc} answered {method} without a result"))
}

/// A reader of transaction receipts on one chain.
pub struct Node<'a> {
    world: &'a dyn World,
    rpc: String,
}

impl<'a> Node<'a> {
    /// Connects to `rpc`, or to the chain's public node, and makes sure it serves the chain asked for.
    pub fn connect(world: &'a dyn World, rpc: Option<&str>, chain_id: u64) -> Result<Self> {
        let Some(rpc) = rpc.or(public_node(chain_id)) else {
            bail!("no public node is known for chain {chain_id}: give one with --rpc")
        };
        let served = rpc_call(world, rpc, "eth_chainId", json!([]))?;
        let served = served.as_str().and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok());
        if served != Some(chain_id) {
            bail!("the node at {rpc} serves another chain than {chain_id}");
        }
        Ok(Self { world, rpc: rpc.to_string() })
    }

    /// The receipt of a transaction as the node gives it, as JSON text; `null` when it knows none.
    pub fn receipt(&self, tx: &str) -> Result<String> {
        Ok(rpc_call(self.world, &self.rpc, "eth_getTransactionReceipt", json!([tx]))?.to_string())
    }
}

#[cfg(test)]
pub mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    /// quicknet's round 1000 and its signature. Its time is 23 August 2023, 15:59:24 UTC.
    pub const ROUND: u64 = 1000;
    pub const ROUND_MS: u64 = 1_692_806_364_000;
    pub const SIGNATURE: &str =
        "b44679b9a59af2ec876b1a6b1ad52ea9b1615fc3982b19576350f93447cb1125e342b73a8dd2bacbe47e4b6b63ed5e39";

    type Handler = Box<dyn Fn(&str, Option<&str>, &str) -> Result<(u16, String)> + Sync>;

    /// A scripted outside: a clock the test sets, and answers the test wrote.
    pub struct Scripted {
        pub now: AtomicU64,
        pub paused: AtomicU64,
        /// How many POSTs were made.
        pub posts: AtomicUsize,
        /// Called for every POST with the URL, the bearer token and the body.
        pub on_post: Handler,
        /// Answers to GETs, by URL suffix; an entry may be served many times.
        pub pages: Mutex<Vec<(String, u16, String)>>,
        pub gets: Mutex<VecDeque<String>>,
    }

    impl Scripted {
        pub fn at(now_ms: u64) -> Self {
            Self {
                now: AtomicU64::new(now_ms),
                paused: AtomicU64::new(0),
                posts: AtomicUsize::new(0),
                on_post: Box::new(|url, _, _| bail!("no answer scripted for POST {url}")),
                pages: Mutex::new(Vec::new()),
                gets: Mutex::new(VecDeque::new()),
            }
        }

        /// From now on every relay serves quicknet's round 1000.
        pub fn publish_round_1000(&self) {
            self.page(&drand::round_path(ROUND), 200, &format!("{{\"round\":1000,\"signature\":\"{SIGNATURE}\"}}"));
        }

        /// Serves quicknet's round 1000 from every relay.
        pub fn with_round_1000(self) -> Self {
            self.publish_round_1000();
            self
        }

        pub fn page(&self, suffix: &str, status: u16, body: &str) {
            self.pages.lock().unwrap().push((suffix.to_string(), status, body.to_string()));
        }

        pub fn set_now(&self, now_ms: u64) {
            self.now.store(now_ms, Ordering::SeqCst);
        }
    }

    impl World for Scripted {
        fn now_ms(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }

        fn pause(&self, time: Duration) {
            self.paused.fetch_add(u64::try_from(time.as_millis()).unwrap(), Ordering::SeqCst);
        }

        fn get(&self, url: &str) -> Result<(u16, String)> {
            self.gets.lock().unwrap().push_back(url.to_string());
            let pages = self.pages.lock().unwrap();
            match pages.iter().rev().find(|(suffix, _, _)| url.ends_with(suffix.as_str())) {
                Some((_, status, body)) => Ok((*status, body.clone())),
                None => bail!("no answer scripted for GET {url}"),
            }
        }

        fn post(&self, url: &str, bearer: Option<&str>, body: &str, _: Duration) -> Result<(u16, String)> {
            self.posts.fetch_add(1, Ordering::SeqCst);
            (self.on_post)(url, bearer, body)
        }
    }

    #[test]
    fn the_first_relay_with_the_true_signature_wins() {
        let world = Scripted::at(ROUND_MS + 1).with_round_1000();
        let body = fetch_beacon(&world, ROUND).unwrap();
        assert!(Beacon::parse(ROUND, &body).is_ok());
        assert_eq!(world.gets.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_relay_that_lies_is_passed_over() {
        let world = Scripted::at(ROUND_MS + 1);
        let wrong = SIGNATURE.replace('b', "a");
        world.page(&drand::round_path(ROUND), 200, &format!("{{\"signature\":\"{wrong}\"}}"));
        world.page(&format!("api2.drand.sh{}", drand::round_path(ROUND)), 200, SIGNATURE);
        let body = fetch_beacon(&world, ROUND).unwrap();
        assert_eq!(body, SIGNATURE);
        assert_eq!(world.gets.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_round_no_relay_has_is_an_error_after_a_short_wait() {
        let world = Scripted::at(ROUND_MS + 1);
        world.page(&drand::round_path(ROUND), 404, "not yet");
        let error = fetch_beacon(&world, ROUND).unwrap_err().to_string();
        assert!(error.contains("no drand relay gave round 1000") && error.contains("answered 404"), "{error}");
        assert_eq!(world.paused.load(Ordering::SeqCst), 12_000);
        // Long after the round there is nothing to wait for.
        let world = Scripted::at(ROUND_MS + 3_600_000);
        assert!(fetch_beacon(&world, ROUND).is_err());
        assert_eq!(world.paused.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_round_counts_as_published_only_on_proof() {
        let world = Scripted::at(0);
        assert_eq!(peek_beacon(&world, ROUND), None, "no relay can be reached");
        world.page(&drand::round_path(ROUND), 404, "");
        assert_eq!(peek_beacon(&world, ROUND), None);
        world.page(&drand::round_path(ROUND), 200, &SIGNATURE.replace('b', "a"));
        assert_eq!(peek_beacon(&world, ROUND), None, "a relay that says yes without the signature");
        // One relay with the true signature is enough, and nothing is waited for.
        world.page(&format!("drand.cloudflare.com{}", drand::round_path(ROUND)), 200, SIGNATURE);
        assert_eq!(peek_beacon(&world, ROUND).as_deref(), Some(SIGNATURE));
        assert_eq!(peek_beacon(&world, ROUND + 1), None);
        assert_eq!(world.paused.load(Ordering::SeqCst), 0);
    }

    fn node(answers: fn(&str) -> Value) -> Scripted {
        let mut world = Scripted::at(0);
        world.on_post = Box::new(move |_, bearer, body| {
            assert_eq!(bearer, None);
            let request: Value = serde_json::from_str(body).unwrap();
            Ok((
                200,
                json!({"jsonrpc": "2.0", "id": 1, "result": answers(request["method"].as_str().unwrap())}).to_string(),
            ))
        });
        world
    }

    #[test]
    fn receipts_come_from_a_node_of_the_right_chain() {
        let world = node(|method| if method == "eth_chainId" { json!("0x1237") } else { json!({"status": "0x1"}) });
        let node = Node::connect(&world, None, 4663).unwrap();
        assert_eq!(node.receipt("0xabc").unwrap(), "{\"status\":\"0x1\"}");
        assert!(Node::connect(&world, None, 1).is_err(), "no public node is known for chain 1");
        assert!(
            Node::connect(&world, Some("https://node.example"), 1).err().unwrap().to_string().contains("another chain")
        );
    }

    #[test]
    fn a_transaction_the_node_does_not_know_reads_as_null() {
        let world = node(|method| if method == "eth_chainId" { json!("0x1237") } else { Value::Null });
        assert_eq!(Node::connect(&world, None, 4663).unwrap().receipt("0xabc").unwrap(), "null");
    }

    #[test]
    fn a_node_that_errors_is_an_error() {
        let mut world = Scripted::at(0);
        world.on_post = Box::new(|_, _, _| Ok((200, "{\"error\":{\"code\":-32000,\"message\":\"busy\"}}".into())));
        assert!(Node::connect(&world, None, 4663).err().unwrap().to_string().contains("busy"));
        world.on_post = Box::new(|_, _, _| Ok((503, String::new())));
        assert!(Node::connect(&world, None, 4663).err().unwrap().to_string().contains("503"));
        world.on_post = Box::new(|_, _, _| Ok((200, "<html>".into())));
        assert!(Node::connect(&world, None, 4663).is_err());
    }
}
