//! Sealing an exam, and the manifest that is published next to the sealed file.

use serde::{Deserialize, Serialize};

use crate::drand::{self, Beacon};
use crate::exam::{Exam, FORMAT, ID_RULE, Provenance, chain_exam_id, valid_id};
use crate::hash::{same_hex, sha256_hex};
use crate::{GENERATOR_VERSION, NAME, Result, date, invalid, timelock};

/// Where an exam's seal and results were recorded on chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ChainRef {
    /// The chain's id.
    pub chain_id: u64,
    /// The address of the contract that holds the record.
    pub contract: String,
    /// The transaction that recorded the seal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seal_tx: Option<String>,
    /// The transaction that recorded the results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_tx: Option<String>,
}

/// What is public about an exam while it is sealed: when it opens and the hashes that pin its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Manifest {
    /// The file format.
    pub format: u32,
    /// The project's name.
    pub name: String,
    /// The exam's id.
    pub id: String,
    /// The exam's id on chain: the SHA-256 of `id`.
    pub exam_id: String,
    /// The name shown to people.
    pub title: String,
    /// When the exam opens.
    pub open_at: String,
    /// The drand round whose signature opens it.
    pub open_round: u64,
    /// The drand chain that round belongs to.
    pub drand_chain: String,
    /// SHA-256 of the sealed file.
    pub ciphertext_sha256: String,
    /// SHA-256 of the exam inside it.
    pub plaintext_sha256: String,
    /// How many questions the exam has.
    pub question_count: u32,
    /// Which code made the exam.
    pub generator: Provenance,
    /// Where the seal and the results were recorded on chain, once they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<ChainRef>,
}

impl Manifest {
    /// Reads a manifest from its JSON.
    pub fn parse(json: &str) -> Result<Self> {
        let manifest: Self = serde_json::from_str(json)?;
        if manifest.format != FORMAT {
            return invalid(format!("manifest format {}: this version reads format {FORMAT}", manifest.format));
        }
        if !valid_id(&manifest.id) {
            return invalid(ID_RULE);
        }
        Ok(manifest)
    }
}

/// Seals an exam to a round. Returns the sealed file and its manifest.
///
/// The seed and the keys exist only inside the sealed file until the round's signature is published.
pub fn seal_exam(exam: &Exam, round: u64) -> Result<(String, Manifest)> {
    if exam.generator.version != GENERATOR_VERSION || exam.generator.name != NAME {
        return invalid("this exam was made by another generator");
    }
    let Ok(question_count) = u32::try_from(exam.questions.len()) else {
        return invalid("the exam has too many questions");
    };
    let plaintext = exam.bytes()?;
    let ciphertext = timelock::seal_bytes(plaintext.as_bytes(), round)?;
    let manifest = Manifest {
        format: FORMAT,
        name: NAME.into(),
        id: exam.id.clone(),
        exam_id: chain_exam_id(&exam.id),
        title: exam.title.clone(),
        open_at: date::rfc3339(drand::round_time(round)?),
        open_round: round,
        drand_chain: drand::CHAIN_HASH.into(),
        ciphertext_sha256: sha256_hex(ciphertext.as_bytes()),
        plaintext_sha256: sha256_hex(plaintext.as_bytes()),
        question_count,
        generator: exam.generator.clone(),
        chain: None,
    };
    Ok((ciphertext, manifest))
}

/// Opens a sealed exam. Returns the exam and the exact text that was sealed.
///
/// With a manifest, the opened text must have the hash the manifest promised.
pub fn open_exam(ciphertext: &[u8], beacon: &Beacon, manifest: Option<&Manifest>) -> Result<(Exam, String)> {
    let Ok(plaintext) = String::from_utf8(timelock::open_bytes(ciphertext, beacon)?) else {
        return invalid("the opened file is not text");
    };
    if manifest.is_some_and(|m| !same_hex(&sha256_hex(plaintext.as_bytes()), &m.plaintext_sha256)) {
        return invalid("the opened exam does not have the hash in the manifest");
    }
    Ok((Exam::parse(&plaintext)?, plaintext))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::drand::tests::{ROUND, SIGNATURE};
    use crate::exam::{make_exam, tests::recipe};
    use crate::hash::from_hex;

    pub(crate) fn beacon() -> Beacon {
        Beacon::verify(ROUND, &from_hex(SIGNATURE).unwrap()).unwrap()
    }

    #[test]
    fn a_sealed_exam_opens_to_the_same_exam() {
        let exam = make_exam(&recipe()).unwrap();
        let (ciphertext, manifest) = seal_exam(&exam, ROUND).unwrap();
        assert_eq!(manifest.open_round, ROUND);
        assert_eq!(manifest.open_at, "2023-08-23T15:59:24.000Z");
        assert_eq!(manifest.question_count, 30);
        assert_eq!(manifest.exam_id, chain_exam_id("test-01"));
        assert_eq!(manifest.ciphertext_sha256, sha256_hex(ciphertext.as_bytes()));
        assert_eq!(manifest.plaintext_sha256, sha256_hex(exam.bytes().unwrap().as_bytes()));
        let (opened, plaintext) = open_exam(ciphertext.as_bytes(), &beacon(), Some(&manifest)).unwrap();
        assert_eq!(opened, exam);
        assert_eq!(plaintext, exam.bytes().unwrap());
    }

    #[test]
    fn the_sealed_file_shows_nothing_of_the_exam() {
        let exam = make_exam(&recipe()).unwrap();
        let (ciphertext, manifest) = seal_exam(&exam, ROUND).unwrap();
        let public = format!("{ciphertext}{}", serde_json::to_string(&manifest).unwrap());
        assert!(!public.contains(&exam.seed));
        assert!(
            exam.questions
                .iter()
                .all(|q| !public.contains(&q.prompt) && (q.answer.len() < 6 || !public.contains(&q.answer)))
        );
    }

    #[test]
    fn a_manifest_reads_only_with_an_id_seal_could_have_written() {
        let (_, manifest) = seal_exam(&make_exam(&recipe()).unwrap(), ROUND).unwrap();
        let json = serde_json::to_string(&manifest).unwrap();
        assert_eq!(Manifest::parse(&json).unwrap(), manifest);
        let forged = json.replacen("\"id\":\"test-01\"", "\"id\":\"test-01\\nPASS exams/x (open)\"", 1);
        assert!(Manifest::parse(&forged).unwrap_err().to_string().contains("exam id"));
    }

    #[test]
    fn an_exam_that_does_not_match_its_manifest_is_refused() {
        let exam = make_exam(&recipe()).unwrap();
        let (ciphertext, manifest) = seal_exam(&exam, ROUND).unwrap();
        let other = Manifest { plaintext_sha256: sha256_hex(b"something else"), ..manifest };
        assert!(open_exam(ciphertext.as_bytes(), &beacon(), Some(&other)).is_err());
        assert!(open_exam(ciphertext.as_bytes(), &beacon(), None).is_ok());
    }

    #[test]
    fn sealed_data_that_is_not_an_exam_is_refused() {
        let ciphertext = timelock::seal_bytes(b"{\"not\":\"an exam\"}", ROUND).unwrap();
        assert!(open_exam(ciphertext.as_bytes(), &beacon(), None).is_err());
        let ciphertext = timelock::seal_bytes(&[0xff, 0xfe], ROUND).unwrap();
        assert!(open_exam(ciphertext.as_bytes(), &beacon(), None).is_err());
    }

    #[test]
    fn an_exam_from_another_generator_is_not_sealed() {
        let mut exam = make_exam(&recipe()).unwrap();
        exam.generator.version = "2".into();
        assert!(seal_exam(&exam, ROUND).is_err());
    }

    #[test]
    fn manifests_read_strictly() {
        let exam = make_exam(&recipe()).unwrap();
        let (_, manifest) = seal_exam(&exam, ROUND).unwrap();
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(!json.contains("\"chain\""));
        assert_eq!(Manifest::parse(&json).unwrap(), manifest);
        assert!(Manifest::parse(&json.replacen("\"format\":1", "\"format\":3", 1)).is_err());
        assert!(Manifest::parse(&json.replacen("{\"format\"", "{\"extra\":true,\"format\"", 1)).is_err());
        assert!(Manifest::parse(&json.replacen("\"openRound\":1000", "\"openRound\":-1", 1)).is_err());
        let with_chain = json.replacen(
            "{\"format\"",
            "{\"chain\":{\"chainId\":4663,\"contract\":\"0x00\",\"sealTx\":\"0x01\"},\"format\"",
            1,
        );
        let read = Manifest::parse(&with_chain).unwrap();
        assert_eq!(
            read.chain,
            Some(ChainRef { chain_id: 4663, contract: "0x00".into(), seal_tx: Some("0x01".into()), publish_tx: None })
        );
    }
}
