//! Spawned local Base (anvil --chain-id 8453) + SvgMintRailVault lock + DREGG Rail::Base entitle.
//!
//! Host RPC is **not** 18545 (lab ETH). Docker/anvil `eth_chainId` must quote 0x2105 (8453).
//! If anvil cannot start: spawned_base=false, entitled=false (no fake).
//!
//! ```sh
//! cargo run --example svg_mint_base_local -p ict-rs-cw-orch --features docker,ethereum
//! ```

use ict_rs::prelude::*;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const LAB_MNEMONIC_OWNER: &str =
    "terp1wug8sewp6cedgkmrmvhl3lf3tulagm9hnvy8p0rppz9yjw0g4wtqw39a47";
const BASE_HOST_PORT: u16 = 28453;
const DREGG_QUERY_PORT: u16 = 18422;

#[tokio::main]
async fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().unwrap()),
        )
        .init();
    match run().await {
        Ok(v) => println!("{v}"),
        Err(e) => {
            eprintln!("{e}");
            println!(
                "{}",
                serde_json::json!({
                    "spawned_base": false,
                    "chain_id": "",
                    "rpc": "",
                    "entitled": false,
                    "evidence": e.to_string(),
                    "summary": "base anvil / vault / dregg path failed; not faked"
                })
            );
            std::process::exit(2);
        }
    }
}

async fn run() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("svg_mint_base_local refuses ICT_MOCK=1".into());
    }
    let _ = Command::new("docker")
        .args(["rm", "-f", "ict-svg-mint-base-8453-anvil-0"])
        .status();

    let rt = match DockerBackend::new(Default::default()).await {
        Ok(r) => Arc::new(r),
        Err(e) => {
            return Ok(fail_json(
                format!("DockerBackend: {e}"),
                "laptop docker unavailable",
            ));
        }
    };

    let mut cfg = builtin_chain_config("anvil")?;
    cfg.chain_id = "8453".into();
    cfg.name = "base-anvil".into();
    let mut anvil = AnvilChain::new(cfg, rt);
    anvil.rpc_host_port = BASE_HOST_PORT;
    anvil.anvil_chain_id = Some(8453);
    let started = anvil
        .initialize(&TestContext {
            test_name: "svg-mint-base".into(),
            network_id: "ict-svg-mint-base".into(),
        })
        .await
        .and(anvil.start(&[]).await);
    if let Err(e) = started {
        let _ = anvil.stop().await;
        return Ok(fail_json(
            format!("anvil start: {e}"),
            "docker/anvil could not start",
        ));
    }

    let rpc = anvil.host_rpc_address();
    let chain_rpc = json_rpc(
        &rpc,
        "eth_chainId",
        serde_json::json!([]),
    );
    let chain_id = match &chain_rpc {
        Ok(v) => v
            .get("result")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        Err(e) => {
            let _ = anvil.stop().await;
            return Ok(fail_json(
                format!("eth_chainId: {e}"),
                "anvil RPC did not quote chain id",
            ));
        }
    };
    if chain_id != "0x2105" {
        let evidence = format!("eth_chainId JSON-RPC={chain_rpc:?}");
        let _ = anvil.stop().await;
        return Ok(serde_json::json!({
            "spawned_base": false,
            "chain_id": chain_id,
            "rpc": rpc,
            "entitled": false,
            "evidence": evidence,
            "summary": "spawned anvil but eth_chainId was not Base 8453 (0x2105)"
        }));
    }

    let from = ANVIL_DEFAULT_ACCOUNTS[0].1;
    let treasury = ANVIL_DEFAULT_ACCOUNTS[1].0;
    let mut fed_child: Option<Child> = None;
    let dregg_url = format!("http://127.0.0.1:{DREGG_QUERY_PORT}");
    let entitled;
    let evidence;
    let mut summary;

    let vault_pre = match eth_vault_deploy(&anvil, from, treasury).await {
        Ok(v) => v,
        Err(e) => {
            let _ = anvil.stop().await;
            return Ok(fail_json(format!("vault deploy: {e}"), "spawned Base; vault deploy failed"));
        }
    };

    let pay_ok = spawn_fed(&dregg_url, &rpc, &vault_pre, &mut fed_child);
    match pay_ok {
        Err(e) => {
            entitled = false;
            evidence = format!(
                "anvil eth_chainId={chain_id} rpc={rpc}; BASE_RPC fed not started: {e}"
            );
            summary = "spawned_base true; DREGG fed unavailable so entitled=false".into();
        }
        Ok(()) => {
            let setc = fed_signed(
                &dregg_url,
                "set-collection",
                "/v1/collection",
                serde_json::json!({"id": "svg", "max_supply": 8}),
            );
            let pay = http_json(
                "GET",
                &format!(
                    "{}/v1/pay-targets?recipient={LAB_MNEMONIC_OWNER}",
                    dregg_url.trim_end_matches('/')
                ),
                None,
            );
            match (setc, pay) {
                (Ok(sc), Ok(pay)) if sc.get("ok") == Some(&serde_json::Value::Bool(true)) => {
                    let memo_hex = pay
                        .get("memo_hex")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if memo_hex.is_empty() || memo_hex.contains("terp1") || pay.get("recipient").is_some()
                    {
                        entitled = false;
                        evidence = format!("pay-targets memo leak or missing: {pay}");
                        summary = "spawned Base; pay-targets memo invalid".into();
                    } else {
                        match eth_vault_lock(&anvil, from, &vault_pre, &memo_hex).await {
                            Ok(tx_hash) => {
                                let mut phase = String::new();
                                let mut st = serde_json::json!({});
                                for _ in 0..20 {
                                    std::thread::sleep(Duration::from_secs(1));
                                    st = http_json(
                                        "GET",
                                        &format!(
                                            "{}/v1/status?recipient={LAB_MNEMONIC_OWNER}",
                                            dregg_url.trim_end_matches('/')
                                        ),
                                        None,
                                    )
                                    .unwrap_or(serde_json::json!({}));
                                    phase = st
                                        .get("phase")
                                        .and_then(|x| x.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    if phase == "Entitled" {
                                        break;
                                    }
                                }
                                entitled = phase == "Entitled";
                                evidence = format!(
                                    "BASE_RPC={rpc} DREGG_BASE_VAULT={vault_pre} eth_chainId={chain_id} lock_tx={tx_hash} watch_base_once (no POST /v1/deposit) status={st}"
                                );
                                summary = if entitled {
                                    "spawned anvil --chain-id 8453; vault lock; watch_base_once via BASE_RPC Entitled"
                                        .into()
                                } else {
                                    "spawned Base; BASE_RPC watcher did not reach Entitled"
                                        .into()
                                };
                            }
                            Err(e) => {
                                entitled = false;
                                evidence = format!("vault lock: {e}");
                                summary = "spawned Base; SvgMintRailVault lock failed".into();
                            }
                        }
                    }
                }
                (sc, pay) => {
                    entitled = false;
                    evidence = format!("collection/pay-targets: set={sc:?} pay={pay:?}");
                    summary = "spawned Base; DREGG pay-targets failed".into();
                }
            }
        }
    }

    if let Some(mut c) = fed_child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    let _ = anvil.stop().await;
    let _ = Command::new("docker")
        .args(["rm", "-f", "ict-svg-mint-base-8453-anvil-0"])
        .status();

    Ok(serde_json::json!({
        "spawned_base": true,
        "chain_id": chain_id,
        "rpc": rpc,
        "entitled": entitled,
        "evidence": evidence,
        "summary": summary,
    }))
}

fn fail_json(evidence: String, summary: &str) -> serde_json::Value {
    serde_json::json!({
        "spawned_base": false,
        "chain_id": "",
        "rpc": "",
        "entitled": false,
        "evidence": evidence,
        "summary": summary,
    })
}

fn spawn_fed(
    url: &str,
    base_rpc: &str,
    base_vault: &str,
    child: &mut Option<Child>,
) -> Result<(), String> {
    if http_json("GET", &format!("{}/health", url.trim_end_matches('/')), None)
        .ok()
        .and_then(|h| h.get("ok").cloned())
        == Some(serde_json::Value::Bool(true))
    {
        return Ok(());
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../terp-rs/tools/svg-mint/Cargo.toml");
    let listen = format!("0.0.0.0:{DREGG_QUERY_PORT}");
    let mut cmd = Command::new("cargo");
    cmd.args([
        "run",
        "--quiet",
        "--manifest-path",
        manifest.to_str().unwrap_or(""),
        "--bin",
        "svg-mint-fed",
    ])
    .env("SVG_MINT_FED_BIND", &listen)
    .env("SVG_MINT_FED_STORE", "/tmp/svg-mint-fed-base-cell.json")
    .env("BASE_RPC", base_rpc)
    .env("DREGG_BASE_VAULT", base_vault)
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let c = cmd
        .spawn()
        .map_err(|e| format!("spawn svg-mint-fed: {e}"))?;
    *child = Some(c);
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(500));
        if http_json("GET", &format!("{}/health", url.trim_end_matches('/')), None)
            .ok()
            .and_then(|h| h.get("ok").cloned())
            == Some(serde_json::Value::Bool(true))
        {
            return Ok(());
        }
    }
    Err("svg-mint-fed health timeout".into())
}

async fn eth_vault_deploy(
    anvil: &AnvilChain,
    from_pk: &str,
    operator: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let bin = vault_creation_bytecode()?;
    let code = if bin.starts_with("0x") {
        bin.clone()
    } else {
        format!("0x{bin}")
    };
    let ops = format!("[{operator}]");
    let rpc = "http://127.0.0.1:8545";
    let deploy = anvil
        .exec_cast(&[
            "send",
            "--rpc-url",
            rpc,
            "--private-key",
            from_pk,
            "--json",
            "--gas-limit",
            "4000000",
            "--create",
            &code,
            "constructor(address[],uint256)",
            &ops,
            "1",
        ])
        .await?;
    let deploy_out = format!("{}{}", deploy.stdout_str(), deploy.stderr_str());
    let deploy_tx =
        extract_tx_hash(&deploy_out).ok_or_else(|| format!("vault deploy tx missing: {deploy_out}"))?;
    let rec = anvil.get_receipt(&deploy_tx).await?;
    let rec_v: serde_json::Value = serde_json::from_str(rec.trim()).unwrap_or_default();
    let vault = rec_v
        .get("contractAddress")
        .or_else(|| rec_v.get("contract_address"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("vault address missing: {rec}"))?
        .to_string();
    Ok(vault)
}

async fn eth_vault_lock(
    anvil: &AnvilChain,
    _from_pk: &str,
    vault: &str,
    memo: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let nhex = format!("0x{}", "11".repeat(32));
    let encoded = anvil
        .exec_cast(&["calldata", "lock(bytes32,uint64,string)", &nhex, "1", memo])
        .await?;
    let data = encoded.stdout_str().trim().to_string();
    if !data.starts_with("0x") || data.len() < 10 {
        return Err(format!("lock calldata missing: {data}").into());
    }
    let from_addr = ANVIL_DEFAULT_ACCOUNTS[0].0;
    let host_rpc = anvil.host_rpc_address();
    // 0.0005 ETH = 500000000000000 wei
    let sent = http_json(
        "POST",
        &host_rpc,
        Some(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendTransaction",
            "params": [{
                "from": from_addr,
                "to": vault,
                "value": "0x1c6bf52634000",
                "gas": "0x7a120",
                "data": data
            }]
        })),
    )?;
    if let Some(err) = sent.get("error") {
        return Err(format!("vault lock: {err}").into());
    }
    let lock_tx = sent
        .get("result")
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("vault lock tx missing: {sent}"))?
        .to_string();
    let _ = anvil.get_receipt(&lock_tx).await?;
    Ok(lock_tx)
}

fn vault_creation_bytecode() -> Result<String, String> {
    let sol = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../terp-rs/tools/svg-mint/chain/SvgMintRailVault.sol");
    let out_dir = std::env::temp_dir().join("svg-mint-vault-solc-base");
    let _ = std::fs::create_dir_all(&out_dir);
    let status = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-v",
            &format!("{}:/src/SvgMintRailVault.sol:ro", sol.display()),
            "-v",
            &format!("{}:/out", out_dir.display()),
            "ethereum/solc:0.8.20",
            "--optimize",
            "--bin",
            "-o",
            "/out",
            "/src/SvgMintRailVault.sol",
            "--overwrite",
        ])
        .status()
        .map_err(|e| format!("solc docker: {e}"))?;
    if !status.success() {
        return Err("solc docker failed".into());
    }
    let bin = std::fs::read_to_string(out_dir.join("SvgMintRailVault.bin"))
        .map_err(|e| format!("vault bin: {e}"))?;
    Ok(bin.trim().trim_start_matches("0x").to_string())
}

fn extract_tx_hash(s: &str) -> Option<String> {
    for tok in s.split_whitespace() {
        let t = tok.trim_matches(|c| c == '"' || c == ',' || c == '{' || c == '}');
        if t.starts_with("0x") && t.len() == 66 {
            return Some(t.to_string());
        }
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s.trim()) {
        if let Some(h) = v
            .get("transactionHash")
            .or_else(|| v.get("hash"))
            .and_then(|x| x.as_str())
        {
            return Some(h.to_string());
        }
    }
    None
}

fn json_rpc(url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method": method, "params": params});
    http_json("POST", url, Some(&body))
}

fn extract_digest(v: &serde_json::Value) -> Option<serde_json::Value> {
    if v.get("digest").is_some() {
        return v.get("digest").cloned();
    }
    None
}

fn fed_signed(
    dregg_url: &str,
    tag: &str,
    path: &str,
    mut body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let base = dregg_url.trim_end_matches('/');
    body["turn_tag"] = serde_json::json!(tag);
    let prev = http_json(
        "POST",
        &format!("{base}/v1/checkpoint-preview"),
        Some(&body),
    )?;
    let digest = extract_digest(&prev).ok_or_else(|| format!("checkpoint-preview missing digest: {prev}"))?;
    let signed = http_json(
        "POST",
        &format!("{base}/v1/sign"),
        Some(&serde_json::json!({"digest": digest})),
    )?;
    body["signatures"] = signed
        .get("signatures")
        .cloned()
        .unwrap_or(serde_json::json!([]));
    http_json("POST", &format!("{base}{path}"), Some(&body))
}

fn http_json(
    method: &str,
    url: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "--max-time", "8", "-X", method, url]);
    if let Some(b) = body {
        cmd.args(["-H", "Content-Type: application/json", "-d", &b.to_string()]);
    }
    let out = cmd.output().map_err(|e| format!("curl {url}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.trim()).map_err(|e| {
        format!(
            "curl {url} json: {e} stdout={} stderr={}",
            stdout.trim(),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}
