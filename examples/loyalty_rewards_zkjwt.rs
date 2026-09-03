//! Curated zk-jwt elevation of loyalty_rewards (design + contract path; chain optional).
//!
//! Offline: SHA-256 merkle of loyalty points + structural zk-jwt public-input
//! layout used by terp-zkjwt (nullifier || claim_commitment || msg_bind).
//! Does not require Docker or a linked host circuit.
//!
//! ```sh
//! cargo run -p ict-rs --example loyalty_rewards_zkjwt
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

/// terp-zkjwt `build_public_inputs` layout: nullifier || claim_commitment || msg_bind.
fn zkjwt_public_inputs(nullifier: &[u8; 32], claim: &[u8; 32], msg_bind: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    out.extend_from_slice(nullifier);
    out.extend_from_slice(claim);
    out.extend_from_slice(msg_bind);
    out
}

fn main() {
    let members = [
        ("jane", 875u64),
        ("bob", 1_525),
        ("ada", 410),
        ("lin", 90),
    ];
    let leaves: Vec<[u8; 32]> = members
        .iter()
        .map(|(n, pts)| sha256(format!("{n}|{pts}").as_bytes()))
        .collect();
    let root = merkle_root(&leaves);

    let jane_claim = sha256(b"jane|875");
    assert_eq!(jane_claim, leaves[0]);
    let nullifier = sha256(b"nf|sess-loyalty|jane");
    let msg_bind = sha256(b"account|terp1loyaltyjane");
    let publics = zkjwt_public_inputs(&nullifier, &jane_claim, &msg_bind);
    assert_eq!(publics.len(), 96);
    assert_eq!(&publics[0..32], nullifier.as_slice());
    assert_eq!(&publics[32..64], jane_claim.as_slice());
    assert_eq!(&publics[64..96], msg_bind.as_slice());

    let replay = sha256(b"nf|sess-loyalty|jane");
    assert_eq!(nullifier, replay, "same session+subject must bind the same nf");

    println!(
        "loyalty_rewards_zkjwt ok merkle_root={} nullifier={} claim={} publics_len={}",
        hex::encode(root),
        hex::encode(nullifier),
        hex::encode(jane_claim),
        publics.len()
    );
}
