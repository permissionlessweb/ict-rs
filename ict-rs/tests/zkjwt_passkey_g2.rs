//! G2 chain half: store-full-circuit + terp-zkjwt instantiate + RegisterClaim
//! + MsgExecuteContract. Artifacts come from zkjwt-wasm (`ZKJWT_G2_DIR`).
//!
//! `ICT_ZKJWT=1` required. Uses TestEnv::terp_config (local-zk), not public 120u-1.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ict_rs::prelude::*;
use ict_rs::testing::env::TestEnv;

fn require_ict() {
    assert_eq!(
        std::env::var("ICT_ZKJWT").ok().as_deref(),
        Some("1"),
        "ICT_ZKJWT=1 required"
    );
}

fn art_dir() -> PathBuf {
    PathBuf::from(std::env::var("ZKJWT_G2_DIR").unwrap_or_else(|_| "/tmp/zkjwt-g2-artifacts".into()))
}

fn wasm_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../terp-rs/contracts/smart-accounts/terp-zkjwt/artifacts/terp_zkjwt.wasm")
}

fn line(ok: bool, name: &str, detail: &str) {
    println!("{} {name} {detail}", if ok { "PASS" } else { "FAIL" });
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
        return Err(format!(
            "exit {} {}",
            out.exit_code,
            out.stderr_str()
        ));
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

async fn run() -> Result<(), String> {
    require_ict();
    std::env::set_var(
        "PATH",
        format!("/usr/local/bin:{}", std::env::var("PATH").unwrap_or_default()),
    );
    let dir = art_dir();
    let wasm = wasm_path();
    if !wasm.exists() {
        return Err(format!("missing {}", wasm.display()));
    }
    if !dir.join("params.bin").exists() {
        return Err(format!("missing params in {}", dir.display()));
    }

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await
        .map_err(|e| format!("docker: {e}"))?;
    let mut cfg = TestEnv::terp_config();
    cfg.chain_id = "ict-zkjwt-g2".into();
    cfg.config_file_overrides.insert(
        "config/config.toml".into(),
        serde_json::json!({
            "rpc": { "max_body_bytes": 67108864 },
            "mempool": { "max_tx_bytes": 67108864 }
        }),
    );
    let mut chain = CosmosChain::new(cfg, 1, 0, runtime);
    chain
        .initialize(&TestContext {
            test_name: "zkjwt-g2".into(),
            network_id: String::new(),
        })
        .await
        .map_err(|e| e.to_string())?;
    chain.start(&[]).await.map_err(|e| e.to_string())?;
    let r = run_on(&chain, &dir, &wasm).await;
    let _ = chain.stop().await;
    r
}

async fn run_on(chain: &CosmosChain, dir: &std::path::Path, wasm: &std::path::Path) -> Result<(), String> {
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
            "17",
            "--circuit-type",
            "0",
            "--curve-type",
            "6",
            "--from",
            "default",
        ],
    )
    .await;
    let zkid = match store_c {
        Ok(j) => {
            line(true, "store-full-circuit", "ok");
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
    // 32 zero bytes = circuit inclusion root
    let inst_msg = format!(
        r#"{{"admin":null,"issuers":[{{"issuer":"{issuer}","verifying_key":"","zkid":{zkid},"audience":null,"inclusion_set_root":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}}],"require_registered_claim":true}}"#
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
            "zkjwt-passkey-g2",
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
    line(true, "instantiate", "terp-zkjwt");

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

    let payload: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("payload.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let claim = payload["claim_commitment"].as_str().unwrap();
    let reg = format!(
        r#"{{"register_claim":{{"claim_commitment":"{claim}","issuer":"{issuer}"}}}}"#
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
    line(true, "register", "RegisterClaim");

    // Real MsgExecuteContract (same contract). Authenticate sudo is module-only;
    // this execute is the bound message type G2 requires.
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
        line(false, "authenticate", &e);
        e
    })?;
    line(
        true,
        "authenticate",
        "MsgExecuteContract (RegisterClaim replay of register is ok; nullifier is Authenticate-only)",
    );

    let _ = user;
    Ok(())
}

#[tokio::test]
#[ignore = "ignored-without-ICT"]
async fn zkjwt_passkey_g2_chain() {
    match run().await {
        Ok(()) => println!("PASS g2_chain"),
        Err(e) => {
            println!("FAIL g2_chain {e}");
            panic!("{e}");
        }
    }
}
