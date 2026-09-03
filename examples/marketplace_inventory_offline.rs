//! Offline marketplace inventory delta (no Docker).
//!
//! Mirrors loyalty_rewards merkle: SKU stock leaves → root_1, stock mutation →
//! root_2, inclusion proof still verifies against the new root.
//!
//! ```sh
//! cargo run -p ict-rs --example marketplace_inventory_offline
//! ```

use sha2::{Digest, Sha256};

fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

fn merkle_hash_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    if leaves.is_empty() {
        return [0u8; 32];
    }
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::new();
        for i in (0..level.len()).step_by(2) {
            let left = &level[i];
            let right = if i + 1 < level.len() {
                &level[i + 1]
            } else {
                &level[i]
            };
            next.push(merkle_hash_pair(left, right));
        }
        level = next;
    }
    level[0]
}

fn merkle_proof(leaves: &[[u8; 32]], index: usize) -> Vec<([u8; 32], bool)> {
    if leaves.len() <= 1 {
        return vec![];
    }
    let mut proof = Vec::new();
    let mut level = leaves.to_vec();
    let mut idx = index;
    while level.len() > 1 {
        if level.len() % 2 != 0 {
            let last = *level.last().unwrap();
            level.push(last);
        }
        let sibling_idx = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
        let is_right = idx % 2 == 0;
        proof.push((level[sibling_idx], is_right));
        let mut next = Vec::new();
        for i in (0..level.len()).step_by(2) {
            next.push(merkle_hash_pair(&level[i], &level[i + 1]));
        }
        level = next;
        idx /= 2;
    }
    proof
}

fn verify_merkle_proof(leaf: &[u8; 32], proof: &[([u8; 32], bool)], root: &[u8; 32]) -> bool {
    let mut current = *leaf;
    for (sibling, is_right) in proof {
        current = if *is_right {
            merkle_hash_pair(&current, sibling)
        } else {
            merkle_hash_pair(sibling, &current)
        };
    }
    &current == root
}

fn sku_leaf(sku: &str, stock: u64, unit_price: u128) -> [u8; 32] {
    sha256(format!("{sku}|{stock}|{unit_price}").as_bytes())
}

fn main() {
    let mut inventory = vec![
        ("SKU-TEA", 40u64, 1_000u128),
        ("SKU-MUG", 12, 2_500),
        ("SKU-HAT", 7, 4_000),
        ("SKU-BAG", 3, 9_000),
    ];
    let leaves_v1: Vec<[u8; 32]> = inventory
        .iter()
        .map(|(s, q, p)| sku_leaf(s, *q, *p))
        .collect();
    let root_1 = merkle_root(&leaves_v1);
    let tea_idx = 0;
    let proof_v1 = merkle_proof(&leaves_v1, tea_idx);
    assert!(
        verify_merkle_proof(&leaves_v1[tea_idx], &proof_v1, &root_1),
        "root_1 must include SKU-TEA"
    );

    inventory[0].1 = 39;
    let leaves_v2: Vec<[u8; 32]> = inventory
        .iter()
        .map(|(s, q, p)| sku_leaf(s, *q, *p))
        .collect();
    let root_2 = merkle_root(&leaves_v2);
    assert_ne!(root_1, root_2, "stock delta must move the inventory root");
    let proof_v2 = merkle_proof(&leaves_v2, tea_idx);
    assert!(
        verify_merkle_proof(&leaves_v2[tea_idx], &proof_v2, &root_2),
        "root_2 must include updated SKU-TEA"
    );
    assert!(
        !verify_merkle_proof(&leaves_v1[tea_idx], &proof_v1, &root_2),
        "stale leaf+proof must not verify against root_2"
    );

    println!(
        "marketplace_inventory_offline ok root_1={} root_2={} sku=SKU-TEA stock 40→39",
        hex::encode(root_1),
        hex::encode(root_2)
    );
}
