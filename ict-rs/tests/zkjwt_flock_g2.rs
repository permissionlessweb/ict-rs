//! Flock Path A on ict-rs TestEnv (terp_config / local-zk).
//! Footer-only VK (`curve_id=7`, `prover_id=3`) + BLAKE3 action bind.
//! `ICT_ZKJWT=1` (same gate as Halo2 G2). Not public 120u-1.
//!
//! Auth waist is ante → sudo `AuthSudoMsg::Authenticate` (not wasm execute).

use std::path::PathBuf;
use std::time::Duration;

use ict_rs::prelude::*;
use ict_rs::testing::env::TestEnv;
use sha2::{Digest, Sha256};
use zk_cosmwasm::curves::{prove_flock, CurveType, FlockVerifyingKey};
use zk_cosmwasm::{CircuitFooter, CircuitType};

fn require_ict() {
    assert_eq!(
        std::env::var("ICT_ZKJWT").ok().as_deref(),
        Some("1"),
        "ICT_ZKJWT=1 required"
    );
}

fn line(ok: bool, name: &str, detail: &str) {
    println!("{} {name} {detail}", if ok { "PASS" } else { "FAIL" });
}

fn wasm_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../terp-rs/contracts/smart-accounts/terp-zkjwt/artifacts/terp_zkjwt.wasm")
}

async fn wait_tx(chain: &CosmosChain, hash: &str) -> Result<serde_json::Value, String> {
    for _ in 0..25 {
        let _ = wait_for_blocks(chain, 1).await;
        let q = chain
            .chain_exec(&["query", "tx", hash, "--output", "json"])
            .await
            .map_err(|e| e.to_string())?;
        let s = q.stdout_str().trim().to_string();
        if s.is_empty() || s.contains("not found") {
            tokio::time::sleep(Duration::from_millis(400)).await;
            continue;
        }
        return serde_json::from_str(&s).map_err(|e| format!("{e}: {s}"));
    }
    Err(format!("timeout {hash}"))
}

async fn broadcast(chain: &CosmosChain, args: &[&str]) -> Result<serde_json::Value, String> {
    let out = chain.chain_exec_tx(args).await.map_err(|e| e.to_string())?;
    if out.exit_code != 0 {
        return Err(format!("exit {} {}", out.exit_code, out.stderr_str()));
    }
    let j: serde_json::Value =
        serde_json::from_str(out.stdout_str().trim()).unwrap_or(serde_json::Value::Null);
    let hash = j["txhash"].as_str().unwrap_or("").to_string();
    if hash.is_empty() {
        return Err(format!("no txhash {}", out.stdout_str()));
    }
    let q = wait_tx(chain, &hash).await?;
    let code = q["code"].as_u64().unwrap_or(999);
    if code != 0 {
        return Err(format!(
            "code={code} {}",
            q["raw_log"].as_str().unwrap_or("")
        ));
    }
    Ok(q)
}

fn extract_zkid(j: &serde_json::Value) -> Option<u64> {
    let logs = j.get("logs")?.as_array()?;
    for log in logs {
        let evs = log.get("events")?.as_array()?;
        for ev in evs {
            let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if ty == "store_full_circuit" || ty == "store_circuit" {
                if let Some(attrs) = ev.get("attributes").and_then(|a| a.as_array()) {
                    for a in attrs {
                        let k = a.get("key").and_then(|x| x.as_str()).unwrap_or("");
                        if k == "zk_id" || k == "zkid" {
                            return a.get("value").and_then(|v| v.as_str())?.parse().ok();
                        }
                    }
                }
            }
        }
    }
    None
}

/// Host-only (no Docker): Flock prove/verify + footer dispatch.
#[test]
fn flock_host_roundtrip_no_docker() {
    let mut inst = [0u8; 64];
    inst[32..64].copy_from_slice(&[0xAB; 32]);
    let proof = prove_flock(&inst);
    zk_cosmwasm::curves::verify_flock_proof(&proof, &inst).expect("verify");
    let vk = FlockVerifyingKey::lean_default();
    let any = zk_cosmwasm::AnyVerifyingKey::try_from(vk.to_blob().as_slice()).expect("vk");
    assert_eq!(any.curve_id(), 7);
    assert_eq!(any.prover_id(), 3);
    line(true, "flock_host", "curve=7 prover=3 blake3-bind");
}

async fn run() -> Result<(), String> {
    require_ict();
    let dir = PathBuf::from("/tmp/zkjwt-flock-g2");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // 64B PI: nullifier||claim — no msg_bind so sudo bind check is skipped.
    let mut inst = [0u8; 64];
    inst[32..64].copy_from_slice(&[0xAB; 32]);
    let proof = prove_flock(&inst);
    let params = [0u8; 32];
    let ph: [u8; 32] = Sha256::digest(params).into();
    let vh: [u8; 32] = Sha256::digest([]).into();
    let vk = FlockVerifyingKey {
        footer: CircuitFooter::new(
            CircuitType::Flock,
            CurveType::FlockBlake3,
            1,
            2,
            params.len() as u32,
            0,
            0,
            ph,
            vh,
        ),
    };
    std::fs::write(dir.join("params.bin"), params).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("vk_body.bin"), vk.to_blob()).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("proof.bin"), &proof).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("instances.bin"), inst).map_err(|e| e.to_string())?;

    let wasm = wasm_path();
    if !wasm.exists() {
        return Err(format!("missing {}", wasm.display()));
    }

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await
        .map_err(|e| format!("docker: {e}"))?;
    let mut cfg = TestEnv::terp_config();
    cfg.chain_id = "ict-zkjwt-flock".into();
    cfg.modify_genesis = Some(Box::new(|_cfg, bytes| {
        let mut g: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| ict_rs::error::IctError::Other(anyhow::anyhow!(e)))?;
        if let Some(p) = g.pointer_mut("/app_state/smartaccount/params") {
            p["maximum_unauthenticated_gas"] = serde_json::json!(2_000_000);
        }
        serde_json::to_vec(&g).map_err(|e| ict_rs::error::IctError::Other(anyhow::anyhow!(e)))
    }));
    let mut chain = CosmosChain::new(cfg, 1, 0, runtime);
    chain
        .initialize(&TestContext {
            test_name: "zkjwt-flock".into(),
            network_id: String::new(),
        })
        .await
        .map_err(|e| e.to_string())?;
    chain.start(&[]).await.map_err(|e| e.to_string())?;
    let r = run_on(&chain, &dir, &wasm, &inst, &proof).await;
    let _ = chain.stop().await;
    r
}

async fn run_on(
    chain: &CosmosChain,
    dir: &std::path::Path,
    wasm: &std::path::Path,
    inst: &[u8; 64],
    proof: &[u8],
) -> Result<(), String> {
    chain.create_key("default").await.map_err(|e| e.to_string())?;
    let user = chain
        .primary_node()
        .map_err(|e| e.to_string())?
        .get_key_address("default")
        .await
        .map_err(|e| e.to_string())?;
    chain
        .send_funds(
            "validator",
            &WalletAmount {
                address: user.clone(),
                denom: "uterp".into(),
                amount: 50_000_000_000,
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    wait_for_blocks(chain, 2).await.map_err(|e| e.to_string())?;

    let node = chain.primary_node().map_err(|e| e.to_string())?;
    node.copy_file_from_host(wasm, "/tmp/terp_zkjwt.wasm")
        .await
        .map_err(|e| e.to_string())?;
    node.copy_file_from_host(&dir.join("params.bin"), "/tmp/params.bin")
        .await
        .map_err(|e| e.to_string())?;
    node.copy_file_from_host(&dir.join("vk_body.bin"), "/tmp/vk_body.bin")
        .await
        .map_err(|e| e.to_string())?;

    let store_c = broadcast(
        chain,
        &[
            "tx",
            "wasm",
            "store-full-circuit",
            "/tmp/params.bin",
            "/tmp/vk_body.bin",
            "--k",
            "1",
            "--circuit-type",
            "3",
            "--curve-type",
            "7",
            "--from",
            "default",
        ],
    )
    .await;
    let zkid = match store_c {
        Ok(j) => {
            line(true, "store-full-circuit", "flock curve=7");
            extract_zkid(&j).unwrap_or(1)
        }
        Err(e) => {
            line(false, "store-full-circuit", &e);
            return Err(format!("store-full-circuit BLOCK: {e}"));
        }
    };

    let _ = broadcast(
        chain,
        &[
            "tx",
            "wasm",
            "store",
            "/tmp/terp_zkjwt.wasm",
            "--from",
            "default",
        ],
    )
    .await?;

    let issuer = "terp.network/passkey-zkjwt";
    let inst_msg = format!(
        r#"{{"admin":null,"issuers":[{{"issuer":"{issuer}","verifying_key":"","zkid":{zkid},"audience":null,"inclusion_set_root":null}}],"require_registered_claim":false}}"#
    );
    broadcast(
        chain,
        &[
            "tx",
            "wasm",
            "instantiate",
            "1",
            &inst_msg,
            "--label",
            "zkjwt-flock-g2",
            "--no-admin",
            "--from",
            "default",
        ],
    )
    .await
    .map_err(|e| {
        line(false, "instantiate", &e);
        e
    })?;
    line(true, "instantiate", "terp-zkjwt flock zkid");

    let q = chain
        .chain_exec(&[
            "query",
            "wasm",
            "list-contract-by-code",
            "1",
            "--output",
            "json",
        ])
        .await
        .map_err(|e| e.to_string())?;
    let qj: serde_json::Value = serde_json::from_str(q.stdout_str().trim()).unwrap_or_default();
    let contract = qj["contracts"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("no contract {}", q.stdout_str()))?
        .to_string();

    let claim_b64 = base64_simple(&inst[32..64]);
    let reg = format!(
        r#"{{"register_claim":{{"claim_commitment":"{claim_b64}","issuer":"{issuer}"}}}}"#
    );
    broadcast(
        chain,
        &[
            "tx",
            "wasm",
            "execute",
            &contract,
            &reg,
            "--from",
            "default",
        ],
    )
    .await
    .map_err(|e| {
        line(false, "register", &e);
        e
    })?;
    line(true, "register", "RegisterClaim execute");

    let init_json = format!(
        r#"{{"contract":"{contract}","params":"{}"}}"#,
        base64_simple(b"{}")
    );
    let init_b64 = base64_simple(init_json.as_bytes());
    broadcast(
        chain,
        &[
            "tx",
            "smartaccount",
            "add-authenticator",
            "CosmwasmAuthenticatorV1",
            &init_b64,
            "--from",
            "default",
        ],
    )
    .await
    .map_err(|e| {
        line(false, "add-authenticator", &e);
        e
    })?;
    line(true, "add-authenticator", "CosmwasmAuthenticatorV1");

    let aq = chain
        .chain_exec(&[
            "query",
            "smartaccount",
            "authenticators",
            &user,
            "--output",
            "json",
        ])
        .await
        .map_err(|e| e.to_string())?;
    let aj: serde_json::Value = serde_json::from_str(aq.stdout_str().trim()).unwrap_or_default();
    let auth_id = extract_auth_id(&aj).ok_or_else(|| format!("no authenticator id {}", aq.stdout_str()))?;
    line(true, "authenticator-id", &auth_id.to_string());

    let proof_b64 = base64_simple(proof);
    let pi_b64 = base64_simple(inst);
    let payload = format!(
        r#"{{"issuer":"{issuer}","claim_commitment":"{claim_b64}","public_inputs":"{pi_b64}","proof":"{proof_b64}","circuit_id":"zkjwt.passkey.flock.v1"}}"#
    );

    let gen = chain
        .chain_exec(&[
            "tx",
            "bank",
            "send",
            &user,
            &user,
            "1uterp",
            "--generate-only",
            "--from",
            "default",
            "--output",
            "json",
        ])
        .await
        .map_err(|e| e.to_string())?;
    if gen.exit_code != 0 {
        return Err(format!("generate-only {}", gen.stderr_str()));
    }
    let mut txj: serde_json::Value =
        serde_json::from_str(gen.stdout_str().trim()).map_err(|e| format!("gen json {e}"))?;
    if txj.get("body").is_none() && txj.get("tx").is_some() {
        txj = txj["tx"].clone();
    }
    txj["auth_info"]["fee"] = serde_json::json!({
        "amount": [{"denom": "uterp", "amount": "50000"}],
        "gas_limit": "2000000",
        "payer": "",
        "granter": ""
    });
    txj["body"]["non_critical_extension_options"] = serde_json::json!([{
        "@type": "/terp.smartaccount.v1beta1.TxExtension",
        "selected_authenticators": [auth_id],
    }]);
    let unsigned_host = dir.join("unsigned.json");
    std::fs::write(&unsigned_host, serde_json::to_vec_pretty(&txj).unwrap())
        .map_err(|e| e.to_string())?;
    node.copy_file_from_host(&unsigned_host, "/tmp/unsigned.json")
        .await
        .map_err(|e| e.to_string())?;

    let signed = chain
        .chain_exec_tx(&["tx", "sign", "/tmp/unsigned.json", "--from", "default"])
        .await
        .map_err(|e| e.to_string())?;
    if signed.exit_code != 0 {
        return Err(format!("sign {}", signed.stderr_str()));
    }
    let mut signed_j: serde_json::Value = serde_json::from_str(signed.stdout_str().trim())
        .map_err(|e| format!("signed json {e}: {}", signed.stdout_str()))?;
    if signed_j.get("body").is_none() && signed_j.get("tx").is_some() {
        signed_j = signed_j["tx"].clone();
    }
    let sig_b64 = base64_simple(payload.as_bytes());
    signed_j["signatures"] = serde_json::json!([sig_b64]);
    let signed_host = dir.join("signed.json");
    std::fs::write(&signed_host, serde_json::to_vec_pretty(&signed_j).unwrap())
        .map_err(|e| e.to_string())?;
    node.copy_file_from_host(&signed_host, "/tmp/signed.json")
        .await
        .map_err(|e| e.to_string())?;

    let bc = chain
        .chain_exec_tx(&["tx", "broadcast", "/tmp/signed.json"])
        .await
        .map_err(|e| e.to_string())?;
    if bc.exit_code != 0 {
        line(false, "authenticate", &bc.stderr_str());
        return Err(format!("broadcast {}", bc.stderr_str()));
    }
    let bj: serde_json::Value =
        serde_json::from_str(bc.stdout_str().trim()).unwrap_or(serde_json::Value::Null);
    let hash = bj["txhash"].as_str().unwrap_or("").to_string();
    if hash.is_empty() {
        return Err(format!("no txhash {}", bc.stdout_str()));
    }
    let bcode = bj["code"].as_u64().unwrap_or(0);
    if bcode != 0 {
        let log = bj["raw_log"].as_str().unwrap_or("");
        line(false, "authenticate", log);
        return Err(format!("ante/sudo Authenticate broadcast code={bcode} {log}"));
    }
    let qtx = wait_tx(chain, &hash).await.map_err(|e| {
        line(false, "authenticate", format!("timeout hash={hash} broadcast={}", bc.stdout_str()).as_str());
        format!("timeout {hash} broadcast={}", bc.stdout_str())
    })?;
    let code = qtx["code"].as_u64().unwrap_or(999);
    if code != 0 {
        let log = qtx["raw_log"].as_str().unwrap_or("");
        line(false, "authenticate", log);
        return Err(format!("ante/sudo Authenticate failed code={code} {log}"));
    }
    line(true, "authenticate", "ante sudo AuthSudoMsg::Authenticate");
    let _ = zkid;
    Ok(())
}

fn extract_auth_id(j: &serde_json::Value) -> Option<u64> {
    let arr = j
        .get("account_authenticators")
        .or_else(|| j.get("authenticators"))
        .and_then(|v| v.as_array())?;
    let last = arr.last()?;
    last.get("id")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
}

fn base64_simple(data: &[u8]) -> String {
    const CH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CH[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CH[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CH[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CH[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[tokio::test]
#[ignore = "ignored-without-ICT"]
async fn zkjwt_flock_g2_chain() {
    match run().await {
        Ok(()) => println!("PASS flock_g2_chain"),
        Err(e) => {
            println!("FAIL flock_g2_chain {e}");
            panic!("{e}");
        }
    }
}
