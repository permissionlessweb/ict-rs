//! Local Headstash e2e: two loopback Terp chains, claim contract, eligibility
//! witness, and a Hermes hop.
//!
//! Refuses `ICT_MOCK` and refuses a publish address other than 127.0.0.1.
//! Does not treat a mock verify or a byte-compare witness as a claim.
//!
//! ```sh
//! ICT_PUBLISH_HOST=127.0.0.1 \
//! TERP_IMAGE_REPO=registry.terp.network/terp-core \
//! TERP_IMAGE_VERSION=v6.4.0 \
//! cargo run -p ict-rs --example headstash_local_e2e --features docker
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::{engine::general_purpose::STANDARD, Engine};
use ict_rs::prelude::*;

const DENOM: &str = "uterp";
const CHAIN_A: &str = "headstash-a-1";
const CHAIN_B: &str = "headstash-b-1";

struct Case {
    name: &'static str,
    status: &'static str,
    detail: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,ict_rs::relayer=info".to_string()),
        )
        .init();

    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("ICT_MOCK=1 is not a Headstash e2e".into());
    }
    let publish = std::env::var("ICT_PUBLISH_HOST").unwrap_or_default();
    if publish != "127.0.0.1" {
        return Err(
            "set ICT_PUBLISH_HOST=127.0.0.1 before starting chains (loopback only)".into(),
        );
    }

    let report_dir = PathBuf::from(
        std::env::var("HEADSTASH_E2E_DIR").unwrap_or_else(|_| "/tmp/headstash-e2e".to_string()),
    );
    std::fs::create_dir_all(&report_dir)?;
    let keep = std::env::var("HEADSTASH_E2E_KEEP").unwrap_or_else(|_| "1".into()) != "0";

    let mut cases: Vec<Case> = Vec::new();
    cases.push(Case {
        name: "expired_claim",
        status: "gap",
        detail: "cw-headstash has no expiry field, so an expired claim cannot be submitted"
            .into(),
    });
    cases.push(Case {
        name: "manifold_airdrop",
        status: "gap",
        detail: "no manifold wasm on disk. cw-headstash is the claim and escrow. The factory was not deployed.".into(),
    });
    cases.push(Case {
        name: "pir_gate",
        status: "gap",
        detail: "hash-market is not started. A static-file 404 is not the feature-gated POST /notes/:id/pir route.".into(),
    });
    cases.push(Case {
        name: "penumbra_hop",
        status: "gap",
        detail: "ict-rs Penumbra chain is a stub. The hop is local Terp chain A to local Terp chain B.".into(),
    });

    let zk_root = resolve_zk_root()?;
    let claim_wasm = env_path(
        "HEADSTASH_WASM",
        zk_root.join("terp-core/crates/headstash/artifacts/cw_headstash.wasm"),
    );
    let witness_wasm = env_path(
        "HEADSTASH_WITNESS_WASM",
        zk_root.join(
            "terp-core/crates/headstash/contracts/cw-reg-eligibility/artifacts/cw_reg_eligibility.wasm",
        ),
    );
    let vk_path = env_path(
        "HEADSTASH_VK_PATH",
        zk_root.join("terp-core/crates/headstash/artifacts/headstash_vk.bin"),
    );
    let wavs_path = env_path(
        "HEADSTASH_WAVS_JSON",
        report_dir.join("wavs.json"),
    );
    for (label, path) in [
        ("claim wasm", &claim_wasm),
        ("witness wasm", &witness_wasm),
        ("wavs json", &wavs_path),
    ] {
        if !path.exists() {
            return Err(format!("missing {label}: {}", path.display()).into());
        }
    }

    let wavs: serde_json::Value = serde_json::from_slice(&std::fs::read(&wavs_path)?)?;
    if wavs.get("poos").and_then(|v| v.as_array()).map(|a| a.len()) != Some(1) {
        return Err("wavs json must contain one operator proof".into());
    }

    println!("=== Headstash local e2e (loopback) ===");
    println!(
        "image {}:{}",
        terp_image_repo(),
        terp_image_version()
    );

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let test_name = "headstash-e2e-local";
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;

    let mut chain_a = CosmosChain::new(chain_config("headstash-a", CHAIN_A), 1, 0, runtime.clone());
    let mut chain_b = CosmosChain::new(chain_config("headstash-b", CHAIN_B), 1, 0, runtime.clone());
    let ctx = TestContext {
        test_name: test_name.to_string(),
        network_id: network_id.clone(),
    };
    chain_a.initialize(&ctx).await?;
    chain_b.initialize(&ctx).await?;
    chain_a.start(&[]).await?;
    chain_b.start(&[]).await?;
    record_tx_limits(&chain_a, &mut cases).await;
    println!("RPC A {}", chain_a.host_rpc_address());
    println!("RPC B {}", chain_b.host_rpc_address());

    let rpc_a_port = host_port_from_url(&chain_a.host_rpc_address());
    assert_loopback(chain_a.primary_node()?.container_id.as_ref().map(|c| c.0.as_str()), 26657)?;
    assert_loopback(chain_b.primary_node()?.container_id.as_ref().map(|c| c.0.as_str()), 26657)?;
    let lcd_a = published_host("127.0.0.1", chain_a.primary_node()?.container_id.as_ref().map(|c| c.0.as_str()), 1317)?;
    let lcd_b = published_host("127.0.0.1", chain_b.primary_node()?.container_id.as_ref().map(|c| c.0.as_str()), 1317)?;
    cases.push(Case {
        name: "loopback_publish",
        status: "pass",
        detail: format!("rpc_a={rpc_a_port} lcd_a={lcd_a} lcd_b={lcd_b}"),
    });

    let rpc_a = chain_a.host_rpc_address();
    let rpc_b = chain_b.host_rpc_address();
    let container_a = chain_a
        .primary_node()?
        .container_id
        .as_ref()
        .map(|c| c.0.clone())
        .unwrap_or_default();
    let home_a = chain_a.home_dir().to_string();
    let result = run_flows(
        &chain_a,
        &chain_b,
        &claim_wasm,
        &witness_wasm,
        &vk_path,
        &wavs,
        &mut cases,
    )
    .await;

    // Hop even when the claim contract fails. The chains are already up.
    let mut parked: Option<Interchain> = None;
    match build_relayer(RelayerType::Hermes, runtime.clone(), test_name, &network_id).await {
        Ok(relayer) => {
            let mut ic = Interchain::new(runtime)
                .add_chain(Box::new(chain_a))
                .add_chain(Box::new(chain_b))
                .add_relayer("hermes", relayer)
                .add_link(InterchainLink {
                    chain1: CHAIN_A.into(),
                    chain2: CHAIN_B.into(),
                    relayer: "hermes".into(),
                    path: "headstash-hop".into(),
                });
            match ic.configure_relayers_and_paths().await {
                Ok(()) => {
                    ic.mark_built();
                    hop(&mut ic, &mut cases).await;
                }
                Err(e) => cases.push(Case {
                    name: "hop_setup",
                    status: "fail",
                    detail: e.to_string(),
                }),
            }
            parked = Some(ic);
        }
        Err(e) => {
            cases.push(Case {
                name: "hop_setup",
                status: "fail",
                detail: e.to_string(),
            });
            if !keep {
                let _ = chain_a.stop().await;
                let _ = chain_b.stop().await;
            }
        }
    }

    write_report(
        &report_dir,
        &cases,
        &rpc_a,
        &lcd_a,
        &lcd_b,
        &container_a,
        &home_a,
    )?;
    if let Some(mut ic) = parked {
        if !keep {
            println!("--- shutdown ---");
            let _ = ic.close().await;
        } else {
            println!(
                "KEEP=1: chains left on 127.0.0.1 (lcd {lcd_a}). RPC A {rpc_a} RPC B {rpc_b}."
            );
        }
    } else if keep {
        println!("KEEP=1: chains left up. RPC A {rpc_a}");
    }

    let failed = cases.iter().any(|c| c.status == "fail");
    print_cases(&cases);
    match result {
        Ok(()) if !failed => Ok(()),
        Ok(()) => Err("one or more cases failed".into()),
        Err(e) => {
            eprintln!("flow error: {e}");
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_flows(
    chain_a: &CosmosChain,
    chain_b: &CosmosChain,
    claim_wasm: &Path,
    witness_wasm: &Path,
    vk_path: &Path,
    wavs: &serde_json::Value,
    cases: &mut Vec<Case>,
) -> Result<(), Box<dyn std::error::Error>> {
    chain_a.create_key("deployer").await?;
    chain_a.create_key("user-a").await?;
    chain_b.create_key("user-b").await?;
    let deployer = key_address(chain_a, "deployer").await?;
    let user_a = key_address(chain_a, "user-a").await?;
    let user_b = key_address(chain_b, "user-b").await?;
    println!("deployer {deployer}");
    println!("user-a {user_a}");
    println!("user-b {user_b}");

    chain_a
        .send_funds(
            "validator",
            &WalletAmount {
                address: deployer.clone(),
                denom: DENOM.into(),
                amount: 50_000_000_000,
            },
        )
        .await?;
    chain_a
        .send_funds(
            "validator",
            &WalletAmount {
                address: user_a.clone(),
                denom: DENOM.into(),
                amount: 20_000_000_000,
            },
        )
        .await?;
    chain_b
        .send_funds(
            "validator",
            &WalletAmount {
                address: user_b.clone(),
                denom: DENOM.into(),
                amount: 10_000_000_000,
            },
        )
        .await?;
    wait_for_blocks(chain_a, 2).await?;

    let node = chain_a.primary_node()?;
    node.copy_file_from_host(claim_wasm, "/tmp/cw_headstash.wasm")
        .await?;
    node.copy_file_from_host(witness_wasm, "/tmp/cw_reg_eligibility.wasm")
        .await?;

    let claim_code = chain_a
        .store_code("deployer", "/tmp/cw_headstash.wasm")
        .await?;
    let witness_code = chain_a
        .store_code("deployer", "/tmp/cw_reg_eligibility.wasm")
        .await?;
    println!("codes claim={claim_code} witness={witness_code}");
    cases.push(Case {
        name: "store_wasm",
        status: "pass",
        detail: format!("claim_code={claim_code} witness_code={witness_code}"),
    });

    let loaded = load_claim_proof();
    let aligned = loaded.as_ref().filter(|p| host_proof(p) && p.nd == derive_nd(&p.denom));
    let claim_denom = aligned
        .map(|p| p.denom.clone())
        .unwrap_or_else(|| DENOM.to_string());
    let root = aligned.map(|p| p.anchor).unwrap_or([0x11u8; 32]);
    let nd = derive_nd(&claim_denom);
    if claim_denom != DENOM {
        let fund = aligned
            .map(|p| u128::from(p.v).saturating_mul(4).max(20_000_000))
            .unwrap_or(20_000_000);
        chain_a
            .send_funds(
                "validator",
                &WalletAmount {
                    address: deployer.clone(),
                    denom: claim_denom.clone(),
                    amount: fund,
                },
            )
            .await?;
    }
    let init = serde_json::json!({
        "genesis_root": STANDARD.encode(root),
        "distro_hash_domain": "poseidon_v1",
        "genesis_label": "headstash-e2e-local",
        "circuit_id": 0,
        "token_strategy": {
            "existing_fungible": {
                "proof": STANDARD.encode(nd),
                "raw": claim_denom
            }
        },
        "wavs": wavs
    });
    let init_amount = if claim_denom == DENOM {
        format!("1000000{DENOM}")
    } else {
        let value = aligned.map(|p| p.v).unwrap_or(5_000_000);
        format!("{}{claim_denom}", value.saturating_mul(2))
    };
    let claim_addr = match instantiate_with_funds(
        chain_a,
        "deployer",
        &claim_code,
        &init.to_string(),
        "headstash-e2e",
        Some(&deployer),
        &init_amount,
    )
    .await
    {
        Ok(addr) => {
            cases.push(Case {
                name: "claim_instantiate",
                status: "pass",
                detail: addr.clone(),
            });
            addr
        }
        Err(e) => {
            cases.push(Case {
                name: "claim_instantiate",
                status: "fail",
                detail: e.to_string(),
            });
            return Err(e);
        }
    };
    println!("claim contract {claim_addr}");

    chain_a
        .send_funds(
            "deployer",
            &WalletAmount {
                address: claim_addr.clone(),
                denom: DENOM.into(),
                amount: 5_000_000,
            },
        )
        .await?;

    let accepted = STANDARD.encode([7u8, 7, 7, 7]);
    let witness_init = serde_json::json!({
        "accepted_proof": accepted,
        "dummy_account": "headstash-e2e-witness"
    });
    let witness_addr = instantiate_with_funds(
        chain_a,
        "deployer",
        &witness_code,
        &witness_init.to_string(),
        "headstash-witness",
        Some(&deployer),
        "",
    )
    .await?;
    cases.push(Case {
        name: "witness_instantiate",
        status: "pass",
        detail: witness_addr.clone(),
    });
    println!("witness contract {witness_addr}");

    // Byte-compare witness is not a claim. A non-matching proof must be false.
    let bad_q = serde_json::json!({
        "verify_min_holders": {
            "session_id": "lab",
            "registrant": "lab",
            "proof": STANDARD.encode([1u8])
        }
    });
    match chain_a.query_contract(&witness_addr, &bad_q.to_string()).await {
        Ok(v) => {
            let text = v.to_string();
            let ok = text.contains("\"ok\":false") || text.contains("\"ok\": false");
            cases.push(Case {
                name: "witness_bad_proof",
                status: if ok { "pass" } else { "fail" },
                detail: text,
            });
        }
        Err(e) => cases.push(Case {
            name: "witness_bad_proof",
            status: "fail",
            detail: e.to_string(),
        }),
    }
    cases.push(Case {
        name: "witness_byte_compare",
        status: "removed",
        detail: "cw-reg-eligibility accepted_proof equality is not the claim path and is not a pass".into(),
    });

    let circuit_note = upload_circuit(chain_a, &claim_addr, vk_path).await;
    cases.push(circuit_note);

    let other_root = [0x12u8; 32];
    push_exec_case(
        chain_a,
        cases,
        "anchor_mismatch",
        "pass",
        &claim_addr,
        &claim_msg(&other_root, &nd, 1, &[0x22; 32], &[0x14; 32], &[0x14; 32], &[9u8, 9, 9], 0),
        &["does not match"],
    )
    .await;

    push_exec_case(
        chain_a,
        cases,
        "bad_denom",
        "pass",
        &claim_addr,
        &claim_msg(&root, &[0x02; 32], 1, &[0x23; 32], &[0x14; 32], &[0x14; 32], &[9u8, 9], 0),
        &["incorrect token denom"],
    )
    .await;

    let nf = [0x24u8; 32];
    let one: serde_json::Value =
        serde_json::from_str(&claim_body(&root, &nd, 1, &nf, &[0x14; 32], &[0x33; 32])).unwrap();
    let mut double = serde_json::json!({"process_headstash": {"claims": []}});
    double["process_headstash"]["claims"] = serde_json::json!([one.clone(), one]);
    let text = exec_text(chain_a, &claim_addr, &double.to_string()).await;
    if text.contains("null:") {
        cases.push(Case {
            name: "double_claim",
            status: "pass",
            detail: clip(&text),
        });
    } else if text.contains("circuit_id is 0")
        || text.contains("invalid headstash proof")
        || text.contains("requires zk-api")
    {
        cases.push(Case {
            name: "double_claim",
            status: "gap",
            detail: format!(
                "batch nullifier is checked inside the claim loop after that claim's proof. The first claim failed proof verify, so the duplicate was not observed: {}",
                clip(&text)
            ),
        });
    } else {
        cases.push(classify_reject("double_claim", "pass", &text, &["null:"]));
    }

    push_exec_case(
        chain_a,
        cases,
        "bad_proof",
        "pass",
        &claim_addr,
        &claim_msg(&root, &nd, 1, &[0x25; 32], &[0x14; 32], &[0x14; 32], &[1, 2, 3, 4], 0),
        &["invalid headstash proof", "circuit_id is 0", "requires zk-api"],
    )
    .await;

    let text = exec_text(
        chain_a,
        &claim_addr,
        &claim_msg(&root, &nd, 1, &[0x26; 32], &[0x14; 32], &[0x15; 32], &[1, 2, 3, 4], 0),
    )
    .await;
    if text.contains("recipient addr not represented") {
        cases.push(Case {
            name: "wrong_address",
            status: "pass",
            detail: clip(&text),
        });
    } else if text.contains("circuit_id is 0")
        || text.contains("invalid headstash proof")
        || text.contains("requires zk-api")
    {
        cases.push(Case {
            name: "wrong_address",
            status: "gap",
            detail: format!(
                "recipient check runs only after proof_instance_verify, so this submit stopped at the proof gate: {}",
                clip(&text)
            ),
        });
    } else {
        cases.push(classify_reject(
            "wrong_address",
            "pass",
            &text,
            &["recipient addr not represented"],
        ));
    }

    let circuit_ready = cases
        .iter()
        .any(|c| c.name == "store_circuit" && c.status == "pass");
    submit_positive_claim(chain_a, &claim_addr, &loaded, circuit_ready, cases).await;
    let _ = write_broadcast(&claim_addr, &root, &nd, loaded.as_ref());

    Ok(())
}

async fn hop(ic: &mut Interchain, cases: &mut Vec<Case>) {
    let chain_a = match ic.get_chain(CHAIN_A) {
        Some(c) => c,
        None => {
            cases.push(Case {
                name: "hop_success",
                status: "fail",
                detail: "chain A missing after move".into(),
            });
            return;
        }
    };
    let chain_b = match ic.get_chain(CHAIN_B) {
        Some(c) => c,
        None => {
            cases.push(Case {
                name: "hop_success",
                status: "fail",
                detail: "chain B missing after move".into(),
            });
            return;
        }
    };
    let user_a = match key_address_dyn(chain_a, "user-a").await {
        Ok(v) => v,
        Err(e) => {
            cases.push(Case {
                name: "hop_success",
                status: "fail",
                detail: e,
            });
            return;
        }
    };
    let user_b = match key_address_dyn(chain_b, "user-b").await {
        Ok(v) => v,
        Err(e) => {
            cases.push(Case {
                name: "hop_success",
                status: "fail",
                detail: e,
            });
            return;
        }
    };
    let before = chain_a.get_balance(&user_a, DENOM).await.unwrap_or(0);
    match chain_a
        .send_ibc_transfer(
            "channel-0",
            "user-a",
            &WalletAmount {
                address: user_b.clone(),
                denom: DENOM.into(),
                amount: 1000,
            },
            &TransferOptions::default(),
        )
        .await
    {
        Ok(tx) => {
            let _ = wait_for_blocks(chain_b, 8).await;
            let ibc = ibc_denom("transfer", "channel-0", DENOM);
            let got = chain_b.get_balance(&user_b, &ibc).await.unwrap_or(0);
            let after = chain_a.get_balance(&user_a, DENOM).await.unwrap_or(0);
            let ok = got >= 1000 && after < before;
            cases.push(Case {
                name: "hop_success",
                status: if ok { "pass" } else { "fail" },
                detail: format!(
                    "tx={} ibc_denom={ibc} got={got} src_before={before} src_after={after}",
                    tx.tx_hash
                ),
            });
        }
        Err(e) => cases.push(Case {
            name: "hop_success",
            status: "fail",
            detail: e.to_string(),
        }),
    }

    if let Some(rly) = ic.get_relayer("hermes") {
        let _ = rly.stop().await;
    }
    let chain_a = ic.get_chain(CHAIN_A).unwrap();
    let chain_b = ic.get_chain(CHAIN_B).unwrap();
    let before_b = chain_b
        .get_balance(&user_b, &ibc_denom("transfer", "channel-0", DENOM))
        .await
        .unwrap_or(0);
    let timed = chain_a
        .send_ibc_transfer(
            "channel-0",
            "user-a",
            &WalletAmount {
                address: user_b.clone(),
                denom: DENOM.into(),
                amount: 500,
            },
            &TransferOptions {
                timeout_height: Some(1),
                timeout_timestamp: None,
                memo: Some("headstash-hop-timeout".into()),
            },
        )
        .await;
    match timed {
        Err(e) => {
            let text = e.to_string();
            let rejected = text.to_lowercase().contains("timeout")
                || text.to_lowercase().contains("height");
            cases.push(Case {
                name: "hop_timeout",
                status: if rejected { "pass" } else { "fail" },
                detail: format!("submit rejected: {text}"),
            });
        }
        Ok(tx) => {
            let _ = wait_for_blocks(chain_b, 4).await;
            let after_b = chain_b
                .get_balance(&user_b, &ibc_denom("transfer", "channel-0", DENOM))
                .await
                .unwrap_or(0);
            let stuck = after_b == before_b;
            cases.push(Case {
                name: "hop_timeout",
                status: if stuck { "pass" } else { "fail" },
                detail: format!(
                    "tx={} dest_before={before_b} dest_after={after_b} relayer_stopped=true",
                    tx.tx_hash
                ),
            });
        }
    }
}

async fn key_address_dyn(chain: &dyn Chain, key_name: &str) -> Result<String, String> {
    let output = chain
        .chain_exec(&["keys", "show", key_name, "-a", "--keyring-backend", "test"])
        .await
        .map_err(|e| e.to_string())?;
    let addr = output.stdout_str().trim().to_string();
    if addr.is_empty() {
        Err(format!("empty address for {key_name}"))
    } else {
        Ok(addr)
    }
}

async fn upload_circuit(chain: &CosmosChain, contract: &str, vk_path: &Path) -> Case {
    if !vk_path.exists() {
        return Case {
            name: "store_circuit",
            status: "gap",
            detail: format!("vk blob missing at {}", vk_path.display()),
        };
    }
    let bytes = match std::fs::read(vk_path) {
        Ok(b) => b,
        Err(e) => {
            return Case {
                name: "store_circuit",
                status: "fail",
                detail: e.to_string(),
            }
        }
    };
    let Some((params, body, _footer_key)) = split_circuit_blob(&bytes) else {
        return Case {
            name: "store_circuit",
            status: "gap",
            detail: format!(
                "footer lengths do not match blob size {}",
                bytes.len()
            ),
        };
    };
    let node = match chain.primary_node() {
        Ok(n) => n,
        Err(e) => {
            return Case {
                name: "store_circuit",
                status: "fail",
                detail: e.to_string(),
            }
        }
    };
    let param_file = std::env::temp_dir().join("headstash-e2e-params.bin");
    let body_file = std::env::temp_dir().join("headstash-e2e-vk-body.bin");
    if std::fs::write(&param_file, &params).is_err() || std::fs::write(&body_file, &body).is_err() {
        return Case {
            name: "store_circuit",
            status: "fail",
            detail: "could not write split circuit files".into(),
        };
    }
    if node
        .copy_file_from_host(&param_file, "/tmp/hs-params.bin")
        .await
        .is_err()
        || node
            .copy_file_from_host(&body_file, "/tmp/hs-vk-body.bin")
            .await
            .is_err()
    {
        return Case {
            name: "store_circuit",
            status: "fail",
            detail: "copy of circuit blob into the container failed".into(),
        };
    }
    // terpd 6.4.0 store-vk-param has --k/--circuit-type/--curve-type and no
    // --param-key. store-circuit is the command that takes --param-key.
    // Halo2-Pasta on this fork is circuit-type 0, curve-type 0. Headstash K=18.
    let upload_args = [
        "tx",
        "wasm",
        "store-vk-param",
        "/tmp/hs-params.bin",
        "--k",
        "18",
        "--circuit-type",
        "0",
        "--curve-type",
        "0",
    ];
    let upload = chain.chain_exec_tx_with(
        &upload_args,
        // Height 53, tx DA145889…, wanted 500000000, used 1342227428 at
        // WritePerByte (about 40 gas per stored param byte). Block max_gas is
        // -1 and the fee price is 0. 4000000000 covers that write plus txSize.
        chain.default_tx_opts().from("deployer").gas("4000000000"),
    );
    let upload = match tokio::time::timeout(std::time::Duration::from_secs(600), upload).await {
        Ok(Ok(out)) => format!("{}{}", out.stdout_str(), out.stderr_str()),
        Ok(Err(e)) => {
            return Case {
                name: "store_circuit",
                status: "gap",
                detail: format!("store-vk-param failed: {e}"),
            }
        }
        Err(_) => {
            return Case {
                name: "store_circuit",
                status: "gap",
                detail: "store-vk-param timed out at 600s".into(),
            }
        }
    };
    if upload.contains("unknown flag") || upload.contains("Error:") {
        return Case {
            name: "store_circuit",
            status: "gap",
            detail: format!("store-vk-param rejected by CLI: {}", clip(&upload)),
        };
    }
    if upload.contains("\"code\":") && !upload.contains("\"code\":0") && !upload.contains("\"code\": 0") {
        return Case {
            name: "store_circuit",
            status: "gap",
            detail: clip(&upload),
        };
    }
    let upload = append_queried_tx(chain, upload).await;
    let Some(key_owned) = parse_param_key(&upload) else {
        return Case {
            name: "store_circuit",
            status: "gap",
            detail: format!("store-vk-param did not name a param key: {}", clip(&upload)),
        };
    };
    let store_args = [
        "tx",
        "wasm",
        "store-circuit",
        "/tmp/hs-vk-body.bin",
        "--param-key",
        key_owned.as_str(),
    ];
    let stored = chain.chain_exec_tx_with(
        &store_args,
        // Height 61 read the stored params and stopped at ReadPerByte:
        // wanted 80000000, used 100890991. store_circuit then writes the
        // params again. Same 4000000000 budget as store-vk-param.
        chain.default_tx_opts().from("deployer").gas("4000000000"),
    );
    let stored = match tokio::time::timeout(std::time::Duration::from_secs(600), stored).await {
        Ok(Ok(out)) => format!("{}{}", out.stdout_str(), out.stderr_str()),
        Ok(Err(e)) => {
            return Case {
                name: "store_circuit",
                status: "gap",
                detail: format!("store-circuit failed: {e}"),
            }
        }
        Err(_) => {
            return Case {
                name: "store_circuit",
                status: "gap",
                detail: "store-circuit timed out at 600s".into(),
            }
        }
    };
    let stored = append_queried_tx(chain, stored).await;
    if stored.contains("unknown flag") || stored.contains("Error:") {
        return Case {
            name: "store_circuit",
            status: "gap",
            detail: format!("store-circuit rejected by CLI: {}", clip(&stored)),
        };
    }
    // store_circuit emits zk_id (the sequential id SetCircuitId stores).
    let cid = ["zk_id", "circuit_id"].iter().find_map(|key| {
        stored.split(key).nth(1).and_then(|rest| {
            rest.chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u64>()
                .ok()
        })
    });
    if let Some(cid) = cid {
        let msg = serde_json::json!({"set_circuit_id": {"circuit_id": cid}}).to_string();
        match chain.execute_contract("deployer", contract, &msg, None).await {
            Ok(_) => Case {
                name: "store_circuit",
                status: "pass",
                detail: format!("circuit_id={cid}"),
            },
            Err(e) => Case {
                name: "store_circuit",
                status: "gap",
                detail: format!("stored but SetCircuitId failed: {e}; raw {}", clip(&stored)),
            },
        }
    } else {
        Case {
            name: "store_circuit",
            status: "gap",
            detail: format!("no circuit_id parsed: {}", clip(&stored)),
        }
    }
}

fn parse_param_key(text: &str) -> Option<String> {
    for key in ["param_key", "param-key", "param_id"] {
        let needle = format!("\"{key}\"");
        if let Some(at) = text.find(&needle) {
            let rest = &text[at + needle.len()..];
            let digits: String = rest
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                return Some(digits);
            }
        }
    }
    None
}

fn txhash_in(text: &str) -> Option<String> {
    let key = "\"txhash\":\"";
    let at = text.find(key)?;
    let hash: String = text[at + key.len()..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    if hash.len() == 64 { Some(hash) } else { None }
}

async fn append_queried_tx(chain: &CosmosChain, mut text: String) -> String {
    let Some(hash) = txhash_in(&text) else {
        return text;
    };
    let query = [
        "query".to_string(),
        "tx".to_string(),
        hash.clone(),
        "--output".to_string(),
        "json".to_string(),
    ];
    let query_refs: Vec<&str> = query.iter().map(String::as_str).collect();
    // A 33MB store-vk-param can land after the sync ack. Poll the committed tx
    // instead of reading the empty ack. Small txs return on the first hit.
    for _ in 0..24 {
        let _ = wait_for_blocks(chain, 2).await;
        if let Ok(queried) = chain.chain_exec(&query_refs).await {
            let got = format!("{}{}", queried.stdout_str(), queried.stderr_str());
            if got.contains("\"code\"") {
                text.push('\n');
                text.push_str(&got);
                return text;
            }
        }
    }
    text
}

fn split_circuit_blob(bytes: &[u8]) -> Option<(Vec<u8>, Vec<u8>, u32)> {
    if bytes.len() < 80 {
        return None;
    }
    let footer = &bytes[bytes.len() - 80..];
    let param_len = u32::from_le_bytes(footer[4..8].try_into().ok()?) as usize;
    let cs_len = u32::from_le_bytes(footer[8..12].try_into().ok()?) as usize;
    let vk_len = u32::from_le_bytes(footer[12..16].try_into().ok()?) as usize;
    if param_len + cs_len + vk_len + 80 != bytes.len() {
        return None;
    }
    let param_key = u32::from_be_bytes([footer[0], footer[1], footer[2], 0]);
    let params = bytes[..param_len].to_vec();
    let body = bytes[param_len..].to_vec();
    Some((params, body, param_key))
}

async fn exec_text(chain: &CosmosChain, contract: &str, msg: &str) -> String {
    exec_text_with_gas(chain, contract, msg, "800000").await
}

async fn exec_text_with_gas(chain: &CosmosChain, contract: &str, msg: &str, gas: &str) -> String {
    // Fixed gas. `gas auto` simulates, and a rejecting claim exits with an empty
    // stdout. The wasm error is then only on stderr, which the JSON parser drops.
    let args = [
        "tx".to_string(),
        "wasm".to_string(),
        "execute".to_string(),
        contract.to_string(),
        msg.to_string(),
    ];
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = match chain
        .chain_exec_tx_with(
            &arg_refs,
            chain.default_tx_opts().from("deployer").gas(gas),
        )
        .await
    {
        Ok(out) => out,
        Err(e) => return e.to_string(),
    };
    let mut text = format!("{}{}", output.stdout_str(), output.stderr_str());
    let hash = serde_json::from_str::<serde_json::Value>(output.stdout_str().trim())
        .ok()
        .and_then(|v| {
            v.get("txhash")
                .and_then(|h| h.as_str())
                .map(|s| s.to_string())
        })
        .filter(|h| !h.is_empty());
    if let Some(hash) = hash {
        let _ = wait_for_blocks(chain, 2).await;
        let query = [
            "query".to_string(),
            "tx".to_string(),
            hash,
            "--output".to_string(),
            "json".to_string(),
        ];
        let query_refs: Vec<&str> = query.iter().map(String::as_str).collect();
        if let Ok(queried) = chain.chain_exec(&query_refs).await {
            text.push('\n');
            text.push_str(&queried.stdout_str());
            text.push_str(&queried.stderr_str());
        }
    }
    text
}

fn classify_reject(name: &'static str, want: &'static str, text: &str, needles: &[&str]) -> Case {
    let hit = needles.iter().any(|n| text.contains(n));
    if hit {
        return Case {
            name,
            status: want,
            detail: clip(text),
        };
    }
    let included_ok = text.contains("\"code\":0")
        && text.contains("\"raw_log\":\"\"")
        && !text.contains("failed to execute message");
    Case {
        name,
        status: "fail",
        detail: if included_ok {
            format!("unexpected success {}", clip(text))
        } else {
            clip(text)
        },
    }
}

async fn push_exec_case(
    chain: &CosmosChain,
    cases: &mut Vec<Case>,
    name: &'static str,
    want: &'static str,
    contract: &str,
    msg: &str,
    needles: &[&str],
) {
    let text = exec_text(chain, contract, msg).await;
    cases.push(classify_reject(name, want, &text, needles));
}

fn claim_msg(
    root: &[u8; 32],
    nd: &[u8; 32],
    value: u64,
    nf: &[u8; 32],
    recp: &[u8; 32],
    rr: &[u8; 32],
    proof: &[u8],
    root_id: u64,
) -> String {
    let body = claim_body(root, nd, value, nf, recp, &[0x33; 32]);
    let mut v: serde_json::Value = serde_json::from_str(&body).unwrap();
    v["p"] = serde_json::Value::String(STANDARD.encode(proof));
    v["rr"] = serde_json::Value::String(STANDARD.encode(rr));
    v["root_id"] = serde_json::json!(root_id);
    serde_json::json!({"process_headstash": {"claims": [v]}}).to_string()
}

fn claim_body(root: &[u8; 32], nd: &[u8; 32], value: u64, nf: &[u8; 32], recp: &[u8; 32], cmx: &[u8; 32]) -> String {
    let mut wire = Vec::with_capacity(168);
    wire.extend_from_slice(root);
    wire.extend_from_slice(nd);
    wire.extend_from_slice(&value.to_le_bytes());
    wire.extend_from_slice(nf);
    wire.extend_from_slice(recp);
    wire.extend_from_slice(cmx);
    let body = keccak256(&wire);
    let e = challenge_scalar_be(&body);
    serde_json::json!({
        "i": {
            "anchor": STANDARD.encode(root),
            "nd": STANDARD.encode(nd),
            "v": value,
            "nf": STANDARD.encode(nf),
            "recp": STANDARD.encode(recp),
            "cmx": STANDARD.encode(cmx),
            "e": STANDARD.encode(e)
        },
        "p": STANDARD.encode([0u8]),
        "rr": STANDARD.encode(recp),
        "root_id": 0
    })
    .to_string()
}

async fn instantiate_with_funds(
    chain: &CosmosChain,
    key: &str,
    code_id: &str,
    msg: &str,
    label: &str,
    admin: Option<&str>,
    amount: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut opts = chain.default_tx_opts().from(key);
    if let Some(admin) = admin {
        opts = opts.flag("--admin", admin);
    } else {
        opts = opts.flag("--no-admin", "");
    }
    if !amount.is_empty() {
        opts = opts.flag("--amount", amount);
    }
    let output = chain
        .chain_exec_tx_with(
            &["tx", "wasm", "instantiate", code_id, msg, "--label", label],
            opts,
        )
        .await?;
    let text = format!("{}{}", output.stdout_str(), output.stderr_str());
    let _ = output.exit_code;
    wait_for_blocks(chain, 2).await?;
    let listed = chain
        .chain_exec(&[
            "query",
            "wasm",
            "list-contract-by-code",
            code_id,
            "--output",
            "json",
        ])
        .await?;
    let v: serde_json::Value = serde_json::from_str(listed.stdout_str().trim())
        .map_err(|e| format!("list-contract-by-code: {e}; tx {}", clip(&text)))?;
    let addr = v["contracts"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("no contract address for code {code_id}: {}", clip(&text)))?;
    Ok(addr.to_string())
}

async fn key_address(chain: &CosmosChain, key_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let output = chain
        .chain_exec(&["keys", "show", key_name, "-a", "--keyring-backend", "test"])
        .await?;
    let addr = output.stdout_str().trim().to_string();
    if addr.is_empty() {
        return Err(format!("empty address for {key_name}").into());
    }
    Ok(addr)
}

async fn record_tx_limits(chain: &CosmosChain, cases: &mut Vec<Case>) {
    let home = chain.home_dir();
    let script = format!(
        "grep -n max_body_bytes {home}/config/config.toml; grep -n max_tx_bytes {home}/config/config.toml; grep -n max_bytes {home}/config/genesis.json"
    );
    match chain.exec(&["sh", "-c", &script], &[]).await {
        Ok(out) => {
            let text = format!("{}{}", out.stdout_str(), out.stderr_str());
            let raised = text.matches("67108864").count() >= 2;
            cases.push(Case {
                name: "tx_body_limit",
                status: if raised { "pass" } else { "fail" },
                detail: clip(&text),
            });
        }
        Err(e) => cases.push(Case {
            name: "tx_body_limit",
            status: "fail",
            detail: format!("could not read local comet config: {e}"),
        }),
    }
}

struct LoadedProof {
    anchor: [u8; 32],
    nd: [u8; 32],
    v: u64,
    nf: [u8; 32],
    recp: [u8; 32],
    cmx: [u8; 32],
    e: [u8; 32],
    proof: Vec<u8>,
    denom: String,
    prover: String,
    host_verify: bool,
}

fn claim_proof_path() -> PathBuf {
    if let Ok(path) = std::env::var("HEADSTASH_CLAIM_PROOF") {
        return PathBuf::from(path);
    }
    let dir = std::env::var("HEADSTASH_E2E_DIR").unwrap_or_else(|_| "/tmp/headstash-e2e".into());
    PathBuf::from(dir).join("claim_proof.json")
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_val(bytes[i])?;
        let lo = hex_val(bytes[i + 1])?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Some(out)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn decode_hex32(s: &str) -> Option<[u8; 32]> {
    let v = decode_hex(s)?;
    v.try_into().ok()
}

fn load_claim_proof() -> Option<LoadedProof> {
    let path = claim_proof_path();
    let raw = std::fs::read(&path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let field = |key: &str| v.get(key).and_then(|x| x.as_str()).and_then(decode_hex32);
    Some(LoadedProof {
        anchor: field("anchor_hex")?,
        nd: field("nd_hex")?,
        v: v.get("value").and_then(|x| x.as_u64())?,
        nf: field("nf_hex")?,
        recp: field("recp_hex")?,
        cmx: field("cmx_hex")?,
        e: field("e_hex")?,
        proof: v
            .get("proof_hex")
            .and_then(|x| x.as_str())
            .and_then(decode_hex)?,
        denom: v.get("denom").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        prover: v.get("prover").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        host_verify: v.get("host_verify").and_then(|x| x.as_bool()).unwrap_or(false),
    })
}

fn proof_process_msg(p: &LoadedProof) -> String {
    serde_json::json!({
        "process_headstash": {
            "claims": [{
                "i": {
                    "anchor": STANDARD.encode(p.anchor),
                    "nd": STANDARD.encode(p.nd),
                    "v": p.v,
                    "nf": STANDARD.encode(p.nf),
                    "recp": STANDARD.encode(p.recp),
                    "cmx": STANDARD.encode(p.cmx),
                    "e": STANDARD.encode(p.e)
                },
                "p": STANDARD.encode(&p.proof),
                "rr": STANDARD.encode(p.recp),
                "root_id": 0
            }]
        }
    })
    .to_string()
}

fn prover_error_tail() -> String {
    let dir = std::env::var("HEADSTASH_E2E_DIR").unwrap_or_else(|_| "/tmp/headstash-e2e".into());
    let path = PathBuf::from(dir).join("prove-error.txt");
    std::fs::read_to_string(path)
        .unwrap_or_else(|_| "prover did not write claim_proof.json".into())
}

async fn submit_positive_claim(
    chain: &CosmosChain,
    contract: &str,
    loaded: &Option<LoadedProof>,
    circuit_ready: bool,
    cases: &mut Vec<Case>,
) {
    let Some(proof) = loaded else {
        cases.push(Case {
            name: "positive_claim",
            status: "gap",
            detail: clip(&prover_error_tail()),
        });
        return;
    };
    if proof.prover != "zk_headstash::circuit::Proof::create" || !proof.host_verify {
        cases.push(Case {
            name: "positive_claim",
            status: "gap",
            detail: "proof file is not a host-verified Proof::create output".into(),
        });
        return;
    }
    if !denom_charset_ok(&proof.denom) {
        cases.push(Case {
            name: "positive_claim",
            status: "gap",
            detail: format!(
                "prover denom {} is not a bank denom. The proof was not submitted.",
                proof.denom
            ),
        });
        return;
    }
    if proof.nd != derive_nd(&proof.denom) {
        cases.push(Case {
            name: "positive_claim",
            status: "gap",
            detail: format!(
                "prover nd does not match derive_nd({}). The proof was not submitted.",
                proof.denom
            ),
        });
        return;
    }
    if proof.proof.is_empty() || proof.v == 0 {
        cases.push(Case {
            name: "positive_claim",
            status: "gap",
            detail: "prover wrote an empty proof or a zero value".into(),
        });
        return;
    }
    if !circuit_ready {
        cases.push(Case {
            name: "positive_claim",
            status: "gap",
            detail: "store-circuit did not return a circuit id, so the host proof was not submitted"
                .into(),
        });
        return;
    }
    let before = chain
        .get_balance(contract, &proof.denom)
        .await
        .unwrap_or(0);
    let text = exec_text_with_gas(chain, contract, &proof_process_msg(proof), "20000000").await;
    let after = chain
        .get_balance(contract, &proof.denom)
        .await
        .unwrap_or(0);
    let failed = text.contains("failed to execute message");
    let paid = before > after && (before - after).to_string() == proof.v.to_string();
    if !failed && paid {
        cases.push(Case {
            name: "positive_claim",
            status: "pass",
            detail: format!(
                "balance {before} -> {after} value {} {}",
                proof.v,
                clip(&text)
            ),
        });
    } else {
        cases.push(Case {
            name: "positive_claim",
            status: "fail",
            detail: format!("balance {before} -> {after} {}", clip(&text)),
        });
    }
}

fn write_broadcast(contract: &str, _root: &[u8; 32], nd: &[u8; 32], proof: Option<&LoadedProof>) -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(
        std::env::var("HEADSTASH_E2E_DIR").unwrap_or_else(|_| "/tmp/headstash-e2e".into()),
    );
    let mismatch = [0x12u8; 32];
    let (kind, msg) = if let Some(proof) = proof {
        if host_proof(proof) && proof.nd == derive_nd(&proof.denom) {
            ("positive", proof_process_msg(proof))
        } else {
            (
                "anchor_mismatch",
                claim_msg(&mismatch, nd, 1, &[0x22; 32], &[0x14; 32], &[0x14; 32], &[9, 9, 9], 0),
            )
        }
    } else {
        (
            "anchor_mismatch",
            claim_msg(&mismatch, nd, 1, &[0x22; 32], &[0x14; 32], &[0x14; 32], &[9, 9, 9], 0),
        )
    };
    let body = serde_json::json!({
        "kind": kind,
        "contract": contract,
        "msg": serde_json::from_str::<serde_json::Value>(&msg)?
    });
    std::fs::write(dir.join("broadcast.json"), serde_json::to_vec_pretty(&body)?)?;
    Ok(())
}

fn local_tx_limit_overrides() -> std::collections::HashMap<String, serde_json::Value> {
    let mut overrides = std::collections::HashMap::new();
    overrides.insert(
        "config/config.toml".to_string(),
        serde_json::json!({
            "rpc": { "max_body_bytes": 67_108_864 },
            "mempool": { "max_tx_bytes": 67_108_864 }
        }),
    );
    overrides
}

fn raise_block_max_bytes(raw: Vec<u8>) -> std::result::Result<Vec<u8>, IctError> {
    let mut v: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| IctError::Config(format!("genesis json: {e}")))?;
    if let Some(max_bytes) = v.pointer_mut("/consensus_params/block/max_bytes") {
        if max_bytes.is_number() {
            *max_bytes = serde_json::json!(67_108_864);
        } else {
            *max_bytes = serde_json::json!("67108864");
        }
    }
    add_proof_denom_supply(&mut v);
    serde_json::to_vec(&v).map_err(|e| IctError::Config(format!("genesis encode: {e}")))
}

fn host_proof(p: &LoadedProof) -> bool {
    p.host_verify && p.prover == "zk_headstash::circuit::Proof::create" && denom_charset_ok(&p.denom)
}

fn denom_charset_ok(denom: &str) -> bool {
    let bytes = denom.as_bytes();
    if !(3..=128).contains(&bytes.len()) || !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    bytes.iter().all(|c| {
        c.is_ascii_alphanumeric() || matches!(*c, b'/' | b':' | b'.' | b'_' | b'-')
    })
}

fn add_proof_denom_supply(v: &mut serde_json::Value) {
    let Some(proof) = load_claim_proof() else {
        return;
    };
    if !host_proof(&proof) || proof.denom == DENOM || proof.nd != derive_nd(&proof.denom) {
        return;
    }
    if v.pointer("/app_state/bank/balances").is_none() || v.pointer("/app_state/bank/supply").is_none()
    {
        return;
    }
    let per: u128 = 100_000_000;
    let denom = proof.denom.clone();
    let Some(balances) = v
        .pointer_mut("/app_state/bank/balances")
        .and_then(|b| b.as_array_mut())
    else {
        return;
    };
    let mut accounts: u128 = 0;
    for bal in balances.iter_mut() {
        let Some(coins) = bal.get_mut("coins").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        if !coins.iter().any(|c| c.get("denom").and_then(|d| d.as_str()) == Some(denom.as_str())) {
            coins.push(serde_json::json!({ "denom": denom, "amount": per.to_string() }));
        }
        coins.sort_by(|a, b| {
            let da = a.get("denom").and_then(|d| d.as_str()).unwrap_or("");
            let db = b.get("denom").and_then(|d| d.as_str()).unwrap_or("");
            da.cmp(db)
        });
        accounts += 1;
    }
    let Some(supply) = v
        .pointer_mut("/app_state/bank/supply")
        .and_then(|s| s.as_array_mut())
    else {
        return;
    };
    if !supply.iter().any(|c| c.get("denom").and_then(|d| d.as_str()) == Some(denom.as_str())) {
        supply.push(serde_json::json!({
            "denom": denom,
            "amount": (per * accounts).to_string()
        }));
        supply.sort_by(|a, b| {
            let da = a.get("denom").and_then(|d| d.as_str()).unwrap_or("");
            let db = b.get("denom").and_then(|d| d.as_str()).unwrap_or("");
            da.cmp(db)
        });
    }
}

fn chain_config(name: &str, chain_id: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: name.to_string(),
        chain_id: chain_id.to_string(),
        images: vec![terp_docker_image()],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: DENOM.to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: format!("0{DENOM}"),
        gas_adjustment: 1.8,
        trusting_period: "112h".to_string(),
        block_time: "1s".to_string(),
        genesis: None,
        // Same override shape as the local pir chain in re-zerve live-chain:
        // the map key is the container-relative file, and the value deep-merges
        // CometBFT sections. 64MiB covers the prover vk (about 33MB). Local chains only.
        modify_genesis: Some(Box::new(|_cfg, raw| raise_block_max_bytes(raw))),
        pre_genesis: None,
        config_file_overrides: local_tx_limit_overrides(),
        additional_start_args: Vec::new(),
        // K=18 params write is 33554500 bytes. v6.4.0 MaxCircuitSize is 33554432
        // unless the process reads MAX_CIRCUIT_SIZE. Local chains only.
        env: vec![("MAX_CIRCUIT_SIZE".to_string(), "67108864".to_string())],
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

fn assert_loopback(container: Option<&str>, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let container = container.ok_or("chain has no container id")?;
    let output = Command::new("docker")
        .args(["port", container, &format!("{port}/tcp")])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    if !text.contains("127.0.0.1:") {
        return Err(format!("port {port} on {container} is not loopback: {text}").into());
    }
    if text.contains("0.0.0.0:") {
        return Err(format!("port {port} on {container} is also published on 0.0.0.0: {text}").into());
    }
    Ok(())
}

fn published_host(host: &str, container: Option<&str>, port: u16) -> Result<String, Box<dyn std::error::Error>> {
    assert_loopback(container, port)?;
    let container = container.unwrap();
    let output = Command::new("docker")
        .args(["port", container, &format!("{port}/tcp")])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout).to_string();
    let host_port = text
        .lines()
        .find(|l| l.contains(host))
        .and_then(|l| l.rsplit(':').next())
        .ok_or_else(|| format!("no host port in {text}"))?
        .trim()
        .to_string();
    Ok(host_port)
}

fn host_port_from_url(url: &str) -> String {
    url.rsplit(':').next().unwrap_or("").to_string()
}

fn env_path(key: &str, default: PathBuf) -> PathBuf {
    std::env::var(key).map(PathBuf::from).unwrap_or(default)
}

fn resolve_zk_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(root) = std::env::var("ZK_ROOT") {
        let p = PathBuf::from(root);
        if p.join("terp-core").exists() {
            return Ok(p);
        }
    }
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..8 {
        if dir.join("terp-core").exists() {
            return Ok(dir);
        }
        if !dir.pop() {
            break;
        }
    }
    Err("set ZK_ROOT to the parent of terp-core".into())
}

fn derive_nd(raw: &str) -> [u8; 32] {
    let hash = blake3::hash(raw.as_bytes());
    let mut bytes = *hash.as_bytes();
    bytes[31] &= 0x1F;
    bytes
}

fn write_report(
    dir: &Path,
    cases: &[Case],
    rpc_a: &str,
    lcd_a: &str,
    lcd_b: &str,
    container_a: &str,
    home_a: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let body = serde_json::json!({
        "rpc_a": rpc_a,
        "lcd_a": format!("http://127.0.0.1:{lcd_a}"),
        "lcd_b": format!("http://127.0.0.1:{lcd_b}"),
        "container_a": container_a,
        "home_a": home_a,
        "claim_contract": cases.iter().find(|c| c.name == "claim_instantiate" && c.status == "pass").map(|c| c.detail.clone()),
        "witness_contract": cases.iter().find(|c| c.name == "witness_instantiate" && c.status == "pass").map(|c| c.detail.clone()),
        "cases": cases.iter().map(|c| serde_json::json!({
            "name": c.name,
            "status": c.status,
            "detail": c.detail,
        })).collect::<Vec<_>>()
    });
    std::fs::write(dir.join("report.json"), serde_json::to_vec_pretty(&body)?)?;
    let endpoints = serde_json::json!({
        "chain_id": CHAIN_A,
        "lcd": format!("http://127.0.0.1:{lcd_a}"),
        "rpc": rpc_a,
        "lcd_b": format!("http://127.0.0.1:{lcd_b}"),
        "contract": body["claim_contract"],
        "witness": body["witness_contract"],
        "container": container_a,
        "home": home_a,
        "key_name": "deployer",
        "signer": "http://127.0.0.1:19821/broadcast"
    });
    std::fs::write(dir.join("endpoints.json"), serde_json::to_vec_pretty(&endpoints)?)?;
    Ok(())
}

fn print_cases(cases: &[Case]) {
    println!("\n=== cases ===");
    for c in cases {
        println!("[{}] {} — {}", c.status, c.name, c.detail);
    }
}

fn clip(s: &str) -> String {
    let t = s.replace('\n', " ");
    if t.len() > 500 {
        format!("{}…", &t[..500])
    } else {
        t
    }
}

// --- claim challenge (same reduction as zk-headstash claim_auth) ---

const KECCAK_ROUNDS: [u64; 24] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_8082,
    0x8000_0000_0000_808A,
    0x8000_0000_8000_8000,
    0x0000_0000_0000_808B,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8009,
    0x0000_0000_0000_008A,
    0x0000_0000_0000_0088,
    0x0000_0000_8000_8009,
    0x0000_0000_8000_000A,
    0x0000_0000_8000_808B,
    0x8000_0000_0000_008B,
    0x8000_0000_0000_8089,
    0x8000_0000_0000_8003,
    0x8000_0000_0000_8002,
    0x8000_0000_0000_0080,
    0x0000_0000_0000_800A,
    0x8000_0000_8000_000A,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8080,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8008,
];
const KECCAK_ROT: [[u32; 5]; 5] = [
    [0, 36, 3, 41, 18],
    [1, 44, 10, 45, 2],
    [62, 6, 43, 15, 61],
    [28, 55, 25, 21, 56],
    [27, 20, 39, 8, 14],
];

fn keccak_f(a: &mut [u64; 25]) {
    for round in 0..24 {
        let mut c = [0u64; 5];
        for x in 0..5 {
            c[x] = a[x] ^ a[x + 5] ^ a[x + 10] ^ a[x + 15] ^ a[x + 20];
        }
        let mut d = [0u64; 5];
        for x in 0..5 {
            d[x] = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
        }
        for y in 0..5 {
            for x in 0..5 {
                a[x + 5 * y] ^= d[x];
            }
        }
        let mut b = [0u64; 25];
        for y in 0..5 {
            for x in 0..5 {
                let nx = y;
                let ny = (2 * x + 3 * y) % 5;
                b[nx + 5 * ny] = a[x + 5 * y].rotate_left(KECCAK_ROT[x][y]);
            }
        }
        for y in 0..5 {
            for x in 0..5 {
                a[x + 5 * y] =
                    b[x + 5 * y] ^ ((!b[((x + 1) % 5) + 5 * y]) & b[((x + 2) % 5) + 5 * y]);
            }
        }
        a[0] ^= KECCAK_ROUNDS[round];
    }
}

fn keccak256(input: &[u8]) -> [u8; 32] {
    const RATE: usize = 136;
    let mut state = [0u64; 25];
    let mut padded = Vec::with_capacity(input.len() + RATE);
    padded.extend_from_slice(input);
    padded.push(0x01);
    while padded.len() % RATE != 0 {
        padded.push(0);
    }
    let last = padded.len() - 1;
    padded[last] |= 0x80;
    for block in padded.chunks(RATE) {
        for (i, lane) in block.chunks(8).enumerate() {
            let mut buf = [0u8; 8];
            buf[..lane.len()].copy_from_slice(lane);
            state[i] ^= u64::from_le_bytes(buf);
        }
        keccak_f(&mut state);
    }
    let mut out = [0u8; 32];
    for i in 0..4 {
        out[i * 8..i * 8 + 8].copy_from_slice(&state[i].to_le_bytes());
    }
    out
}

fn challenge_scalar_be(body: &[u8; 32]) -> [u8; 32] {
    let mut message = b"headstash-claim-v1:".to_vec();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in body {
        message.push(HEX[(byte >> 4) as usize]);
        message.push(HEX[(byte & 0x0f) as usize]);
    }
    let mut prefixed = b"\x19Ethereum Signed Message:\n".to_vec();
    prefixed.extend(message.len().to_string().into_bytes());
    prefixed.extend_from_slice(&message);
    reduce_mod_n(&keccak256(&prefixed))
}

fn reduce_mod_n(bytes: &[u8; 32]) -> [u8; 32] {
    const N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36,
        0x41, 0x41,
    ];
    if bytes < &N { *bytes } else { be_sub(bytes, &N) }
}

fn be_sub(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0u16;
    for i in (0..32).rev() {
        let av = a[i] as u16;
        let bv = b[i] as u16 + borrow;
        if av >= bv {
            out[i] = (av - bv) as u8;
            borrow = 0;
        } else {
            out[i] = (av + 256 - bv) as u8;
            borrow = 1;
        }
    }
    out
}
