//! A Merkle tree over the answers of an exam. Its root is what gets published, and a short proof then shows
//! that one answer was part of it.

use serde::{Deserialize, Serialize};

use crate::hash::{from_hex, same_hex, sha256, to_hex};
use crate::{Result, invalid};

/// A SHA-256 digest.
pub type Hash = [u8; 32];

// A different first byte for leaves and for inner nodes (as in RFC 6962), so that a leaf can never be passed
// off as an inner node.
const LEAF: u8 = 0;
const NODE: u8 = 1;

/// The hash of one leaf's data.
#[must_use]
pub fn leaf_hash(data: &[u8]) -> Hash {
    let mut bytes = Vec::with_capacity(1 + data.len());
    bytes.push(LEAF);
    bytes.extend_from_slice(data);
    sha256(&bytes)
}

fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut bytes = [0u8; 65];
    bytes[0] = NODE;
    bytes[1..33].copy_from_slice(left);
    bytes[33..].copy_from_slice(right);
    sha256(&bytes)
}

/// Every level of the tree, leaves first. An odd last node moves up unchanged; it is never duplicated.
fn levels(leaves: &[Hash]) -> Result<Vec<Vec<Hash>>> {
    if leaves.is_empty() {
        return invalid("merkle: no leaves");
    }
    let mut out = vec![leaves.to_vec()];
    while let Some(level) = out.last().filter(|level| level.len() > 1) {
        let next = level
            .chunks(2)
            .map(|pair| if let [left, right] = pair { node_hash(left, right) } else { pair[0] })
            .collect();
        out.push(next);
    }
    Ok(out)
}

/// The root of the tree over these leaf hashes.
pub fn merkle_root(leaves: &[Hash]) -> Result<Hash> {
    let levels = levels(leaves)?;
    match levels.last().and_then(|top| top.first()) {
        Some(root) => Ok(*root),
        None => invalid("merkle: no leaves"),
    }
}

/// Which side of the path a sibling hash sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    /// The sibling is the left child; the path is the right one.
    Left,
    /// The sibling is the right child; the path is the left one.
    Right,
}

/// One step from a leaf towards the root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProofStep {
    /// The hash to combine with, as `0x` hex.
    pub sibling: String,
    /// Where that hash goes.
    pub side: Side,
}

/// The steps that lead from the leaf at `index` to the root.
pub fn merkle_proof(leaves: &[Hash], index: usize) -> Result<Vec<ProofStep>> {
    if index >= leaves.len() {
        return invalid("merkle: no leaf at that index");
    }
    let levels = levels(leaves)?;
    let mut proof = Vec::new();
    let mut i = index;
    for level in &levels[..levels.len() - 1] {
        let (sibling, side) = if i % 2 == 1 { (i - 1, Side::Left) } else { (i + 1, Side::Right) };
        if let Some(hash) = level.get(sibling) {
            proof.push(ProofStep { sibling: to_hex(hash), side });
        }
        i /= 2;
    }
    Ok(proof)
}

/// Whether the proof leads from this leaf hash to the root.
#[must_use]
pub fn verify_proof(leaf: &Hash, proof: &[ProofStep], root: &str) -> bool {
    let mut hash = *leaf;
    for step in proof {
        let Ok(Ok(sibling)) = from_hex(&step.sibling).map(Hash::try_from) else {
            return false;
        };
        hash = match step.side {
            Side::Left => node_hash(&sibling, &hash),
            Side::Right => node_hash(&hash, &sibling),
        };
    }
    same_hex(&to_hex(&hash), root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaves(n: usize) -> Vec<Hash> {
        (0..n).map(|i| leaf_hash(format!("leaf {i}").as_bytes())).collect()
    }

    #[test]
    fn every_leaf_proves_into_the_root_for_every_size() {
        for n in 1..=33 {
            let all = leaves(n);
            let root = to_hex(&merkle_root(&all).unwrap_or_default());
            for (i, leaf) in all.iter().enumerate() {
                let proof = merkle_proof(&all, i).unwrap_or_default();
                assert!(verify_proof(leaf, &proof, &root), "size {n}, leaf {i}");
                assert!(verify_proof(leaf, &proof, &root.to_uppercase().replace("0X", "0x")));
            }
        }
    }

    #[test]
    fn a_wrong_leaf_or_a_bent_proof_fails() {
        let all = leaves(7);
        let root = to_hex(&merkle_root(&all).unwrap_or_default());
        let proof = merkle_proof(&all, 3).unwrap_or_default();
        assert!(!verify_proof(&all[4], &proof, &root));
        let mut flipped = proof.clone();
        flipped[0].side = Side::Right;
        assert!(!verify_proof(&all[3], &flipped, &root));
        let mut short = proof.clone();
        short.pop();
        assert!(!verify_proof(&all[3], &short, &root));
        let mut broken = proof;
        broken[1].sibling = "0x12".into();
        assert!(!verify_proof(&all[3], &broken, &root));
    }

    #[test]
    fn the_root_depends_on_order_and_on_every_leaf() {
        let all = leaves(5);
        let root = merkle_root(&all).ok();
        let mut swapped = all.clone();
        swapped.swap(0, 1);
        assert_ne!(merkle_root(&swapped).ok(), root);
        assert_ne!(merkle_root(&all[..4]).ok(), root);
        assert_eq!(merkle_root(&all[..1]).ok(), Some(all[0]));
    }

    #[test]
    fn a_leaf_cannot_stand_in_for_a_node() {
        let all = leaves(2);
        let mut joined = Vec::new();
        joined.extend_from_slice(&all[0]);
        joined.extend_from_slice(&all[1]);
        assert_ne!(Some(leaf_hash(&joined)), merkle_root(&all).ok());
    }

    #[test]
    fn nothing_to_hash_is_an_error() {
        assert!(merkle_root(&[]).is_err());
        assert!(merkle_proof(&leaves(3), 3).is_err());
    }
}
