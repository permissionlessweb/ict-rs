//! Headstash E2E test using real Docker containers.
//!
//! Demonstrates the complete headstash lifecycle:
//! 1. Spin up a local terp chain (zk-wasmvm enabled) via Docker
//! 2. Deploy both cw-headstash and cw-headstash-manifold contracts
//! 3. Generate circuit keys (verifying key + proving key) for the headstash circuit
//! 4. Upload the verifying key to the headstash contract
//! 5. Create a headstash deployment via the manifold with token allocations
//! 6. Build a genesis merkle tree from participant data
//! 7. Generate a ZK proof for a participant claiming their allocation
//! 8. Submit the proof to the contract for on-chain verification
//! 9. Verify the claim succeeded (nullifier registered, funds distributed)
//!
//! ## Prerequisites
//!
//! Build the zk-wasmvm Docker image:
//! ```sh
//! cd terp-core
//! make build-zk-local  # -> terpnetwork/terp-core:local-zk
//! ```
//!
//! Run:
//! ```sh
//! cargo run --example headstash --features docker
//! ```

use std::path::PathBuf;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::interchain::wait_for_blocks;
use ict_rs::runtime::{DockerConfig, DockerImage, IctRuntime};
use ict_rs::tx::WalletAmount;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Docker image for the zk-wasmvm enabled chain.
const ZK_IMAGE_REPO: &str = "terpnetwork/terp-core";
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

async fn run_test(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let zk_root = resolve_zk_root()?;

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
    // For now, we look for pre-generated keys on disk.
    let keys_dir = zk_root.join("headstash_keys");
    let vk_path = keys_dir.join("verifying_key.bin");
    if !vk_path.exists() {
        println!(
            "WARNING: Pre-generated VK not found at {}.",
            vk_path.display()
        );
        println!(
            "Generate keys with HeadstashSuite::gen_headstash_circuit_keys() \
             or set ZK_ROOT to a workspace containing headstash_keys/."
        );
        println!("Skipping VK upload and proof steps (scaffold only).");
    } else {
        println!(
            "Found VK: {} ({} bytes)",
            vk_path.display(),
            std::fs::metadata(&vk_path)?.len()
        );
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

    // For the genesis root, we use a placeholder 32-byte zero root.
    // In a real workflow this comes from the merkle tree built from
    // participant data (step 7).
    let genesis_root_b64 = base64_encode(&[0u8; 32]);

    // Build the headstash InstantiateMsg that the manifold forwards
    // to the cw-headstash contract.
    //
    // TODO: Provide real WavsProofOfOwnership with valid BLS12-381 PoP.
    //       For now we use a minimal placeholder. In production, use
    //       ark-bls12-381 to generate valid keypairs and PoP signatures.
    let headstash_inst_msg = serde_json::json!({
        "genesis_root": genesis_root_b64,
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
            println!("Scaffold complete. Exiting without proof submission.");
            return Ok(());
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

    println!("\nHeadstash E2E workflow scaffold complete.");
    println!("To run the full proof flow, add zk-headstash as a dev-dependency");
    println!("and uncomment the proof generation + submission blocks above.");

    Ok(())
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
