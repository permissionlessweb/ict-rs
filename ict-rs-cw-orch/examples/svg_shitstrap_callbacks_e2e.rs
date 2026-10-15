//! Cross-chain SVG mint over ibc-callbacks (ADR-8). Replaces the ibc-hooks wasm memo.
//!
//! Two local Docker chains (Terp `terp-local-1`, counterparty `cpty-local-1`, both
//! terp-core with the callbacks middleware) plus Hermes. Ports publish on 127.0.0.1.
//! Nothing is mocked; ICT_MOCK=1 is refused.
//!
//! On Terp: cw721-svg (mainnet code-71 bytes), cw-shitstrap (callbacks memo fix).
//! On the counterparty: cw-svg-ibc-courier (sends the transfer, receives src callbacks).
//!
//! Scenarios, each asserted:
//!  S0 legacy frontend memo `{"ibc_callback":..,"nonce":..}`: no callback fires; funds strand.
//!  S1 courier -> strap, valid `a`: dest callback mints, NFT handed to user; src ack = success.
//!  S2 courier -> strap, `a` overspends: dest callback errors -> error ack -> src AckError -> refund.
//!  S3 courier -> strap, 2s timeout, relayed late: src Timeout -> refund.
//!  S4 user MsgTransfer with the new frontend memo `{"dest_callback":..,"a":..}`: mint succeeds.
//!
//! ```sh
//! ICT_PUBLISH_HOST=127.0.0.1 TERP_IMAGE_REPO=terp-core TERP_IMAGE_VERSION=release-v640-arm64 \
//! SVG_CB_ARTIFACTS=<dir with cw721_svg.wasm cw_shitstrap.wasm cw_svg_ibc_courier.wasm> \
//! cargo run -p ict-rs-cw-orch --example svg_shitstrap_callbacks_e2e --features docker
//! ```
use std::collections::HashMap;
use std::time::Duration;

use ict_rs::ibc::ibc_denom;
use ict_rs::ibc_wasm_path;
use ict_rs::prelude::*;
use serde_json::{json, Value};

const T: &str = "terp-local-1";
const C: &str = "cpty-local-1";
const PATH: &str = "svg-cb-path";

type R<X> = Result<X, Box<dyn std::error::Error>>;

fn cfg(chain_id: &str, name: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: name.to_string(),
        chain_id: chain_id.to_string(),
        images: vec![terp_docker_image()],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 2.0,
        trusting_period: "112h".to_string(),
        block_time: "1s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: HashMap::new(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

fn b64(v: &Value) -> String {
    use base64_lite::encode;
    encode(v.to_string().as_bytes())
}
mod base64_lite {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    pub fn encode(b: &[u8]) -> String {
        let mut o = String::new();
        for c in b.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() { o.push(A[(n >> (18 - 6 * i) & 63) as usize] as char) } else { o.push('=') }
            }
        }
        o
    }
}

async fn tx(chain: &dyn Chain, from: &str, args: &[&str]) -> R<Value> {
    let out = chain
        .chain_exec_tx_with(args, chain.default_tx_opts().from(from).gas("20000000"))
        .await?;
    let v: Value = serde_json::from_str(out.stdout_str().trim())
        .map_err(|e| format!("tx json {e}: {} {}", out.stdout_str(), out.stderr_str()))?;
    if v["code"].as_u64().unwrap_or(0) != 0 {
        return Err(format!("checktx failed: {}", v["raw_log"]).into());
    }
    let h = v["txhash"].as_str().ok_or("no txhash")?.to_string();
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let q = chain.chain_exec(&["query", "tx", &h, "--output", "json"]).await?;
        if q.exit_code != 0 {
            continue;
        }
        let r: Value = serde_json::from_str(q.stdout_str().trim())?;
        if r["code"].as_u64().unwrap_or(0) != 0 {
            return Err(format!("deliver failed: {}", r["raw_log"]).into());
        }
        return Ok(r);
    }
    Err(format!("tx {h} not included").into())
}

fn ev(r: &Value, typ: &str, key: &str) -> Option<String> {
    r["events"].as_array()?.iter().filter(|e| e["type"] == typ).find_map(|e| {
        e["attributes"].as_array()?.iter().find(|a| a["key"] == key).and_then(|a| a["value"].as_str().map(String::from))
    })
}

async fn store(chain: &dyn Chain, from: &str, host: &std::path::Path) -> R<String> {
    let name = host.file_name().unwrap().to_string_lossy().to_string();
    let dest = format!("/tmp/{name}");
    ibc_wasm_path::put_file(chain, &dest, &std::fs::read(host)?).await?;
    let r = tx(chain, from, &["tx", "wasm", "store", &dest]).await?;
    Ok(ev(&r, "store_code", "code_id").ok_or("no code_id")?)
}

async fn inst(chain: &dyn Chain, from: &str, code: &str, msg: &Value, label: &str) -> R<String> {
    let r = tx(chain, from, &["tx", "wasm", "instantiate", code, &msg.to_string(), "--label", label, "--no-admin"]).await?;
    Ok(ev(&r, "instantiate", "_contract_address").ok_or("no contract address")?)
}

async fn q(chain: &dyn Chain, addr: &str, m: &Value) -> R<Value> {
    let o = chain.chain_exec(&["query", "wasm", "contract-state", "smart", addr, &m.to_string(), "--output", "json"]).await?;
    if o.exit_code != 0 {
        return Err(format!("query {addr} {m}: {}", o.stderr_str()).into());
    }
    let v: Value = serde_json::from_str(o.stdout_str().trim())?;
    Ok(v["data"].clone())
}

async fn addr(chain: &dyn Chain, k: &str) -> R<String> {
    let o = chain.chain_exec(&["keys", "show", k, "-a", "--keyring-backend", "test"]).await?;
    Ok(o.stdout_str().trim().to_string())
}

fn check(ok: bool, what: &str) -> R<()> {
    if ok {
        println!("PASS {what}");
        Ok(())
    } else {
        Err(format!("FAIL {what}").into())
    }
}

#[tokio::main]
async fn main() -> R<()> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("refusing ICT_MOCK: this suite runs real Docker chains".into());
    }
    if std::env::var("ICT_PUBLISH_HOST").ok().as_deref() != Some("127.0.0.1") {
        return Err("set ICT_PUBLISH_HOST=127.0.0.1 (loopback only)".into());
    }
    let arts = std::path::PathBuf::from(std::env::var("SVG_CB_ARTIFACTS")?);
    let runtime = IctRuntime::Docker(DockerConfig::default()).into_backend().await?;
    let name = "svg-cb";
    let net = format!("ict-{name}");
    runtime.create_network(&net).await?;
    let relayer = build_relayer(RelayerType::Hermes, runtime.clone(), name, &net).await?;
    let terp_chain = CosmosChain::new(cfg(T, "terp"), 1, 0, runtime.clone());
    let cpty_chain = CosmosChain::new(cfg(C, "cpty"), 1, 0, runtime.clone());
    let mut ic = Interchain::new(runtime)
        .add_chain(Box::new(terp_chain))
        .add_chain(Box::new(cpty_chain))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink { chain1: T.into(), chain2: C.into(), relayer: "hermes".into(), path: PATH.into() });
    if let Err(e) = ic.build(InterchainBuildOptions { test_name: name.into(), ..Default::default() }).await {
        let _ = ic.close().await;
        return Err(e.into());
    }
    let res = run(&mut ic, &arts).await;
    if std::env::var("SVG_CB_KEEP").is_err() {
        let _ = ic.close().await;
    }
    match &res {
        Ok(()) => println!("svg_shitstrap_callbacks_e2e: ALL PASS"),
        Err(e) => println!("svg_shitstrap_callbacks_e2e: FAILED {e}"),
    }
    res
}

async fn relay(rly: &dyn Relayer, terp: &dyn Chain, cpty: &dyn Chain, c_ch: &str, t_ch: &str) -> R<()> {
    rly.flush(PATH, c_ch).await?;
    wait_for_blocks(terp, 3).await?;
    rly.flush(PATH, t_ch).await?;
    wait_for_blocks(cpty, 3).await?;
    Ok(())
}

async fn num_tokens(terp: &dyn Chain, svg: &str) -> R<u64> {
    Ok(q(terp, svg, &json!({"num_tokens": {}})).await?["count"].as_u64().unwrap_or(0))
}

async fn run(ic: &mut Interchain, arts: &std::path::Path) -> R<()> {
    let terp = ic.get_chain(T).unwrap();
    let cpty = ic.get_chain(C).unwrap();
    let rly: &dyn Relayer = ic.get_relayer("hermes").expect("hermes");
    let chans = rly.get_channels(T).await.unwrap_or_default();
    let t_ch = chans.iter().find(|c| c.port_id == "transfer").map(|c| c.channel_id.clone()).unwrap_or_else(|| "channel-0".into()); // fresh single link
    let c_ch = rly.get_channels(C).await.unwrap_or_default().iter().find(|c| c.port_id == "transfer").map(|c| c.channel_id.clone()).unwrap_or_else(|| "channel-0".into());
    println!("channels terp={t_ch} cpty={c_ch}");
    let ibc_uterp = ibc_denom("transfer", &t_ch, "uterp");

    for (ch, k) in [(cpty, "user-c"), (terp, "user-t")] {
        ch.create_key(k).await?;
        let a = addr(ch, k).await?;
        ch.send_funds("validator", &WalletAmount { address: a, denom: "uterp".into(), amount: 1_000_000_000 }).await?;
    }
    wait_for_blocks(terp, 2).await?;
    let user_c = addr(cpty, "user-c").await?;
    let user_t = addr(terp, "user-t").await?;
    let val_t = addr(terp, "validator").await?;

    // Terp side
    let svg_code = store(terp, "validator", &arts.join("cw721_svg.wasm")).await?;
    let ss_code = store(terp, "validator", &arts.join("cw_shitstrap.wasm")).await?;
    let corpus: Value = serde_json::from_slice(&std::fs::read(arts.join("collection.json"))?)?;
    let svg = inst(terp, "validator", &svg_code, &json!({
        "name": corpus["name"], "symbol": "SVGCB", "minter": null, "creator": null, "withdraw_address": null,
        "collection_info_extension": {
            "seed": "c3ZnLWNiLWUyZQ==", "svg_template": corpus["svg_template"], "variables": corpus["variables"],
            "template_slots": corpus["template_slots"], "total": 100, "mint_start_time": null, "mint_end_time": null,
            "whitelist": null, "price_tiers": [{"until_count": 100, "price": {"denom": "uterp", "amount": "1000"}}]
        }}), "svg-cb").await?;
    let strap = inst(terp, "validator", &ss_code, &json!({
        "daos": [], "owner": val_t, "cutoff": "1000000000000",
        "accepted": [{"token": {"native": ibc_uterp}, "shit_rate": "1000000000000000000"}],
        "shitmos": {"native": "uterp"}, "title": "svg-cb strap", "description": "e2e"}), "svg-cb-strap").await?;
    terp.send_funds("validator", &WalletAmount { address: strap.clone(), denom: "uterp".into(), amount: 100_000_000 }).await?;
    println!("terp svg={svg} strap={strap} ibc_denom={ibc_uterp}");

    // Counterparty side
    let cour_code = store(cpty, "validator", &arts.join("cw_svg_ibc_courier.wasm")).await?;
    let courier = inst(cpty, "validator", &cour_code, &json!({}), "svg-cb-courier").await?;
    println!("cpty courier={courier}");

    let mint_action = |amount: u128, funds: u128| json!({"wasm": {"execute": {
        "contract_addr": strap,
        "msg": b64(&json!({"shit_strap_and_mint": {
            "shitstrap": {"shitter": user_t, "shit": {"denom": {"native": ibc_uterp}, "amount": amount.to_string()}, "recipient": user_t, "dao": null},
            "mint": {"c": svg, "msg": {"amnt": 1, "proof_hashes": [], "alloc": 0, "ext": null}}}})),
        "funds": [{"denom": ibc_uterp, "amount": funds.to_string()}]}}});

    // S0 legacy memo: no callback
    let n0 = num_tokens(terp, &svg).await?;
    let b0 = terp.get_balance(&strap, &ibc_uterp).await?;
    cpty.send_ibc_transfer(&c_ch, "user-c", &WalletAmount { address: strap.clone(), denom: "uterp".into(), amount: 1000 },
        &TransferOptions { memo: Some(json!({"ibc_callback": strap, "nonce": "legacy-1"}).to_string()), ..Default::default() }).await?;
    relay(rly, terp, cpty, &c_ch, &t_ch).await?;
    let b1 = terp.get_balance(&strap, &ibc_uterp).await?;
    check(b1 == b0 + 1000 && num_tokens(terp, &svg).await? == n0, "S0 legacy ibc_callback memo is inert: 1000 ibc-uterp stranded in strap, no mint")?;

    // S1 success via courier
    let send = |to: &str, timeout: u64, a: Value| json!({"send": {"channel_id": c_ch, "to": to, "timeout_seconds": timeout, "actions": [a], "dest_gas_limit": null}});
    let uc0 = cpty.get_balance(&user_c, "uterp").await?;
    tx(cpty, "user-c", &["tx", "wasm", "execute", &courier, &send(&strap, 600, mint_action(1000, 1000)).to_string(), "--amount", "1000uterp"]).await?;
    relay(rly, terp, cpty, &c_ch, &t_ch).await?;
    println!("S1 courier records {}", q(cpty, &courier, &json!({"records": {}})).await?);
    println!("S1 strap ibc balance {}", terp.get_balance(&strap, &ibc_uterp).await?);
    let owner0 = q(terp, &svg, &json!({"owner_of": {"token_id": "0"}})).await?;
    check(owner0["owner"] == user_t, "S1 dest callback minted token 0 and handed it to the Terp user")?;
    let recs = q(cpty, &courier, &json!({"records": {}})).await?;
    check(recs[0]["outcome"] == "acked", "S1 src callback: success ack recorded on counterparty")?;
    check(cpty.get_balance(&user_c, "uterp").await? == uc0 - 1000, "S1 user paid exactly 1000 on counterparty")?;

    // S2 ack failure: action spends more than was sent
    let uc1 = cpty.get_balance(&user_c, "uterp").await?;
    tx(cpty, "user-c", &["tx", "wasm", "execute", &courier, &send(&strap, 600, mint_action(2000, 2000)).to_string(), "--amount", "1000uterp"]).await?;
    relay(rly, terp, cpty, &c_ch, &t_ch).await?;
    let recs = q(cpty, &courier, &json!({"records": {}})).await?;
    let o = &recs[1]["outcome"];
    check(o.get("ack_error").is_some(), &format!("S2 overspend -> error ack -> src AckError ({})", o))?;
    check(cpty.get_balance(&user_c, "uterp").await? == uc1, "S2 user refunded in full on counterparty")?;
    check(num_tokens(terp, &svg).await? == n0 + 1, "S2 no extra mint on Terp")?;

    // S3 timeout
    let uc2 = cpty.get_balance(&user_c, "uterp").await?;
    // Halt the background relayer so the packet cannot land before its deadline.
    let _ = rly.stop().await;
    tx(cpty, "user-c", &["tx", "wasm", "execute", &courier, &send(&strap, 2, mint_action(1000, 1000)).to_string(), "--amount", "1000uterp"]).await?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    wait_for_blocks(terp, 3).await?;
    relay(rly, terp, cpty, &c_ch, &t_ch).await?;
    let recs = q(cpty, &courier, &json!({"records": {}})).await?;
    check(recs[2]["outcome"].get("timed_out").is_some(), &format!("S3 timeout -> src Timeout callback ({})", recs[2]["outcome"]))?;
    check(cpty.get_balance(&user_c, "uterp").await? == uc2, "S3 user refunded after timeout")?;
    check(num_tokens(terp, &svg).await? == 1, "S3 no mint on Terp for the timed-out packet")?;
    let _ = rly.start(&[PATH]).await;

    // S4 new frontend memo straight from a user wallet
    cpty.send_ibc_transfer(&c_ch, "user-c", &WalletAmount { address: strap.clone(), denom: "uterp".into(), amount: 1000 },
        &TransferOptions { memo: Some(json!({"dest_callback": {"address": strap}, "a": [mint_action(1000, 1000)]}).to_string()), ..Default::default() }).await?;
    relay(rly, terp, cpty, &c_ch, &t_ch).await?;
    let owner1 = q(terp, &svg, &json!({"owner_of": {"token_id": "1"}})).await?;
    check(owner1["owner"] == user_t, "S4 wallet MsgTransfer with dest_callback memo minted token 1 to the Terp user")?;
    let _ = user_c;
    Ok(())
}
