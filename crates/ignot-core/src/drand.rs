//! The drand "quicknet" beacon: the public clock every seal is locked to.
//!
//! quicknet is run by the League of Entropy. Every three seconds it publishes one BLS signature. The message
//! signed at round `n` is the round number alone, so everyone knows it in advance, while the signature cannot
//! exist until the network produces it. That signature is the key to whatever was sealed to round `n`.

use ark_bls12_381::{Bls12_381, G1Affine, G2Affine, g1};
use ark_ec::hashing::{HashToCurve, curve_maps::wb::WBMap, map_to_curve_hasher::MapToCurveBasedHasher};
use ark_ec::models::short_weierstrass::Projective;
use ark_ec::{AffineRepr, pairing::Pairing};
use ark_ff::field_hashers::DefaultFieldHasher;
use ark_serialize::CanonicalDeserialize;
use sha2::Sha256;

use crate::hash::{from_hex, sha256};
use crate::{Error, Result, invalid};

/// The hash that names the quicknet chain.
pub const CHAIN_HASH: &str = "52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971";
/// The chain's public key: a point of the group G2, compressed.
pub const PUBLIC_KEY: &str = "83cf0f2896adee7eb8b5f01fcad3912212c437e0073e911fb90022d3e760183c8c4b450b6a0a6c3ac6a5776a2d1064510d1fec758c921cc22b0e17e63aaf4bcb5ed66304de9cf809bd274ca73bab4af5a6e9c76a4bc09e76eae8991ef5ece45a";
/// Unix time of round 1.
pub const GENESIS_TIME: u64 = 1_692_803_367;
/// Seconds between rounds.
pub const PERIOD: u64 = 3;
/// Public relays that serve the chain's rounds. A relay is not trusted: what it returns is checked against
/// [`PUBLIC_KEY`].
pub const RELAYS: [&str; 3] = ["https://api.drand.sh", "https://api2.drand.sh", "https://drand.cloudflare.com"];

/// Bytes of a signature: a point of the group G1, compressed.
const SIGNATURE_BYTES: usize = 48;
/// The domain of the chain's signatures (RFC 9380, BLS on G1).
const DOMAIN: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_";

/// Unix milliseconds at which a round's signature is published.
pub fn round_time(round: u64) -> Result<u64> {
    let seconds = round.checked_sub(1).and_then(|n| n.checked_mul(PERIOD)).and_then(|n| n.checked_add(GENESIS_TIME));
    match seconds.and_then(|n| n.checked_mul(1000)) {
        Some(ms) => Ok(ms),
        None => invalid(format!("drand round {round} is out of range")),
    }
}

/// The first round published at or after a time in Unix milliseconds: a seal to it never opens early.
#[must_use]
pub fn round_at_or_after(ms: u64) -> u64 {
    ms.saturating_sub(GENESIS_TIME * 1000).div_ceil(PERIOD * 1000) + 1
}

/// The path, on any relay, of a round's signature.
#[must_use]
pub fn round_path(round: u64) -> String {
    format!("/{CHAIN_HASH}/public/{round}")
}

/// A round's signature that has been checked against the chain's public key.
///
/// The only way to hold one is [`Beacon::verify`], so code that takes a `Beacon` never works with a signature
/// somebody merely claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Beacon {
    round: u64,
    signature: G1Affine,
}

impl Beacon {
    /// Checks that `signature` is quicknet's signature of `round`.
    pub fn verify(round: u64, signature: &[u8]) -> Result<Self> {
        round_time(round)?;
        if signature.len() != SIGNATURE_BYTES {
            return Err(Error::Beacon(round));
        }
        // Reading the point also checks that it is on the curve and in the right subgroup.
        let signature = G1Affine::deserialize_compressed(signature).map_err(|_| Error::Beacon(round))?;
        if Bls12_381::pairing(signature, G2Affine::generator())
            == Bls12_381::pairing(round_point(round)?, public_key()?)
        {
            Ok(Self { round, signature })
        } else {
            Err(Error::Beacon(round))
        }
    }

    /// Reads what a relay answers for a round, or a bare signature in hex, and checks it.
    pub fn parse(round: u64, text: &str) -> Result<Self> {
        let text = text.trim();
        let signature = if text.starts_with('{') {
            match serde_json::from_str::<serde_json::Value>(text)?.get("signature").and_then(serde_json::Value::as_str)
            {
                Some(hex) => from_hex(hex),
                None => invalid("the relay's answer has no signature"),
            }
        } else {
            from_hex(text)
        };
        Self::verify(round, &signature.map_err(|_| Error::Beacon(round))?)
    }

    /// The round this signature belongs to.
    #[must_use]
    pub fn round(&self) -> u64 {
        self.round
    }

    pub(crate) fn signature(&self) -> G1Affine {
        self.signature
    }
}

/// The chain's public key as a point.
pub(crate) fn public_key() -> Result<G2Affine> {
    G2Affine::deserialize_compressed(from_hex(PUBLIC_KEY)?.as_slice())
        .map_err(|_| Error::Sealed("the chain's public key does not read".into()))
}

/// The point a round's signature signs: the round number, hashed, mapped to the curve.
pub(crate) fn round_point(round: u64) -> Result<G1Affine> {
    let failed = |_| Error::Sealed("hashing to the curve failed".into());
    let hasher =
        MapToCurveBasedHasher::<Projective<g1::Config>, DefaultFieldHasher<Sha256, 128>, WBMap<g1::Config>>::new(
            DOMAIN,
        )
        .map_err(failed)?;
    hasher.hash(&sha256(&round.to_be_bytes())).map_err(failed)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Round 1000 as quicknet published it.
    pub(crate) const ROUND: u64 = 1000;
    pub(crate) const SIGNATURE: &str =
        "b44679b9a59af2ec876b1a6b1ad52ea9b1615fc3982b19576350f93447cb1125e342b73a8dd2bacbe47e4b6b63ed5e39";

    #[test]
    fn a_real_signature_verifies() {
        let beacon = Beacon::verify(ROUND, &from_hex(SIGNATURE).unwrap()).unwrap();
        assert_eq!(beacon.round(), ROUND);
        assert_eq!(Beacon::parse(ROUND, &format!("  0x{SIGNATURE}\n")).unwrap(), beacon);
        let answer = format!("{{\"round\":1000,\"randomness\":\"00\",\"signature\":\"{SIGNATURE}\"}}");
        assert_eq!(Beacon::parse(ROUND, &answer).unwrap(), beacon);
    }

    #[test]
    fn a_signature_of_another_round_does_not() {
        let signature = from_hex(SIGNATURE).unwrap();
        for round in [ROUND - 1, ROUND + 1, 1, 34_000_000] {
            assert!(matches!(Beacon::verify(round, &signature), Err(Error::Beacon(r)) if r == round));
        }
        assert!(Beacon::verify(0, &signature).is_err());
        assert!(Beacon::verify(u64::MAX, &signature).is_err());
    }

    #[test]
    fn anything_that_is_not_the_signature_does_not() {
        let good = from_hex(SIGNATURE).unwrap();
        let mut bent = good.clone();
        bent[20] ^= 1;
        let generator =
            "97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb";
        let infinity = format!("c0{}", "00".repeat(47));
        for bad in [
            &bent[..],
            &good[..47],
            &[good.as_slice(), &[0]].concat(),
            &[],
            &from_hex(generator).unwrap(),
            &from_hex(&infinity).unwrap(),
            &[0xff; 48],
        ] {
            assert!(Beacon::verify(ROUND, bad).is_err(), "{bad:02x?}");
        }
        for bad in ["", "{}", "{\"signature\":7}", "{\"signature\":\"zz\"}", "not hex", "{", "[]"] {
            assert!(Beacon::parse(ROUND, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rounds_and_times_agree() {
        assert_eq!(round_time(1).unwrap(), 1_692_803_367_000);
        assert_eq!(round_time(2).unwrap(), 1_692_803_370_000);
        assert!(round_time(0).is_err());
        assert!(round_time(u64::MAX).is_err());
        assert_eq!(round_at_or_after(0), 1);
        assert_eq!(round_at_or_after(1_692_803_367_000), 1);
        assert_eq!(round_at_or_after(1_692_803_367_001), 2);
        assert_eq!(round_at_or_after(1_692_803_370_000), 2);
        assert_eq!(round_at_or_after(u64::MAX), (u64::MAX - 1_692_803_367_000).div_ceil(3000) + 1);
        // A seal never opens before the time asked for, and never a whole round late.
        for ms in [1_692_803_367_000, 1_795_528_800_000, 1_795_528_800_001, 1_795_528_802_999, 4_102_444_800_123] {
            let round = round_at_or_after(ms);
            let opens = round_time(round).unwrap();
            assert!(opens >= ms && opens - ms < 3000, "{ms}");
        }
    }

    #[test]
    fn the_public_key_is_a_point() {
        assert!(public_key().is_ok());
        assert_eq!(round_path(1000), "/52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971/public/1000");
    }
}
