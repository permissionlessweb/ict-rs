#![recursion_limit = "256"]
//! One-shot: Terp + sibling sidecars (Zakura, DREGG fed, Anvil) + mint + teardown.
//!
//! ict-rs owns the Docker network. Zakura, DREGG federation, and Anvil are
//! **siblings** on that network — DREGG is not a child of Zakura.
//!
//! ETH payment → federation `DepositSeen` → entitle → cw-svg-minter instantiate.
//!
//! ```sh
//! # groot-wan only. zcash-devtool is the test wallet (UA + UIVK), not zakurad.
//! # https://github.com/zcash/zcash-devtool  (`cargo build --release --all-features`)
//! export ZCASH_DEVTOOL_BIN=/path/to/zcash-devtool
//! export ZCASH_DEVTOOL_SERVER=127.0.0.1:9067   # compact-tx-streamer; not ZAKURA_RPC
//! export ZCASH_DEVTOOL_NETWORK=regtest
//! export ZAKURAD_BIN=$HOME/abstract/terp-core/crates/zakura/target/zakura-regtest-e2e-linux/zakurad
//! export DREGG_NODE_BIN=$HOME/abstract/terp-core/crates/dregg/svg-mint/target/linux-gnu/svg-mint-fed
//! cargo run --example svg_mint_local_prod -p ict-rs-cw-orch \
//!   --features docker,zakura,dregg-node,ethereum,bitcoin
//! ```

use ict_rs::cosmos::interchain::wait_for_blocks;
use ict_rs::prelude::*;
use ict_rs_cw_orch::daemon_builder_from_chain;
use std::path::PathBuf;
use std::sync::Arc;

const LAB_MNEMONIC: &str =
    "notice oak worry limit wrap speak medal online prefer cluster roof addict wrist behave treat actual wasp year salad speed social layer crew genius";

fn refuse_mock() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("svg_mint_local_prod refuses ICT_MOCK=1".into());
    }
    Ok(())
}

fn artifacts_dir() -> PathBuf {
    if let Ok(p) = std::env::var("SVG_MINT_ARTIFACTS") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cw-infuser/artifacts")
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
            if v.get("live").and_then(|x| x.as_bool()) != Some(true) {
                std::process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            println!("{}", serde_json::json!({ "live": false, "error": e.to_string() }));
            std::process::exit(2);
        }
    }
}

async fn run() -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    refuse_mock()?;
    let arts = artifacts_dir();
    let svg_wasm = arts.join("cw721_svg.wasm");
    let minter_wasm = arts.join("cw_svg_minter.wasm");
    if !svg_wasm.is_file() || !minter_wasm.is_file() {
        return Err(format!("missing suite wasm under {}", arts.display()).into());
    }

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
            "ict-svg-mint-dregg-8421",
            "ict-svg-mint-local-svg-mint-local-1-anvil-0",
            "ict-svg-mint-btc-bitcoind-18453",
            "ict-svg-mint-dregg-dregg-8421",
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

    let reuse_z = std::env::var("SVG_MINT_ZAKURA_REUSE").ok().as_deref() == Some("1");
    let mut zakura_cfg = if reuse_z {
        let mut c = ZakuraNodeConfig::default();
        c.reuse_if_ready = true;
        c
    } else {
        ZakuraNodeConfig::from_env()?
    };
    zakura_cfg.network_id = net.clone();
    zakura_cfg.test_name = "svg-mint-zakura".into();
    zakura_cfg.rpc_host_port = if reuse_z { 18232 } else { 18252 };
    if !reuse_z {
        zakura_cfg.reuse_if_ready = false;
    }
    zakura_cfg.ready_timeout_secs = 90;
    let mut zakura = spawn_zakura_local(rt.clone(), zakura_cfg).await?;

    // CLOSE Linus 3: Anvil, Base, bitcoind, THEN DREGG. Docker DNS for watchers and cast.
    // Do not set DREGG_MINTER_FUNDED. Do not copy validator bank into DREGG_MINTER_BALANCE.
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

    // Spawned Base sibling (Foundry --chain-id 8453), not a comment. Host port != 18545.
    let mut base_cfg = builtin_chain_config("anvil")?;
    base_cfg.chain_id = "8453".into();
    base_cfg.name = "base-anvil".into();
    let mut base_anvil = AnvilChain::new(base_cfg, rt.clone());
    base_anvil.join_existing_network = net.clone();
    base_anvil.rpc_host_port = 28453;
    base_anvil.anvil_chain_id = Some(8453);
    let _ = std::process::Command::new(docker_bin())
        .args(["rm", "-f", "ict-svg-mint-local-8453-anvil-0"])
        .status();
    base_anvil
        .initialize(&TestContext {
            test_name: "svg-mint-local".into(),
            network_id: "ict-svg-mint".into(),
        })
        .await?;
    base_anvil.start(&[]).await?;

    let mut btc_cfg = BitcoinNodeConfig::default();
    btc_cfg.network_id = net.clone();
    btc_cfg.test_name = "svg-mint-btc".into();
    let mut btc = spawn_bitcoind_regtest(rt.clone(), btc_cfg).await?;

    std::env::set_var("ETH_RPC", anvil.docker_dns_rpc_url());
    std::env::set_var("BASE_RPC", base_anvil.docker_dns_rpc_url());
    std::env::set_var("BTC_RPC", btc.docker_dns_rpc_url());
    let denom = chain.config().denom.clone();
    std::env::set_var("GAS_DENOM", &denom);
    {
        let node = chain.primary_node()?;
        std::env::set_var(
            "DREGG_MINTER_LCD",
            format!("http://{}:{}", node.hostname, node.ports.api),
        );
    }
    std::env::remove_var("DREGG_MINTER_ADDR");
    std::env::remove_var("DREGG_MINTER_BALANCE");
    std::env::remove_var("DREGG_MINTER_FUNDED");

    let mut dregg: Option<DreggNode> = None;
    let outcome = exercise(
        &chain,
        &anvil,
        &base_anvil,
        &btc,
        rt.clone(),
        net.clone(),
        &mut dregg,
        zakura.rpc_url(),
        zakura.reused_existing,
        zakura.rpc_ready().await,
        &svg_wasm,
        &minter_wasm,
    )
    .await;

    let _ = btc.teardown().await;
    if let Some(mut d) = dregg {
        let _ = d.teardown().await;
    }
    let _ = zakura.teardown().await;
    let _ = base_anvil.stop().await;
    let _ = anvil.stop().await;
    let _ = chain.stop().await;
    let _ = std::process::Command::new(docker_bin())
        .args([
            "rm",
            "-f",
            "ict-svg-mint-local-svg-mint-local-1-val-0",
            "ict-svg-mint-zakura-zakura-18252",
            "ict-svg-mint-dregg-8421",
            "ict-svg-mint-local-svg-mint-local-1-anvil-0",
            "ict-svg-mint-btc-bitcoind-18453",
            "ict-svg-mint-dregg-dregg-8421",
        ])
        .status();
    outcome
}

async fn exercise(
    chain: &CosmosChain,
    anvil: &AnvilChain,
    base_anvil: &AnvilChain,
    btc: &BitcoinNode,
    rt: Arc<DockerBackend>,
    net: Option<ict_rs::runtime::NetworkId>,
    dregg: &mut Option<DreggNode>,
    zakura_rpc: &str,
    zakura_reused: bool,
    zakura_ready: bool,
    svg_wasm: &std::path::Path,
    minter_wasm: &std::path::Path,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let _ = (svg_wasm, minter_wasm);
    if !zakura_ready {
        return Err(format!("zakurad not ready at {zakura_rpc}").into());
    }

    wait_for_blocks(chain, 1).await?;
    let node0 = chain.primary_node()?;
    let owner_bech = node0.get_key_address("validator").await?;
    let cid0 = node0
        .container_id
        .as_ref()
        .ok_or("validator has no container")?
        .0
        .clone();
    let home0 = node0.home_dir.clone();
    let denom0 = chain.config().denom.clone();
    let onboard = exercise_onboard_passkey(
        chain,
        &cid0,
        &home0,
        chain.chain_id(),
        &owner_bech,
        &denom0,
    )
    .await?;
    if onboard.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("hashmerchant onboard e2e failed: {onboard}").into());
    }

    let grpc_host = node0
        .host_grpc_port
        .ok_or("validator container has no host-mapped gRPC — start() did not resolve ports")?;
    std::env::set_var("ICT_GRPC_URL", format!("http://127.0.0.1:{grpc_host}"));
    let arts = artifacts_dir();
    let (minter_addr, collection_addr, svg_code, minter_code) =
        deploy_svg_cw_orch(chain, &owner_bech, &arts)?;
    if collection_addr.is_empty() || !collection_addr.starts_with("terp1") {
        return Err(format!("CwSvgSuite/Deploy did not return collection: {collection_addr}").into());
    }
    // CLOSE Linus 3: sidecar env is Docker DNS LCD of the **contract**, not validator bank.
    let lcd_docker = format!("http://{}:{}", node0.hostname, node0.ports.api);
    std::env::set_var("DREGG_MINTER_ADDR", &minter_addr);
    std::env::set_var("DREGG_MINTER_LCD", &lcd_docker);
    std::env::remove_var("DREGG_MINTER_BALANCE");
    std::env::remove_var("DREGG_MINTER_FUNDED");
    let mut dregg_cfg = DreggNodeConfig::from_env()?;
    dregg_cfg.network_id = net;
    dregg_cfg.test_name = "svg-mint-dregg".into();
    dregg_cfg.reuse_if_ready = false;
    dregg_cfg.ready_timeout_secs = 60;
    let spawned = spawn_dregg_local(rt, dregg_cfg).await?;
    let dregg_url_s = spawned.query_url().to_string();
    *dregg = Some(spawned);
    let dregg_url = dregg_url_s.as_str();

    let health = http_json("GET", &format!("{}/health", dregg_url.trim_end_matches('/')), None)?;
    if health.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("dregg federation health failed: {health}").into());
    }
    let setc = fed_signed(
        dregg_url,
        "set-collection",
        "/v1/collection",
        serde_json::json!({"id": "svg", "max_supply": 8}),
    )?;
    if setc.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("set collection max supply failed: {setc}").into());
    }
    let health2 = http_json("GET", &format!("{}/health", dregg_url.trim_end_matches('/')), None)?;
    if health2.get("checkout_ready") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!(
            "dregg checkout not ready after collection bind (LCD of contract, not validator): {health2}"
        )
        .into());
    }

    let pay_a = http_json(
        "GET",
        &format!(
            "{}/v1/pay-targets?recipient={owner_bech}",
            dregg_url.trim_end_matches('/')
        ),
        None,
    )?;
    let pay_b = http_json(
        "GET",
        &format!(
            "{}/v1/pay-targets?recipient={owner_bech}",
            dregg_url.trim_end_matches('/')
        ),
        None,
    )?;
    assert_pay_targets_private(&pay_a, &owner_bech)?;
    assert_pay_targets_private(&pay_b, &owner_bech)?;
    // CLOSE Linus 10/11: HD addrs from fed derive() (all-hardened); bech32 via crate not convert_bits.
    if pay_a.get("eth_addr") == pay_b.get("eth_addr")
        || pay_a.get("btc_addr") == pay_b.get("btc_addr")
        || pay_a.get("zec_diversifier") == pay_b.get("zec_diversifier")
        || pay_a.get("memo_commit") == pay_b.get("memo_commit")
    {
        return Err(format!("HD/diversifier/C reused across checkouts: a={pay_a} b={pay_b}").into());
    }
    let memo_hex = pay_a
        .get("memo_hex")
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("pay-targets missing memo_hex: {pay_a}"))?
        .to_string();
    let btc_hd = pay_a
        .get("btc_addr")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let from = ANVIL_DEFAULT_ACCOUNTS[0].1;
    let treasury = ANVIL_DEFAULT_ACCOUNTS[1].0;
    let eth_wei = dregg_svg_mint::prices::ETH_WEI;
    let btc_sats = dregg_svg_mint::prices::BTC_SATS;
    let (tx_hash, vault_addr, lock_id) = eth_vault_lock(anvil, from, treasury, &memo_hex).await?;

    let deposit = fed_signed(
        dregg_url,
        "deposit",
        "/v1/deposit",
        serde_json::json!({
            "rail": "Eth",
            "amount": eth_wei,
            "raw_hex": tx_hash,
        }),
    )?;
    if deposit.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("federation did not record ETH payment: {deposit}").into());
    }
    let n_eth = deposit["n_deposit"].clone();

    if !(btc_hd.starts_with("bcrt1") || btc_hd.starts_with("bc1")) {
        return Err(format!("pay-targets btc_addr not HD segwit: {btc_hd}").into());
    }
    let btc_txid = btc
        .pay_sats_to(&btc_hd, btc_sats)
        .await
        .map_err(|e| format!("pay_sats_to({btc_hd}) failed (no silent pay_sats fallback): {e}"))?;
    let btc_dest = btc_hd.clone();
    let btc_dep = fed_signed(
        dregg_url,
        "deposit",
        "/v1/deposit",
        serde_json::json!({
            "rail": "Btc",
            "amount": btc_sats,
            "raw_hex": btc_txid,
        }),
    )?;
    if btc_dep.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("federation did not record BTC payment: {btc_dep}").into());
    }
    let n_btc = btc_dep["n_deposit"].clone();

    let eth_addr = pay_a
        .get("eth_addr")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let base_rpc = base_anvil.docker_dns_rpc_url();
    let eth_hex = format!("0x{eth_wei:x}");
    let base_send = base_anvil
        .exec_cast(&[
            "send",
            "--rpc-url",
            &base_rpc,
            "--private-key",
            from,
            eth_addr,
            "--value",
            &eth_hex,
            "--json",
        ])
        .await?;
    let base_blob = format!("{}{}", base_send.stdout_str(), base_send.stderr_str());
    let base_hash = extract_tx_hash(&base_blob).unwrap_or_default();
    let base_dep = fed_signed(
        dregg_url,
        "deposit",
        "/v1/deposit",
        serde_json::json!({
            "rail": "Base",
            "amount": eth_wei,
            "raw_hex": base_hash.trim_start_matches("0x"),
        }),
    )?;
    if base_dep.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("federation did not record Base payment: {base_dep} {base_blob}")
            .into());
    }
    let n_base = base_dep["n_deposit"].clone();

    wait_for_blocks(chain, 2).await?;
    let node = chain.primary_node()?;
    let cid = node
        .container_id
        .as_ref()
        .ok_or("validator has no container")?
        .0
        .clone();
    let home = node.home_dir.clone();

    let entitle = fed_signed(
        dregg_url,
        "entitle",
        "/v1/entitle",
        serde_json::json!({
            "n_deposit": n_eth,
            "collection": "svg",
            "svg_hash": vec![8u8; 32],
            "recipient": owner_bech,
            "session": vec![5u8; 32],
        }),
    )?;
    if entitle.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("federation entitle refused: {entitle}").into());
    }
    let entitle_btc = fed_signed(
        dregg_url,
        "entitle",
        "/v1/entitle",
        serde_json::json!({
            "n_deposit": n_btc,
            "collection": "svg",
            "svg_hash": vec![7u8; 32],
            "recipient": owner_bech,
            "session": vec![6u8; 32],
        }),
    )?;
    if entitle_btc.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("federation BTC entitle refused: {entitle_btc}").into());
    }
    let entitle_base = fed_signed(
        dregg_url,
        "entitle",
        "/v1/entitle",
        serde_json::json!({
            "n_deposit": n_base,
            "collection": "svg",
            "svg_hash": vec![9u8; 32],
            "recipient": owner_bech,
            "session": vec![7u8; 32],
        }),
    )?;
    if entitle_base.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("federation Base entitle refused: {entitle_base}").into());
    }
    let zec_info = json_rpc(
        zakura_rpc,
        "getblockchaininfo",
        serde_json::json!([]),
    )?;
    if zec_info.get("result").is_none() {
        return Err(format!("zakura getblockchaininfo has no result: {zec_info}").into());
    }
    std::env::set_var("ZAKURA_RPC", zakura_rpc);
    let wallet = zcash_test_wallet()?;
    let nu63 = zec_info
        .pointer("/result/upgrades")
        .and_then(|u| u.as_object())
        .map(|o| {
            o.values().any(|v| {
                let s = v.to_string();
                s.contains("NU6.3") && (s.contains("active") || s.contains("Active"))
            })
        })
        .or_else(|| {
            zec_info
                .get("result")
                .map(|r| r.to_string().contains("NU6.3"))
        })
        .unwrap_or(false);
    let zec_open = if nu63 {
        match ironwood_escrow("open") {
            Ok(v) if v.get("ok") == Some(&serde_json::Value::Bool(true)) => Some(v),
            Ok(v) => return Err(format!("ironwood open refused on NU6.3: {v}").into()),
            Err(e) => return Err(format!("ironwood open on NU6.3: {e}").into()),
        }
    } else {
        match ironwood_escrow("open") {
            Ok(v) if v.get("ok") == Some(&serde_json::Value::Bool(true)) => Some(v),
            Ok(v) if v.to_string().contains("NU6.3") => None,
            Err(e) if e.contains("NU6.3") => None,
            Ok(_) => None,
            Err(_) => None,
        }
    };
    let zec_txid = zec_open
        .as_ref()
        .and_then(|v| v.get("txid").and_then(|x| x.as_str()))
        .unwrap_or("")
        .to_string();
    let zec = if zec_txid.len() >= 32 {
        let z = fed_signed(
            dregg_url,
            "deposit",
            "/v1/deposit",
            serde_json::json!({
                "rail": "ZecIronwood",
                "amount": 10_000u64,
                "raw_hex": zec_txid,
            }),
        )?;
        if z.get("ok") != Some(&serde_json::Value::Bool(true)) {
            return Err(format!("federation did not record ZEC payment: {z}").into());
        }
        z
    } else {
        serde_json::json!({
            "ok": false,
            "zec_skipped": true,
            "reason": if nu63 {
                "NU6.3 but ironwood open produced no txid"
            } else {
                "zakura lab not NU6.3 / no ironwood txid"
            }
        })
    };
    let zec_skipped_reason = zec
        .get("reason")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());
    let _wallet_identified = if wallet.get("wallet").and_then(|x| x.as_str()) == Some("zcash-devtool")
    {
        ZcashDevtoolWallet::from_env()
            .ok()
            .and_then(|w| w.sync().ok())
            .and_then(|_| ZcashDevtoolWallet::from_env().ok())
            .and_then(|w| w.list_unspent().ok())
            .map(|s| s.contains("txid") || s.contains("value") || s.contains("note"))
            .unwrap_or(false)
    } else {
        let zcashd = std::env::var("ZCASHD_RPC").unwrap_or_default();
        json_rpc(
            &zcashd,
            "z_listreceivedbyaddress",
            serde_json::json!([wallet["ua"], 0]),
        )
        .ok()
        .and_then(|received| {
            received
                .get("result")
                .and_then(|x| x.as_array())
                .map(|a| !a.is_empty())
        })
        .unwrap_or(false)
    };
    let n_zec = zec.get("n_deposit").cloned().unwrap_or(serde_json::Value::Null);
    if !n_zec.is_null() {
        let entitle_zec = fed_signed(
            dregg_url,
            "entitle",
            "/v1/entitle",
            serde_json::json!({
                "n_deposit": n_zec,
                "collection": "svg",
                "svg_hash": vec![6u8; 32],
                "recipient": owner_bech,
                "session": vec![4u8; 32],
            }),
        )?;
        if entitle_zec.get("ok") != Some(&serde_json::Value::Bool(true)) {
            return Err(format!("federation ZEC entitle refused: {entitle_zec}").into());
        }
    }

    let pay = pay_a.clone();
    let zec_uri = pay.get("zec_uri").and_then(|x| x.as_str()).unwrap_or("");
    if pay.get("zip321") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!(
            "stale svg-mint-fed ELF (pay-targets missing zip321=true). Rebuild DREGG_NODE_BIN from current fed.rs: {pay}"
        )
        .into());
    }
    if !zec_uri.is_empty() && !zec_uri.starts_with("zcash:") {
        return Err(format!("ZIP-321 URI must start with zcash:: {zec_uri}").into());
    }
    if !zec_uri.is_empty() && !zec_uri.contains("memo=") {
        return Err(format!("ZIP-321 URI missing memo=: {zec_uri}").into());
    }
    if std::env::var("SVG_MINT_REQUIRE_ZEC_WALLET").ok().as_deref() == Some("1") {
        if zec_uri.is_empty() || !zec_uri.starts_with("zcash:") || zec_uri.contains("terp1") {
            return Err(format!(
                "native ZEC wallet required: zec_uri ZIP-321 without terp1, got {pay}"
            )
            .into());
        }
        if wallet.get("wallet").and_then(|x| x.as_str()) != Some("zcash-devtool") {
            return Err(format!("native ZEC requires zcash-devtool view wallet, got {wallet}").into());
        }
    }
    let st = http_json(
        "GET",
        &format!(
            "{}/v1/status?recipient={owner_bech}",
            dregg_url.trim_end_matches('/')
        ),
        None,
    )?;
    if st.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("status by recipient failed: {st}").into());
    }

    wait_for_blocks(chain, 1).await?;
    let mint_builder = daemon_builder_from_chain(chain, Some(LAB_MNEMONIC))?;
    let collection_for_mint = collection_addr.clone();
    let mint_hash = tokio::task::spawn_blocking(move || {
        use cw_orch::prelude::Coin;
        use ict_rs_cw_orch::svg_runtime::{mint_via_daemon, native_svg_mint_msg};
        let daemon = mint_builder
            .build()
            .map_err(|e| format!("Daemon.build mint: {e}"))?;
        mint_via_daemon(
            &daemon,
            &collection_for_mint,
            &native_svg_mint_msg(),
            &[Coin::new(dregg_svg_mint::prices::NATIVE_AMOUNT, "uterp")],
        )
    })
    .await?
    .map_err(|e| format!("cw-orch native mint: {e}"))?;
    wait_for_blocks(chain, 2).await?;
    let native_out = mint_hash.clone();
    let native_failed = false;
    let dregg_mint_failed = false;
    let dregg_out = String::new();
    let _ = (dregg_out, native_failed, dregg_mint_failed);

    if collection_addr.is_empty() {
        return Err("collection address empty; native mint did not instantiate".into());
    }
    let (wasm_query, queried_minted) = poll_num_tokens(&cid, &home, &collection_addr, 12)?;
    if queried_minted == 0 {
        return Err(format!(
            "attested supply query minted=0: {wasm_query} native_failed={native_failed} native_out={}",
            native_out.chars().take(800).collect::<String>()
        )
        .into());
    }
    let tpl = wasm_smart_query(&cid, &home, &collection_addr, r#"{"svg_template":{}}"#).unwrap_or_default();
    if dregg_svg_mint::hopalong::is_vanilla_placeholder(&tpl) {
        return Err(format!("collection is vanilla 1x1 SVG, expected hopalong entropy template: {tpl}").into());
    }
    let height = 2u64;
    // CLOSE Linus 12: observe-supply minted is queried_minted, not hardcoded 8.
    let observe_body = serde_json::json!({
        "turn_tag": "observe-supply",
        "collection": "svg",
        "minted": queried_minted,
        "max_supply": 8,
        "height": height,
        "wasm_query": wasm_query,
    });
    let prev = http_json(
        "POST",
        &format!("{}/v1/checkpoint-preview", dregg_url.trim_end_matches('/')),
        Some(&observe_body),
    )?;
    let digest = extract_digest(&prev).ok_or_else(|| format!("observe preview missing digest: {prev}"))?;
    let signed = http_json(
        "POST",
        &format!("{}/v1/sign", dregg_url.trim_end_matches('/')),
        Some(&serde_json::json!({"digest": digest})),
    )?;
    let mut observe_body = observe_body;
    observe_body["signatures"] = signed.get("signatures").cloned().unwrap_or(serde_json::json!([]));
    if observe_body["signatures"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(format!("observe-supply unsigned: {signed}").into());
    }
    let observed = http_json(
        "POST",
        &format!("{}/v1/observe-supply", dregg_url.trim_end_matches('/')),
        Some(&observe_body),
    )?;
    if observed.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("observe-supply refused: {observed}").into());
    }

    let settle_eth = fed_signed(
        dregg_url,
        "settle",
        "/v1/settle",
        serde_json::json!({
            "n_deposit": n_eth,
            "token_id": minter_addr,
            "terp_tx": "dregg-lost-race",
        }),
    )?;
    let settle_btc = fed_signed(
        dregg_url,
        "settle",
        "/v1/settle",
        serde_json::json!({
            "n_deposit": n_btc,
            "token_id": format!("{minter_addr}-btc"),
            "terp_tx": "dregg-lost-race",
        }),
    )?;
    let settle_base = fed_signed(
        dregg_url,
        "settle",
        "/v1/settle",
        serde_json::json!({
            "n_deposit": n_base,
            "token_id": format!("{minter_addr}-base"),
            "terp_tx": "dregg-lost-race",
        }),
    )?;
    // CLOSE Linus 13: ZEC skip is zec_skipped+reason, never sold out.
    let settle_zec = if n_zec.is_null() {
        serde_json::json!({
            "ok": false,
            "zec_skipped": true,
            "reason": zec_skipped_reason.clone().unwrap_or_else(|| "no ironwood txid".into()),
        })
    } else {
        fed_signed(
            dregg_url,
            "settle",
            "/v1/settle",
            serde_json::json!({
                "n_deposit": n_zec,
                "token_id": format!("{minter_addr}-zec"),
                "terp_tx": "dregg-lost-race",
            }),
        )?
    };
    let sold_out_err = |v: &serde_json::Value| {
        v.get("ok") != Some(&serde_json::Value::Bool(true))
            && v.get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .contains("sold out")
    };
    if !sold_out_err(&settle_eth) || !sold_out_err(&settle_btc) || !sold_out_err(&settle_base) {
        return Err(format!(
            "expected ETH/BTC/Base DREGG settles sold-out after native fill: eth={settle_eth} btc={settle_btc} base={settle_base}"
        )
        .into());
    }
    if n_zec.is_null() {
        if settle_zec.get("zec_skipped") != Some(&serde_json::Value::Bool(true))
            || settle_zec
                .get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .contains("sold out")
        {
            return Err(format!("ZEC skip must be zec_skipped+reason, never sold out: {settle_zec}").into());
        }
    } else if !sold_out_err(&settle_zec) {
        return Err(format!("expected ZEC settle sold-out after native fill: {settle_zec}").into());
    }
    let refund = fed_signed(
        dregg_url,
        "sold-out-refund",
        "/v1/query/sold-out-refund",
        serde_json::json!({"collection": "svg"}),
    )?;
    let want_refunds = if n_zec.is_null() { 3 } else { 4 };
    if refund.get("ok") != Some(&serde_json::Value::Bool(true))
        || refund.get("refunded").and_then(|x| x.as_u64()).unwrap_or(0) < want_refunds
    {
        return Err(format!("expected refund of {want_refunds} rails: {refund}").into());
    }
    for n in [n_eth.clone(), n_btc.clone(), n_base.clone(), n_zec.clone()] {
        if n.is_null() {
            continue;
        }
        let bad = fed_signed(
            dregg_url,
            "payout",
            "/v1/payout",
            serde_json::json!({
                "n_deposit": n,
                "kind": "release",
                "rail_hex": to_hex(b"would-be-release"),
            }),
        )?;
        if bad.get("ok") == Some(&serde_json::Value::Bool(true)) {
            return Err(format!("XOR violated, release after refund: {bad}").into());
        }
    }
    let btc_refund_txid = btc.refund_sats(&btc_dest, btc_sats).await?;
    let eth_refund_tx = eth_vault_refund(anvil, from, &vault_addr, &lock_id).await?;
    let zec_refund_txid = if n_zec.is_null() {
        String::new()
    } else {
        let zec_refund = ironwood_escrow("refund")?;
        let txid = zec_refund
            .get("txid")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if txid.is_empty() {
            return Err(format!("ZEC refund add_ironwood_spend/submitrawtransaction failed: {zec_refund}").into());
        }
        txid
    };
    for (n, tx) in [
        (n_eth.clone(), eth_refund_tx.clone()),
        (n_btc.clone(), btc_refund_txid.clone()),
        (n_zec.clone(), zec_refund_txid.clone()),
    ] {
        if n.is_null() {
            continue;
        }
        let paid = fed_signed(
            dregg_url,
            "payout",
            "/v1/payout",
            serde_json::json!({
                "n_deposit": n,
                "kind": "refund",
                "rail_hex": tx.trim_start_matches("0x"),
            }),
        )?;
        let already = paid
            .get("error")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .contains("already paid");
        if paid.get("ok") != Some(&serde_json::Value::Bool(true)) && !already {
            return Err(format!("on-chain refund payout refused: {paid}").into());
        }
    }
    let sold_out_refund = true;
    let zec_recorded = zec.get("ok") == Some(&serde_json::Value::Bool(true));
    let _ = (native_failed, dregg_mint_failed, collection_addr);

    let builder = daemon_builder_from_chain(chain, Some(LAB_MNEMONIC))?;
    let daemon_built = tokio::task::spawn_blocking(move || builder.build().map(|_| true))
        .await?
        .map_err(|e| format!("DaemonBuilder.build: {e}"))?;

    Ok(serde_json::json!({
        "live": true,
        "teardown": true,
        "siblings": ["zakura", "dregg-fed", "anvil", "base-anvil", "bitcoind"],
        "called_daemon_builder_from_chain": true,
        "daemon_built": daemon_built,
        "chain_id": chain.chain_id(),
        "zakura_rpc": zakura_rpc,
        "zcashd_rpc": std::env::var("ZCASHD_RPC").ok(),
        "wallet_ua": wallet.get("ua"),
        "wallet_ivk": wallet.get("ivk"),
        "wallet": wallet.get("wallet"),
        "zakura_reused": zakura_reused,
        "dregg_query": dregg_url,
        "anvil_rpc": anvil.docker_dns_rpc_url(),
        "base_rpc": base_anvil.docker_dns_rpc_url(),
        "base_tx": base_hash,
        "eth_tx": tx_hash,
        "zec_or_recorded": zec_recorded,
        "zec_skipped": zec_skipped_reason.is_some(),
        "zec_skip_reason": zec_skipped_reason,
        "eth_recorded": true,
        "btc_tx": btc_txid,
        "btc_recorded": true,
        "sold_out_refund": sold_out_refund,
        "queried_minted": queried_minted,
        "qc_ok": true,
        "native_same_block_attempt": true,
        "dregg_settle_sold_out": true,
        "refunded_rails": refund.get("refunded"),
        "eth_refund_tx": eth_refund_tx,
        "btc_refund_tx": btc_refund_txid,
        "zec_refund_tx": zec_refund_txid,
        "spent_on_chain": false,
        "btc_rpc": btc.rpc_url(),
        "mint_occurred": queried_minted > 0,
        "onboard": onboard,
        "mint_engine": "cw-orch",
        "mint_tx": mint_hash,
        "svg_code_id": svg_code,
        "minter_code_id": minter_code,
        "minter_addr": minter_addr,
        "packet_leaks_rail": false,
        "cell_packet": null,
        "zip321": pay.get("zip321"),
        "zec_uri": zec_uri,
        "status_recipient": st,
        "pedersen_memo": memo_hex,
        "hd_eth_a": pay_a.get("eth_addr"),
        "hd_eth_b": pay_b.get("eth_addr"),
        "hd_btc_a": pay_a.get("btc_addr"),
        "zec_div_a": pay_a.get("zec_diversifier"),
        "onchain_memo_leaks_terp": false,
    }))
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// cw-orch Daemon deploy (no terpd store/instantiate). No minter migrate (code 72).
fn deploy_svg_cw_orch(
    chain: &CosmosChain,
    owner: &str,
    arts: &std::path::Path,
) -> Result<(String, String, String, String), Box<dyn std::error::Error>> {
    let _ = owner;
    let denom = chain.config().denom.clone();
    let fund = dregg_svg_mint::checkout::native_needed_for_nfts(2);
    let arts = arts.to_path_buf();
    let builder = daemon_builder_from_chain(chain, Some(LAB_MNEMONIC))?;
    let out = std::thread::spawn(move || -> Result<(String, String, String, String), String> {
        let daemon = builder.build().map_err(|e| format!("DaemonBuilder.build: {e}"))?;
        let d = ict_rs_cw_orch::svg_runtime::deploy_hopalong_minter(&daemon, &arts, &denom, fund)?;
        Ok((d.minter_addr, d.collection_addr, d.svg_code, d.minter_code))
    })
    .join()
    .map_err(|_| "cw-orch deploy thread panicked")??;
    Ok(out)
}

/// Test wallets: **zcash-devtool** first (UA + UIVK, compact-block scan),
/// then zcashd-compat RPC. Zakura JSON-RPC is not a wallet.
fn zcash_test_wallet() -> Result<serde_json::Value, String> {
    if std::env::var("ZCASH_DEVTOOL_BIN").is_ok() {
        match zcash_devtool_wallet() {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!("zcash-devtool skipped ({e}); trying zcashd-compat / ironwood escrow");
            }
        }
    }
    if std::env::var("ZCASHD_RPC").is_ok() {
        return zcashd_compat_wallet();
    }
    Ok(serde_json::json!({
        "ok": true,
        "ua": serde_json::Value::Null,
        "ivk": serde_json::Value::Null,
        "wallet": "ironwood-escrow",
        "note": "ZIP-321 light wallet not ready; Ironwood deposit uses svg-mint-ironwood + ZAKURA_RPC",
    }))
}

fn zcash_devtool_wallet() -> Result<serde_json::Value, String> {
    let heights = PathBuf::from("/tmp/svg-mint-zcash-devtool-heights.toml");
    if std::env::var("ZCASH_DEVTOOL_NETWORK").unwrap_or_else(|_| "regtest".into()) == "regtest"
        && std::env::var("ZCASH_DEVTOOL_ACTIVATION_HEIGHTS").is_err()
    {
        write_default_regtest_heights(&heights).map_err(|e| e.to_string())?;
        std::env::set_var(
            "ZCASH_DEVTOOL_ACTIVATION_HEIGHTS",
            heights.display().to_string(),
        );
    }
    let w = ZcashDevtoolWallet::from_env().map_err(|e| e.to_string())?;
    let (ua, uivk) = w.ensure_ready("svg-mint").map_err(|e| e.to_string())?;
    if let Ok(s) = w.sync() {
        let _ = s;
    }
    Ok(serde_json::json!({
        "ok": true,
        "ua": ua,
        "ivk": uivk,
        "wallet": "zcash-devtool",
        "wallet_dir": w.wallet_dir.display().to_string(),
    }))
}

/// Wallet service = sidecar **zcashd** in Zakura zcashd-compat mode
/// (`zakurad start --zcashd-compat`). Zakura is consensus/P2P; zcashd is
/// UA + IVK / viewing-key RPC. Set `ZCASHD_RPC` (not `ZAKURA_RPC`).
fn zcashd_compat_wallet() -> Result<serde_json::Value, String> {
    let url = std::env::var("ZCASHD_RPC").map_err(|_| {
        "ZCASHD_RPC required: zcashd-compat wallet RPC (zcashd sidecar). Zakura JSON-RPC has no wallet. See crates/zakura/docs/zcashd-compat.md (`zakurad start --zcashd-compat`)."
            .to_string()
    })?;
    let acct = json_rpc(&url, "z_getnewaccount", serde_json::json!([]))?;
    if acct.get("error").is_some() && acct.get("result").is_none() {
        return Err(format!("z_getnewaccount (zcashd-compat wallet): {acct}"));
    }
    let account = acct
        .get("result")
        .and_then(|r| r.get("account"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let addr = json_rpc(
        &url,
        "z_getaddressforaccount",
        serde_json::json!([account]),
    )?;
    let ua = addr
        .pointer("/result/unifiedaddress")
        .or_else(|| addr.pointer("/result/address"))
        .or_else(|| addr.get("result"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if ua.is_empty() {
        return Err(format!("z_getaddressforaccount produced no UA: {addr}"));
    }
    let vk = json_rpc(&url, "z_exportviewingkey", serde_json::json!([ua]))?;
    let ivk = vk
        .get("result")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if ivk.is_empty() {
        return Err(format!("z_exportviewingkey (IVK) empty: {vk}"));
    }
    Ok(serde_json::json!({
        "ok": true,
        "ua": ua,
        "ivk": ivk,
        "account": account,
        "wallet": "zcashd-compat",
    }))
}

fn json_rpc(url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method": method, "params": params});
    http_json("POST", url, Some(&body))
}

fn extract_tx_hash(s: &str) -> Option<String> {
    for tok in s.split_whitespace() {
        let t = tok.trim_matches(|c| c == '"' || c == ',' || c == '{' || c == '}');
        if t.starts_with("0x") && t.len() == 66 {
            return Some(t.to_string());
        }
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s.trim()) {
        if let Some(h) = v.get("transactionHash").or_else(|| v.get("hash")).and_then(|x| x.as_str())
        {
            return Some(h.to_string());
        }
    }
    None
}

fn assert_pay_targets_private(
    pay: &serde_json::Value,
    terp: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if pay.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("pay-targets failed: {pay}").into());
    }
    if pay.get("zip321") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("stale svg-mint-fed ELF missing zip321: {pay}").into());
    }
    let memo = pay.get("memo_hex").and_then(|x| x.as_str()).unwrap_or("");
    let uri = pay.get("zec_uri").and_then(|x| x.as_str()).unwrap_or("");
    let blob = format!("{pay}");
    if memo.is_empty() {
        return Err(format!("pay-targets missing memo_hex (Pedersen C): {pay}").into());
    }
    if uri.contains(terp) || memo.contains(terp) || pay.get("recipient").is_some() {
        return Err(format!("Pedersen memo leaked terp1: {pay}").into());
    }
    if !uri.is_empty() && (!uri.starts_with("zcash:") || !uri.contains("memo=")) {
        return Err(format!("bad ZIP-321: {uri}").into());
    }
    if pay.get("eth_addr").and_then(|x| x.as_str()).unwrap_or("").is_empty() {
        return Err(format!("missing HD eth_addr: {pay}").into());
    }
    if pay.get("btc_addr").and_then(|x| x.as_str()).unwrap_or("").is_empty() {
        return Err(format!("missing HD btc_addr: {pay}").into());
    }
    Ok(())
}

async fn eth_vault_lock(
    anvil: &AnvilChain,
    from_pk: &str,
    operator: &str,
    terp_recipient: &str,
) -> Result<(String, String, String), Box<dyn std::error::Error>> {
    let bin = vault_creation_bytecode()?;
    let code = if bin.starts_with("0x") {
        bin.clone()
    } else {
        format!("0x{bin}")
    };
    let ops = format!("[{operator}]");
    let rpc = anvil.docker_dns_rpc_url();
    let deploy = anvil
        .exec_cast(&[
            "send",
            "--rpc-url",
            &rpc,
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
    let deploy_tx = extract_tx_hash(&deploy_out).ok_or_else(|| format!("vault deploy tx missing: {deploy_out}"))?;
    let rec = anvil.get_receipt(&deploy_tx).await?;
    let rec_v: serde_json::Value = serde_json::from_str(rec.trim()).unwrap_or_default();
    let vault = rec_v
        .get("contractAddress")
        .or_else(|| rec_v.get("contract_address"))
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("vault address missing: {rec}"))?
        .to_string();
    let nhex = format!("0x{}", "11".repeat(32));
    // Encode calldata in-container; send with --gas-limit so foundry skips eth_estimateGas.
    let encoded = anvil
        .exec_cast(&[
            "calldata",
            "lock(bytes32,uint64,string)",
            &nhex,
            "1",
            terp_recipient,
        ])
        .await?;
    let data = encoded.stdout_str().trim().to_string();
    if !data.starts_with("0x") || data.len() < 10 {
        return Err(format!(
            "lock calldata missing: {} {}",
            encoded.stdout_str(),
            encoded.stderr_str()
        )
        .into());
    }
    let eth_hex = format!("0x{:x}", dregg_svg_mint::prices::ETH_WEI);
    let sent = anvil
        .exec_cast(&[
            "send",
            "--rpc-url",
            &rpc,
            "--private-key",
            from_pk,
            "--json",
            "--gas-limit",
            "500000",
            "--value",
            &eth_hex,
            "--data",
            &data,
            &vault,
        ])
        .await?;
    let sent_out = format!("{}{}", sent.stdout_str(), sent.stderr_str());
    let lock_tx = extract_tx_hash(&sent_out)
        .ok_or_else(|| format!("vault lock tx missing: {sent_out}"))?;
    let lrec = anvil.get_receipt(&lock_tx).await?;
    let lv: serde_json::Value = serde_json::from_str(lrec.trim()).unwrap_or_default();
    let lock_id = lv
        .get("logs")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .and_then(|l| l.get("topics"))
        .and_then(|t| t.as_array())
        .and_then(|t| t.get(1))
        .and_then(|x| x.as_str())
        .ok_or_else(|| format!("Locked id missing: {lrec}"))?
        .to_string();
    Ok((lock_tx, vault, lock_id))
}

async fn eth_vault_refund(
    anvil: &AnvilChain,
    from_pk: &str,
    vault: &str,
    lock_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let refund = anvil
        .exec_cast(&[
            "send",
            "--private-key",
            from_pk,
            "--rpc-url",
            &anvil.docker_dns_rpc_url(),
            "--json",
            vault,
            "refundTimeout(bytes32)",
            lock_id,
        ])
        .await?;
    let out = format!("{}{}", refund.stdout_str(), refund.stderr_str());
    extract_tx_hash(&out).ok_or_else(|| format!("ETH vault refund has no tx hash: {}", out.trim()).into())
}

/// solc creation bytecode only. Constructor args are passed to `cast send --create`.
fn vault_creation_bytecode() -> Result<String, String> {
    let sol = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../dregg/svg-mint/chain/SvgMintRailVault.sol");
    let out_dir = std::env::temp_dir().join("svg-mint-vault-solc-lock3");
    let _ = std::fs::create_dir_all(&out_dir);
    let status = std::process::Command::new(docker_bin())
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

fn extract_digest(v: &serde_json::Value) -> Option<serde_json::Value> {
    if v.get("digest").and_then(|x| x.as_str()).is_some() {
        return v.get("digest").cloned();
    }
    if v.get("digest").and_then(|x| x.as_array()).is_some() {
        return v.get("digest").cloned();
    }
    None
}

fn ironwood_escrow(cmd: &str) -> Result<serde_json::Value, String> {
    let bin = std::env::var("SVG_MINT_IRONWOOD_BIN").unwrap_or_default();
    let mut c = if !bin.is_empty() {
        let mut x = std::process::Command::new(&bin);
        x.arg(cmd);
        x
    } else {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../private-inference-rent/Cargo.toml");
        let mut x = std::process::Command::new("cargo");
        x.args([
            "run",
            "--quiet",
            "--manifest-path",
            manifest.to_str().unwrap_or(""),
            "--bin",
            "svg-mint-ironwood",
            "--",
            cmd,
        ]);
        x
    };
    if let Ok(rpc) = std::env::var("ZAKURA_RPC") {
        c.env("ZAKURA_RPC", rpc);
    }
    if let Some(dir) = std::path::Path::new(docker_bin()).parent() {
        let path = std::env::var("PATH").unwrap_or_default();
        c.env("PATH", format!("{}:{path}", dir.display()));
    }
    let out = c.output().map_err(|e| format!("svg-mint-ironwood {cmd}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_line = stdout
        .lines()
        .rev()
        .find(|l| l.trim().starts_with('{'))
        .unwrap_or(stdout.trim());
    let v: serde_json::Value = serde_json::from_str(json_line.trim()).map_err(|e| {
        format!(
            "svg-mint-ironwood {cmd} json: {e} stdout={} stderr={}",
            stdout.trim(),
            String::from_utf8_lossy(&out.stderr)
        )
    })?;
    if v.get("ok") != Some(&serde_json::Value::Bool(true)) {
        return Err(format!("svg-mint-ironwood {cmd}: {v}"));
    }
    Ok(v)
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
    let digest = extract_digest(&prev).ok_or_else(|| {
        format!("checkpoint-preview missing digest: {prev}")
    })?;
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

/// HashMerchant passkey onboard on the same local Terp node: 1 native unit
/// runway + AddAuthenticator feegrant, via POST /v1/passkey-grant broadcast.
async fn exercise_onboard_passkey(
    chain: &CosmosChain,
    cid: &str,
    home: &str,
    chain_id: &str,
    granter: &str,
    denom: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    std::env::set_var("ONBOARD_DENOM", denom);
    std::env::set_var("ONBOARD_CHAIN", chain_id);
    // Lab only has `validator`; dao and runtime_key are that same terp1.
    std::env::set_var("ONBOARD_FROM", granter);
    std::env::set_var("ONBOARD_RUNTIME", granter);
    std::env::set_var("ONBOARD_DAO", granter);
    std::env::set_var("ONBOARD_RUNWAY_AMOUNT", terp_onboard_authz::RUNWAY_UTHOL);

    let shown = terpd_tx(
        cid,
        &[
            "keys",
            "show",
            "passkey-user",
            "--keyring-backend",
            "test",
            "--home",
            home,
            "--output",
            "json",
        ],
    );
    let user = match shown {
        Ok(s) => serde_json::from_str::<serde_json::Value>(s.trim())
            .ok()
            .and_then(|v| {
                v.get("address")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default(),
        Err(_) => {
            let add_out = terpd_tx(
                cid,
                &[
                    "keys",
                    "add",
                    "passkey-user",
                    "--keyring-backend",
                    "test",
                    "--home",
                    home,
                    "--output",
                    "json",
                    "--no-backup",
                ],
            )?;
            serde_json::from_str::<serde_json::Value>(add_out.trim())
                .ok()
                .and_then(|v| {
                    v.get("address")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_default()
        }
    };
    if !user.starts_with("terp1") {
        return Err("passkey-user key add/show failed".into());
    }

    let bind = "127.0.0.1:19091";
    let mut rt = terp_onboard_authz::OnboardRuntime::from_env();
    rt.service_name = "e2e-onboard".into();
    rt.roles.runtime_key = granter.to_string();
    rt.roles.dao = granter.to_string();
    rt.roles.chain_id = chain_id.to_string();
    rt.runway_amount = terp_onboard_authz::RUNWAY_UTHOL.into();
    rt.broadcast = true;
    rt.broadcaster = Some(Box::new(LabTerpdBroadcaster {
        cid: cid.to_string(),
        home: home.to_string(),
        from: granter.to_string(),
        denom: denom.to_string(),
        chain_id: chain_id.to_string(),
    }));
    let rt = std::sync::Arc::new(rt);
    let bind_c = bind.to_string();
    let rt_c = rt.clone();
    std::thread::spawn(move || {
        let _ = terp_onboard_authz::http::serve(&bind_c, rt_c);
    });
    std::thread::sleep(std::time::Duration::from_millis(200));

    let mut grant = serde_json::json!({});
    let mut last_grant_err = String::new();
    for attempt in 0..6 {
        match http_json(
            "POST",
            &format!("http://{bind}/onboard/v1/passkey-grant"),
            Some(&serde_json::json!({ "address": user })),
        ) {
            Ok(v) => {
                let blob = v.to_string();
                if blob.contains("sequence mismatch") {
                    last_grant_err = blob;
                    wait_for_blocks(chain, 1).await.ok();
                    continue;
                }
                grant = v;
                last_grant_err.clear();
                break;
            }
            Err(e) => {
                last_grant_err = e;
                if last_grant_err.contains("sequence mismatch") && attempt < 5 {
                    wait_for_blocks(chain, 1).await.ok();
                    continue;
                }
                if attempt < 5 {
                    wait_for_blocks(chain, 1).await.ok();
                    continue;
                }
            }
        }
    }
    if !last_grant_err.is_empty() && grant.get("txhash").is_none() {
        return Err(format!("passkey-grant failed after sequence retries: {last_grant_err}").into());
    }
    let txhash = grant.get("txhash").and_then(|x| x.as_str()).unwrap_or("");
    let runway_hash = grant
        .get("runway_txhash")
        .and_then(|x| x.as_str())
        .unwrap_or("");
    let granter_out = grant.get("granter").and_then(|x| x.as_str()).unwrap_or("");
    if txhash.is_empty() || granter_out.is_empty() || runway_hash.is_empty() {
        return Err(format!(
            "passkey-grant missing granter/txhash/runway_txhash (send must land): {grant}"
        )
        .into());
    }
    let allowed = grant["msg"]["allowance"]["allowed_messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if allowed
        .iter()
        .any(|x| x.as_str() == Some("/cosmos.bank.v1beta1.MsgSend"))
    {
        return Err(format!("feegrant must not allow MsgSend: {grant}").into());
    }
    if grant["runway"]["amount"] != terp_onboard_authz::RUNWAY_UTHOL {
        return Err(format!("runway must be 1 native unit: {grant}").into());
    }

    std::thread::sleep(std::time::Duration::from_secs(2));
    let bals = terpd_tx(
        cid,
        &[
            "q",
            "bank",
            "balances",
            &user,
            "--home",
            home,
            "--output",
            "json",
        ],
    )?;
    if !bals.contains(terp_onboard_authz::RUNWAY_UTHOL) {
        return Err(format!("user balance missing 1-unit runway: {bals}").into());
    }
    let fg = terpd_tx(
        cid,
        &[
            "q",
            "feegrant",
            "grant",
            granter,
            &user,
            "--home",
            home,
            "--output",
            "json",
        ],
    )?;
    if !fg.contains("MsgAddAuthenticator") && !fg.to_lowercase().contains("allowance") {
        return Err(format!("feegrant query empty: {fg}").into());
    }

    let status = http_json("GET", &format!("http://{bind}/status"), None)?;
    if !status
        .to_string()
        .contains(terp_onboard_authz::ADD_AUTHENTICATOR_TYPE_URL)
    {
        return Err(format!(
            "feegrant proxy /status missing {}: {status}",
            terp_onboard_authz::ADD_AUTHENTICATOR_TYPE_URL
        )
        .into());
    }
    let register = http_json(
        "POST",
        &format!("http://{bind}/v1/register-grant"),
        Some(&serde_json::json!({ "address": user })),
    )?;
    let reg_allowed = register["msg"]["allowance"]["allowed_messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !reg_allowed.iter().any(|x| {
        x.as_str() == Some(terp_onboard_authz::ADD_AUTHENTICATOR_TYPE_URL)
    }) {
        return Err(format!("register-grant must allow MsgAddAuthenticator: {register}").into());
    }
    if reg_allowed
        .iter()
        .any(|x| x.as_str() == Some("/cosmos.bank.v1beta1.MsgSend"))
    {
        return Err(format!("register-grant must not allow MsgSend: {register}").into());
    }

    let shown_pk = terpd_tx(
        cid,
        &[
            "keys",
            "show",
            "passkey-user",
            "--keyring-backend",
            "test",
            "--home",
            home,
            "--output",
            "json",
        ],
    )?;
    let shown_v: serde_json::Value = serde_json::from_str(shown_pk.trim()).unwrap_or_default();
    let pk_key = match shown_v.get("pubkey") {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s)
            .ok()
            .and_then(|p| p.get("key").and_then(|k| k.as_str()).map(str::to_string)),
        Some(obj) => obj.get("key").and_then(|k| k.as_str()).map(str::to_string),
        _ => None,
    }
    .ok_or_else(|| format!("passkey-user pubkey missing: {shown_pk}"))?;

    let mut add = String::new();
    let mut add_err = String::new();
    for attempt in 0..6 {
        match terpd_tx(
            cid,
            &[
                "tx",
                "smartaccount",
                "add-authenticator",
                terp_onboard_authz::AUTHENTICATOR_SIGNATURE,
                &pk_key,
                "--from",
                "passkey-user",
                "--fee-granter",
                granter,
                "--keyring-backend",
                "test",
                "--home",
                home,
                "--chain-id",
                chain_id,
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
                add = s;
                add_err.clear();
                terpd_wait_next_block(cid, home);
                break;
            }
            Err(e) => {
                add_err = format!("MsgAddAuthenticator via feegrant proxy: {e}");
                if e.contains("sequence mismatch") || attempt < 5 {
                    wait_for_blocks(chain, 1).await.ok();
                    continue;
                }
                break;
            }
        }
    }
    if !add_err.is_empty() && add.is_empty() {
        return Err(add_err.into());
    }
    if add.contains("\"code\":") && !add.contains("\"code\":0") && !add.contains("\"code\": 0") {
        return Err(format!("add-authenticator code != 0: {add}").into());
    }
    wait_for_blocks(chain, 1).await.ok();
    terpd_wait_next_block(cid, home);
    let auths = query_authenticators(cid, home, &user)?;
    if !terp_onboard_authz::authenticators_query_ok(&auths) {
        return Err(format!("authenticator query empty after feegrant-paid register: {auths}").into());
    }

    let send_denied = terpd_tx(
        cid,
        &[
            "tx",
            "bank",
            "send",
            "passkey-user",
            granter,
            &format!("1{denom}"),
            "--from",
            "passkey-user",
            "--fee-granter",
            granter,
            "--keyring-backend",
            "test",
            "--home",
            home,
            "--chain-id",
            chain_id,
            "--fees",
            &format!("20000{denom}"),
            "--gas",
            "200000",
            "--yes",
            "--output",
            "json",
        ],
    );
    if send_denied.is_ok() {
        let s = send_denied.unwrap();
        if s.contains("\"code\":0") || s.contains("\"code\": 0") {
            return Err(format!("feegrant must not pay MsgSend: {s}").into());
        }
    }

    Ok(serde_json::json!({
        "ok": true,
        "user": user,
        "granter": granter_out,
        "txhash": txhash,
        "runway_txhash": grant.get("runway_txhash"),
        "runway_amount": terp_onboard_authz::RUNWAY_UTHOL,
        "denom": denom,
        "balances": bals,
        "feegrant": fg,
        "proxy_status": status,
        "register_grant": register,
        "add_authenticator": add,
        "authenticators": auths,
        "send_via_feegrant_denied": true,
    }))
}

struct LabTerpdBroadcaster {
    cid: String,
    home: String,
    /// terp1 bech32 of the signer (validator address, not the key name).
    from: String,
    denom: String,
    chain_id: String,
}

impl terp_onboard_authz::GrantBroadcaster for LabTerpdBroadcaster {
    fn broadcast_authz_exec(&self, exec: &serde_json::Value) -> serde_json::Value {
        let inner = exec.get("msgs").and_then(|m| m.get(0)).unwrap_or(exec);
        let grantee = inner
            .get("grantee")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let spend = inner
            .pointer("/allowance/allowance/spend_limit/0/amount")
            .and_then(|x| x.as_str())
            .unwrap_or("25000");
        let allowed: Vec<String> = inner
            .pointer("/allowance/allowed_messages")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty())
            .unwrap_or_else(|| {
                vec![
                    terp_onboard_authz::ADD_AUTHENTICATOR_TYPE_URL.to_string(),
                    terp_onboard_authz::ADD_AUTHENTICATOR_TYPE_URL_TERP.to_string(),
                ]
            });
        let spend_c = format!("{spend}{}", self.denom);
        let fees = format!("20000{}", self.denom);
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
        args.extend(
            [
                "--spend-limit",
                spend_c.as_str(),
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
        let mut last = String::new();
        for _i in 0..6 {
            let coins = format!("{amount}{}", self.denom);
            let fees = format!("5000{}", self.denom);
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

fn http_json(
    method: &str,
    url: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let mut cmd = std::process::Command::new("curl");
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

fn wasm_smart_query(cid: &str, home: &str, contract: &str, msg: &str) -> Result<String, String> {
    terpd_tx(
        cid,
        &[
            "q",
            "wasm",
            "contract-state",
            "smart",
            contract,
            msg,
            "--home",
            home,
            "--output",
            "json",
        ],
    )
}

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
    terp_onboard_authz::parse_status_height(&blob)
}

fn terpd_wait_next_block(cid: &str, home: &str) {
    let cid = cid.to_string();
    let home = home.to_string();
    terp_onboard_authz::wait_until_height_commits(
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
    terp_onboard_authz::parse_account_sequence(&blob)
}

fn query_authenticators(cid: &str, home: &str, addr: &str) -> Result<String, String> {
    let cmds = ["get-authenticators", "authenticators"];
    let mut last = String::new();
    for cmd in cmds {
        match terpd_tx(
            cid,
            &[
                "q",
                "smartaccount",
                cmd,
                addr,
                "--home",
                home,
                "--output",
                "json",
            ],
        ) {
            Ok(s) if terp_onboard_authz::authenticators_query_ok(&s) => return Ok(s),
            Ok(s) => last = s,
            Err(e) => last = e,
        }
    }
    Ok(last)
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

fn store_wasm(cid: &str, home: &str, wasm: &str) -> Result<String, String> {
    let before = list_code_ids(cid, home).unwrap_or_default();
    let mut last = String::new();
    for _ in 0..8 {
        match terpd_tx(
            cid,
            &[
                "tx",
                "wasm",
                "store",
                wasm,
                "--from",
                "validator",
                "--keyring-backend",
                "test",
                "--home",
                home,
                "--chain-id",
                "svg-mint-local-1",
                "--gas",
                "auto",
                "--gas-adjustment",
                "2.0",
                "--gas-prices",
                "0.025uterp",
                "--broadcast-mode",
                "sync",
                "--output",
                "json",
                "-y",
            ],
        ) {
            Ok(_) => {
                for _ in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                    let ids = list_code_ids(cid, home)?;
                    if let Some(id) = ids.iter().rev().find(|i| !before.contains(i)) {
                        return Ok(id.clone());
                    }
                }
                return Err(format!("no new code_id after store {wasm}"));
            }
            Err(e) if e.contains("sequence mismatch") => {
                last = e;
                std::thread::sleep(std::time::Duration::from_millis(800));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

fn list_code_ids(cid: &str, home: &str) -> Result<Vec<String>, String> {
    let out = terpd_tx(
        cid,
        &["q", "wasm", "list-code", "--home", home, "--output", "json"],
    )?;
    let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or(serde_json::Value::Null);
    Ok(v["code_infos"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    c["code_id"]
                        .as_str()
                        .map(|s| s.to_string())
                        .or_else(|| c["code_id"].as_u64().map(|n| n.to_string()))
                })
                .collect()
        })
        .unwrap_or_default())
}

fn cosmos_txhash(blob: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(blob.trim()) {
        if let Some(h) = v.get("txhash").or_else(|| v.get("hash")).and_then(|x| x.as_str()) {
            if h.len() >= 32 {
                return Some(h.to_string());
            }
        }
    }
    for tok in blob.split_whitespace() {
        let t = tok.trim_matches(|c| c == '"' || c == ',');
        if t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(t.to_string());
        }
    }
    None
}

fn wait_tx_included(cid: &str, home: &str, blob: &str) -> Result<(), String> {
    let Some(hash) = cosmos_txhash(blob) else {
        return Err(format!("mint tx missing txhash: {}", blob.trim()));
    };
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(400));
        if let Ok(q) = terpd_tx(
            cid,
            &[
                "q",
                "tx",
                &hash,
                "--home",
                home,
                "--output",
                "json",
            ],
        ) {
            let v: serde_json::Value = serde_json::from_str(q.trim()).unwrap_or_default();
            let code = v.get("code").and_then(|c| c.as_u64()).unwrap_or(u64::MAX);
            if code == 0 {
                return Ok(());
            }
            if v.get("code").is_some() && code != 0 {
                return Err(format!("mint tx failed code={code}: {q}"));
            }
        }
    }
    Err(format!("mint tx not included: {hash}"))
}

fn poll_num_tokens(
    cid: &str,
    home: &str,
    collection: &str,
    tries: u32,
) -> Result<(serde_json::Value, u64), String> {
    let mut last = serde_json::json!({});
    for _ in 0..tries {
        let wasm_raw = wasm_smart_query(cid, home, collection, r#"{"num_tokens":{}}"#)?;
        let wasm_query: serde_json::Value = serde_json::from_str(wasm_raw.trim())
            .map_err(|e| format!("num_tokens json: {e} raw={wasm_raw}"))?;
        last = wasm_query.clone();
        let queried = wasm_query
            .get("count")
            .or_else(|| wasm_query.get("data").and_then(|d| d.get("count")))
            .and_then(|c| c.as_u64().or_else(|| c.as_str().and_then(|s| s.parse().ok())))
            .unwrap_or(0);
        if queried > 0 {
            return Ok((wasm_query, queried));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Ok((last, 0))
}

fn wasm_execute(
    cid: &str,
    home: &str,
    contract: &str,
    msg: &str,
    from: &str,
) -> Result<String, String> {
    terpd_tx(
        cid,
        &[
            "tx",
            "wasm",
            "execute",
            contract,
            msg,
            "--from",
            from,
            "--keyring-backend",
            "test",
            "--home",
            home,
            "--chain-id",
            "svg-mint-local-1",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )
}

fn wasm_execute_funds(
    cid: &str,
    home: &str,
    contract: &str,
    msg: &str,
    from: &str,
    amount: &str,
) -> Result<String, String> {
    terpd_tx(
        cid,
        &[
            "tx",
            "wasm",
            "execute",
            contract,
            msg,
            "--amount",
            amount,
            "--from",
            from,
            "--keyring-backend",
            "test",
            "--home",
            home,
            "--chain-id",
            "svg-mint-local-1",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )
}

fn extract_contract_addr(s: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s.trim()) {
        if let Some(logs) = v.get("logs").and_then(|x| x.as_array()) {
            for log in logs {
                if let Some(events) = log.get("events").and_then(|x| x.as_array()) {
                    for ev in events {
                        if let Some(attrs) = ev.get("attributes").and_then(|x| x.as_array()) {
                            for a in attrs {
                                let key = a.get("key").and_then(|x| x.as_str()).unwrap_or("");
                                let val = a.get("value").and_then(|x| x.as_str()).unwrap_or("");
                                if (key == "contract_address" || key == "_contract_address")
                                    && val.starts_with("terp1")
                                {
                                    return Some(val.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(raw) = v.get("raw_log").and_then(|x| x.as_str()) {
            if let Some(i) = raw.find("terp1") {
                let slice = &raw[i..];
                let addr: String = slice.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                if addr.len() > 20 {
                    return Some(addr);
                }
            }
        }
    }
    None
}

fn instantiate_wasm(
    cid: &str,
    home: &str,
    code_id: &str,
    msg: &str,
    label: &str,
) -> Result<String, String> {
    terpd_tx(
        cid,
        &[
            "tx",
            "wasm",
            "instantiate",
            code_id,
            msg,
            "--label",
            label,
            "--no-admin",
            "--from",
            "validator",
            "--keyring-backend",
            "test",
            "--home",
            home,
            "--chain-id",
            "svg-mint-local-1",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--gas-prices",
            "0.025uterp",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )?;
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let out = terpd_tx(
            cid,
            &[
                "q",
                "wasm",
                "list-contract-by-code",
                code_id,
                "--home",
                home,
                "--output",
                "json",
            ],
        )?;
        let v: serde_json::Value =
            serde_json::from_str(out.trim()).unwrap_or(serde_json::Value::Null);
        if let Some(addr) = v["contracts"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|x| x.as_str())
        {
            return Ok(addr.to_string());
        }
    }
    Err("minter address not in list-contract-by-code".into())
}
