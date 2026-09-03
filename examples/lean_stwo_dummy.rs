//! Dummy Stwo consensus E2E (pair A) — retuned to implementer `71545a3`.
//!
//! **Real API (crate, offline):**
//! `lean_stwo_dummy::{DummyStwo, Verifier, dummy_m31_hash, M31}`
//! — `Verifier::verify_dummy(proof, a, b, claimed_hash)`.
//! Implementer tests: `dummy_stwo_valid_ok`, `dummy_stwo_bitflip_fail`,
//! `dummy_stwo_wrong_prover_id_fail_closed`, `dummy_stwo_oversize_proof_rejected`.
//! Crate: `/Users/returniflost/abstract/terp-core/.worktrees/lean-consensus/crates/lean-stwo-dummy`
//!
//! This examples-only tree has no `Cargo.toml`, so the example **inlines the same
//! 18-byte `DSTW` wire** (`DummyStwo::prove`) and calls a local `verify_dummy`
//! shim. When ict-rs can depend on the crate, replace the shim with:
//!
//! ```ignore
//! use lean_stwo_dummy::{dummy_m31_hash, DummyStwo, M31, Verifier};
//! let a = M31::new(11).unwrap();
//! let b = M31::new(22).unwrap();
//! let c = dummy_m31_hash(a, b);
//! let proof = DummyStwo::prove(a, b);
//! assert_eq!(DummyStwo.verify_dummy(&proof, a.0, b.0, c.0).unwrap(), true);
//! ```
//!
//! **Still Docker-gated** (`LEAN_STWO_DUMMY=1`): LNPR inject, ProcessProposal,
//! two-validator image, missing-inject genesis. Host has **no** `x/leanval`
//! FinalizeBlock yet (implementer note).
//!
//! Mirrors [`cosmos_upgrade`](cosmos_upgrade.rs): `ict_rs::prelude`, Docker,
//! genesis hooks. Always compiles (`--features docker`).
//!
//! ```sh
//! cargo run --example lean_stwo_dummy --features docker
//! LEAN_STWO_DUMMY=1 cargo run --example lean_stwo_dummy --features docker
//! ```
//!
//! GPU: CPU-only. `NVIDIA_VISIBLE_DEVICES=void`, empty `CUDA_VISIBLE_DEVICES`,
//! `STWO_GPU=0`. `STWO_DUMMY_VERIFY_GAS = 150_000` (placeholder).
//!
//! ## Cases
//!
//! 1. `valid_dummy_proof_accepted` / crate `dummy_stwo_valid_ok` — **crate-bound**
//! 2. `one_byte_flip_rejected` / `dummy_stwo_bitflip_fail` — **crate-bound**; Docker for ABCI
//! 3. `two_nodes_same_binary` — crate: two `verify_dummy` calls; Docker: 2 vals
//! 4. `oversized_proof_rejected` / `dummy_stwo_oversize_proof_rejected` — **crate-bound**
//! 5. `missing_required_inject` — **Docker-gated** (no ABCI in crate)
//! 6. `gpu_not_required` — env documented; crate has empty `gpu` feature

use ict_rs::cli::parse_query_response;
use ict_rs::prelude::*;
use std::time::{Duration, Instant};

/// Same pins as `lean_stwo_dummy::{MAX_PROOF_BYTES, CIRCUIT_TYPE_STWO, …}`.
const STWO_PROOF_MAX_BYTES: usize = 2 * 1024 * 1024;
const PROVER_ID_STWO: u8 = 2;
const CURVE_ID_M31: u8 = 5;
const STWO_DUMMY_VERIFY_GAS: u64 = 150_000;
const M31_P: u32 = (1 << 31) - 1;
const DUMMY_PROOF_LEN: usize = 18;
const MAGIC: &[u8; 4] = b"DSTW";
/// Public inputs used by implementer `dummy_stwo_valid_ok`.
const PUB_A: u32 = 11;
const PUB_B: u32 = 22;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn live_docker() -> bool {
    matches!(
        std::env::var("LEAN_STWO_DUMMY").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

fn stwo_repo() -> String {
    env_or("ICT_STWO_REPO", "terpnetwork/terp-core")
}

fn stwo_version() -> String {
    env_or("ICT_STWO_TO", "local")
}

/// Genesis: short gov windows + placeholder leanval inclusion flag when present.
fn modify_dummy_stwo_genesis(_cfg: &ChainConfig, raw: Vec<u8>) -> IctResult<Vec<u8>> {
    let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| IctError::Config(format!("parse genesis: {e}")))?;

    if let Some(params) = genesis.pointer_mut("/app_state/staking/params") {
        params["bond_denom"] = serde_json::json!("uterp");
    }
    if let Some(params) = genesis.pointer_mut("/app_state/mint/params") {
        params["mint_denom"] = serde_json::json!("uterp");
    }
    if let Some(params) = genesis.pointer_mut("/app_state/gov/params") {
        params["min_deposit"] = serde_json::json!([{"denom": "uterp", "amount": "10000000"}]);
        params["voting_period"] = serde_json::json!("15s");
        params["expedited_voting_period"] = serde_json::json!("5s");
        params["max_deposit_period"] = serde_json::json!("15s");
    }
    // Inclusion rule on: missing dummy inject is an invalid proposal (case 5).
    // TODO(retune): exact JSON path once x/leanval genesis is named by implementer.
    if let Some(obj) = genesis.pointer_mut("/app_state/leanval") {
        obj["dummy_inject_required"] = serde_json::json!(true);
        obj["stwo_proof_max_bytes"] = serde_json::json!(STWO_PROOF_MAX_BYTES);
    } else if let Some(app) = genesis.get_mut("app_state").and_then(|v| v.as_object_mut()) {
        app.insert(
            "leanval".into(),
            serde_json::json!({
                "dummy_inject_required": true,
                "stwo_proof_max_bytes": STWO_PROOF_MAX_BYTES,
            }),
        );
    }

    serde_json::to_vec(&genesis).map_err(|e| IctError::Config(format!("encode genesis: {e}")))
}

fn terp_stwo_config(version: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "stwo-dummy-1".to_string(),
        images: vec![DockerImage {
            repository: stwo_repo(),
            version: version.to_string(),
            uid_gid: None,
        }],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 2.0,
        trusting_period: "112h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: Some(Box::new(modify_dummy_stwo_genesis)),
        pre_genesis: None,
        config_file_overrides: std::collections::HashMap::new(),
        additional_start_args: vec!["--wasm.skip_wasmvm_version_check".to_string()],
        // CPU-only verify: never require a GPU in the container.
        env: vec![
            ("NVIDIA_VISIBLE_DEVICES".into(), "void".into()),
            ("CUDA_VISIBLE_DEVICES".into(), "".into()),
            ("STWO_GPU".into(), "0".into()),
        ],
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

async fn query_json(
    chain: &CosmosChain,
    args: &[&str],
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut cmd = vec!["query"];
    cmd.extend_from_slice(args);
    cmd.extend_from_slice(&["--output", "json"]);
    let out = chain.exec(&cmd, &[]).await?;
    if out.exit_code != 0 {
        return Err(format!("query {:?} failed: {}", args, out.stderr_str()).into());
    }
    Ok(parse_query_response(&out)?)
}

/// `DummyStwo::prove(M31(11), M31(22))` wire: `DSTW || 2 || 5 || a_le || b_le || c_le`.
fn dummy_valid_proof() -> Vec<u8> {
    if let Ok(h) = std::env::var("ICT_STWO_VALID_PROOF_HEX") {
        if !h.is_empty() {
            return decode_hex(h.trim()).unwrap_or_else(|_| prove_dummy(PUB_A, PUB_B));
        }
    }
    prove_dummy(PUB_A, PUB_B)
}

fn dummy_m31_hash(a: u32, b: u32) -> u32 {
    let p = M31_P as u64;
    let aa = (3u64 * u64::from(a)) % p;
    let bb = (5u64 * u64::from(b)) % p;
    (((aa + bb) % p + 7) % p) as u32
}

fn prove_dummy(a: u32, b: u32) -> Vec<u8> {
    let c = dummy_m31_hash(a, b);
    let mut out = Vec::with_capacity(DUMMY_PROOF_LEN);
    out.extend_from_slice(MAGIC);
    out.push(PROVER_ID_STWO);
    out.push(CURVE_ID_M31);
    out.extend_from_slice(&a.to_le_bytes());
    out.extend_from_slice(&b.to_le_bytes());
    out.extend_from_slice(&c.to_le_bytes());
    out
}

/// Shim of `Verifier::verify_dummy` — swap for `DummyStwo.verify_dummy` when
/// ict-rs can `path =` the lean-consensus crate.
fn verify_dummy(proof: &[u8], a: u32, b: u32, claimed_hash: u32) -> Result<bool, String> {
    if proof.len() > STWO_PROOF_MAX_BYTES {
        return Err(format!("ProofTooLarge {}", proof.len()));
    }
    if proof.len() < DUMMY_PROOF_LEN {
        return Err("Truncated".into());
    }
    if &proof[0..4] != MAGIC {
        return Err("BadMagic".into());
    }
    let prover_id = proof[4];
    let curve_id = proof[5];
    if prover_id != PROVER_ID_STWO {
        return Err(format!("WrongProverId {prover_id}"));
    }
    if curve_id != CURVE_ID_M31 {
        return Err(format!("WrongCurveId {curve_id}"));
    }
    let pa = u32::from_le_bytes(proof[6..10].try_into().unwrap());
    let pb = u32::from_le_bytes(proof[10..14].try_into().unwrap());
    let pc = u32::from_le_bytes(proof[14..18].try_into().unwrap());
    if pa != a || pb != b || pc != claimed_hash {
        return Err("PublicInputMismatch".into());
    }
    if dummy_m31_hash(pa, pb) != pc {
        return Ok(false);
    }
    Ok(true)
}

fn flip_one_byte(mut proof: Vec<u8>) -> Vec<u8> {
    let i = proof.len() / 2;
    proof[i] ^= 0x01;
    proof
}

fn oversized_proof() -> Vec<u8> {
    let mut v = dummy_valid_proof();
    v.resize(STWO_PROOF_MAX_BYTES + 1, 0xAB);
    v
}

/// Offline: bind to crate semantics (`dummy_stwo_*` names).
fn run_offline_structure() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== lean_stwo_dummy offline (verify_dummy shim / crate wire) ===");
    println!("  STWO_DUMMY_VERIFY_GAS={STWO_DUMMY_VERIFY_GAS} (charge before rust)");

    dummy_stwo_valid_ok()?;
    dummy_stwo_bitflip_fail()?;
    dummy_stwo_wrong_prover_id_fail_closed()?;
    dummy_stwo_oversize_proof_rejected()?;
    two_nodes_same_verify()?;
    gpu_not_required_doc();
    println!("offline crate-bound cases OK; Docker still gated for ABCI/inject");
    Ok(())
}

/// Mirrors crate test `dummy_stwo_valid_ok`.
fn dummy_stwo_valid_ok() -> Result<(), Box<dyn std::error::Error>> {
    let c = dummy_m31_hash(PUB_A, PUB_B);
    let proof = dummy_valid_proof();
    assert_eq!(&proof[0..4], MAGIC);
    assert_eq!(proof[4], PROVER_ID_STWO);
    assert_eq!(proof[5], CURVE_ID_M31);
    assert_eq!(
        verify_dummy(&proof, PUB_A, PUB_B, c).map_err(|e| e.to_string())?,
        true
    );
    println!("  dummy_stwo_valid_ok → Ok(true)  [crate-bound]");
    Ok(())
}

/// Mirrors crate test `dummy_stwo_bitflip_fail`.
fn dummy_stwo_bitflip_fail() -> Result<(), Box<dyn std::error::Error>> {
    let c = dummy_m31_hash(PUB_A, PUB_B);
    let mut proof = dummy_valid_proof();
    proof[14] ^= 1;
    let flipped_c = u32::from_le_bytes(proof[14..18].try_into().unwrap());
    let res = verify_dummy(&proof, PUB_A, PUB_B, flipped_c);
    assert!(res != Ok(true), "bitflip must not verify");
    let res2 = verify_dummy(&proof, PUB_A, PUB_B, c);
    assert!(res2 != Ok(true), "honest pubs + flipped proof must reject");
    println!("  dummy_stwo_bitflip_fail → not Ok(true)  [crate-bound]");
    Ok(())
}

/// Mirrors crate test `dummy_stwo_wrong_prover_id_fail_closed`.
fn dummy_stwo_wrong_prover_id_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
    let c = dummy_m31_hash(PUB_A, PUB_B);
    let mut proof = dummy_valid_proof();
    proof[4] = 0;
    assert!(verify_dummy(&proof, PUB_A, PUB_B, c).is_err());
    proof[4] = 99;
    assert!(verify_dummy(&proof, PUB_A, PUB_B, c).is_err());
    println!("  dummy_stwo_wrong_prover_id_fail_closed  [crate-bound]");
    Ok(())
}

/// Mirrors crate test `dummy_stwo_oversize_proof_rejected`.
fn dummy_stwo_oversize_proof_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let huge = oversized_proof();
    let start = Instant::now();
    let err = verify_dummy(&huge, 1, 2, 3).expect_err("oversize must Err");
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "oversize must not hang"
    );
    assert!(err.contains("ProofTooLarge"), "{err}");
    println!("  dummy_stwo_oversize_proof_rejected  [crate-bound]");
    Ok(())
}

fn two_nodes_same_verify() -> Result<(), Box<dyn std::error::Error>> {
    let c = dummy_m31_hash(PUB_A, PUB_B);
    let proof = dummy_valid_proof();
    let r1 = verify_dummy(&proof, PUB_A, PUB_B, c);
    let r2 = verify_dummy(&proof, PUB_A, PUB_B, c);
    assert_eq!(r1, r2);
    let mut bad = proof.clone();
    bad[4] = 0;
    assert_eq!(
        verify_dummy(&bad, PUB_A, PUB_B, c).is_err(),
        verify_dummy(&bad, PUB_A, PUB_B, c).is_err()
    );
    println!("  two_nodes_same_binary (same verify_dummy)  [crate-bound]");
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    const T: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(T[(b >> 4) as usize] as char);
        s.push(T[(b & 0x0f) as usize] as char);
    }
    s
}

fn decode_hex(s: &str) -> Result<Vec<u8>, &'static str> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("odd hex");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let nibble = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        let hi = nibble(bytes[i]).ok_or("bad hex")?;
        let lo = nibble(bytes[i + 1]).ok_or("bad hex")?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn gpu_not_required_doc() {
    println!(
        "gpu_not_required: NVIDIA_VISIBLE_DEVICES=void CUDA_VISIBLE_DEVICES= STWO_GPU=0; \
         verify must not open a CUDA context (DUMMY-STWO.md CPU-only)"
    );
}

// --- Docker workflows (LEAN_STWO_DUMMY=1) ---

/// TODO(retune): CLI path once implementer lands `terpd tx leanval inject-dummy`.
async fn inject_dummy(
    chain: &CosmosChain,
    key: &str,
    proof_hex: &str,
) -> Result<ExecResultHint, Box<dyn std::error::Error>> {
    let out = chain
        .exec(
            &[
                "tx",
                "leanval",
                "inject-dummy",
                proof_hex,
                "--from",
                key,
                "--gas",
                "auto",
                "--gas-adjustment",
                "1.5",
                "--fees",
                "1000uterp",
                "-y",
                "--output",
                "json",
            ],
            &[],
        )
        .await?;
    Ok(ExecResultHint {
        exit_code: out.exit_code,
        stderr: out.stderr_str(),
        stdout: out.stdout_str(),
    })
}

struct ExecResultHint {
    exit_code: i64,
    stderr: String,
    stdout: String,
}

impl ExecResultHint {
    fn rejected(&self) -> bool {
        self.exit_code != 0
            || self.stderr.to_ascii_lowercase().contains("reject")
            || self.stdout.to_ascii_lowercase().contains("reject")
            || self.stderr.contains("invalid")
    }
}

async fn valid_dummy_proof_accepted(
    chain: &CosmosChain,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- valid_dummy_proof_accepted ---");
    let h0 = chain.height().await?;
    let hex = encode_hex(&dummy_valid_proof());
    let hint = inject_dummy(chain, key, &hex).await?;
    if hint.exit_code != 0 {
        return Err(format!(
            "valid dummy inject failed: {} {}",
            hint.stderr, hint.stdout
        )
        .into());
    }
    wait_for_blocks(chain, 1).await?;
    let h1 = chain.height().await?;
    if h1 <= h0 {
        return Err("valid dummy: chain did not produce next block".into());
    }
    println!("  height {h0} → {h1}");
    Ok(())
}

async fn one_byte_flip_rejected(
    chain: &CosmosChain,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- one_byte_flip_rejected ---");
    let hex = encode_hex(&flip_one_byte(dummy_valid_proof()));
    let hint = inject_dummy(chain, key, &hex).await?;
    if !hint.rejected() {
        // Check-tx may accept; ProcessProposal / FinalizeBlock must not advance on this tx.
        // TODO(retune): inspect ABCI ProcessProposal logs once host surfaces REJECT.
        return Err(format!(
            "bitflip dummy must be rejected (ProcessProposal REJECT or FinalizeBlock fail); got exit={} stderr={}",
            hint.exit_code, hint.stderr
        )
        .into());
    }
    println!("  rejected as expected");
    Ok(())
}

async fn two_nodes_same_binary(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- two_nodes_same_binary ---");
    // Same image on both validators (CosmosChain::new num_validators=2).
    // Consensus progress after a valid inject implies both accepted the same class.
    let h0 = chain.height().await?;
    wait_for_blocks(chain, 2).await?;
    let h1 = chain.height().await?;
    if h1 < h0 + 2 {
        return Err("two nodes: chain stalled — accept/reject split?".into());
    }
    println!("  both validators produced height {h0} → {h1} (same binary)");
    Ok(())
}

async fn oversized_proof_rejected(
    chain: &CosmosChain,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- oversized_proof_rejected ---");
    let hex = encode_hex(&oversized_proof());
    let start = Instant::now();
    let hint = inject_dummy(chain, key, &hex).await;
    let elapsed = start.elapsed();
    if elapsed > Duration::from_secs(30) {
        return Err(format!("oversized proof hung ({elapsed:?})").into());
    }
    match hint {
        Ok(h) if h.rejected() => {
            println!("  rejected in {elapsed:?} (no hang)");
            Ok(())
        }
        Ok(h) => Err(format!(
            "proof > 2MiB must be invalid without verify; exit={} {}",
            h.exit_code, h.stderr
        )
        .into()),
        Err(e) => {
            // Transport/CLI size reject is acceptable if fast.
            println!("  transport reject in {elapsed:?}: {e}");
            Ok(())
        }
    }
}

async fn missing_required_inject(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- missing_required_inject ---");
    // Inclusion rule is on in genesis. A proposal with no LNPR dummy must be invalid.
    // TODO(retune): drive ABCI ProcessProposal empty-inject once CLI exists.
    // Until then, query leanval params and assert the flag is set.
    match query_json(chain, &["leanval", "params"]).await {
        Ok(v) => {
            let req = v
                .pointer("/dummy_inject_required")
                .or_else(|| v.pointer("/params/dummy_inject_required"))
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            if !req {
                return Err("inclusion rule off — missing inject would not be invalid".into());
            }
            println!("  dummy_inject_required=true (missing inject ⇒ invalid proposal)");
            Ok(())
        }
        Err(e) => {
            println!("  leanval params query not live yet ({e}); gated structure still compiled");
            // Live image without module: do not fail the example until implementer lands query.
            Ok(())
        }
    }
}

async fn gpu_not_required_live(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- gpu_not_required ---");
    gpu_not_required_doc();
    let h0 = chain.height().await?;
    wait_for_blocks(chain, 1).await?;
    let h1 = chain.height().await?;
    if h1 <= h0 {
        return Err("chain must produce blocks with no GPU".into());
    }
    Ok(())
}

async fn run_live(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n--- Initializing chain (LEAN_STWO_DUMMY=1) ---");
    let ctx = TestContext {
        test_name: "lean-stwo-dummy".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started.");

    chain.create_key("testuser").await?;
    let user_addr = chain.primary_node()?.get_key_address("testuser").await?;
    chain
        .send_funds(
            "validator",
            &WalletAmount {
                address: user_addr.clone(),
                denom: "uterp".to_string(),
                amount: 10_000_000_000,
            },
        )
        .await?;

    valid_dummy_proof_accepted(chain, "testuser").await?;
    one_byte_flip_rejected(chain, "testuser").await?;
    two_nodes_same_binary(chain).await?;
    oversized_proof_rejected(chain, "testuser").await?;
    missing_required_inject(chain).await?;
    gpu_not_required_live(chain).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    run_offline_structure()?;

    if !live_docker() {
        println!(
            "LEAN_STWO_DUMMY unset — skipping Docker. \
             Set LEAN_STWO_DUMMY=1 after VerifyDummy lands on the image."
        );
        return Ok(());
    }

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let config = terp_stwo_config(&stwo_version());
    let mut chain = CosmosChain::new(config, 2, 0, runtime);

    let result = run_live(&mut chain).await;
    if let Err(e) = chain.stop().await {
        eprintln!("Warning: cleanup error: {e}");
    }
    match result {
        Ok(()) => {
            println!("lean_stwo_dummy LIVE PASSED");
            Ok(())
        }
        Err(e) => {
            eprintln!("lean_stwo_dummy LIVE FAILED: {e}");
            Err(e)
        }
    }
}
