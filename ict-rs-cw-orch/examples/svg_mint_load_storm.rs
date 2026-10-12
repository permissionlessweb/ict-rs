#![recursion_limit = "256"]
//! Concurrent load against DREGG + feegrant authenticator proxy.
//!
//! Multiple fresh Terp wallets register authenticators, then pay ETH / Base / BTC
//! (3× unit price) in parallel. Intended to **stress** the runtime: sequence
//! clashes, checkout_ready, sold-out, batch qty. Logs JSONL.
//!
//! ```sh
//! export ZAKURAD_BIN=... DREGG_NODE_BIN=...
//! export LOAD_WALLETS=8 LOAD_LOG=/tmp/dregg-load-storm.jsonl
//! cargo run --example svg_mint_load_storm -p ict-rs-cw-orch \
//!   --features docker,zakura,dregg-node,ethereum,bitcoin --offline
//! ```

use ict_rs::chain::ethereum::ANVIL_DEFAULT_ACCOUNTS;
use ict_rs::cosmos::interchain::wait_for_blocks;
use ict_rs::prelude::*;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const LAB_MNEMONIC: &str =
    "notice oak worry limit wrap speak medal online prefer cluster roof addict wrist behave treat actual wasp year salad speed social layer crew genius";

fn docker_bin() -> &'static str {
    for p in [
        "/usr/local/bin/docker",
        "/usr/bin/docker",
        "/opt/homebrew/bin/docker",
    ] {
        if std::path::Path::new(p).exists() {
            return p;
        }
    }
    "docker"
}

fn http_json(
    method: &str,
    url: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-sS", "--max-time", "120", "-X", method, url]);
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

fn terpd_height(cid: &str, home: &str) -> u64 {
    let out = std::process::Command::new(docker_bin())
        .args(["exec", cid, "terpd", "status", "--home", home])
        .output();
    let blob = match out {
        Ok(o) => format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
        Err(_) => return 0,
    };
    for key in ["latest_block_height", "latestBlockHeight"] {
        if let Some(i) = blob.find(key) {
            let rest = &blob[i + key.len()..];
            let digits: String = rest
                .chars()
                .skip_while(|c| !c.is_ascii_digit())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(n) = digits.parse::<u64>() {
                return n;
            }
        }
    }
    0
}

/// Do not broadcast the next granter/user tx until this one is in a block.
/// Height 0 is "not synced yet": wait until >0, then wait for one more bump.
fn terpd_wait_next_block(cid: &str, home: &str) {
    let cid = cid.to_string();
    let home = home.to_string();
    hash_market::onboard_authz::wait_until_height_commits(
        terpd_height(&cid, &home),
        || terpd_height(&cid, &home),
        || std::thread::sleep(std::time::Duration::from_millis(400)),
    );
}

fn terpd_account_sequence(cid: &str, home: &str, addr: &str) -> Option<u64> {
    if !addr.starts_with("terp1") {
        return None;
    }
    let out = std::process::Command::new(docker_bin())
        .args([
            "exec",
            cid,
            "terpd",
            "q",
            "auth",
            "account",
            addr,
            "--home",
            home,
            "--output",
            "json",
        ])
        .output()
        .ok()?;
    let blob = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    hash_market::onboard_authz::parse_account_sequence(&blob)
}

fn terpd_tx(cid: &str, extra: &[&str]) -> Result<String, String> {
    let mut args = vec!["exec", cid, "terpd"];
    args.extend_from_slice(extra);
    let out = std::process::Command::new(docker_bin())
        .args(&args)
        .output()
        .map_err(|e| format!("docker exec: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let blob = format!("{stdout}\n{stderr}");
    if blob.contains("sequence mismatch") {
        return Err(format!("sequence mismatch: {}", blob.trim()));
    }
    if !out.status.success() {
        return Err(format!(
            "terpd exit={} stdout={} stderr={}",
            out.status,
            stdout.trim(),
            stderr.trim()
        ));
    }
    Ok(stdout)
}

fn extract_digest(v: &serde_json::Value) -> Option<serde_json::Value> {
    v.get("digest").cloned()
}

fn evm_txhash(blob: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(blob.trim()) {
        for k in ["transactionHash", "hash", "txHash"] {
            if let Some(h) = v.get(k).and_then(|x| x.as_str()) {
                if h.starts_with("0x") && h.len() >= 66 {
                    return Some(h.to_string());
                }
            }
        }
    }
    blob.split(|c: char| !c.is_ascii_hexdigit() && c != 'x')
        .find(|t| t.starts_with("0x") && t.len() >= 66)
        .map(|s| s.to_string())
}

fn fed_signed(
    dregg_url: &str,
    tag: &str,
    path: &str,
    mut body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let base = dregg_url.trim_end_matches('/');
    body["turn_tag"] = serde_json::json!(tag);
    let prev = http_json("POST", &format!("{base}/v1/checkpoint-preview"), Some(&body))?;
    let digest = extract_digest(&prev).ok_or_else(|| format!("no digest: {prev}"))?;
    let signed = http_json(
        "POST",
        &format!("{base}/v1/sign"),
        Some(&serde_json::json!({ "digest": digest })),
    )?;
    body["signatures"] = signed
        .get("signatures")
        .cloned()
        .unwrap_or(serde_json::json!([]));
    http_json("POST", &format!("{base}{path}"), Some(&body))
}

fn log_line(log: &Mutex<std::fs::File>, v: &serde_json::Value) {
    if let Ok(mut f) = log.lock() {
        let _ = writeln!(f, "{v}");
        let _ = f.flush();
    }
}

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
        Ok(v) => {
            println!("{v}");
            let broken = v.get("broke").and_then(|x| x.as_bool()) == Some(true);
            std::process::exit(if broken { 3 } else { 0 });
        }
        Err(e) => {
            eprintln!("{e}");
            println!("{}", serde_json::json!({ "broke": true, "error": e.to_string() }));
            std::process::exit(2);
        }
    }
}

async fn run() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("load storm refuses ICT_MOCK=1".into());
    }
    let n_wallets: usize = std::env::var("LOAD_WALLETS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100)
        .clamp(2, 100);
    let log_path = std::env::var("LOAD_LOG").unwrap_or_else(|_| "/tmp/dregg-load-storm.jsonl".into());
    let log = Mutex::new(std::fs::File::create(&log_path)?);

    let rt = Arc::new(DockerBackend::new(Default::default()).await?);
    let mut cfg = terp_chain_config();
    cfg.chain_id = "svg-mint-local-1".into();
    cfg.name = "svg-mint-local".into();
    cfg.genesis_style = GenesisStyle::Legacy;
    cfg.faucet = Some(FaucetConfig {
        key_name: "deployer".into(),
        port: 4500,
        start_cmd: vec![],
        env: vec![],
        mnemonic: Some(LAB_MNEMONIC.into()),
        coins: Some("100000000000uterp".into()),
    });
    let _ = std::process::Command::new(docker_bin())
        .args([
            "rm",
            "-f",
            "ict-svg-mint-local-svg-mint-local-1-val-0",
            "ict-svg-mint-zakura-zakura-18252",
            "ict-svg-mint-dregg-dregg-8421",
            "ict-svg-mint-local-svg-mint-local-1-anvil-0",
            "ict-svg-mint-local-8453-anvil-0",
            "ict-svg-mint-btc-bitcoind-18453",
        ])
        .status();

    let mut chain = CosmosChain::new(cfg, 1, 0, rt.clone());
    let ctx = TestContext {
        test_name: "svg-mint-local".into(),
        network_id: "ict-svg-mint".into(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    wait_for_blocks(&chain, 2).await?;
    let net = chain.network_id().cloned();

    let mut zakura_cfg = ZakuraNodeConfig::from_env()?;
    zakura_cfg.network_id = net.clone();
    zakura_cfg.test_name = "svg-mint-zakura".into();
    zakura_cfg.rpc_host_port = 18252;
    zakura_cfg.reuse_if_ready = false;
    let mut zakura = spawn_zakura_local(rt.clone(), zakura_cfg).await?;

    // CLOSE Linus 3: Anvil, Base, bitcoind, THEN DREGG. Docker DNS for watchers and cast.
    // CLOSE Linus 10/11: HD is fed-side all-hardened derive(); no convert_bits/polymod here.
    let mut anvil = AnvilChain::new(builtin_chain_config("anvil")?, rt.clone());
    anvil.join_existing_network = net.clone();
    anvil.rpc_host_port = 18545;
    anvil
        .initialize(&TestContext {
            test_name: "svg-mint-local".into(),
            network_id: "ict-svg-mint".into(),
        })
        .await?;
    anvil.start(&[]).await?;

    let mut base_cfg = builtin_chain_config("anvil")?;
    base_cfg.chain_id = "8453".into();
    let mut base_anvil = AnvilChain::new(base_cfg, rt.clone());
    base_anvil.join_existing_network = net.clone();
    base_anvil.rpc_host_port = 28453;
    base_anvil.anvil_chain_id = Some(8453);
    base_anvil
        .initialize(&TestContext {
            test_name: "svg-mint-local".into(),
            network_id: "ict-svg-mint".into(),
        })
        .await
        .map_err(|e| format!("Base Anvil initialize rpc_host_port=28453: {e}"))?;
    base_anvil
        .start(&[])
        .await
        .map_err(|e| format!("Base Anvil start on 28453 required: {e}"))?;

    let mut btc_cfg = BitcoinNodeConfig::default();
    btc_cfg.network_id = net.clone();
    btc_cfg.test_name = "svg-mint-btc".into();
    let mut btc = spawn_bitcoind_regtest(rt.clone(), btc_cfg).await?;

    std::env::set_var("ETH_RPC", anvil.docker_dns_rpc_url());
    std::env::set_var("BASE_RPC", base_anvil.docker_dns_rpc_url());
    std::env::set_var("BTC_RPC", btc.docker_dns_rpc_url());
    let denom = chain.config().denom.clone();
    std::env::set_var("GAS_DENOM", &denom);
    std::env::remove_var("DREGG_MINTER_FUNDED");
    std::env::remove_var("DREGG_MINTER_BALANCE");
    {
        let node = chain.primary_node()?;
        let grpc_host = node
            .host_grpc_port
            .ok_or("validator has no host-mapped gRPC")?;
        std::env::set_var("ICT_GRPC_URL", format!("http://127.0.0.1:{grpc_host}"));
        std::env::set_var(
            "DREGG_MINTER_LCD",
            format!("http://{}:{}", node.hostname, node.ports.api),
        );
        wait_for_blocks(&chain, 1).await?;
        let arts = std::env::var("SVG_MINT_ARTIFACTS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cw-infuser/artifacts")
            });
        let fund = dregg::checkout::native_needed_for_nfts(2);
        let builder = ict_rs_cw_orch::daemon_builder_from_chain(&chain, Some(LAB_MNEMONIC))?;
        let deployed = std::thread::spawn(move || {
            let daemon = builder.build().map_err(|e| format!("Daemon.build: {e}"))?;
            ict_rs_cw_orch::svg_runtime::deploy_hopalong_minter(&daemon, &arts, &denom, fund)
        })
        .join()
        .map_err(|_| "cw-orch deploy thread panicked")??;
        std::env::set_var("DREGG_MINTER_ADDR", &deployed.minter_addr);
    }

    let mut dregg_cfg = DreggNodeConfig::from_env()?;
    dregg_cfg.network_id = net.clone();
    dregg_cfg.test_name = "svg-mint-dregg".into();
    dregg_cfg.reuse_if_ready = false;
    let mut dregg = spawn_dregg_local(rt.clone(), dregg_cfg).await?;

    let out = storm(
        &chain,
        &anvil,
        &base_anvil,
        &btc,
        dregg.query_url(),
        n_wallets,
        &log,
        &log_path,
    )
    .await;

    let _ = btc.teardown().await;
    let _ = dregg.teardown().await;
    let _ = zakura.teardown().await;
    let _ = base_anvil.stop().await;
    let _ = anvil.stop().await;
    let _ = chain.stop().await;
    out
}

#[allow(clippy::too_many_arguments)]
async fn storm(
    chain: &CosmosChain,
    anvil: &AnvilChain,
    base_anvil: &AnvilChain,
    btc: &BitcoinNode,
    dregg_url: &str,
    n_wallets: usize,
    log: &Mutex<std::fs::File>,
    log_path: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let health = http_json("GET", &format!("{}/health", dregg_url.trim_end_matches('/')), None)?;
    log_line(log, &serde_json::json!({"event":"health0","body": health}));
    // CLOSE Linus 12/13: this storm does not observe-supply (no hardcoded minted=8)
    // and does not emit ZEC skip as sold out.
    // Headroom for 100 wallets × 3 rails × 3 units × BOGO; leftover still stresses batches.
    let setc = fed_signed(
        dregg_url,
        "set-collection",
        "/v1/collection",
        serde_json::json!({"id": "svg", "max_supply": 10000}),
    )?;
    log_line(log, &serde_json::json!({"event":"set-collection","body": setc}));
    let health2 = http_json("GET", &format!("{}/health", dregg_url.trim_end_matches('/')), None)?;
    log_line(log, &serde_json::json!({"event":"health_ready","body": health2}));

    let node = chain.primary_node()?;
    let granter = node.get_key_address("validator").await?;
    let cid = node
        .container_id
        .as_ref()
        .ok_or("no container")?
        .0
        .clone();
    let home = node.home_dir.clone();
    let denom = chain.config().denom.clone();
    let chain_id = chain.chain_id().to_string();

    std::env::set_var("ONBOARD_DENOM", &denom);
    std::env::set_var("ONBOARD_CHAIN", &chain_id);
    // Lab CosmosChain only ships the `validator` key. Distinct dao vs runtime_key
    // needs chain authz MsgGrant; here both bech32s are the validator address.
    // Sequence is always `q auth account` on that terp1, never the key name.
    std::env::set_var("ONBOARD_FROM", &granter);
    std::env::set_var("ONBOARD_RUNTIME", &granter);
    std::env::set_var("ONBOARD_DAO", &granter);
    let bind = "127.0.0.1:19091";
    let mut rt = hash_market::onboard_authz::OnboardRuntime::from_env();
    rt.service_name = "load-onboard".into();
    std::env::set_var("ONBOARD_ALLOW_COLLAPSE", "1");
    rt.roles.hashmerchant = granter.clone();
    rt.roles.runtime = granter.clone();
    rt.roles.chain_id = chain_id.clone();
    rt.runway_amount = hash_market::onboard_authz::RUNWAY_UTHOL.into();
    rt.broadcast = true;
    rt.broadcaster = Some(Box::new(LabBroadcaster {
        cid: cid.clone(),
        home: home.clone(),
        from: granter.clone(),
        denom: denom.clone(),
        chain_id: chain_id.clone(),
        lock: Mutex::new(()),
    }));
    let rt = Arc::new(rt);
    let bind_c = bind.to_string();
    let rt_c = rt.clone();
    std::thread::spawn(move || {
        let _ = hash_market::onboard_authz::http::serve(&bind_c, rt_c);
    });
    std::thread::sleep(std::time::Duration::from_millis(250));

    let mut users = Vec::new();
    for i in 0..n_wallets {
        let name = format!("load-{i}");
        let add = terpd_tx(
            &cid,
            &[
                "keys",
                "add",
                &name,
                "--keyring-backend",
                "test",
                "--home",
                &home,
                "--output",
                "json",
                "--no-backup",
            ],
        );
        let addr = add
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s.trim()).ok())
            .and_then(|v| v.get("address").and_then(|x| x.as_str()).map(str::to_string))
            .unwrap_or_default();
        users.push((name, addr));
    }

    // Serialized grants so authenticator_ok==wallets; sequence mismatch retries with wait_for_blocks.
    let mut auth_rows = Vec::new();
    for (name, addr) in users.clone() {
        let mut g = Err("grant".into());
        let mut v = serde_json::json!({});
        for attempt in 0..6 {
            match http_json(
                "POST",
                &format!("http://{bind}/onboard/v1/passkey-grant"),
                Some(&serde_json::json!({ "address": addr })),
            ) {
                Ok(body) => {
                    let blob = body.to_string();
                    if blob.contains("sequence mismatch") && attempt < 5 {
                        wait_for_blocks(chain, 1).await.ok();
                        v = body;
                        continue;
                    }
                    v = body.clone();
                    if body.get("ok") == Some(&serde_json::Value::Bool(true)) {
                        g = Ok(body);
                    } else {
                        g = Err("grant not ok".into());
                    }
                    break;
                }
                Err(e) => {
                    if e.contains("sequence mismatch") && attempt < 5 {
                        wait_for_blocks(chain, 1).await.ok();
                        continue;
                    }
                    g = Err(e);
                    break;
                }
            }
        }
        let row = match g {
            Ok(v) => {
                wait_for_blocks(chain, 1).await.ok();
                let pk = terpd_tx(
                    &cid,
                    &[
                        "keys",
                        "show",
                        &name,
                        "--keyring-backend",
                        "test",
                        "--home",
                        &home,
                        "--output",
                        "json",
                    ],
                )
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s.trim()).ok());
                let key = pk.as_ref().and_then(|p| match p.get("pubkey") {
                    Some(serde_json::Value::String(s)) => {
                        serde_json::from_str::<serde_json::Value>(s)
                            .ok()
                            .and_then(|q| q.get("key").and_then(|k| k.as_str()).map(str::to_string))
                    }
                    Some(o) => o.get("key").and_then(|k| k.as_str()).map(str::to_string),
                    _ => None,
                });
                if let Some(key) = key {
                    let mut add = Err("add".into());
                    for attempt in 0..6 {
                        match terpd_tx(
                            &cid,
                            &[
                                "tx",
                                "smartaccount",
                                "add-authenticator",
                                hash_market::onboard_authz::AUTHENTICATOR_SIGNATURE,
                                &key,
                                "--from",
                                &name,
                                "--fee-granter",
                                &granter,
                                "--keyring-backend",
                                "test",
                                "--home",
                                &home,
                                "--chain-id",
                                &chain_id,
                                "--fees",
                                &format!("20000{denom}"),
                                "--gas",
                                "400000",
                                "--broadcast-mode",
                                "sync",
                                "--yes",
                                "--output",
                                "json",
                            ],
                        ) {
                            Ok(s) => {
                                terpd_wait_next_block(&cid, &home);
                                add = Ok(s);
                                break;
                            }
                            Err(e) if e.contains("sequence mismatch") && attempt < 5 => {
                                wait_for_blocks(chain, 1).await.ok();
                            }
                            Err(e) => {
                                add = Err(e);
                                break;
                            }
                        }
                    }
                    let queried = {
                        let mut last = String::new();
                        for cmd in ["get-authenticators", "authenticators"] {
                            match terpd_tx(
                                &cid,
                                &[
                                    "q",
                                    "smartaccount",
                                    cmd,
                                    &addr,
                                    "--home",
                                    &home,
                                    "--output",
                                    "json",
                                ],
                            ) {
                                Ok(s) if hash_market::onboard_authz::authenticators_query_ok(&s) => {
                                    last = s;
                                    break;
                                }
                                Ok(s) => last = s,
                                Err(e) => last = e,
                            }
                        }
                        last
                    };
                    (name, addr, v, add, queried)
                } else {
                    (name, addr, v, Err("no pubkey".into()), String::new())
                }
            }
            Err(e) => (
                name,
                addr,
                if v.is_null() || v.as_object().map(|o| o.is_empty()).unwrap_or(true) {
                    serde_json::json!({"error": e})
                } else {
                    v
                },
                Err("grant http".into()),
                String::new(),
            ),
        };
        auth_rows.push(row);
    }
    let mut auth_ok = 0usize;
    let mut auth_fail = Vec::new();
    for (name, addr, grant, add, queried) in auth_rows {
        let add_ok = add
            .as_ref()
            .ok()
            .map(|s| s.contains("\"code\":0") || s.contains("\"code\": 0"))
            .unwrap_or(false);
        let q_ok = hash_market::onboard_authz::authenticators_query_ok(&queried);
        if add_ok && q_ok {
            auth_ok += 1;
        } else {
            auth_fail.push(serde_json::json!({
                "wallet": name,
                "addr": addr,
                "grant": grant,
                "add": add.as_ref().err().cloned().unwrap_or_else(|| "code!=0".into()),
                "query": queried,
            }));
        }
        log_line(
            log,
            &serde_json::json!({
                "event": "authenticator",
                "wallet": name,
                "addr": addr,
                "grant": grant,
                "add_ok": add_ok,
                "query_ok": q_ok,
                "query": queried,
            }),
        );
    }

    // 100 fresh EVM payers, funded from Anvil's 10 (same keys on ETH + Base).
    let evm_pks = fund_fresh_evm_wallets(anvil, base_anvil, n_wallets, log).await?;

    // Parallel rail payments: 3× unit (ETH wei / BTC sats / Base wei).
    let eth_unit: u128 = dregg::prices::ETH_WEI;
    let btc_unit: u64 = dregg::prices::BTC_SATS;
    let triple_eth = format!("0x{:x}", eth_unit.saturating_mul(3));
    let triple_btc = btc_unit.saturating_mul(3);

    let mut pay_ok = 0u32;
    let mut base_ok_n = 0u32;
    let mut btc_ok_n = 0u32;
    let mut pay_fail = Vec::new();
    let mut jobs = Vec::new();
    for (i, (_name, addr)) in users.iter().enumerate() {
        let dregg = dregg_url.to_string();
        let rec = addr.clone();
        jobs.push(async move {
            let pay = http_json(
                "GET",
                &format!(
                    "{}/v1/pay-targets?recipient={rec}",
                    dregg.trim_end_matches('/')
                ),
                None,
            );
            (i, rec, pay)
        });
    }
    let pays = futures_join(jobs).await;

    for (i, rec, pay) in pays {
        match pay {
            Ok(p) if p.get("ok") == Some(&serde_json::Value::Bool(true)) => {
                let hmid = http_json(
                    "GET",
                    &format!("{}/health", dregg_url.trim_end_matches('/')),
                    None,
                )
                .unwrap_or_default();
                log_line(
                    log,
                    &serde_json::json!({
                        "event": "checkout_mid",
                        "i": i,
                        "checkout_ready": hmid.get("checkout_ready"),
                        "remaining_supply": hmid.get("remaining_supply"),
                    }),
                );
                let eth_addr = p.get("eth_addr").and_then(|x| x.as_str()).unwrap_or("");
                let btc_addr = p.get("btc_addr").and_then(|x| x.as_str()).unwrap_or("");
                let btc_unit_here = p
                    .get("btc_sats")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(btc_unit);
                let triple_btc_here = btc_unit_here.saturating_mul(3);
                let pk = evm_pks.get(i).map(|s| s.as_str()).unwrap_or("");
                // ETH
                let eth_rpc = anvil.docker_dns_rpc_url();
                let eth_send = anvil
                    .exec_cast(&[
                        "send",
                        "--rpc-url",
                        &eth_rpc,
                        "--private-key",
                        pk,
                        eth_addr,
                        "--value",
                        &triple_eth,
                        "--json",
                    ])
                    .await;
                let eth_blob = eth_send
                    .as_ref()
                    .map(|o| format!("{}{}", o.stdout_str(), o.stderr_str()))
                    .unwrap_or_else(|e| e.to_string());
                let eth_hash = evm_txhash(&eth_blob).unwrap_or_default();
                let eth_dep = if eth_hash.len() >= 66 {
                    fed_signed(
                        dregg_url,
                        "deposit",
                        "/v1/deposit",
                        serde_json::json!({
                            "rail": "Eth",
                            "amount": eth_unit.saturating_mul(3),
                            "raw_hex": eth_hash.trim_start_matches("0x"),
                        }),
                    )
                } else {
                    Err(format!("eth send: {eth_blob}"))
                };
                // Base (8453)
                let base_rpc = base_anvil.docker_dns_rpc_url();
                let base_send = base_anvil
                    .exec_cast(&[
                        "send",
                        "--rpc-url",
                        &base_rpc,
                        "--private-key",
                        pk,
                        eth_addr,
                        "--value",
                        &triple_eth,
                        "--json",
                    ])
                    .await;
                let base_blob = base_send
                    .as_ref()
                    .map(|o| format!("{}{}", o.stdout_str(), o.stderr_str()))
                    .unwrap_or_else(|e| e.to_string());
                let base_hash = evm_txhash(&base_blob).unwrap_or_default();
                let base_dep = if base_hash.len() >= 66 {
                    fed_signed(
                        dregg_url,
                        "deposit",
                        "/v1/deposit",
                        serde_json::json!({
                            "rail": "Base",
                            "amount": eth_unit.saturating_mul(3),
                            "raw_hex": base_hash.trim_start_matches("0x"),
                        }),
                    )
                } else {
                    Err(format!("base send: {base_blob}"))
                };
                // BTC 3×
                let btc_dep = if btc_addr.starts_with("bcrt1") || btc_addr.starts_with("bc1") {
                    match btc.pay_sats_to(btc_addr, triple_btc_here).await {
                        Ok(txid) => fed_signed(
                            dregg_url,
                            "deposit",
                            "/v1/deposit",
                            serde_json::json!({
                                "rail": "Btc",
                                "amount": triple_btc_here as u128,
                                "raw_hex": txid,
                            }),
                        ),
                        Err(e) => Err(e.to_string()),
                    }
                } else {
                    Err("no btc dest".into())
                };

                let st = http_json(
                    "GET",
                    &format!(
                        "{}/v1/status?recipient={rec}",
                        dregg_url.trim_end_matches('/')
                    ),
                    None,
                )
                .unwrap_or(serde_json::json!({}));
                let row = serde_json::json!({
                    "event": "payments",
                    "i": i,
                    "recipient": rec,
                    "pay_targets_ok": true,
                    "qty": p.get("bogo"),
                    "eth": eth_dep.as_ref().ok(),
                    "eth_err": eth_dep.as_ref().err(),
                    "base": base_dep.as_ref().ok(),
                    "base_err": base_dep.as_ref().err(),
                    "btc": btc_dep.as_ref().ok(),
                    "btc_err": btc_dep.as_ref().err(),
                    "status": st,
                    "mint_batches": st.get("mint_batches"),
                });
                log_line(log, &row);
                let ok3 = [&eth_dep, &base_dep, &btc_dep]
                    .iter()
                    .filter(|r| r.as_ref().ok().and_then(|v| v.get("ok")).and_then(|x| x.as_bool()) == Some(true))
                    .count();
                pay_ok += ok3 as u32;
                if base_dep.as_ref().ok().and_then(|v| v.get("ok")).and_then(|x| x.as_bool()) == Some(true) {
                    base_ok_n += 1;
                }
                if btc_dep.as_ref().ok().and_then(|v| v.get("ok")).and_then(|x| x.as_bool()) == Some(true) {
                    btc_ok_n += 1;
                }
                if ok3 < 3 {
                    pay_fail.push(row);
                }
            }
            Ok(p) => {
                pay_fail.push(serde_json::json!({"event":"pay-targets","i": i, "body": p}));
                log_line(log, &serde_json::json!({"event":"pay-targets-fail","i": i, "body": p}));
            }
            Err(e) => {
                pay_fail.push(serde_json::json!({"event":"pay-targets","i": i, "error": e}));
            }
        }
    }

    let health_end = http_json("GET", &format!("{}/health", dregg_url.trim_end_matches('/')), None)
        .unwrap_or(serde_json::json!({}));
    log_line(log, &serde_json::json!({"event":"health_end","body": health_end}));
    let refund = fed_signed(
        dregg_url,
        "sold-out-refund",
        "/v1/query/sold-out-refund",
        serde_json::json!({"collection": "svg"}),
    )
    .unwrap_or(serde_json::json!({"ok": false}));
    log_line(log, &serde_json::json!({"event":"sold-out-refund","body": refund}));

    let auth_broken = auth_ok < n_wallets;
    let pay_broken = !pay_fail.is_empty();
    let sold = health_end
        .get("reasons")
        .and_then(|r| r.as_array())
        .map(|a| a.iter().any(|x| x.as_str() == Some("sold_out")))
        .unwrap_or(false);
    let refunded = refund.get("ok") == Some(&serde_json::Value::Bool(true));
    // sold_out is expected under this load; unpaid overflow without refund is a break.
    let broke = auth_broken || pay_broken || (sold && !refunded);
    let report = serde_json::json!({
        "broke": broke,
        "log": log_path,
        "wallets": n_wallets,
        "authenticator_ok": auth_ok,
        "authenticator_fail": auth_fail.len(),
        "authenticator_errors": auth_fail,
        "rail_payments_ok": pay_ok,
        "rail_payments_fail": pay_fail.len(),
        "base_ok": base_ok_n,
        "btc_ok": btc_ok_n,
        "rail_failures": pay_fail.iter().take(12).cloned().collect::<Vec<_>>(),
        "checkout_ready_end": health_end.get("checkout_ready"),
        "remaining_supply": health_end.get("remaining_supply"),
        "reasons": health_end.get("reasons"),
        "sold_out": sold,
        "refund": refund,
        "mint_batch_size": health_end.get("mint_batch_size"),
        "note": "100 fresh wallets funded from Anvil's 10; ETH+Base+BTC 3× unit; authenticators via feegrant",
    });
    Ok(report)
}

const STORM_EVM_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

/// Derive N unfunded secp256k1 keys, then fill them from Anvil's 10 on ETH and Base.
async fn fund_fresh_evm_wallets(
    anvil: &AnvilChain,
    base_anvil: &AnvilChain,
    n: usize,
    log: &Mutex<std::fs::File>,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut pks = Vec::with_capacity(n);
    let mut addrs = Vec::with_capacity(n);
    for i in 0..n {
        let pk_out = anvil
            .exec_cast(&[
                "wallet",
                "private-key",
                "--mnemonic",
                STORM_EVM_MNEMONIC,
                "--mnemonic-index",
                &i.to_string(),
            ])
            .await?;
        let pk_stdout = pk_out.stdout_str().to_string();
        let pk = pk_stdout
            .lines()
            .find(|l| l.trim().starts_with("0x") && l.trim().len() >= 66)
            .or_else(|| pk_stdout.lines().last())
            .unwrap_or("")
            .trim()
            .to_string();
        if !pk.starts_with("0x") {
            return Err(format!("cast private-key {i}: {} {}", pk_out.stdout_str(), pk_out.stderr_str()).into());
        }
        let addr_out = anvil
            .exec_cast(&["wallet", "address", "--private-key", &pk])
            .await?;
        let addr = addr_out
            .stdout_str()
            .lines()
            .find(|l| l.contains("0x"))
            .unwrap_or("")
            .trim()
            .to_string();
        pks.push(pk);
        addrs.push(addr);
    }
    // Fund from the 10 Anvil whales, sequential per whale (nonce).
    let fund_wei = "0x2386f26fc10000"; // 0.01 ETH
    for (i, addr) in addrs.iter().enumerate() {
        let from = ANVIL_DEFAULT_ACCOUNTS[i % ANVIL_DEFAULT_ACCOUNTS.len()].1;
        let base_rpc = base_anvil.docker_dns_rpc_url();
        let eth_rpc = anvil.docker_dns_rpc_url();
        for (chain_anvil, rpc) in [(anvil, eth_rpc.as_str()), (base_anvil, base_rpc.as_str())]
        {
            let sent = chain_anvil
                .exec_cast(&[
                    "send",
                    "--rpc-url",
                    rpc,
                    "--private-key",
                    from,
                    addr,
                    "--value",
                    fund_wei,
                    "--json",
                ])
                .await;
            let blob = sent
                .as_ref()
                .map(|o| format!("{}{}", o.stdout_str(), o.stderr_str()))
                .unwrap_or_else(|e| e.to_string());
            log_line(
                log,
                &serde_json::json!({
                    "event": "fund_evm",
                    "i": i,
                    "addr": addr,
                    "rpc": rpc,
                    "ok": blob.contains("0x") && sent.is_ok(),
                }),
            );
            if sent.is_err() {
                return Err(format!("fund {addr} on {rpc}: {blob}").into());
            }
        }
    }
    Ok(pks)
}

async fn futures_join<T, F>(jobs: Vec<F>) -> Vec<T>
where
    F: std::future::Future<Output = T>,
{
    let mut out = Vec::with_capacity(jobs.len());
    for j in jobs {
        out.push(j.await);
    }
    out
}

struct LabBroadcaster {
    cid: String,
    home: String,
    /// terp1 bech32 (validator address). Used for `q auth account` sequence.
    from: String,
    denom: String,
    chain_id: String,
    lock: Mutex<()>,
}

impl hash_market::onboard_authz::GrantBroadcaster for LabBroadcaster {
    fn broadcast_authz_exec(&self, exec: &serde_json::Value) -> serde_json::Value {
        let _g = self.lock.lock();
        let inner = exec.get("msgs").and_then(|m| m.get(0)).unwrap_or(exec);
        let grantee = inner.get("grantee").and_then(|x| x.as_str()).unwrap_or("");
        let allowed: Vec<String> = inner
            .pointer("/allowance/allowed_messages")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_else(|| {
                vec![
                    hash_market::onboard_authz::ADD_AUTHENTICATOR_TYPE_URL.to_string(),
                    hash_market::onboard_authz::ADD_AUTHENTICATOR_TYPE_URL_TERP.to_string(),
                ]
            });
        let mut args: Vec<String> = [
            "tx",
            "feegrant",
            "grant",
            self.from.as_str(),
            grantee,
            "--from",
            self.from.as_str(),
            "--keyring-backend",
            "test",
            "--home",
            self.home.as_str(),
            "--chain-id",
            self.chain_id.as_str(),
        ]
        .into_iter()
        .map(|s| s.to_string())
        .collect();
        for m in &allowed {
            args.push("--allowed-messages".into());
            args.push(m.clone());
        }
        let spend = format!("25000{}", self.denom);
        let fees = format!("20000{}", self.denom);
        args.extend(
            [
                "--spend-limit",
                spend.as_str(),
                "--fees",
                fees.as_str(),
                "--gas",
                "200000",
                "--broadcast-mode",
                "sync",
                "--yes",
                "--output",
                "json",
            ]
            .into_iter()
            .map(|s| s.to_string()),
        );
        let mut last = String::new();
        for _i in 0..6 {
            let mut attempt = args.clone();
            if let Some(seq) = terpd_account_sequence(&self.cid, &self.home, &self.from) {
                attempt.push("--sequence".into());
                attempt.push(seq.to_string());
            }
            let refs: Vec<&str> = attempt.iter().map(|s| s.as_str()).collect();
            match terpd_tx(&self.cid, &refs) {
                Ok(s) => {
                    terpd_wait_next_block(&self.cid, &self.home);
                    return serde_json::from_str(s.trim()).unwrap_or(serde_json::json!({"stdout": s}));
                }
                Err(e) => {
                    last = e;
                    if !last.contains("sequence mismatch") {
                        break;
                    }
                    terpd_wait_next_block(&self.cid, &self.home);
                }
            }
        }
        serde_json::json!({"error": last})
    }

    fn broadcast_bank_send(&self, to: &str, amount: &str) -> serde_json::Value {
        let _g = self.lock.lock();
        let mut last = String::new();
        for _i in 0..6 {
            let coins = format!("{amount}{}", self.denom);
            let fees = format!("20000{}", self.denom);
            let seq = terpd_account_sequence(&self.cid, &self.home, &self.from)
                .map(|n| n.to_string())
                .unwrap_or_default();
            let mut args = vec![
                "tx",
                "bank",
                "send",
                self.from.as_str(),
                to,
                coins.as_str(),
                "--from",
                self.from.as_str(),
                "--keyring-backend",
                "test",
                "--home",
                self.home.as_str(),
                "--chain-id",
                self.chain_id.as_str(),
                "--fees",
                fees.as_str(),
                "--gas",
                "200000",
                "--broadcast-mode",
                "sync",
                "--yes",
                "--output",
                "json",
            ];
            if !seq.is_empty() {
                args.push("--sequence");
                args.push(seq.as_str());
            }
            match terpd_tx(&self.cid, &args) {
                Ok(s) => {
                    terpd_wait_next_block(&self.cid, &self.home);
                    return serde_json::from_str(s.trim()).unwrap_or(serde_json::json!({"stdout": s}));
                }
                Err(e) => {
                    last = e;
                    if !last.contains("sequence mismatch") {
                        break;
                    }
                    terpd_wait_next_block(&self.cid, &self.home);
                }
            }
        }
        serde_json::json!({"error": last})
    }
}
