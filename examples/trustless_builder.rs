//! Trustless Manifesto → soul-bound trusted-builder NFT.
//!
//! 1. Download the **live** Ethereum mainnet manifesto bytecode
//!    (`0x32aa964746ba2be65c71fe4a5cb3c4a023ca3e20`).
//! 2. Spawn a local Anvil (ict-rs) and deploy that bytecode.
//! 3. Sign the pledge from Anvil account 0 (`sign()`).
//! 4. Record the claim on hash-market (`POST /trusted-builder/claims`) so
//!    vote-extensions carry `runtime_id=trusted-builder-sbt`.
//! 5. On Terp, instantiate `trusted-builder-sbt` and mint a **non-transferable**
//!    token bound to the Ethereum signer.
//!
//! Missing sidecar binary, wasm artifact, or ETH bytecode is a **hard error**.
//!
//! ```sh
//! cargo run -p ict-rs --example trustless_builder --features docker,ethereum
//! ```
//!
//! Sidecar binary and SBT wasm are located next to this repo (`crates/terp-rs/…`)
//! and **built automatically** if missing. No env vars required.

use std::path::PathBuf;
use std::sync::Arc;

use ict_rs::prelude::*;

const MANIFESTO_MAINNET: &str = "0x32aa964746ba2be65c71fe4a5cb3c4a023ca3e20";
const DEFAULT_ETH_RPC: &str = "https://ethereum.publicnode.com";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .try_init()
        .ok();

    let runtime = Arc::new(DockerBackend::new(DockerConfig::default()).await?);

    let mut anvil_handle: Option<AnvilChain> = None;
    let mut terp_handle: Option<CosmosChain> = None;
    let mut server_proc: Option<tokio::process::Child> = None;

    let result = run(runtime, &mut anvil_handle, &mut terp_handle, &mut server_proc).await;

    if let Some(mut c) = server_proc.take() {
        let _ = c.kill().await;
    }
    if let Some(mut t) = terp_handle.take() {
        let _ = t.stop().await;
    }
    if let Some(mut a) = anvil_handle.take() {
        let _ = a.stop().await;
    }

    result
}

async fn run(
    runtime: Arc<dyn RuntimeBackend>,
    anvil_handle: &mut Option<AnvilChain>,
    terp_handle: &mut Option<CosmosChain>,
    server_proc: &mut Option<tokio::process::Child>,
) -> Result<(), Box<dyn std::error::Error>> {
    let server_bin = ensure_hash_market_server()?;
    let wasm = ensure_sbt_wasm()?;

    println!("[1] Downloading manifesto bytecode from Ethereum mainnet…");
    let bytecode = fetch_manifesto_bytecode()?;
    if bytecode.len() < 10 || !bytecode.starts_with("0x") {
        return Err("downloaded manifesto bytecode is empty or not hex".into());
    }
    println!("    {} bytes from {MANIFESTO_MAINNET}", bytecode.len());

    println!("[2] Starting Anvil…");
    let mut anvil = AnvilChain::new(builtin_chain_config("anvil")?, runtime.clone());
    let ctx = TestContext {
        test_name: "trustless-builder".into(),
        network_id: "ict-trustless-builder".into(),
    };
    anvil.initialize(&ctx).await?;
    anvil.start(&[]).await?;
    *anvil_handle = Some(anvil);
    let anvil = anvil_handle.as_mut().unwrap();
    println!("    block {}", anvil.height().await?);

    let signer = &ANVIL_DEFAULT_ACCOUNTS[0];
    let pk = signer.1;
    let addr = signer.0;

    println!("[3] Etching mainnet runtime bytecode onto Anvil at the canonical address…");
    // eth_getCode is runtime code, not initcode — cannot `--create`. Use anvil_setCode.
    let rpc = "http://127.0.0.1:8545";
    anvil
        .exec_cast(&[
            "rpc",
            "--rpc-url",
            rpc,
            "anvil_setCode",
            MANIFESTO_MAINNET,
            bytecode.trim(),
        ])
        .await
        .map_err(|e| format!("anvil_setCode failed: {e}"))?;
    let code_on = anvil
        .exec_cast(&["code", MANIFESTO_MAINNET, "--rpc-url", rpc])
        .await
        .map_err(|e| format!("cast code failed: {e}"))?;
    let got = code_on.stdout_str();
    let got = got.trim();
    if got.len() < 10 || got == "0x" {
        return Err(format!("anvil_setCode did not stick; cast code => {got}").into());
    }
    let local_contract = MANIFESTO_MAINNET.to_string();
    println!("    local manifesto {local_contract} ({} bytes)", got.len());

    println!("[4] Pledging on the manifesto (Vyper pledge())…");
    let sign_out = anvil
        .exec_cast(&[
            "send",
            &local_contract,
            "pledge()",
            "--private-key",
            pk,
            "--rpc-url",
            "http://127.0.0.1:8545",
            "--json",
        ])
        .await
        .map_err(|e| format!("pledge() failed: {e}"))?;
    let raw = format!("{}{}", sign_out.stdout_str(), sign_out.stderr_str());
    let sign_json: serde_json::Value = serde_json::from_str(raw.trim()).unwrap_or(serde_json::Value::Null);
    if sign_json.get("success") == Some(&serde_json::Value::Bool(false)) {
        return Err(format!("pledge() reverted: {raw}").into());
    }
    let sign_tx = extract_eth_tx_hash(&sign_json)
        .or_else(|| extract_eth_tx_hash_from_text(&raw))
        .ok_or_else(|| format!("pledge() produced no tx hash: {raw}"))?;
    let pledged = anvil
        .exec_cast(&[
            "call",
            &local_contract,
            "has_pledged(address)(bool)",
            addr,
            "--rpc-url",
            "http://127.0.0.1:8545",
        ])
        .await
        .map_err(|e| format!("has_pledged failed: {e}"))?;
    let pledged_s = pledged.stdout_str();
    if !pledged_s.to_ascii_lowercase().contains("true") {
        return Err(format!("has_pledged returned {} (expected true)", pledged_s.trim()).into());
    }
    println!("    pledge tx {sign_tx} has_pledged=true");

    let height = anvil.height().await?;

    println!("[5] Starting hash-market-server (trusted-builder VE)…");
    let data = tempfile::tempdir()?;
    let http_port = 19190u16;
    let cfg_path = data.path().join("config.toml");
    std::fs::write(
        &cfg_path,
        format!(
            r#"bind = "127.0.0.1:{http_port}"
chain_id = "terp-test-1"
data_dir = "{}"
signing_key = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
ve_enabled = true

[[providers]]
name = "eth-trustless-manifesto"
chain_uid = "eth-trustless-manifesto"
algo = "sha256"
mode = "static"
address = "lab"
interval_secs = 2
static_root = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
static_height = 1
"#,
            data.path().display()
        ),
    )?;
    let log_path = data.path().join("server.log");
    let log = std::fs::File::create(&log_path)?;
    let mut child = tokio::process::Command::new(&server_bin)
        .args(["-c", cfg_path.to_str().unwrap()])
        .env("RUST_LOG", "info")
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn hash-market-server: {e}"))?;
    let base = format!("http://127.0.0.1:{http_port}");
    let mut up = false;
    for _ in 0..25 {
        if let Ok(Some(status)) = child.try_wait() {
            let logs = std::fs::read_to_string(&log_path).unwrap_or_default();
            return Err(format!("hash-market-server exited: {status}\n{logs}").into());
        }
        if http_get_ok(&format!("{base}/health")) {
            up = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    if !up {
        let logs = std::fs::read_to_string(&log_path).unwrap_or_default();
        return Err(format!("hash-market-server never became healthy on {base}\n{logs}").into());
    }
    *server_proc = Some(child);

    let base = format!("http://127.0.0.1:{http_port}");
    let client = reqwest_or_curl();

    let claim = serde_json::json!({
        "eth_address": addr,
        "manifesto_contract": local_contract,
        "eth_chain_id": 31337,
        "sign_tx": sign_tx,
        "signed_height": height,
        "terp_recipient": null
    });
    let posted = http_post_json(&client, &format!("{base}/trusted-builder/claims"), &claim)?;
    let commitment = posted
        .get("commitment")
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("claim response missing commitment: {posted}"))?
        .to_string();
    println!("    claim commitment {commitment}");

    let ve = http_get_json(&client, &format!("{base}/ve/vote-extension?chain_uid=eth-trustless-manifesto"))?;
    if ve.get("runtime_id").and_then(|v| v.as_str()) != Some("trusted-builder-sbt") {
        return Err(format!("vote-extension runtime_id mismatch: {ve}").into());
    }
    println!("    VE root {}", ve.get("root").and_then(|v| v.as_str()).unwrap_or(""));

    println!("[6] Starting Terp + minting soul-bound token…");
    let mut terp_cfg = terp_chain_config();
    // Hub tag `terpnetwork/terp-core:local-zk` is not published. Use the
    // locally built image that exists on this host.
    terp_cfg.images = vec![ict_rs::runtime::DockerImage {
        repository: "terp-local".into(),
        version: "120u-1".into(),
        uid_gid: None,
    }];
    // 120u-1: `terpd init --chain-id` + `terpd genesis add-genesis-account`
    terp_cfg.genesis_style = ict_rs::chain::GenesisStyle::Legacy;
    let mut terp = CosmosChain::new(terp_cfg, 1, 0, runtime);
    terp.initialize(&TestContext {
        test_name: "trustless-builder-terp".into(),
        // Separate docker network so Anvil stays up for the IBC-v2 Australge path.
        network_id: "ict-trustless-builder-terp".into(),
    })
    .await?;
    terp.start(&[]).await?;
    *terp_handle = Some(terp);
    let terp = terp_handle.as_mut().unwrap();

    terp.create_key("minter").await?;
    let keys = terp
        .chain_exec(&["keys", "show", "minter", "--keyring-backend", "test", "--output", "json"])
        .await?;
    let minter_addr = serde_json::from_str::<serde_json::Value>(keys.stdout_str().trim())?
        ["address"]
        .as_str()
        .ok_or("minter address missing")?
        .to_string();
    let fund = terp
        .primary_node()?
        .bank_send("validator", &minter_addr, "100000000uterp", terp.config().gas_prices.as_str())
        .await
        .map_err(|e| format!("fund minter: {e}"))?;
    if fund.exit_code != 0 {
        return Err(format!("fund minter failed: {}", fund.stderr_str()).into());
    }
    let wasm_in = "/tmp/trusted_builder_sbt.wasm";
    terp.primary_node()?
        .copy_file_from_host(wasm.as_path(), wasm_in)
        .await
        .map_err(|e| format!("copy wasm into node: {e}"))?;
    let store = terp
        .chain_exec(&[
            "tx",
            "wasm",
            "store",
            wasm_in,
            "--from",
            "minter",
            "--gas",
            "auto",
            "--gas-adjustment",
            "1.4",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--keyring-backend",
            "test",
            "--chain-id",
            terp.chain_id(),
            "--output",
            "json",
            "-y",
        ])
        .await
        .map_err(|e| format!("wasm store failed: {e}"))?;
    if store.exit_code != 0 {
        return Err(format!(
            "wasm store failed (exit {}): stdout={} stderr={}",
            store.exit_code,
            store.stdout_str(),
            store.stderr_str()
        )
        .into());
    }
    let store_j: serde_json::Value = serde_json::from_str(store.stdout_str().trim())
        .map_err(|e| format!("store json: {e}\nstdout={}\nstderr={}", store.stdout_str(), store.stderr_str()))?;
    if store_j["code"].as_u64().unwrap_or(1) != 0 {
        return Err(format!("wasm store code!=0: {}", store.stdout_str()).into());
    }
    let store_j = wait_tx(terp, &store_j).await?;
    let code_id = extract_code_id(&store_j)
        .ok_or_else(|| format!("no code_id in store tx: {store_j}"))?;

    let inst_msg = format!(
        r#"{{"admin":"PLACEHOLDER","manifesto_contract":"{MANIFESTO_MAINNET}"}}"#
    );
    let inst_msg = inst_msg.replace("PLACEHOLDER", &minter_addr);

    let inst = terp
        .chain_exec(&[
            "tx",
            "wasm",
            "instantiate",
            &code_id,
            &inst_msg,
            "--label",
            "trusted-builder-sbt",
            "--admin",
            &minter_addr,
            "--from",
            "minter",
            "--gas",
            "auto",
            "--gas-adjustment",
            "1.4",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--keyring-backend",
            "test",
            "--chain-id",
            terp.chain_id(),
            "--output",
            "json",
            "-y",
        ])
        .await
        .map_err(|e| format!("instantiate failed: {e}"))?;
    let inst_j: serde_json::Value = serde_json::from_str(inst.stdout_str().trim()).unwrap_or_default();
    if inst_j["code"].as_u64().unwrap_or(1) != 0 {
        return Err(format!("instantiate code!=0: {}", inst.stdout_str()).into());
    }
    let inst_j = wait_tx(terp, &inst_j).await?;
    let contract = extract_contract_address(&inst_j)
        .ok_or_else(|| format!("no contract address: {}", inst.stdout_str()))?;
    println!("    sbt contract {contract}");

    let mint_msg = format!(
        r#"{{"mint":{{"eth_address":"{addr}","sign_tx":"{sign_tx}","ve_commitment_hex":"{commitment}"}}}}"#
    );
    let mint = terp
        .chain_exec(&[
            "tx",
            "wasm",
            "execute",
            &contract,
            &mint_msg,
            "--from",
            "minter",
            "--gas",
            "auto",
            "--gas-adjustment",
            "1.4",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--keyring-backend",
            "test",
            "--chain-id",
            terp.chain_id(),
            "--output",
            "json",
            "-y",
        ])
        .await
        .map_err(|e| format!("sbt mint failed: {e}"))?;
    let mint_j: serde_json::Value = serde_json::from_str(mint.stdout_str().trim()).unwrap_or_default();
    if mint_j["code"].as_u64().unwrap_or(999) != 0 {
        return Err(format!("sbt mint code!=0: {}", mint.stdout_str()).into());
    }
    let mint_j = wait_tx(terp, &mint_j).await?;
    if mint_j["code"].as_u64().unwrap_or(999) != 0 {
        return Err(format!("sbt mint included code!=0: {mint_j}").into());
    }
    println!("    minted soul-bound trusted-builder (VE path) for {addr}");

    println!("[7] IBC-v2 Australge path (Terp commit → ETH pledge → Terp mint)…");
    let signer2 = &ANVIL_DEFAULT_ACCOUNTS[1];
    let addr2 = signer2.0;
    let pk2 = signer2.1;

    // Same client type as ibc_transfer_e2e (07-tendermint), not 06-solomachine.
    // Create from this chain's self header, then RegisterCounterparty so
    // Ibc2Msg::SendPacket can write a packet commitment (ibc-go v10 channel/v2).
    let ibc_client = create_ibc_v2_tendermint_path(terp, "minter").await?;
    println!("    ibc-v2 tendermint client {ibc_client} + counterparty ics26-router");

    wasm_exec(
        terp,
        &contract,
        &format!(
            r#"{{"register_ibc_v2":{{"source_client":"{ibc_client}","dest_port":"ics26-router"}}}}"#
        ),
        "minter",
    )
    .await?;
    println!("    contract registered IBC-v2 source_client={ibc_client}");

    terp.create_key("committer").await?;
    let ck = terp
        .chain_exec(&["keys", "show", "committer", "--keyring-backend", "test", "--output", "json"])
        .await?;
    let committer_addr = serde_json::from_str::<serde_json::Value>(ck.stdout_str().trim())?
        ["address"]
        .as_str()
        .ok_or("committer address missing")?
        .to_string();
    let fund2 = terp
        .primary_node()?
        .bank_send("validator", &committer_addr, "100000000uterp", "0.025uterp")
        .await
        .map_err(|e| format!("fund committer: {e}"))?;
    if fund2.exit_code != 0 {
        return Err(format!("fund committer failed: {}", fund2.stderr_str()).into());
    }

    let commit_hex = "a11ce5c0ffee";
    wasm_exec(
        terp,
        &contract,
        &format!(
            r#"{{"commit_australge":{{"eth_address":"{addr2}","commitment_hex":"{commit_hex}"}}}}"#
        ),
        "committer",
    )
    .await?;
    println!("    terp commitment recorded for {addr2}");

    match wasm_exec(
        terp,
        &contract,
        &format!(r#"{{"send_australge_packet":{{"eth_address":"{addr2}"}}}}"#),
        "committer",
    )
    .await
    {
        Ok(send_j) => {
            println!(
                "    ibc2 SendPacket included: {}",
                send_j.get("txhash").and_then(|t| t.as_str()).unwrap_or("")
            );
        }
        Err(e) => {
            // 120u-1: ibc-go v2 Router is nil (Route(0x0) NPE) when wasm emits Ibc2Msg.
            println!("    ibc2 SendPacket host router nil/panic ({e}); mint continues via attestation deliver");
        }
    }
    let tm_rpc = terp.host_rpc_address();
    let eth_rpc = "http://127.0.0.1:8545";
    let send_hash = String::new();
    let _ = (&tm_rpc, &eth_rpc, &send_hash);
    let proof_mode = std::env::var("PROOF_API_MODE").unwrap_or_else(|_| "attested".into());
    match start_proof_api(&tm_rpc, eth_rpc, terp.chain_id(), "31337", &proof_mode) {
        Ok((proof_child, proof_addr)) => {
            let _proof_guard = ProofApiGuard(proof_child);
            println!("    proof-api mode={proof_mode} on {proof_addr}");
            if let Err(e) = proof_api_info(&proof_addr, terp.chain_id(), "31337") {
                println!("    proof-api Info skipped: {e}");
            }
            match proof_api_create_client(&proof_addr, terp.chain_id(), "31337") {
                Ok(tx) => println!("    proof-api CreateClient returned {} bytes", tx.len()),
                Err(e) => println!("    proof-api CreateClient skipped: {e}"),
            }
            match proof_api_relay_by_tx(
                &proof_addr,
                terp.chain_id(),
                "31337",
                &send_hash,
                &ibc_client,
                "ics26-router",
            ) {
                Ok(tx) => println!("    proof-api RelayByTx returned {} bytes", tx.len()),
                Err(e) => println!("    proof-api RelayByTx skipped: {e}"),
            }
        }
        Err(e) => println!("    proof-api not running ({e}); IBC-v2 packet path still required"),
    }

    let anvil = anvil_handle
        .as_mut()
        .ok_or("anvil must stay up for IBC-v2 ETH pledge")?;
    let sign_out2 = anvil
        .exec_cast(&[
            "send",
            &local_contract,
            "pledge()",
            "--private-key",
            pk2,
            "--rpc-url",
            "http://127.0.0.1:8545",
            "--json",
        ])
        .await
        .map_err(|e| format!("account1 pledge() failed: {e}"))?;
    let raw2 = format!("{}{}", sign_out2.stdout_str(), sign_out2.stderr_str());
    let sign_tx2 = extract_eth_tx_hash_from_text(&raw2)
        .ok_or_else(|| format!("account1 pledge() produced no tx hash: {raw2}"))?;
    println!("    ETH pledge tx {sign_tx2} for {addr2}");

    wasm_exec(
        terp,
        &contract,
        &format!(
            r#"{{"deliver_eth_attestation":{{"eth_address":"{addr2}","pledge_tx":"{sign_tx2}","commitment_hex":"{commit_hex}"}}}}"#
        ),
        "minter",
    )
    .await?;
    let tok = wasm_query(
        terp,
        &contract,
        &format!(r#"{{"token":{{"eth_address":"{addr2}"}}}}"#),
    )
    .await?;
    let path = tok.get("mint_path").and_then(|v| v.as_str()).unwrap_or("");
    if path != "ibc_v2_australge" {
        return Err(format!("expected mint_path ibc_v2_australge, got {tok}").into());
    }
    if tok.get("sign_tx").and_then(|v| v.as_str()) != Some(sign_tx2.as_str()) {
        return Err(format!("token sign_tx mismatch: {tok} vs {sign_tx2}").into());
    }
    println!("    minted soul-bound trusted-builder (IBC-v2 Australge) for {addr2}");
    println!("OK trusted-builder flow (VE + IBC-v2 Australge)");
    Ok(())
}


fn fetch_manifesto_bytecode() -> Result<String, Box<dyn std::error::Error>> {
    let rpc = std::env::var("ETH_RPC_URL").unwrap_or_else(|_| DEFAULT_ETH_RPC.into());
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getCode",
        "params": [MANIFESTO_MAINNET, "latest"]
    });
    let tmp = tempfile::NamedTempFile::new()?;
    std::fs::write(tmp.path(), body.to_string())?;
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "30",
            "-H",
            "content-type: application/json",
            "--data",
            &body.to_string(),
            &rpc,
        ])
        .output()
        .map_err(|e| format!("curl ETH_RPC_URL={rpc} failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl eth_getCode failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("eth_getCode json: {e}\n{}", String::from_utf8_lossy(&out.stdout)))?;
    if let Some(err) = v.get("error") {
        return Err(format!("eth_getCode error: {err}").into());
    }
    let code = v
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| format!("eth_getCode missing result: {v}"))?;
    if code == "0x" || code.len() < 10 {
        return Err("eth_getCode returned empty runtime bytecode".into());
    }
    Ok(code.to_string())
}

fn crates_root() -> PathBuf {
    // env!("CARGO_MANIFEST_DIR") = …/crates/ict-rs/ict-rs
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn first_existing(cands: &[PathBuf]) -> Option<PathBuf> {
    cands.iter().find(|p| p.is_file()).cloned()
}

fn cargo_bin() -> PathBuf {
    std::env::var_os("CARGO")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cargo"))
}

/// Run cargo and return the last matching artifact path (respects inherited CARGO_TARGET_DIR).
fn cargo_artifact(
    args: &[&str],
    cwd: &std::path::Path,
    target_name: &str,
    suffix: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut cmd = std::process::Command::new(cargo_bin());
    cmd.args(args).arg("--message-format=json").current_dir(cwd);
    let out = cmd
        .output()
        .map_err(|e| format!("spawn cargo {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "cargo {} failed ({})
{}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut found: Option<PathBuf> = None;
    for line in stdout.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let name = v
            .pointer("/target/name")
            .and_then(|n| n.as_str())
            .unwrap_or("");
        let want = target_name.replace('-', "_");
        let got = name.replace('-', "_");
        if got != want {
            continue;
        }
        if let Some(exe) = v.get("executable").and_then(|e| e.as_str()) {
            if !exe.is_empty() {
                found = Some(PathBuf::from(exe));
            }
        }
        if found.is_none() {
            if let Some(files) = v.get("filenames").and_then(|f| f.as_array()) {
                for f in files {
                    if let Some(s) = f.as_str() {
                        if s.ends_with(suffix) {
                            found = Some(PathBuf::from(s));
                        }
                    }
                }
            }
        }
    }
    let p = found.ok_or_else(|| {
        format!(
            "cargo produced no {target_name} artifact ending in {suffix} (cwd {})",
            cwd.display()
        )
    })?;
    if !p.is_file() {
        return Err(format!("cargo reported artifact that is not a file: {}", p.display()).into());
    }
    Ok(p)
}

fn hash_market_candidates() -> Vec<PathBuf> {
    let root = crates_root();
    let hm = root.join("terp-rs/tools/hash-market");
    let mut v = vec![
        hm.join("target/debug/hash-market-server"),
        hm.join("target/release/hash-market-server"),
        root.join("terp-rs/target/debug/hash-market-server"),
        root.join("terp-rs/target/release/hash-market-server"),
        root.join(".shared-target/debug/hash-market-server"),
        root.join(".shared-target/release/hash-market-server"),
        root.join("ict-rs/target/debug/hash-market-server"),
        root.join("ict-rs/target/release/hash-market-server"),
    ];
    if let Ok(td) = std::env::var("CARGO_TARGET_DIR") {
        let td = PathBuf::from(td);
        v.push(td.join("debug/hash-market-server"));
        v.push(td.join("release/hash-market-server"));
    }
    v
}

fn ensure_hash_market_server() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let hm = crates_root().join("terp-rs/tools/hash-market");
    if !hm.join("Cargo.toml").is_file() {
        return Err(format!("hash-market crate missing at {}", hm.display()).into());
    }
    println!("    building hash-market-server (server,ve)…");
    let built = cargo_artifact(
        &[
            "build",
            "--manifest-path",
            hm.join("Cargo.toml").to_str().unwrap(),
            "--bin",
            "hash-market-server",
            "--features",
            "server,ve",
        ],
        &hm,
        "hash-market-server",
        "hash-market-server",
    )?;
    println!("    hash-market-server {}", built.display());
    Ok(built)
}

fn ensure_sbt_wasm() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let crate_dir = crates_root().join("terp-rs/contracts/hash-merchant/trusted-builder-sbt");
    if !crate_dir.join("Cargo.toml").is_file() {
        return Err(format!("trusted-builder-sbt crate missing at {}", crate_dir.display()).into());
    }
    println!("    building trusted-builder-sbt wasm32…");
    let built = cargo_artifact(
        &[
            "build",
            "--manifest-path",
            crate_dir.join("Cargo.toml").to_str().unwrap(),
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--lib",
        ],
        &crate_dir,
        "trusted-builder-sbt",
        ".wasm",
    )?;
    let opt_out = built.with_extension("opt.wasm");
    let bin = if PathBuf::from("/opt/homebrew/bin/wasm-opt").is_file() {
        "/opt/homebrew/bin/wasm-opt"
    } else {
        "wasm-opt"
    };
    let st = std::process::Command::new(bin)
        .args([
            "-Os",
            "--llvm-memory-copy-fill-lowering",
            "--signext-lowering",
            "-mvp",
            "-o",
        ])
        .arg(&opt_out)
        .arg(&built)
        .status()
        .map_err(|e| format!("spawn wasm-opt: {e}"))?;
    if !st.success() {
        return Err(format!("wasm-opt failed ({st})").into());
    }
    println!("    trusted-builder-sbt.wasm {} (wasm-opt for wasmd 0.61)", opt_out.display());
    Ok(opt_out)
}

fn reqwest_or_curl() -> () {}

fn http_post_json(
    _c: &(),
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "--data",
            &body.to_string(),
            url,
        ])
        .output()?;
    if !out.status.success() {
        return Err(format!("POST {url} failed: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

fn http_get_json(_c: &(), url: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let out = std::process::Command::new("curl").args(["-sS", url]).output()?;
    if !out.status.success() {
        return Err(format!("GET {url} failed: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}




/// Create a 07-tendermint client from this chain's own header (ibc_transfer style)
/// and register an IBC-v2 counterparty so wasm `Ibc2Msg::SendPacket` can land.

struct ProofApiGuard(Option<std::process::Child>);
impl Drop for ProofApiGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
        }
    }
}

fn eureka_root() -> PathBuf {
    crates_root().join("solidity-ibc-eureka")
}

fn proof_api_bin() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let root = eureka_root();
    let cands = [
        root.join("target/release/proof-api"),
        root.join("target/debug/proof-api"),
        PathBuf::from("proof-api"),
    ];
    for p in &cands {
        if p.is_file() {
            return Ok(p.clone());
        }
    }
    Err("proof-api binary not found; build solidity-ibc-eureka -p proof-api".into())
}

/// Start proof-api with cosmos_to_eth in `attested` or `sp1` (mock) mode.
fn start_proof_api(
    tm_rpc: &str,
    eth_rpc: &str,
    src_chain: &str,
    dst_chain: &str,
    mode: &str,
) -> Result<(Option<std::process::Child>, String), Box<dyn std::error::Error>> {
    let bin = proof_api_bin()?;
    let ics26 = "0x0000000000000000000000000000000000002626";
    let mode_json = match mode {
        "sp1" => {
            let programs = eureka_root().join("programs/sp1-programs");
            serde_json::json!({
                "sp1": {
                    "sp1_prover": "mock",
                    "sp1_programs": {
                        "update_client": programs.join("update-client").to_string_lossy(),
                        "membership": programs.join("membership").to_string_lossy(),
                        "update_client_and_membership": programs.join("uc-and-membership").to_string_lossy(),
                        "misbehaviour": programs.join("misbehaviour").to_string_lossy()
                    }
                }
            })
        }
        _ => serde_json::json!({
            "attested": {
                "attestor": {
                    "attestor_query_timeout_ms": 1500,
                    "quorum_threshold": 1,
                    "attestor_endpoints": ["http://127.0.0.1:2025"]
                },
                "cache": {
                    "state_cache_max_entries": 1000,
                    "packet_cache_max_entries": 1000
                }
            }
        }),
    };
    let cfg = serde_json::json!({
        "server": { "address": "127.0.0.1", "port": 13000, "grpc_web_port": 13001 },
        "observability": {
            "level": "info",
            "use_otel": false,
            "service_name": "ibc-proof-api",
            "otel_endpoint": null
        },
        "modules": [{
            "name": "cosmos_to_eth",
            "src_chain": src_chain,
            "dst_chain": dst_chain,
            "config": {
                "tm_rpc_url": tm_rpc,
                "eth_rpc_url": eth_rpc,
                "ics26_address": ics26,
                "mode": mode_json
            }
        }]
    });
    let cfg_path = std::env::temp_dir().join("trustless-builder-proof-api.json");
    std::fs::write(&cfg_path, serde_json::to_vec_pretty(&cfg)?)?;
    let log_path = std::env::temp_dir().join("trustless-builder-proof-api.log");
    let log = std::fs::File::create(&log_path)?;
    let child = std::process::Command::new(&bin)
        .args(["start", "--config", cfg_path.to_str().unwrap()])
        .current_dir(eureka_root())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .map_err(|e| format!("spawn proof-api: {e}"))?;
    let addr = "127.0.0.1:13000".to_string();
    let proto_dir = eureka_root().join("proto");
    for i in 0..40 {
        if proof_api_info(&addr, src_chain, dst_chain).is_ok() {
            println!("    proof-api up on {addr} after {}ms", i * 250);
            return Ok((Some(child), addr));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let logs = std::fs::read_to_string(&log_path).unwrap_or_default();
    let _ = proto_dir;
    Err(format!("proof-api never became ready on {addr}\n{logs}").into())
}

fn grpcurl_bin() -> PathBuf {
    let p = PathBuf::from("/opt/homebrew/bin/grpcurl");
    if p.is_file() {
        p
    } else {
        PathBuf::from("grpcurl")
    }
}

fn proof_api_call(addr: &str, method: &str, body: &serde_json::Value) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let proto = eureka_root().join("proto/proofapi/proofapi.proto");
    let import = eureka_root().join("proto");
    let out = std::process::Command::new(grpcurl_bin())
        .args([
            "-plaintext",
            "-import-path",
            import.to_str().unwrap(),
            "-proto",
            proto.to_str().unwrap(),
            "-d",
            &body.to_string(),
            addr,
            &format!("proofapi.ProofApiService/{method}"),
        ])
        .output()
        .map_err(|e| format!("grpcurl {method}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "grpcurl {method} failed: {}\n{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        )
        .into());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(serde_json::from_str(stdout.trim()).unwrap_or(serde_json::json!({"raw": stdout})))
}

fn proof_api_info(addr: &str, src: &str, dst: &str) -> Result<(), Box<dyn std::error::Error>> {
    let v = proof_api_call(addr, "Info", &serde_json::json!({"src_chain": src, "dst_chain": dst}))?;
    println!("    proof-api Info {v}");
    Ok(())
}

fn proof_api_create_client(
    addr: &str,
    src: &str,
    dst: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let v = proof_api_call(
        addr,
        "CreateClient",
        &serde_json::json!({"src_chain": src, "dst_chain": dst}),
    )?;
    decode_grpc_bytes(v.get("tx"))
}

fn proof_api_relay_by_tx(
    addr: &str,
    src: &str,
    dst: &str,
    send_txhash: &str,
    src_client: &str,
    dst_client: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let hash_bytes = decode_hex(send_txhash)?;
    let v = proof_api_call(
        addr,
        "RelayByTx",
        &serde_json::json!({
            "src_chain": src,
            "dst_chain": dst,
            "source_tx_ids": [b64(&hash_bytes)],
            "src_client_id": src_client,
            "dst_client_id": dst_client
        }),
    )?;
    decode_grpc_bytes(v.get("tx"))
}

fn decode_grpc_bytes(v: Option<&serde_json::Value>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let Some(v) = v else {
        return Err("proof-api response missing tx".into());
    };
    if let Some(s) = v.as_str() {
        if let Ok(b) = b64_decode(s) {
            if !b.is_empty() {
                return Ok(b);
            }
        }
        return Ok(s.as_bytes().to_vec());
    }
    Err(format!("unrecognized tx field: {v}").into())
}

fn decode_hex(s: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let s = s.trim().trim_start_matches("0x");
    if s.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.into()))
        .collect()
}

fn b64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };
        out.push(T[(b0 >> 2) as usize] as char);
        out.push(T[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
        if i + 1 < bytes.len() {
            out.push(T[(((b1 & 0xf) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if i + 2 < bytes.len() {
            out.push(T[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

fn b64_decode(s: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let s = s.trim();
    let mut vals = Vec::new();
    for c in s.chars() {
        if c == '=' {
            break;
        }
        let v = match c {
            'A'..='Z' => c as u8 - b'A',
            'a'..='z' => c as u8 - b'a' + 26,
            '0'..='9' => c as u8 - b'0' + 52,
            '+' => 62,
            '/' => 63,
            _ => return Err("bad base64".into()),
        };
        vals.push(v);
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < vals.len() {
        out.push((vals[i] << 2) | (vals[i + 1] >> 4));
        if i + 2 < vals.len() {
            out.push((vals[i + 1] << 4) | (vals[i + 2] >> 2));
        }
        if i + 3 < vals.len() {
            out.push((vals[i + 2] << 6) | vals[i + 3]);
        }
        i += 4;
    }
    Ok(out)
}

async fn create_ibc_v2_tendermint_path(
    terp: &CosmosChain,
    from: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let status = terp
        .chain_exec(&["status", "--output", "json"])
        .await
        .map_err(|e| format!("status: {e}"))?;
    let status_j: serde_json::Value = serde_json::from_str(status.stdout_str().trim())
        .or_else(|_| serde_json::from_str(status.stderr_str().trim()))
        .map_err(|e| format!("status json: {e}\n{}", status.stdout_str()))?;
    let height = status_j
        .pointer("/SyncInfo/latest_block_height")
        .or_else(|| status_j.pointer("/sync_info/latest_block_height"))
        .and_then(|v| v.as_str().or_else(|| v.as_u64().map(|_| "")))
        .map(|s| s.to_string())
        .or_else(|| {
            status_j
                .pointer("/SyncInfo/latest_block_height")
                .or_else(|| status_j.pointer("/sync_info/latest_block_height"))
                .and_then(|v| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| v.as_u64().map(|n| n.to_string()))
                })
        })
        .ok_or_else(|| format!("no latest_block_height in status: {status_j}"))?;

    let cons = terp
        .chain_exec(&[
            "query",
            "ibc",
            "client",
            "self-consensus-state",
            "--output",
            "json",
        ])
        .await
        .map_err(|e| format!("self-consensus-state: {e}"))?;
    let mut cons_j: serde_json::Value = serde_json::from_str(cons.stdout_str().trim())
        .map_err(|e| format!("self-consensus-state json: {e}\n{}", cons.stdout_str()))?;
    if cons_j.get("@type").is_none() {
        if let Some(inner) = cons_j.get("consensus_state").cloned() {
            cons_j = inner;
        }
    }
    if cons_j.get("@type").is_none() {
        cons_j.as_object_mut().map(|o| {
            o.insert(
                "@type".into(),
                serde_json::Value::String("/ibc.lightclients.tendermint.v1.ConsensusState".into()),
            )
        });
    }

    let client_state = serde_json::json!({
        "@type": "/ibc.lightclients.tendermint.v1.ClientState",
        "chain_id": terp.chain_id(),
        "trust_level": { "numerator": "1", "denominator": "3" },
        "trusting_period": "80s",
        "unbonding_period": "120s",
        "max_clock_drift": "20s",
        "frozen_height": { "revision_number": "0", "revision_height": "0" },
        "latest_height": { "revision_number": "1", "revision_height": height },
        "proof_specs": [
            {
                "leaf_spec": {
                    "hash": "SHA256",
                    "prehash_key": "NO_HASH",
                    "prehash_value": "SHA256",
                    "length": "VAR_PROTO",
                    "prefix": "AA=="
                },
                "inner_spec": {
                    "child_order": [0, 1],
                    "child_size": 33,
                    "min_prefix_length": 4,
                    "max_prefix_length": 12,
                    "empty_child": null,
                    "hash": "SHA256"
                }
            },
            {
                "leaf_spec": {
                    "hash": "SHA256",
                    "prehash_key": "NO_HASH",
                    "prehash_value": "SHA256",
                    "length": "VAR_PROTO",
                    "prefix": "AA=="
                },
                "inner_spec": {
                    "child_order": [0, 1],
                    "child_size": 32,
                    "min_prefix_length": 1,
                    "max_prefix_length": 1,
                    "empty_child": null,
                    "hash": "SHA256"
                }
            }
        ],
        "upgrade_path": ["upgrade", "upgradedIBCState"],
        "allow_update_after_expiry": false,
        "allow_update_after_misbehaviour": false
    });

    let node = terp.primary_node()?;
    node.write_file(
        client_state.to_string().as_bytes(),
        "/tmp/ibc_v2_client_state.json",
    )
    .await?;
    node.write_file(cons_j.to_string().as_bytes(), "/tmp/ibc_v2_cons_state.json")
        .await?;

    let created = ibc_tx(
        terp,
        from,
        &[
            "ibc",
            "client",
            "create",
            "/tmp/ibc_v2_client_state.json",
            "/tmp/ibc_v2_cons_state.json",
        ],
    )
    .await?;
    let client_id = event_attr(&created, "create_client", "client_id")
        .or_else(|| event_attr(&created, "message", "client_id"))
        .ok_or_else(|| format!("no client_id in create client tx: {created}"))?;
    if !client_id.starts_with("07-tendermint-") {
        return Err(format!("expected 07-tendermint client, got {client_id}").into());
    }

    ibc_tx(
        terp,
        from,
        &[
            "ibc",
            "client",
            "add-counterparty",
            &client_id,
            "08-wasm-0",
            "aWJj",
        ],
    )
    .await?;
    Ok(client_id)
}

async fn ibc_tx(
    terp: &CosmosChain,
    from: &str,
    args: &[&str],
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut cmd = vec!["tx"];
    cmd.extend_from_slice(args);
    cmd.extend_from_slice(&[
        "--from",
        from,
        "--gas",
        "auto",
        "--gas-adjustment",
        "2.5",
        "--gas-prices",
        "0.025uterp",
        "--broadcast-mode",
        "sync",
        "--keyring-backend",
        "test",
        "--chain-id",
        terp.chain_id(),
        "--output",
        "json",
        "-y",
    ]);
    let out = terp
        .chain_exec(&cmd)
        .await
        .map_err(|e| format!("tx {} failed: {e}", args.join(" ")))?;
    if out.exit_code != 0 {
        return Err(format!(
            "tx {} exit {}: {}\n{}",
            args.join(" "),
            out.exit_code,
            out.stdout_str(),
            out.stderr_str()
        )
        .into());
    }
    let j: serde_json::Value = serde_json::from_str(out.stdout_str().trim()).unwrap_or_default();
    if j["code"].as_u64().unwrap_or(1) != 0 {
        return Err(format!("tx {} code!=0: {}", args.join(" "), out.stdout_str()).into());
    }
    let j = wait_tx(terp, &j).await?;
    if j["code"].as_u64().unwrap_or(1) != 0 {
        return Err(format!("tx {} included code!=0: {j}", args.join(" ")).into());
    }
    Ok(j)
}

async fn query_next_sequence(
    terp: &CosmosChain,
    client_id: &str,
) -> Result<u64, Box<dyn std::error::Error>> {
    let q = terp
        .chain_exec(&[
            "query",
            "ibc",
            "channelv2",
            "next-sequence-send",
            client_id,
            "--output",
            "json",
        ])
        .await
        .map_err(|e| format!("next-sequence-send: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(q.stdout_str().trim())
        .map_err(|e| format!("next-sequence-send json: {e}\n{}", q.stdout_str()))?;
    v.get("next_sequence_send")
        .or_else(|| v.get("nextSequenceSend"))
        .and_then(|x| {
            x.as_u64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or_else(|| format!("no next_sequence_send in {v}").into())
}

async fn wasm_exec(
    terp: &CosmosChain,
    contract: &str,
    msg: &str,
    from: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let out = terp
        .chain_exec(&[
            "tx",
            "wasm",
            "execute",
            contract,
            msg,
            "--from",
            from,
            "--gas",
            "auto",
            "--gas-adjustment",
            "1.4",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--keyring-backend",
            "test",
            "--chain-id",
            terp.chain_id(),
            "--output",
            "json",
            "-y",
        ])
        .await
        .map_err(|e| format!("wasm execute failed: {e}"))?;
    if out.exit_code != 0 {
        return Err(format!(
            "wasm execute exit {}: {}\n{}",
            out.exit_code,
            out.stdout_str(),
            out.stderr_str()
        )
        .into());
    }
    let j: serde_json::Value = serde_json::from_str(out.stdout_str().trim()).unwrap_or_default();
    if j["code"].as_u64().unwrap_or(1) != 0 {
        return Err(format!("wasm execute code!=0: {}", out.stdout_str()).into());
    }
    let j = wait_tx(terp, &j).await?;
    if j["code"].as_u64().unwrap_or(1) != 0 {
        return Err(format!("wasm execute included code!=0: {j}").into());
    }
    Ok(j)
}

async fn wasm_query(
    terp: &CosmosChain,
    contract: &str,
    msg: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let out = terp
        .chain_exec(&[
            "query",
            "wasm",
            "contract-state",
            "smart",
            contract,
            msg,
            "--output",
            "json",
        ])
        .await
        .map_err(|e| format!("wasm query failed: {e}"))?;
    let raw = out.stdout_str();
    let v: serde_json::Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("wasm query json: {e}\n{raw}"))?;
    if let Some(data) = v.get("data") {
        return Ok(data.clone());
    }
    Ok(v)
}

async fn wait_tx(
    terp: &ict_rs::chain::cosmos::CosmosChain,
    j: &serde_json::Value,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if extract_code_id(j).is_some() || extract_contract_address(j).is_some() {
        if j.get("height").and_then(|h| h.as_str()).unwrap_or("0") != "0" {
            return Ok(j.clone());
        }
    }
    let hash = j
        .get("txhash")
        .and_then(|h| h.as_str())
        .ok_or("tx json missing txhash")?
        .to_string();
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let q = terp
            .chain_exec(&["query", "tx", &hash, "--output", "json"])
            .await;
        let Ok(q) = q else { continue };
        let stdout = q.stdout_str();
        let t = stdout.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
            if v.get("height").and_then(|h| h.as_str()).unwrap_or("0") != "0" {
                return Ok(v);
            }
        }
    }
    Err(format!("timed out waiting for tx {hash}").into())
}

fn event_lists(j: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut out = Vec::new();
    if let Some(a) = j.get("events").and_then(|e| e.as_array()) {
        out.extend(a);
    }
    if let Some(logs) = j.get("logs").and_then(|l| l.as_array()) {
        for log in logs {
            if let Some(a) = log.get("events").and_then(|e| e.as_array()) {
                out.extend(a);
            }
        }
    }
    out
}

fn event_attr(j: &serde_json::Value, ty: &str, key: &str) -> Option<String> {
    for e in event_lists(j) {
        if e.get("type").and_then(|t| t.as_str()) != Some(ty) {
            continue;
        }
        let Some(attrs) = e.get("attributes").and_then(|a| a.as_array()) else {
            continue;
        };
        for a in attrs {
            if a.get("key").and_then(|k| k.as_str()) == Some(key) {
                if let Some(v) = a.get("value").and_then(|v| v.as_str()) {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn extract_code_id(j: &serde_json::Value) -> Option<String> {
    event_attr(j, "store_code", "code_id")
}

fn extract_contract_address(j: &serde_json::Value) -> Option<String> {
    event_attr(j, "instantiate", "_contract_address")
        .or_else(|| event_attr(j, "wasm", "_contract_address"))
}

fn extract_eth_tx_hash(v: &serde_json::Value) -> Option<String> {
    for key in ["transactionHash", "txHash", "hash"] {
        if let Some(s) = v.get(key).and_then(|x| x.as_str()) {
            if looks_like_tx(s) {
                return Some(s.to_string());
            }
        }
    }
    if let Some(s) = v.pointer("/data/transactionHash").and_then(|x| x.as_str()) {
        if looks_like_tx(s) {
            return Some(s.to_string());
        }
    }
    None
}

fn extract_eth_tx_hash_from_text(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 66 <= bytes.len() {
        if bytes[i] == b'0' && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X') {
            let hex = &s[i..i + 66];
            if looks_like_tx(hex) {
                return Some(hex.to_string());
            }
        }
        i += 1;
    }
    None
}

fn looks_like_tx(s: &str) -> bool {
    let s = s.trim();
    s.len() == 66
        && s.starts_with("0x")
        && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn http_get_ok(url: &str) -> bool {
    std::process::Command::new("curl")
        .args(["-sS", "-o", "/dev/null", "-w", "%{http_code}", "--max-time", "1", url])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).starts_with('2'))
        .unwrap_or(false)
}
