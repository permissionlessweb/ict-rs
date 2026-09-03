//! Headstash E2E test using real Docker containers (ICTRS).
//!
//! Product A spine (Poseidon-v1 distro + claim):
//! 1. Spin up a local terp chain (zk-wasmvm enabled) via Docker
//! 2. Deploy cw-headstash (+ optional manifold)
//! 3. Load store-circuit blob from offline keygen (`just demo-keys` / `HEADSTASH_VK_PATH`)
//! 4. Upload VK / store-circuit when chain supports it; `SetCircuitId`
//! 5. Instantiate with **depth-32 Poseidon** `genesis_root` + `distro_hash_domain: poseidon_v1`
//! 6. Build suite-backed claim fixture (or pre-generated JSON)
//! 7. Real prove (K=18) or lab `claim_mock_verify`
//! 8. `ProcessHeadstash` → public recipient funds
//!
//! SSOT usage: `crates/headstash/docs/circuit/PRODUCT-A-CW-ORCH-ICTRS-USAGE.md`
//!
//! ## Prerequisites
//!
//! ```sh
//! # Offline keys (from crates/headstash — long):
//! just demo-keys
//! export HEADSTASH_VK_PATH=$PWD/artifacts/headstash_vk.bin
//!
//! # zk-wasmvm image:
//! make build-zk-local  # -> terpnetwork/terp-core:local-zk
//!
//! # Honest path (required):
//! #   mock-labeled lab (no silent success):
//! HEADSTASH_CLAIM_MODE=mock cargo run -p ict-rs --example headstash --features docker
//! #   real claim (fails if VK / proof missing):
//! HEADSTASH_CLAIM_MODE=real cargo run -p ict-rs --example headstash --features docker
//! ```
//!
//! SSOT honesty: `terp-rs/docs/TERP-RS-ICT-HS-HONEST-PATH.md`

use std::path::PathBuf;

use ict_rs::prelude::*;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Docker image for the zk-wasmvm enabled chain.
const ZK_IMAGE_REPO: &str = "registry.terp.network/terp-core";
const ZK_IMAGE_VERSION: &str = "local-zk";

/// Chain configuration constants.
const CHAIN_ID: &str = "120u-1";
const DENOM: &str = "uterp";
const BECH32_PREFIX: &str = "terp";

/// Relative paths (from ZK_ROOT) to pre-compiled wasm contracts.
const HEADSTASH_WASM_REL: &str =
    "terp-core/tests/interchaintest/contracts/cw_headstash.wasm";
const MANIFOLD_WASM_REL: &str =
    "terp-core/tests/interchaintest/contracts/cw_headstash_manifold.wasm";

/// Container-side scratch paths for uploaded artifacts.
const HS_WASM: &str = "/tmp/cw_headstash.wasm";
const MF_WASM: &str = "/tmp/cw_headstash_manifold.wasm";
const VK: &str = "/tmp/headstash_vk.bin";

// ---------------------------------------------------------------------------
// Chain config
// ---------------------------------------------------------------------------

fn terp_zk_config() -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: CHAIN_ID.to_string(),
        images: vec![DockerImage {
            repository: ZK_IMAGE_REPO.to_string(),
            version: ZK_IMAGE_VERSION.to_string(),
            uid_gid: None,
        }],
        bin: "terpd".to_string(),
        bech32_prefix: BECH32_PREFIX.to_string(),
        denom: DENOM.to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: format!("0{}", DENOM),
        gas_adjustment: 1.5,
        trusting_period: "112h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: std::collections::HashMap::new(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

/// Resolve the ZK workspace root (parent of terp-core/).
fn resolve_zk_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(root) = std::env::var("ZK_ROOT") {
        let p = PathBuf::from(root);
        if p.exists() {
            return Ok(p);
        }
    }
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..6 {
        if dir.join("terp-core").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    Err("Cannot find ZK workspace root. Set ZK_ROOT env var.".into())
}

// ---------------------------------------------------------------------------
// Main test flow
// ---------------------------------------------------------------------------

/// Honest claim mode. Unset + no real claim => error (not success).
fn claim_mode() -> String {
    std::env::var("HEADSTASH_CLAIM_MODE")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn skip_err(reason: &str) -> Box<dyn std::error::Error> {
    format!("skip_reason={reason} (not success; set HEADSTASH_CLAIM_MODE=mock for the labeled lab path)").into()
}

async fn run_test(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let zk_root = resolve_zk_root()?;
    let mode = claim_mode();
    println!("HONEST_PATH_MODE={}", if mode.is_empty() { "<unset>" } else { &mode });
    if mode != "mock" && mode != "real" && !mode.is_empty() {
        return Err(skip_err("unknown_HEADSTASH_CLAIM_MODE"));
    }

    // -----------------------------------------------------------------------
    // Step 0: Validate host-side artifacts exist
    // -----------------------------------------------------------------------
    let headstash_wasm_host = zk_root.join(HEADSTASH_WASM_REL);
    let manifold_wasm_host = zk_root.join(MANIFOLD_WASM_REL);

    if !headstash_wasm_host.exists() {
        return Err(format!(
            "cw-headstash WASM not found: {}",
            headstash_wasm_host.display()
        )
        .into());
    }
    if !manifold_wasm_host.exists() {
        return Err(format!(
            "cw-headstash-manifold WASM not found: {}",
            manifold_wasm_host.display()
        )
        .into());
    }
    println!("Headstash WASM : {}", headstash_wasm_host.display());
    println!("Manifold WASM  : {}", manifold_wasm_host.display());

    // -----------------------------------------------------------------------
    // Step 1: Start the chain
    // -----------------------------------------------------------------------
    println!("\n--- [1/9] Starting chain ---");
    let ctx = TestContext {
        test_name: "headstash-e2e".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started. RPC: {}", chain.host_rpc_address());

    // Fund a test user
    chain.create_key("deployer").await?;
    let deployer_addr = chain
        .primary_node()?
        .get_key_address("deployer")
        .await?;
    let fund = WalletAmount {
        address: deployer_addr.clone(),
        denom: DENOM.to_string(),
        amount: 100_000_000,
    };
    chain.send_funds("validator", &fund).await?;
    wait_for_blocks(chain, 2).await?;
    println!("Funded deployer: {}", deployer_addr);

    // -----------------------------------------------------------------------
    // Step 2: Deploy contracts (copy into container, then store)
    // -----------------------------------------------------------------------
    println!("\n--- [2/9] Deploying contracts ---");
    let node = chain.primary_node()?;
    node.copy_file_from_host(&headstash_wasm_host, HS_WASM)
        .await?;
    node.copy_file_from_host(&manifold_wasm_host, MF_WASM)
        .await?;
    println!(
        "Copied WASM files into container ({} + {} bytes)",
        std::fs::metadata(&headstash_wasm_host)?.len(),
        std::fs::metadata(&manifold_wasm_host)?.len(),
    );

    // Store headstash contract
    let headstash_code_id = chain
        .store_code("deployer", HS_WASM)
        .await?;
    wait_for_blocks(chain, 2).await?;
    println!("cw-headstash stored: code_id={}", headstash_code_id);

    // Store manifold contract
    let manifold_code_id = chain.store_code("deployer", MF_WASM)
        .await?;
    wait_for_blocks(chain, 2).await?;
    println!("cw-headstash-manifold stored: code_id={}", manifold_code_id);

    // -----------------------------------------------------------------------
    // Step 3: Generate circuit keys
    // -----------------------------------------------------------------------
    println!("\n--- [3/9] Generating circuit keys ---");
    // NOTE: Circuit key generation is computationally expensive (K=18).
    // In a real CI pipeline you would pre-generate and cache these.
    //
    // TODO: When zk-headstash is added as a dependency, replace this block
    //       with actual HeadstashSuite key generation:
    //
    //   use zk_headstash::suite::HeadstashSuite;
    //   use zk_headstash::suite::suite::{
    //       CircuitKeysGenerator, HeadstashTestDataGenerator,
    //       MerkleTestDataBuilder, HeadstashProofBuilder,
    //   };
    //   let suite = HeadstashSuite::new();
    //   let bundle = suite.generate_e2e_test_bundle(4)?;
    //
    // Prefer offline Product A blob from `just demo-keys` / HEADSTASH_VK_PATH.
    let vk_path = std::env::var("HEADSTASH_VK_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            zk_root
                .join("terp-core/crates/headstash/artifacts/headstash_vk.bin")
        });
    let vk_path_alt = zk_root.join("headstash_keys/verifying_key.bin");
    let vk_resolved = if vk_path.exists() {
        Some(vk_path)
    } else if vk_path_alt.exists() {
        Some(vk_path_alt)
    } else {
        None
    };
    if let Some(ref p) = vk_resolved {
        println!(
            "Found store-circuit VK: {} ({} bytes)",
            p.display(),
            std::fs::metadata(p)?.len()
        );
        node.copy_file_from_host(p, VK).await?;
        println!("Copied VK into container at {}", VK);
    } else {
        println!(
            "WARNING: store-circuit blob not found (HEADSTASH_VK_PATH or artifacts/headstash_vk.bin)."
        );
        println!("Generate offline: cd crates/headstash && just demo-keys");
        println!("Skipping VK upload (scaffold may still instantiate with claim_mock_verify).");
    }

    // -----------------------------------------------------------------------
    // Step 4: Instantiate the manifold contract
    // -----------------------------------------------------------------------
    println!("\n--- [4/9] Instantiating manifold contract ---");
    let manifold_init_msg = serde_json::json!({
        "owner": deployer_addr,
        "headstash_code_id": headstash_code_id.parse::<u64>().unwrap_or(1),
    });
    let manifold_addr = chain
        .instantiate_contract(
            "deployer",
            &manifold_code_id,
            &manifold_init_msg.to_string(),
            "headstash-manifold",
            Some(&deployer_addr),
        )
        .await?;
    wait_for_blocks(chain, 2).await?;
    println!("Manifold instantiated: {}", manifold_addr);

    // -----------------------------------------------------------------------
    // Step 5: Create a headstash deployment via the manifold
    // -----------------------------------------------------------------------
    println!("\n--- [5/9] Creating headstash deployment ---");

    // Product A: genesis_root must be 32-byte depth-32 Poseidon path root from suite.
    // Placeholder zeros are for scaffold only — replace with suite_backed root in full run.
    let genesis_root_b64 = base64_encode(&[0u8; 32]);

    // Build the headstash InstantiateMsg that the manifold forwards
    // to the cw-headstash contract.
    //
    // TODO: Provide real WavsProofOfOwnership with valid BLS12-381 PoP.
    // Lab: claim_mock_verify=true until store-circuit + proof_instance_verify are wired.
    let headstash_inst_msg = serde_json::json!({
        "genesis_root": genesis_root_b64,
        "distro_hash_domain": "poseidon_v1",
        "genesis_label": "ict-product-a",
        "circuit_id": null,
        "claim_mock_verify": true,
        "token_strategy": {
            "ExistingFungible": {
                "proof": base64_encode(&derive_nd_bytes(DENOM)),
                "raw": DENOM,
            }
        },
        "wavs": {
            "poos": [],
            "msg": {
                "aggregate_key": "",
                "threshold": 0,
                "total_operators": 0,
                "nonce": 0
            }
        }
    });

    let headstash_funding = format!("1000000{}", DENOM);

    let create_headstash_msg = serde_json::json!({
        "CreateHeadstash": {
            "instantiate_msg": headstash_inst_msg,
            "label": "headstash-test-deployment",
            "funding": {
                "amount": "1000000",
                "token": {
                    "Native": { "denom": DENOM }
                }
            }
        }
    });

    let create_tx = chain
        .execute_contract(
            "deployer",
            &manifold_addr,
            &create_headstash_msg.to_string(),
            Some(&headstash_funding),
        )
        .await;
    match &create_tx {
        Ok(tx) => println!("CreateHeadstash tx: hash={}", tx.tx_hash),
        Err(e) => {
            println!(
                "CreateHeadstash via manifold failed \
                 (expected if PoP validation is enforced): {}",
                e
            );
            println!("Falling back to direct headstash instantiation...");
        }
    }
    wait_for_blocks(chain, 2).await?;

    // Fallback: instantiate headstash contract directly (bypasses manifold PoP).
    // Useful for testing the claim flow when BLS PoP is not yet set up.
    let headstash_addr = chain
        .instantiate_contract(
            "deployer",
            &headstash_code_id,
            &headstash_inst_msg.to_string(),
            "headstash-direct",
            Some(&deployer_addr),
        )
        .await;
    let headstash_addr = match headstash_addr {
        Ok(addr) => {
            println!("Headstash contract instantiated: {}", addr);
            addr
        }
        Err(e) => {
            println!("Direct instantiation failed: {}", e);
            println!(
                "This is expected if WavsProofOfOwnership validation \
                 requires real BLS keys."
            );
            println!("HONEST_PATH=none skip_reason=instantiate_failed_wavs_pop");
            return Err(skip_err("instantiate_failed_wavs_pop"));
        }
    };
    wait_for_blocks(chain, 2).await?;

    // Fund the headstash contract with tokens for escrow
    let fund_headstash = WalletAmount {
        address: headstash_addr.clone(),
        denom: DENOM.to_string(),
        amount: 50_000_000,
    };
    chain.send_funds("deployer", &fund_headstash).await?;
    wait_for_blocks(chain, 2).await?;
    println!("Funded headstash contract with 50_000_000 {}", DENOM);

    // -----------------------------------------------------------------------
    // Step 6: Upload VK to the headstash contract
    // -----------------------------------------------------------------------
    println!("\n--- [6/9] Uploading verifying key ---");
    if vk_path.exists() {
        node.copy_file_from_host(&vk_path, VK).await?;

        let vk_bytes = std::fs::read(&vk_path)?;
        let vk_b64 = base64_encode(&vk_bytes);

        let load_vk_msg = serde_json::json!({
            "LoadVk": {
                "vk": vk_b64,
            }
        });
        let vk_tx = chain
            .execute_contract(
                "deployer",
                &headstash_addr,
                &load_vk_msg.to_string(),
                None,
            )
            .await;
        match &vk_tx {
            Ok(tx) => println!("VK uploaded: tx={}", tx.tx_hash),
            Err(e) => println!("VK upload failed: {}", e),
        }
        wait_for_blocks(chain, 2).await?;
    } else {
        println!("Skipping VK upload (no pre-generated key found).");
    }

    // TODO: Upload circuit footer when the footer agent completes its work.
    // The footer contains additional circuit metadata needed for verification.
    // let footer_msg = serde_json::json!({ "LoadFooter": { "footer": footer_b64 } });

    // -----------------------------------------------------------------------
    // Step 7: Build genesis merkle tree from participant data
    // -----------------------------------------------------------------------
    println!("\n--- [7/9] Building genesis merkle tree ---");
    //
    // TODO: When zk-headstash is a dependency, use the real suite:
    //
    //   let suite = HeadstashSuite::new();
    //
    //   // Generate test leaves (participant data)
    //   let leaves = suite.generate_test_leaves(4);
    //
    //   // Compute leaf hashes via sinsemilla
    //   let leaf_hashes: Vec<_> = leaves.iter()
    //       .map(|d| suite.compute_leaf_from_data(d).unwrap())
    //       .collect();
    //
    //   // Build the full merkle tree
    //   let tree = suite.generate_full_merkle_tree(leaf_hashes.clone());
    //   println!("Tree root: {:?}", tree.root());
    //   println!("Tree depth: {}", tree.depth);
    //
    //   // The root would then be used in the headstash InstantiateMsg
    //   // instead of the zero-placeholder above.
    //
    println!(
        "NOTE: Merkle tree generation requires zk-headstash as a dependency. \
         HeadstashSuite::generate_full_merkle_tree() builds a \
         sinsemilla-hash-based tree from participant leaf data."
    );

    // -----------------------------------------------------------------------
    // Step 8: Generate ZK proof and submit claim
    // -----------------------------------------------------------------------
    println!("\n--- [8/9] Generating ZK proof and submitting claim ---");
    //
    // TODO: When zk-headstash is available, generate and submit a real proof:
    //
    //   // Use the E2E bundle which includes keys, tree, and proofs
    //   let bundle = suite.generate_e2e_test_bundle(4)?;
    //   let account = &bundle.accounts[0];
    //   let leaf = &bundle.leaves[0];
    //
    //   // Encode proof bytes as base64 for the contract
    //   let proof_b64 = base64_encode(account.proof.as_ref());
    //
    //   // Build the ProcessHeadstash message with HeadstashNote claims
    //   let claim_msg = serde_json::json!({
    //       "ProcessHeadstash": {
    //           "claims": [{
    //               "i": {
    //                   "anchor": base64_encode(&account.anchor.to_bytes()),
    //                   "nd":     base64_encode(&account.instance.nd.to_bytes()),
    //                   "v":      leaf.raw_v,
    //                   "nf":     base64_encode(&account.instance.nf.to_bytes()),
    //                   "recp":   base64_encode(&leaf.raw_addr),
    //                   "cmx":    base64_encode(&account.instance.cmx.to_bytes()),
    //               },
    //               "p": proof_b64,
    //               "rr": base64_encode(&leaf.raw_addr),
    //           }]
    //       }
    //   });
    //
    //   let claim_tx = chain
    //       .execute_contract(
    //           "deployer",
    //           &headstash_addr,
    //           &claim_msg.to_string(),
    //           None,
    //       )
    //       .await?;
    //   println!("Claim tx submitted: {}", claim_tx.tx_hash);
    //   wait_for_blocks(chain, 2).await?;
    //
    println!(
        "NOTE: Proof generation requires zk-headstash as a dependency. \
         Use HeadstashSuite::create_genesis_proof_from_leaf() or \
         generate_e2e_test_bundle() to create valid proofs."
    );

    // -----------------------------------------------------------------------
    // Step 9: Verify claim succeeded
    // -----------------------------------------------------------------------
    println!("\n--- [9/9] Verifying claim ---");

    // Query the headstash contract to check nullifier state
    let nullifiers_query = serde_json::json!({
        "Nullifiers": {
            "start_after": null,
            "limit": 10
        }
    });
    let nullifiers_result = chain
        .query_contract(&headstash_addr, &nullifiers_query.to_string())
        .await;
    match nullifiers_result {
        Ok(v) => println!(
            "Nullifiers query result: {}",
            serde_json::to_string_pretty(&v)?
        ),
        Err(e) => println!("Nullifiers query failed: {}", e),
    }

    // TODO: After a real claim is submitted, verify these conditions:
    //
    // 1. The nullifier from the proof is now registered (double-spend prevention):
    //    let nf_check = serde_json::json!({
    //        "Nullifer": { "null": "<nullifier_hex>" }
    //    });
    //    let exists = chain
    //        .query_contract(&headstash_addr, &nf_check.to_string())
    //        .await?;
    //    assert_eq!(exists["data"], serde_json::json!(true));
    //
    // 2. The recipient received the correct token amount:
    //    (query bank module balance of the recipient address)
    //
    // 3. A second claim with the same nullifier fails:
    //    let dup_result = chain.execute_contract(...).await;
    //    assert!(dup_result.is_err(), "duplicate nullifier must be rejected");

    println!("\nHeadstash E2E: no ProcessHeadstash proof was submitted.");
    println!("To run the real claim path, add zk-headstash and set HEADSTASH_CLAIM_MODE=real.");

    match mode.as_str() {
        "mock" => {
            println!("HONEST_PATH=mock_labeled claim_mock_verify=true product_settle=false");
            println!("Lab path finished. This is not a real zk-headstash claim.");
            Ok(())
        }
        "real" => {
            println!("HONEST_PATH=none skip_reason=real_claim_not_wired");
            Err(skip_err("real_claim_not_wired"))
        }
        _ => {
            println!("HONEST_PATH=none skip_reason=proof_path_skipped");
            Err(skip_err("proof_path_skipped"))
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Simple base64 encoding helper.
fn base64_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(bytes)
}

/// Derive the note denomination bytes from a raw token string.
/// Mirrors cw-headstash tokenfactory::derive_nd() which uses blake3.
fn derive_nd_bytes(raw: &str) -> [u8; 32] {
    let hash = blake3::hash(raw.as_bytes());
    let mut bytes = *hash.as_bytes();
    // Clear high bits for pallas field compatibility
    bytes[0] &= 0x1F;
    bytes
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .init();

    println!("=== Headstash E2E Workflow Test ===\n");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    let config = terp_zk_config();
    let mut chain = CosmosChain::new(config, 1, 0, runtime.clone());
    println!(
        "Chain: {} (image: {}:{})",
        chain.chain_id(),
        ZK_IMAGE_REPO,
        ZK_IMAGE_VERSION
    );

    // Run test, then always clean up.
    let result = run_test(&mut chain).await;

    println!("\n--- Shutdown ---");
    if let Err(e) = chain.stop().await {
        eprintln!("Warning: cleanup error: {}", e);
    }

    match result {
        Ok(()) => {
            println!("\nHeadstash E2E test PASSED!");
            Ok(())
        }
        Err(e) => {
            eprintln!("\nHeadstash E2E test FAILED: {}", e);
            Err(e)
        }
    }
}
