//! Reading the on-chain record of an exam out of a transaction receipt.
//!
//! This crate makes no network calls. Whoever runs it asks any node for a receipt
//! (`eth_getTransactionReceipt`) and hands the answer over as text; the functions here pick out the event the
//! record contract emitted.

use serde::Deserialize;
use sha3::{Digest, Keccak256};

use crate::hash::{from_hex, same_hex, to_hex};
use crate::{Result, invalid};

/// A contract that holds the public record of seals and results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    /// The id of the chain the contract is on.
    pub chain_id: u64,
    /// The contract's address.
    pub contract: &'static str,
}

/// The record contract this version trusts. None is deployed yet.
///
/// Its address belongs in the source for the same reason drand's public key does. A record on chain means
/// something only when the contract that holds it enforces the rules: a seal is accepted only while its round
/// is still ahead, a results root only once and only after it. A manifest may name any contract; a record in
/// any other one proves nothing, and is refused.
pub const RECORD: Option<Record> = None;

/// A public node of a chain the record contract lives on, for whoever has no node of their own.
#[must_use]
pub fn public_node(chain_id: u64) -> Option<&'static str> {
    (chain_id == 4663).then_some("https://rpc.mainnet.chain.robinhood.com")
}

/// The event the record contract emits when an exam is sealed.
pub const SEALED: &str = "Sealed(bytes32,bytes32,bytes32,uint64)";
/// The event it emits when the results are published.
pub const PUBLISHED: &str = "Published(bytes32,bytes32)";

/// What the contract recorded when an exam was sealed. The contract only accepts this before the round opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealEvent {
    /// The exam's id on chain.
    pub exam_id: String,
    /// SHA-256 of the sealed file.
    pub ciphertext_hash: String,
    /// SHA-256 of the exam inside it.
    pub plaintext_hash: String,
    /// The drand round that opens it.
    pub drand_round: u64,
}

/// What the contract recorded when results were published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishEvent {
    /// The exam's id on chain.
    pub exam_id: String,
    /// The Merkle root over every answer.
    pub results_root: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Receipt {
    status: Option<String>,
    transaction_hash: Option<String>,
    #[serde(default)]
    logs: Vec<Log>,
}

#[derive(Deserialize)]
struct Log {
    address: String,
    topics: Vec<String>,
    data: String,
}

/// The first topic of an event: the Keccak-256 of its signature.
#[must_use]
pub fn topic(signature: &str) -> String {
    to_hex(&Keccak256::digest(signature.as_bytes()))
}

/// The topics and the 32-byte data words of a log.
type Event = (Vec<String>, Vec<[u8; 32]>);

/// The first log in a receipt that `contract` emitted for `event`, which must carry `words` words of data.
///
/// `Ok(None)` means the receipt is sound but holds no such event: the node knows no such transaction, it
/// failed, or it is another transaction than the one asked for.
fn find(receipt: &str, tx: &str, contract: &str, event: &str, words: usize) -> Result<Option<Event>> {
    let Some(receipt) = serde_json::from_str::<Option<Receipt>>(receipt)? else {
        return Ok(None);
    };
    let succeeded = receipt.status.as_deref().is_some_and(|status| status == "0x1");
    if !succeeded || !receipt.transaction_hash.as_deref().is_some_and(|hash| same_hex(hash, tx)) {
        return Ok(None);
    }
    let wanted = topic(event);
    for log in receipt.logs {
        if !same_hex(&log.address, contract) || !log.topics.first().is_some_and(|first| same_hex(first, &wanted)) {
            continue;
        }
        let data = from_hex(&log.data)?;
        let (whole, rest) = data.as_chunks::<32>();
        if whole.len() != words || !rest.is_empty() {
            return invalid(format!("the {event} event has {} bytes of data", data.len()));
        }
        return Ok(Some((log.topics, whole.to_vec())));
    }
    Ok(None)
}

/// The seal the contract recorded in transaction `tx`, read from that transaction's receipt.
pub fn seal_event(receipt: &str, tx: &str, contract: &str) -> Result<Option<SealEvent>> {
    let Some((topics, words)) = find(receipt, tx, contract, SEALED, 3)? else {
        return Ok(None);
    };
    let ([_, exam_id], [ciphertext_hash, plaintext_hash, round]) = (&topics[..], &words[..]) else {
        return invalid("the Sealed event has the wrong shape");
    };
    let (padding, round) = round.split_at(24);
    let (true, Ok(round)) = (padding.iter().all(|&byte| byte == 0), <[u8; 8]>::try_from(round)) else {
        return invalid("the round in the Sealed event is not a 64-bit number");
    };
    Ok(Some(SealEvent {
        exam_id: exam_id.to_lowercase(),
        ciphertext_hash: to_hex(ciphertext_hash),
        plaintext_hash: to_hex(plaintext_hash),
        drand_round: u64::from_be_bytes(round),
    }))
}

/// The results root the contract recorded in transaction `tx`, read from that transaction's receipt.
pub fn publish_event(receipt: &str, tx: &str, contract: &str) -> Result<Option<PublishEvent>> {
    let Some((topics, words)) = find(receipt, tx, contract, PUBLISHED, 1)? else {
        return Ok(None);
    };
    let ([_, exam_id], [results_root]) = (&topics[..], &words[..]) else {
        return invalid("the Published event has the wrong shape");
    };
    Ok(Some(PublishEvent { exam_id: exam_id.to_lowercase(), results_root: to_hex(results_root) }))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const CONTRACT: &str = "0x5FbDB2315678afecb367f032d93F642f64180aa3";
    pub(crate) const TX: &str = "0x8a1f0d7c3b2e4f5061728394a5b6c7d8e9f00112233445566778899aabbccdde";

    fn word(hex_tail: &str) -> String {
        format!("{hex_tail:0>64}")
    }

    /// A receipt as a node returns it, with one log.
    pub(crate) fn receipt(address: &str, topics: &[&str], data: &str) -> String {
        let topics: Vec<String> = topics.iter().map(|t| format!("\"{t}\"")).collect();
        format!(
            "{{\"status\":\"0x1\",\"transactionHash\":\"{TX}\",\"blockNumber\":\"0x10\",\"logs\":[{{\"address\":\"0x00000000000000000000000000000000000000aa\",\"topics\":[],\"data\":\"0x\"}},{{\"address\":\"{address}\",\"topics\":[{}],\"data\":\"0x{data}\",\"logIndex\":\"0x1\"}}]}}",
            topics.join(",")
        )
    }

    pub(crate) fn seal_receipt(exam_id: &str, ciphertext: &str, plaintext: &str, round: u64) -> String {
        let data = format!(
            "{}{}{}",
            ciphertext.trim_start_matches("0x"),
            plaintext.trim_start_matches("0x"),
            word(&format!("{round:x}"))
        );
        receipt(CONTRACT, &[&topic(SEALED), exam_id], &data)
    }

    pub(crate) fn publish_receipt(exam_id: &str, root: &str) -> String {
        receipt(CONTRACT, &[&topic(PUBLISHED), exam_id], root.trim_start_matches("0x"))
    }

    /// The record contract, as it is written. The engine reads what this contract emits.
    const SOURCE: &str = include_str!("../../../contracts/src/ExamSeals.sol");

    /// An event as the contract declares it: its signature, and which of its values are topics.
    fn declared(event: &str) -> (String, Vec<bool>) {
        let (_, rest) = SOURCE.split_once(&format!("event {event}(")).expect("the contract declares the event");
        let (values, _) = rest.split_once(')').unwrap();
        let types: Vec<&str> = values.split(',').map(|value| value.split_whitespace().next().unwrap()).collect();
        let topics = values.split(',').map(|value| value.split_whitespace().any(|word| word == "indexed")).collect();
        (format!("{event}({})", types.join(",")), topics)
    }

    #[test]
    fn the_engine_reads_the_events_the_contract_emits() {
        // The id is the one topic after the signature; everything else is data, in the order it is read here.
        assert_eq!(declared("Sealed"), (SEALED.to_string(), vec![true, false, false, false]));
        assert_eq!(declared("Published"), (PUBLISHED.to_string(), vec![true, false]));
    }

    #[test]
    fn the_contract_counts_rounds_as_the_engine_does() {
        use crate::drand::{GENESIS_TIME, PERIOD, round_time};
        assert!(SOURCE.contains(&format!("DRAND_GENESIS = {GENESIS_TIME};")));
        assert!(SOURCE.contains(&format!("DRAND_PERIOD = {PERIOD};")));
        assert!(SOURCE.contains("return DRAND_GENESIS + (round - 1) * DRAND_PERIOD;"));
        assert_eq!(round_time(1000).unwrap(), (GENESIS_TIME + (1000 - 1) * PERIOD) * 1000);
    }

    #[test]
    fn only_known_chains_have_a_public_node() {
        assert!(public_node(4663).is_some_and(|url| url.starts_with("https://")));
        assert_eq!(public_node(1), None);
    }

    #[test]
    fn topics_are_keccak_of_the_signature() {
        // The well-known topic of ERC-20's Transfer event pins the hash function.
        assert_eq!(
            topic("Transfer(address,address,uint256)"),
            "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
        );
        assert_ne!(topic(SEALED), topic(PUBLISHED));
    }

    #[test]
    fn a_seal_reads_back() {
        let (id, ct, pt) = (format!("0x{}", word("a1")), format!("0x{}", word("b2")), format!("0x{}", word("c3")));
        let text = seal_receipt(&id, &ct, &pt, 34_188_286);
        let event = seal_event(&text, TX, CONTRACT).unwrap().unwrap();
        assert_eq!(event, SealEvent { exam_id: id, ciphertext_hash: ct, plaintext_hash: pt, drand_round: 34_188_286 });
        // Addresses and hashes compare without regard to letter case.
        assert!(seal_event(&text, &TX.to_uppercase().replace("0X", "0x"), &CONTRACT.to_lowercase()).unwrap().is_some());
    }

    #[test]
    fn a_publication_reads_back() {
        let (id, root) = (format!("0x{}", word("a1")), format!("0x{}", word("d4")));
        let event = publish_event(&publish_receipt(&id, &root), TX, CONTRACT).unwrap().unwrap();
        assert_eq!(event, PublishEvent { exam_id: id, results_root: root });
    }

    #[test]
    fn other_contracts_events_and_transactions_do_not_count() {
        let (id, hash) = (format!("0x{}", word("a1")), format!("0x{}", word("b2")));
        let text = seal_receipt(&id, &hash, &hash, 7);
        assert_eq!(seal_event(&text, TX, "0x00000000000000000000000000000000000000bb").unwrap(), None);
        assert_eq!(publish_event(&text, TX, CONTRACT).unwrap(), None);
        assert_eq!(seal_event(&text, &TX.replace("8a1f", "8a1e"), CONTRACT).unwrap(), None);
        assert_eq!(seal_event(&text.replace("\"status\":\"0x1\"", "\"status\":\"0x0\""), TX, CONTRACT).unwrap(), None);
        assert_eq!(seal_event(&text.replace("\"status\":\"0x1\",", ""), TX, CONTRACT).unwrap(), None);
        assert_eq!(seal_event("null", TX, CONTRACT).unwrap(), None);
        assert_eq!(seal_event("{}", TX, CONTRACT).unwrap(), None);
    }

    #[test]
    fn malformed_receipts_are_errors() {
        let id = format!("0x{}", word("a1"));
        assert!(seal_event("", TX, CONTRACT).is_err());
        assert!(seal_event("[]", TX, CONTRACT).is_err());
        assert!(seal_event(&receipt(CONTRACT, &[&topic(SEALED), &id], &word("1")), TX, CONTRACT).is_err());
        assert!(seal_event(&receipt(CONTRACT, &[&topic(SEALED), &id], "zz"), TX, CONTRACT).is_err());
        assert!(seal_event(&receipt(CONTRACT, &[&topic(SEALED)], &word("1").repeat(3)), TX, CONTRACT).is_err());
        // A round that does not fit in 64 bits.
        let wide = format!("{}{}{}", word("1"), word("1"), word("10000000000000000"));
        assert!(seal_event(&receipt(CONTRACT, &[&topic(SEALED), &id], &wide), TX, CONTRACT).is_err());
        assert!(
            publish_event(&receipt(CONTRACT, &[&topic(PUBLISHED), &id], &word("1").repeat(2)), TX, CONTRACT).is_err()
        );
    }
}
