//! Timelock encryption to a drand round.
//!
//! The file is an [age](https://age-encryption.org) file with one `tlock` recipient, the format of drand's
//! own tools (github.com/drand/tlock), so what is sealed here opens there and the other way round. age
//! encrypts the data under a fresh file key; this module wraps that key so that only the signature of the
//! chosen round unwraps it. Files are written by [`crate::agefile`] and read by the `age` crate.
//!
//! The wrapping is Boneh-Franklin identity-based encryption on BLS12-381 with the Fujisaki-Okamoto check, as
//! in "tlock: Practical Timelock Encryption from Threshold BLS" (eprint 2023/189). The identity is the round
//! number and the private key for it is the round's signature. All curve arithmetic comes from arkworks.

use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::iter;

use age::armor::{ArmoredReader, ArmoredWriter, Format};
use age::{DecryptError, Decryptor};
use age_core::format::{FILE_KEY_BYTES, FileKey, Stanza};
use ark_bls12_381::{Bls12_381, Fr, G2Affine};
use ark_ec::pairing::{Pairing, PairingOutput};
use ark_ec::{AffineRepr, CurveGroup};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use sha2::{Digest, Sha256};

use crate::drand::{self, Beacon};
use crate::{Error, Result, agefile};

const TAG: &str = "tlock";
/// The most bytes a file's header may take before the file is refused unread.
const MAX_HEADER_BYTES: usize = 4096;
type Key = [u8; FILE_KEY_BYTES];
/// Bytes of a compressed point of G2.
const POINT_BYTES: usize = 96;
/// A wrapped key: the point U, then V and W.
const BODY_BYTES: usize = POINT_BYTES + 2 * FILE_KEY_BYTES;

fn sealed(message: impl Into<String>) -> Error {
    Error::Sealed(message.into())
}

/// The first 16 bytes of SHA-256 over a tag and some data: the scheme's H2 and H4.
fn mask(tag: &[u8], data: &[u8]) -> Key {
    let digest = Sha256::new().chain_update(tag).chain_update(data).finalize();
    std::array::from_fn(|i| digest[i])
}

fn xor(a: &Key, b: &Key) -> Key {
    std::array::from_fn(|i| a[i] ^ b[i])
}

/// A pairing value as the bytes the other implementations hash: the reverse of arkworks' own order.
fn pairing_bytes(value: &PairingOutput<Bls12_381>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    value.serialize_compressed(&mut bytes).map_err(|_| sealed("a pairing value does not write"))?;
    bytes.reverse();
    Ok(bytes)
}

/// The scheme's H3: the scalar that ties a wrapped key to the randomness it was wrapped with.
///
/// It hashes with a counter until the digest, with its top bit cleared, is below the group order, exactly as
/// the Go and JavaScript implementations do; any other rule would give a different scalar.
fn scalar(sigma: &Key, key: &Key) -> Result<Fr> {
    let seed = Sha256::new().chain_update(b"IBE-H3").chain_update(sigma).chain_update(key).finalize();
    for counter in 1..u16::MAX {
        let mut digest: [u8; 32] =
            Sha256::new().chain_update(counter.to_le_bytes()).chain_update(seed).finalize().into();
        digest[0] >>= 1;
        digest.reverse();
        if let Ok(scalar) = Fr::deserialize_compressed(&digest[..]) {
            return Ok(scalar);
        }
    }
    Err(sealed("no scalar found"))
}

/// Wraps a file key so that only the signature of `round` unwraps it.
fn wrap(round: u64, key: &Key) -> Result<Vec<u8>> {
    let shared = Bls12_381::pairing(drand::round_point(round)?, drand::public_key()?);
    let mut sigma = Key::default();
    getrandom::getrandom(&mut sigma).map_err(|_| sealed("the system gave no randomness"))?;
    let r = scalar(&sigma, key)?;
    let mut body = Vec::with_capacity(BODY_BYTES);
    (G2Affine::generator() * r)
        .into_affine()
        .serialize_compressed(&mut body)
        .map_err(|_| sealed("a point does not write"))?;
    body.extend(xor(&sigma, &mask(b"IBE-H2", &pairing_bytes(&(shared * r))?)));
    body.extend(xor(key, &mask(b"IBE-H4", &sigma)));
    Ok(body)
}

/// Unwraps a file key with a round's signature.
fn unwrap(beacon: &Beacon, body: &[u8]) -> Result<Key> {
    let wrong_shape = || sealed("the wrapped key has the wrong shape");
    if body.len() != BODY_BYTES {
        return Err(wrong_shape());
    }
    let (point, rest) = body.split_at(POINT_BYTES);
    let (v, w) = rest.split_at(FILE_KEY_BYTES);
    let (v, w) = (Key::try_from(v).map_err(|_| wrong_shape())?, Key::try_from(w).map_err(|_| wrong_shape())?);
    // Reading the point also checks that it is on the curve and in the right subgroup.
    let u = G2Affine::deserialize_compressed(point).map_err(|_| wrong_shape())?;
    let sigma = xor(&v, &mask(b"IBE-H2", &pairing_bytes(&Bls12_381::pairing(beacon.signature(), u))?));
    let key = xor(&w, &mask(b"IBE-H4", &sigma));
    // Without this check a tampered file could unwrap to a key of the attacker's choosing.
    if (G2Affine::generator() * scalar(&sigma, &key)?).into_affine() != u {
        return Err(sealed("the wrapped key does not check out"));
    }
    Ok(key)
}

/// The round a `tlock` stanza names, if it is one of this chain's.
fn stanza_round(stanza: &Stanza) -> Result<u64> {
    let [round, chain] = &stanza.args[..] else {
        return Err(sealed("the timelock is malformed"));
    };
    if chain != drand::CHAIN_HASH {
        return Err(sealed("the file is locked to another drand chain"));
    }
    let plain = !round.is_empty() && round.bytes().all(|c| c.is_ascii_digit()) && !round.starts_with('0');
    round.parse().ok().filter(|_| plain).ok_or_else(|| sealed("the timelock names no round"))
}

/// What age shows every recipient of a file to. It notes the round of each timelock it sees and, when it
/// holds that round's signature, unwraps the key.
struct Opener<'a> {
    beacon: Option<&'a Beacon>,
    seen: RefCell<Vec<Result<u64>>>,
}

impl age::Identity for Opener<'_> {
    fn unwrap_stanza(&self, stanza: &Stanza) -> Option<std::result::Result<FileKey, DecryptError>> {
        if stanza.tag != TAG {
            return None;
        }
        let round = stanza_round(stanza);
        let beacon = self.beacon.filter(|beacon| round.as_ref().is_ok_and(|&r| r == beacon.round()));
        self.seen.borrow_mut().push(round);
        Some(
            unwrap(beacon?, &stanza.body)
                .map(|key| FileKey::new(Box::new(key)))
                .map_err(|_| DecryptError::DecryptionFailed),
        )
    }
}

/// Seals data to a round. The result is text: an armored age file with one recipient, the timelock.
pub fn seal_bytes(data: &[u8], round: u64) -> Result<String> {
    drand::round_time(round)?;
    let failed = |e: io::Error| sealed(format!("sealing failed: {e}"));
    let file = agefile::write(TAG, &[&round.to_string(), drand::CHAIN_HASH], |key| wrap(round, key), data)?;
    let mut armor = ArmoredWriter::wrap_output(Vec::new(), Format::AsciiArmor).map_err(failed)?;
    armor.write_all(&file).map_err(failed)?;
    let text = String::from_utf8(armor.finish().map_err(failed)?)
        .map_err(|_| sealed("sealing failed: the armor is not text"))?;
    // Read back what was written, with the reader everyone else will use, before anyone relies on it.
    if sealed_round(text.as_bytes())? != round {
        return Err(sealed("sealing failed: the file names another round"));
    }
    Ok(text)
}

/// The file in binary form, its armor removed, once its header is known to be short.
///
/// The age reader's work grows with the square of a header's length, so a file with a header of a megabyte
/// would keep it busy for minutes. A header with one timelock is under 300 bytes.
fn binary(ciphertext: &[u8]) -> Result<Vec<u8>> {
    let mut file = Vec::new();
    ArmoredReader::new(ciphertext).read_to_end(&mut file).map_err(|e| sealed(format!("not an age file: {e}")))?;
    // A header ends with the line that starts with three dashes.
    if !file[..file.len().min(MAX_HEADER_BYTES)].windows(5).any(|window| window == b"\n--- ") {
        return Err(sealed("not an age file, or one whose header is too long"));
    }
    Ok(file)
}

/// The round a file in binary form is locked to.
fn round_of(file: &[u8]) -> Result<u64> {
    let opener = Opener { beacon: None, seen: RefCell::default() };
    let decryptor = Decryptor::new(file).map_err(|e| sealed(format!("not an age file: {e}")))?;
    // Nothing unwraps without a signature. This is run for what the opener sees on the way.
    if decryptor.decrypt(iter::once(&opener as &dyn age::Identity)).is_ok() {
        return Err(sealed("the file opens without a signature"));
    }
    let mut seen = opener.seen.into_inner();
    match (seen.pop(), seen.is_empty()) {
        (Some(round), true) => round,
        (None, _) => Err(sealed("the file has no timelock")),
        (Some(_), false) => Err(sealed("the file has more than one timelock")),
    }
}

/// The round a sealed file is locked to. Reads the file's header only; needs no signature.
pub fn sealed_round(ciphertext: &[u8]) -> Result<u64> {
    round_of(&binary(ciphertext)?)
}

/// Opens a sealed file with the signature of the round it is locked to.
pub fn open_bytes(ciphertext: &[u8], beacon: &Beacon) -> Result<Vec<u8>> {
    let file = binary(ciphertext)?;
    let round = round_of(&file)?;
    if round != beacon.round() {
        return Err(sealed(format!(
            "the file is locked to round {round}, and the signature is of round {}",
            beacon.round()
        )));
    }
    let failed = |e: &dyn std::fmt::Display| sealed(format!("the file does not open: {e}"));
    let opener = Opener { beacon: Some(beacon), seen: RefCell::default() };
    let decryptor = Decryptor::new(file.as_slice()).map_err(|e| failed(&e))?;
    let mut reader = decryptor.decrypt(iter::once(&opener as &dyn age::Identity)).map_err(|e| failed(&e))?;
    let mut data = Vec::new();
    reader.read_to_end(&mut data).map_err(|e| failed(&e))?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drand::tests::{ROUND, SIGNATURE};
    use crate::hash::from_hex;

    fn beacon() -> Beacon {
        Beacon::verify(ROUND, &from_hex(SIGNATURE).unwrap()).unwrap()
    }

    /// The armor is base64 of the binary file between two marker lines.
    fn unarmor(text: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        ArmoredReader::new(text.as_bytes()).read_to_end(&mut bytes).unwrap();
        bytes
    }

    fn armor(bytes: &[u8]) -> String {
        let mut writer = ArmoredWriter::wrap_output(Vec::new(), Format::AsciiArmor).unwrap();
        writer.write_all(bytes).unwrap();
        String::from_utf8(writer.finish().unwrap()).unwrap()
    }

    #[test]
    fn what_is_sealed_to_a_round_opens_with_its_signature() {
        for data in [&b""[..], b"x", b"an exam", &[7u8; 70_000]] {
            let sealed = seal_bytes(data, ROUND).unwrap();
            assert!(
                sealed.starts_with("-----BEGIN AGE ENCRYPTED FILE-----\n")
                    && sealed.ends_with("-----END AGE ENCRYPTED FILE-----\n")
            );
            assert_eq!(sealed_round(sealed.as_bytes()).unwrap(), ROUND);
            assert_eq!(open_bytes(sealed.as_bytes(), &beacon()).unwrap(), data);
            // The binary form inside the armor is the same file.
            assert_eq!(open_bytes(&unarmor(&sealed), &beacon()).unwrap(), data);
        }
    }

    #[test]
    fn a_sealed_file_has_the_timelock_as_its_only_recipient() {
        // The plain form drand's own tools write and every one of them reads: no second, random recipient.
        for _ in 0..20 {
            let file = unarmor(&seal_bytes(b"one recipient", ROUND).unwrap());
            let end = file.windows(5).position(|w| w == b"\n--- ").unwrap();
            let header = std::str::from_utf8(&file[..end]).unwrap();
            let lines: Vec<&str> = header.lines().collect();
            assert_eq!(lines[0], "age-encryption.org/v1");
            assert_eq!(lines[1], format!("-> tlock 1000 {}", drand::CHAIN_HASH));
            // 128 bytes of wrapped key are 171 characters: two full lines and one of 43.
            assert_eq!(lines[2..].iter().map(|line| line.len()).collect::<Vec<_>>(), [64, 64, 43]);
            assert!(end < 300);
        }
    }

    /// The same wrapping written through the `age` crate, which adds its random second recipient: the form
    /// other Rust tools produce.
    struct Lock;

    impl age::Recipient for Lock {
        fn wrap_file_key(
            &self,
            file_key: &FileKey,
        ) -> std::result::Result<(Vec<Stanza>, std::collections::HashSet<String>), age::EncryptError> {
            use age::secrecy::ExposeSecret;
            let body = wrap(ROUND, file_key.expose_secret()).unwrap();
            let args = vec![ROUND.to_string(), drand::CHAIN_HASH.to_string()];
            Ok((vec![Stanza { tag: TAG.into(), args, body }], std::collections::HashSet::new()))
        }
    }

    #[test]
    fn files_with_a_second_recipient_from_other_writers_still_open() {
        for _ in 0..20 {
            let encryptor = age::Encryptor::with_recipients(iter::once(&Lock as &dyn age::Recipient)).unwrap();
            let mut writer = encryptor.wrap_output(Vec::new()).unwrap();
            writer.write_all(b"written by the age crate").unwrap();
            let file = writer.finish().unwrap();
            let end = file.windows(5).position(|w| w == b"\n--- ").unwrap();
            assert_eq!(std::str::from_utf8(&file[..end]).unwrap().matches("-> ").count(), 2);
            assert_eq!(sealed_round(&file).unwrap(), ROUND);
            assert_eq!(open_bytes(&file, &beacon()).unwrap(), b"written by the age crate");
        }
    }

    #[test]
    fn a_file_with_an_oversized_header_is_refused_unread() {
        // A megabyte of recipients before the end of the header would keep the reader busy for minutes.
        let honest = unarmor(&seal_bytes(b"x", ROUND).unwrap());
        let end = honest.windows(5).position(|w| w == b"\n--- ").unwrap() + 1;
        let padding = format!("-> pad\n{}\n", "QUJD".repeat(16));
        for stanzas in [70usize, 20_000] {
            let mut file = honest[..end].to_vec();
            for _ in 0..stanzas {
                file.extend_from_slice(padding.as_bytes());
                file.extend_from_slice(b"\n");
            }
            file.extend_from_slice(&honest[end..]);
            for form in [file.clone(), armor(&file).into_bytes()] {
                let error = sealed_round(&form).unwrap_err().to_string();
                assert!(error.contains("header is too long"), "{stanzas} recipients: {error}");
                assert!(open_bytes(&form, &beacon()).is_err());
            }
        }
        // A few extra recipients within the bound are read as before (and then fail the header's own check).
        let mut small = honest[..end].to_vec();
        small.extend_from_slice(padding.as_bytes());
        small.extend_from_slice(b"\n");
        small.extend_from_slice(&honest[end..]);
        assert_eq!(sealed_round(&small).unwrap(), ROUND);
        assert!(open_bytes(&small, &beacon()).unwrap_err().to_string().contains("does not open"));
    }

    #[test]
    fn two_seals_of_the_same_data_share_nothing() {
        let (a, b) = (seal_bytes(b"same", ROUND).unwrap(), seal_bytes(b"same", ROUND).unwrap());
        assert_ne!(a, b);
        let (a, b) = (unarmor(&a), unarmor(&b));
        let body = |file: &[u8]| -> Vec<u8> {
            file.windows(4).position(|w| w == b"e971").map(|at| file[at + 5..at + 60].to_vec()).unwrap()
        };
        assert_ne!(body(&a), body(&b), "the wrapped keys must differ");
    }

    #[test]
    fn a_file_sealed_to_a_later_round_stays_shut() {
        let sealed = seal_bytes(b"later", ROUND + 1).unwrap();
        assert_eq!(sealed_round(sealed.as_bytes()).unwrap(), ROUND + 1);
        let error = open_bytes(sealed.as_bytes(), &beacon()).unwrap_err().to_string();
        assert!(error.contains("locked to round 1001"), "{error}");
        assert!(seal_bytes(b"x", 0).is_err());
    }

    #[test]
    fn the_wrapped_key_unwraps_only_untouched() {
        let key: Key = *b"sixteen byte key";
        let body = wrap(ROUND, &key).unwrap();
        assert_eq!(body.len(), BODY_BYTES);
        assert_eq!(unwrap(&beacon(), &body).unwrap(), key);
        // Any single flipped bit in U, V or W is caught, never turned into another key.
        for at in [0, 1, 47, 95, 96, 111, 112, 127] {
            for bit in [1u8, 0x80] {
                let mut bent = body.clone();
                bent[at] ^= bit;
                assert!(unwrap(&beacon(), &bent).is_err(), "byte {at}");
            }
        }
        assert!(unwrap(&beacon(), &body[..127]).is_err());
        assert!(unwrap(&beacon(), &[body.as_slice(), &[0]].concat()).is_err());
        assert!(unwrap(&beacon(), &[]).is_err());
        // A point that is not on the curve, and the point at infinity.
        assert!(unwrap(&beacon(), &[[0xffu8; 96].as_slice(), &body[96..]].concat()).is_err());
        let mut infinity = vec![0u8; BODY_BYTES];
        infinity[0] = 0xc0;
        assert!(unwrap(&beacon(), &infinity).is_err());
    }

    #[test]
    fn a_key_wrapped_for_another_round_does_not_unwrap() {
        let body = wrap(ROUND + 1, b"sixteen byte key").unwrap();
        assert!(unwrap(&beacon(), &body).is_err());
    }

    #[test]
    fn a_damaged_file_is_refused_not_misread() {
        let sealed = seal_bytes(&[42u8; 3000], ROUND).unwrap();
        let binary = unarmor(&sealed);
        // Flip one bit at a time across the whole file: the header, the wrapped key, the payload.
        for at in (0..binary.len()).step_by(binary.len() / 97) {
            let mut bent = binary.clone();
            bent[at] ^= 0x10;
            for form in [bent.clone(), armor(&bent).into_bytes()] {
                assert!(open_bytes(&form, &beacon()).is_err(), "byte {at} of {}", binary.len());
            }
        }
        // Cut short anywhere, it does not open either.
        for keep in [0, 1, 21, binary.len() / 2, binary.len() - 1] {
            assert!(open_bytes(&binary[..keep], &beacon()).is_err(), "first {keep} bytes");
        }
        assert!(open_bytes(&sealed.as_bytes()[..sealed.len() - 40], &beacon()).is_err());
    }

    #[test]
    fn files_that_are_not_ours_are_refused() {
        for junk in
            [&b""[..], b"hello", b"-----BEGIN AGE ENCRYPTED FILE-----\n", b"age-encryption.org/v1\n", &[0u8; 300]]
        {
            assert!(sealed_round(junk).is_err());
            assert!(open_bytes(junk, &beacon()).is_err());
        }
        let stanza = |args: &[&str]| Stanza {
            tag: TAG.into(),
            args: args.iter().map(|s| (*s).to_string()).collect(),
            body: vec![],
        };
        assert_eq!(stanza_round(&stanza(&["1000", drand::CHAIN_HASH])).unwrap(), 1000);
        for args in [
            &["1000"][..],
            &["1000", "dbd506d6ef76e5f386f41c651dcb808c5bcbd75471cc4eafa3f4df7ad4e4c493"],
            &["01000", drand::CHAIN_HASH],
            &["+1000", drand::CHAIN_HASH],
            &["", drand::CHAIN_HASH],
            &["1e3", drand::CHAIN_HASH],
            &["99999999999999999999999", drand::CHAIN_HASH],
            &["1000", drand::CHAIN_HASH, "x"],
            &[],
        ] {
            assert!(stanza_round(&stanza(args)).is_err(), "{args:?}");
        }
    }

    #[test]
    fn the_scalar_matches_the_reference_rule() {
        // Recompute with the rule spelled out on big-endian bytes, as the Go implementation does.
        const ORDER: [u8; 32] = [
            0x73, 0xed, 0xa7, 0x53, 0x29, 0x9d, 0x7d, 0x48, 0x33, 0x39, 0xd8, 0x08, 0x09, 0xa1, 0xd8, 0x05, 0x53, 0xbd,
            0xa4, 0x02, 0xff, 0xfe, 0x5b, 0xfe, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
        ];
        let mut skipped = 0;
        for n in 0..2000u32 {
            let sigma = mask(b"sigma", &n.to_le_bytes());
            let key = mask(b"key", &n.to_le_bytes());
            let seed = Sha256::new().chain_update(b"IBE-H3").chain_update(sigma).chain_update(key).finalize();
            let mut counter = 1u16;
            let expected = loop {
                let mut digest: [u8; 32] =
                    Sha256::new().chain_update(counter.to_le_bytes()).chain_update(seed).finalize().into();
                digest[0] >>= 1;
                if digest < ORDER {
                    break digest;
                }
                counter += 1;
                skipped += 1;
            };
            let mut written = Vec::new();
            scalar(&sigma, &key).unwrap().serialize_compressed(&mut written).unwrap();
            written.reverse();
            assert_eq!(written, expected);
        }
        assert!(skipped > 50, "the skip path was taken only {skipped} times");
    }
}
