//! Three-chain hasher IBC bench (feat/6.3.0-dev).
//!
//! A = TERP_IAVL_HASHER=blake3 (full)
//! B = hybrid (IBC SHA-256, rest BLAKE3)
//! C = sha256 (counterparty; RecvPacket for both A and B)
//!
//! Docker only. `ICT_MOCK=1` is not a result. Run amd64 on groot2:
//! `TERP_IMAGE_VERSION=… ./scripts/bench/hasher_ibc_groot2.sh`
//!
//! A→C uses `terpnetwork/hermes:08-wasm` (`./scripts/ibc-ops/build_hermes.sh`).
//! Optional `IBC_WASM_LC` stores existing 08-wasm LC on C (client workflow gas).
//!
//! Fast debug (A+C only, N=1, fail-closed):
//! `TERP_IMAGE_VERSION=v6.3.0-dev ./scripts/bench/hasher_wasm_smoke.sh`
//! Full three-chain bench still swallows A→C only if `HASHER_ALLOW_AC_FAIL=1`.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use ict_rs::prelude::*;

/// Pin A→C 08-wasm Hermes to the Terp fork unless the operator overrides.
fn pin_terp_hermes() {
    // SAFETY: called once from `main` before the tokio runtime / relayer spawn.
    unsafe {
        if std::env::var_os("HERMES_IMAGE_REPO").is_none() {
            std::env::set_var("HERMES_IMAGE_REPO", "terpnetwork/hermes");
        }
        if std::env::var_os("HERMES_IMAGE_VERSION").is_none() {
            std::env::set_var("HERMES_IMAGE_VERSION", "08-wasm");
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn env_truthy(key: &str) -> bool {
    matches!(
        std::env::var(key).ok().as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn is_smoke() -> bool {
    env_truthy("HASHER_SMOKE")
}

fn phase(p: &str) {
    // stderr is unbuffered so a watcher sees this before Docker teardown.
    eprintln!("HASHER_PHASE {p}");
}

fn skip_recv(rows: Vec<HasherActionRow>) -> Vec<HasherActionRow> {
    rows.into_iter()
        .filter(|r| r.action != "RecvPacket")
        .collect()
}

fn pair_mode() -> bool {
    matches!(
        std::env::var("HASHER_PAIR").ok().as_deref(),
        Some("both" | "1" | "true" | "blake3" | "sha256")
    )
}

fn pair_hashers() -> Vec<&'static str> {
    match std::env::var("HASHER_PAIR").ok().as_deref() {
        Some("blake3") => vec!["blake3"],
        Some("sha256") => vec!["sha256"],
        _ => vec!["blake3", "sha256"],
    }
}

/// Same-hasher 07-tendermint pairs (no 08-wasm). Two full-BLAKE3 chains still
/// use stock TM `IavlSpec` (SHA-256) — ConnOpenTry is expected to fail if
/// `ibc` proofs are BLAKE3. Two SHA-256 chains are the native TM control.
async fn run_native_pairs() -> Result<(), Box<dyn std::error::Error>> {
    phase("start");
    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let mut actions = Vec::new();
    let mut legs = Vec::new();
    for hasher in pair_hashers() {
        phase(hasher);
        match run_one_tm_pair(runtime.clone(), hasher).await {
            Ok((rows, leg)) => {
                actions.extend(rows);
                if let Some(l) = leg {
                    legs.push(l);
                }
            }
            Err(e) => {
                actions.push(HasherActionRow::fail(
                    "TM handshake",
                    &format!("{hasher}↔{hasher}"),
                    "07-tendermint",
                    e.to_string(),
                ));
            }
        }
    }
    let report = HasherThreeChainReport {
        timestamp: String::new(),
        arch: String::new(),
        image: format!("{}:{}", terp_image_repo(), terp_image_version()),
        topology: "same-hasher 07-tendermint pairs (no 08-wasm)".into(),
        lc_store_gas: 0,
        a_tm_handshake: "native pairs".into(),
        hybrid_inclusion_gas: 0,
        hybrid_exclusion_gas: 0,
        a_inclusion_gas: 0,
        wasm_lc_contract: String::new(),
        a_wasm_client: String::new(),
        a_wasm_path: "n/a (native TM pairs)".into(),
        c_wasm_client: String::new(),
        actions,
        legs,
    };
    phase("done");
    let path = write_three_report(&report, &default_out_dir())?;
    println!("wrote {}", path.display());
    println!("table {}/TABLE.md", default_out_dir().display());
    Ok(())
}

async fn run_one_tm_pair(
    runtime: std::sync::Arc<dyn RuntimeBackend>,
    hasher: &str,
) -> Result<(Vec<HasherActionRow>, Option<HasherIbcReport>), Box<dyn std::error::Error>> {
    let test_name = format!("hasher-pair-{hasher}");
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;
    let id_a = format!("terp-{hasher}-a-1");
    let id_b = format!("terp-{hasher}-b-1");
    let name_a = format!("terp-{hasher}-a");
    let name_b = format!("terp-{hasher}-b");
    let a = CosmosChain::new(chain_cfg(&name_a, &id_a, hasher), 1, 0, runtime.clone());
    let b = CosmosChain::new(chain_cfg(&name_b, &id_b, hasher), 1, 0, runtime.clone());
    let relayer = build_relayer(RelayerType::Hermes, runtime.clone(), &test_name, &network_id).await?;
    let mut ic = Interchain::new(runtime.clone())
        .add_chain(Box::new(a))
        .add_chain(Box::new(b))
        .add_relayer("hermes", relayer);
    ic.build(InterchainBuildOptions {
        test_name: test_name.clone(),
        skip_path_creation: true,
        ..Default::default()
    })
    .await?;
    let chain_a = ic.get_chain(&id_a).ok_or("missing pair A")?;
    let chain_b = ic.get_chain(&id_b).ok_or("missing pair B")?;
    let Some(r) = ic.get_relayer("hermes") else {
        return Err("no hermes".into());
    };
    for (chain, key) in [
        (chain_a, format!("relayer-hermes-{id_a}")),
        (chain_b, format!("relayer-hermes-{id_b}")),
    ] {
        r.add_chain_configuration(chain.config(), &key, chain.rpc_address(), chain.grpc_address())
            .await?;
        let wallet = r.add_key(chain.chain_id(), &key).await?;
        let addr = wallet.formatted_address();
        if addr.len() > 8 {
            chain
                .send_funds(
                    "validator",
                    &WalletAmount {
                        address: addr,
                        denom: "uterp".into(),
                        amount: 100_000_000,
                    },
                )
                .await?;
        }
    }
    chain_a.create_key("user-a").await?;
    let user_a = key_address(chain_a, "user-a").await?;
    chain_a
        .send_funds(
            "validator",
            &WalletAmount {
                address: user_a,
                denom: "uterp".into(),
                amount: 50_000_000_000,
            },
        )
        .await?;
    let dest = key_address(chain_b, "validator").await?;
    wait_for_blocks(chain_a, 2).await?;
    let path = format!("path-{hasher}");
    let h0 = chain_a.height().await.unwrap_or(1);
    r.generate_path(chain_a.chain_id(), chain_b.chain_id(), &path)
        .await?;
    r.link_path(
        &path,
        &ChannelOptions {
            src_port: "transfer".into(),
            dst_port: "transfer".into(),
            ..Default::default()
        },
    )
    .await?;
    wait_for_blocks(chain_b, 2).await?;
    let src_ch = ibc_wasm_path::newest_channel_id(chain_a).await?;
    let dst_ch = ibc_wasm_path::newest_channel_id(chain_b).await?;
    let label = if hasher == "blake3" {
        HasherVariant::F
    } else {
        HasherVariant::H
    };
    let n = env_u64("HASHER_BENCH_N", 1).max(1);
    let ibc_denom = ibc_denom("transfer", &dst_ch, "uterp");
    let h_dst = chain_b.height().await.unwrap_or(1);
    chain_a
        .send_ibc_transfer(
            &src_ch,
            "user-a",
            &WalletAmount {
                address: dest.clone(),
                denom: "uterp".into(),
                amount: 1_000_000,
            },
            &TransferOptions::default(),
        )
        .await?;
    r.flush(&path, &src_ch).await?;
    wait_for_blocks(chain_b, 2).await?;
    let j = hasher_apps::query_txs_json(chain_b, h_dst).await?;
    let recv_gas = first_of(&collect_ibc_msgs(&j, h_dst), IbcMsgKind::RecvPacket)
        .map(|m| m.gas)
        .unwrap_or(0);
    if recv_gas == 0 {
        return Err(format!("{hasher} pair: RecvPacket gas missing after flush").into());
    }
    let samples = vec![TransferSample {
        t_round_trip_ms: 0,
        t_recv_packet_ms: 0,
        t_update_client_ms: 0,
        t_ack_ms: 0,
        gas_recv_packet: recv_gas,
        gas_update_client: 0,
        gas_ack: 0,
        proof_bytes_recv: 0,
    }];
    let leg = HasherIbcReport::from_samples(
        label,
        format!("{}:{}", terp_image_repo(), terp_image_version()),
        "07-tendermint",
        0,
        0,
        0,
        samples,
    );
    let path_label = format!("{hasher}↔{hasher}");
    let mut rows = with_note(
        skip_recv(hasher_apps::lifecycle_rows(chain_a, h0, &path_label, "07-tendermint").await?),
        hasher,
    );
    rows.extend(with_note(
        skip_recv(hasher_apps::lifecycle_rows(chain_b, h0, &path_label, "07-tendermint").await?),
        hasher,
    ));
    if let Some(g) = leg.median_gas_recv {
        let mut row = HasherActionRow::ok("RecvPacket", &path_label, "07-tendermint", g, n);
        row.note = hasher.into();
        rows.push(row);
    }
    let _ = ic.close().await;
    Ok((rows, Some(leg)))
}

fn with_note(mut rows: Vec<HasherActionRow>, note: &str) -> Vec<HasherActionRow> {
    for r in &mut rows {
        r.note = if r.note.is_empty() {
            note.into()
        } else {
            format!("{}; {note}", r.note)
        };
    }
    rows
}

fn short_gov(_cfg: &ChainConfig, raw: Vec<u8>) -> IctResult<Vec<u8>> {
    let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| IctError::Config(format!("parse genesis: {e}")))?;
    if let Some(params) = genesis.pointer_mut("/app_state/gov/params") {
        params["min_deposit"] = serde_json::json!([{"denom": "uterp", "amount": "10000000"}]);
        let vote = if is_smoke() { "5s" } else { "15s" };
        params["voting_period"] = serde_json::json!(vote);
        params["expedited_voting_period"] = serde_json::json!("5s");
        params["max_deposit_period"] = serde_json::json!(vote);
    }
    if let Some(vp) = genesis.pointer_mut("/app_state/gov/voting_params") {
        vp["voting_period"] = serde_json::json!(if is_smoke() { "5s" } else { "15s" });
    }
    if let Some(params) = genesis.pointer_mut("/app_state/ibc/client_genesis/params") {
        let mut allowed = params
            .get("allowed_clients")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let allow_all = allowed.iter().any(|v| v.as_str() == Some("*"));
        let has_wasm = allowed.iter().any(|v| v.as_str() == Some("08-wasm"));
        // ibc-go panics if `*` is mixed with any other client type.
        if !allow_all && !has_wasm {
            allowed.push(serde_json::json!("08-wasm"));
            params["allowed_clients"] = serde_json::Value::Array(allowed);
        }
    }
    serde_json::to_vec(&genesis).map_err(|e| IctError::Config(format!("encode genesis: {e}")))
}

fn hasher_image(mode: &str) -> DockerImage {
    let base = terp_image_version();
    DockerImage {
        repository: terp_image_repo(),
        version: format!("{base}-{mode}"),
        uid_gid: None,
    }
}

fn chain_cfg(name: &str, chain_id: &str, hasher: &str) -> ChainConfig {
    let mut overrides = HashMap::new();
    overrides.insert(
        "config/config.toml".into(),
        serde_json::json!({"rpc": {"max_body_bytes": 10_485_760}}),
    );
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: name.to_string(),
        chain_id: chain_id.to_string(),
        images: vec![hasher_image(hasher)],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 2.0,
        trusting_period: "112h".to_string(),
        block_time: if is_smoke() { "1s".to_string() } else { "2s".to_string() },
        genesis: None,
        modify_genesis: Some(Box::new(short_gov)),
        pre_genesis: None,
        config_file_overrides: overrides,
        additional_start_args: Vec::new(),
        env: vec![("TERP_IAVL_HASHER".into(), hasher.into())],
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

async fn key_address(chain: &dyn Chain, key_name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let output = chain
        .chain_exec(&["keys", "show", key_name, "-a", "--keyring-backend", "test"])
        .await?;
    let addr = output.stdout_str().trim().to_string();
    if addr.is_empty() {
        return Err(format!("empty address for {key_name}").into());
    }
    Ok(addr)
}

async fn query_txs_json(
    chain: &dyn Chain,
    min_height: u64,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let q = format!("tx.height>={min_height}");
    let out = chain
        .chain_exec(&[
            "query",
            "txs",
            "--query",
            &q,
            "--limit",
            "100",
            "--page",
            "1",
            "--output",
            "json",
        ])
        .await?;
    let raw = out.stdout_str();
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({"txs": []}));
    }
    Ok(serde_json::from_str(raw.trim()).unwrap_or_else(|_| serde_json::json!({"txs": []})))
}

fn default_lc_wasm() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("IBC_WASM_LC") {
        let pb = Path::new(&p);
        if pb.is_file() {
            return Some(pb.to_path_buf());
        }
    }
    let mut root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.pop();
    root.pop();
    root.pop();
    let p = root.join("crates/terp-rs/artifacts/cw_ics08_wasm_terp.wasm");
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

async fn store_wasm_lc(
    chain: &dyn Chain,
) -> Result<(u64, String), Box<dyn std::error::Error>> {
    let Some(host) = default_lc_wasm() else {
        return Err("missing crates/terp-rs/artifacts/cw_ics08_wasm_terp.wasm (build with ./scripts/build-ics08-wasm-terp.sh)".into());
    };
    println!(
        "--- {}: gzip + tx ibc-wasm store-code {} ---",
        chain.chain_id(),
        host.display()
    );
    let gz = ibc_wasm_path::gzip_wasm(&host)?;
    ibc_wasm_path::put_file(chain, "/tmp/ibc-lc.wasm.gz", &gz).await?;
    let h0 = chain.height().await.unwrap_or(1);
    let (_id, checksums) = ibc_wasm_path::gov_store_gzipped_lc(chain, "/tmp/ibc-lc.wasm.gz").await?;
    wait_for_blocks(chain, 2).await?;
    let mut checksum = checksums.first().cloned().unwrap_or_default();
    if checksum.is_empty() {
        checksum = chain
            .query_ibc_wasm_checksums()
            .await?
            .first()
            .cloned()
            .unwrap_or_default();
    }
    if checksum.is_empty() {
        return Err(format!(
            "ibc-wasm store-code on {} passed but checksums query empty",
            chain.chain_id()
        )
        .into());
    }
    let mut gas = 0u64;
    if let Ok(j) = query_txs_json(chain, h0).await {
        for tx in j.get("txs").and_then(|t| t.as_array()).cloned().unwrap_or_default() {
            gas = tx
                .get("gas_used")
                .and_then(|g| g.as_str().and_then(|s| s.parse().ok()).or_else(|| g.as_u64()))
                .unwrap_or(gas);
        }
        if gas == 0 {
            for tx in j
                .get("tx_responses")
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default()
            {
                gas = ict_rs::hasher_ibc::tx_gas_used(&tx);
            }
        }
    }
    println!(
        "08-wasm store-code on {} checksum={checksum} gas={gas}",
        chain.chain_id()
    );
    Ok((gas, checksum))
}

async fn one_transfer(
    src: &dyn Chain,
    dst: &dyn Chain,
    src_channel: &str,
    src_key: &str,
    dst_addr: &str,
    ibc_denom: &str,
    amount: u128,
) -> Result<TransferSample, Box<dyn std::error::Error>> {
    let before = dst.get_balance(dst_addr, ibc_denom).await.unwrap_or(0);
    let h_dst = dst.height().await?;
    let h_src = src.height().await?;
    let t0 = Instant::now();
    let tx = src
        .send_ibc_transfer(
            src_channel,
            src_key,
            &WalletAmount {
                address: dst_addr.to_string(),
                denom: "uterp".to_string(),
                amount,
            },
            &TransferOptions::default(),
        )
        .await?;
    if tx.tx_hash.is_empty() {
        return Err("empty transfer txhash".into());
    }

    let mut t_recv = 0u64;
    let mut t_upd = 0u64;
    let mut t_ack = 0u64;
    let mut gas_recv = 0u64;
    let mut gas_upd = 0u64;
    let mut gas_ack = 0u64;
    let mut proof = 0u64;
    let deadline = Instant::now() + Duration::from_secs(90);

    loop {
        if Instant::now() > deadline {
            return Err("timeout waiting for RecvPacket / dest balance".into());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let elapsed = t0.elapsed().as_millis() as u64;

        if gas_recv == 0 {
            let j = query_txs_json(dst, h_dst).await?;
            let msgs = collect_ibc_msgs(&j, h_dst);
            if let Some(u) = first_of(&msgs, IbcMsgKind::UpdateClient) {
                if t_upd == 0 {
                    t_upd = elapsed;
                }
                gas_upd = u.gas;
            }
            if let Some(r) = first_of(&msgs, IbcMsgKind::RecvPacket) {
                if t_recv == 0 {
                    t_recv = elapsed;
                }
                gas_recv = r.gas;
                proof = r.proof_bytes;
            }
        }
        if gas_ack == 0 {
            let j = query_txs_json(src, h_src).await?;
            let msgs = collect_ibc_msgs(&j, h_src);
            if let Some(a) = first_of(&msgs, IbcMsgKind::Acknowledgement) {
                if t_ack == 0 {
                    t_ack = elapsed;
                }
                gas_ack = a.gas;
            }
        }

        let after = dst.get_balance(dst_addr, ibc_denom).await.unwrap_or(0);
        if after >= before.saturating_add(amount) && gas_recv > 0 {
            return Ok(TransferSample {
                t_round_trip_ms: elapsed,
                t_recv_packet_ms: t_recv,
                t_update_client_ms: t_upd,
                t_ack_ms: t_ack,
                gas_recv_packet: gas_recv,
                gas_update_client: gas_upd,
                gas_ack,
                proof_bytes_recv: proof,
            });
        }
    }
}

async fn run_leg(
    src: &dyn Chain,
    dst: &dyn Chain,
    src_key: &str,
    dst_addr: &str,
    src_channel: &str,
    dst_channel: &str,
    label: HasherVariant,
    warmup: u64,
    n: u64,
) -> Result<HasherIbcReport, Box<dyn std::error::Error>> {
    let denom = ibc_denom("transfer", dst_channel, "uterp");
    let amount = 1000u128;
    let mut samples = Vec::new();
    println!(
        "--- {} n={n} warmup={warmup} {src_channel}→C:{dst_channel} denom {denom} ---",
        label.as_str()
    );
    for i in 0..n {
        let s = one_transfer(src, dst, src_channel, src_key, dst_addr, &denom, amount).await?;
        println!(
            "  [{}] round_trip={}ms recv_gas={} proof={}B",
            i + 1,
            s.t_round_trip_ms,
            s.gas_recv_packet,
            s.proof_bytes_recv
        );
        if i >= warmup {
            samples.push(s);
        }
    }
    if samples.is_empty() {
        return Err("no post-warmup RecvPacket samples".into());
    }
    Ok(HasherIbcReport::from_samples(
        label,
        format!("{}:{}", terp_image_repo(), terp_image_version()),
        "07-tendermint",
        warmup,
        0,
        0,
        samples,
    ))
}

async fn hermes_ac_handshake(
    a: &dyn Chain,
    c: &dyn Chain,
    r: &dyn Relayer,
    checksum: &str,
    path: &str,
) -> Result<(String, String, String, String), Box<dyn std::error::Error>> {
    r.generate_path(a.chain_id(), c.chain_id(), path).await?;
    let opts = ClientOptions {
        wasm_checksum: Some(checksum.to_string()),
        wasm_host_chain_id: None,
        ..Default::default()
    };
    r.create_clients(path, &opts).await?;
    println!("A→C clients created (08-wasm on both hosts, path={path})");
    r.create_connections(path).await?;
    println!("A→C connections created path={path}");
    r.create_channel(
        path,
        &ChannelOptions {
            src_port: "transfer".into(),
            dst_port: "transfer".into(),
            ..Default::default()
        },
    )
    .await?;
    println!("A→C channels created path={path}");
    wait_for_blocks(c, 2).await?;
    let client_c = ibc_wasm_path::newest_wasm_client_id(c).await?;
    let client_a = ibc_wasm_path::newest_wasm_client_id(a).await?;
    let src_ch = ibc_wasm_path::newest_channel_id(a).await?;
    let dst_ch = ibc_wasm_path::newest_channel_id(c).await?;
    println!(
        "A→C query ids C.client={client_c} A.client={client_a} A.ch={src_ch} C.ch={dst_ch}"
    );
    if !client_a.starts_with("08-wasm") || !client_c.starts_with("08-wasm") {
        return Err(format!(
            "expected 08-wasm on both hosts, got A={client_a} C={client_c}"
        )
        .into());
    }
    let st_a = ibc_wasm_path::channel_state(a, "transfer", &src_ch).await?;
    let st_c = ibc_wasm_path::channel_state(c, "transfer", &dst_ch).await?;
    println!("A→C dual 08-wasm {client_a}↔{client_c} channels {src_ch} ({st_a}) → {dst_ch} ({st_c})");
    if !st_a.contains("OPEN") || !st_c.contains("OPEN") {
        return Err(
            format!("A→C Hermes channel not OPEN: A {src_ch}={st_a} C {dst_ch}={st_c}").into(),
        );
    }
    Ok((client_c, client_a, src_ch, dst_ch))
}

async fn open_ac_wasm_path(
    a: &dyn Chain,
    c: &dyn Chain,
    relayer: Option<&dyn Relayer>,
    checksum: &str,
    dest: &str,
    n: u64,
    warmup: u64,
    configure_c: bool,
) -> Result<(String, String, String, Option<HasherIbcReport>), Box<dyn std::error::Error>> {
    phase("handshake");
    println!("--- A→C 08-wasm client + handshake ---");
    let checksum = checksum.trim().trim_start_matches("0x");
    let Some(r) = relayer else {
        return Err("no hermes for A→C".into());
    };
    let mut chains: Vec<(&dyn Chain, String)> =
        vec![(a, format!("relayer-hermes-{}", a.chain_id()))];
    if configure_c {
        chains.push((c, format!("relayer-hermes-{}", c.chain_id())));
    }
    for (chain, key) in chains {
        r.add_chain_configuration(chain.config(), &key, chain.rpc_address(), chain.grpc_address())
            .await?;
        let wallet = r.add_key(chain.chain_id(), &key).await?;
        let relayer_addr = wallet.formatted_address();
        if relayer_addr.len() > 8 {
            chain
                .send_funds(
                    "validator",
                    &WalletAmount {
                        address: relayer_addr,
                        denom: "uterp".into(),
                        amount: 100_000_000,
                    },
                )
                .await?;
        }
    }
    wait_for_blocks(a, 2).await?;

    let diy = std::env::var("HASHER_IBC_DIY_HANDSHAKE").ok().as_deref() == Some("1");
    if !diy {
        let (client_c, client_a, src_ch, dst_ch) =
            hermes_ac_handshake(a, c, r, checksum, "path-ac").await?;
        let (client_c, note, leg) = ac_recv_packets(
            a,
            c,
            dest,
            n,
            warmup,
            &client_c,
            &src_ch,
            &dst_ch,
            Some(r),
            Some("path-ac"),
        )
        .await?;
        return Ok((client_c, client_a, note, leg));
    }

    let st = ibc_wasm_path::query_status_json(a).await?;
    let (h, app, _) = ibc_wasm_path::status_height(&st)?;
    let app_hex = if app.len() == 64 && app.chars().all(|ch| ch.is_ascii_hexdigit()) {
        app.to_lowercase()
    } else {
        let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &app)
            .unwrap_or_default();
        to_hex(&raw)
    };
    let rev_a = ibc_wasm_path::revision_of(a.chain_id());
    let inner_cs = serde_json::json!({
        "chain_id": a.chain_id(),
        "latest_height": {"revision_number": rev_a, "revision_height": h},
        "hasher_mode": "blake3",
        "frozen": false,
    });
    let inner_cons = serde_json::json!({
        "timestamp_ns": 1_000_000_000u64,
        "root_hex": app_hex,
    });
    let client_id =
        ibc_wasm_path::create_wasm_client(c, "validator", checksum, &inner_cs, &inner_cons).await?;
    println!("08-wasm client on C of A: {client_id}");

    let cmd = hermes_create_client_cmd(a.chain_id(), c.chain_id());
    let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    let created = r.exec(&cmd_refs, &[]).await?;
    let stdout = format!("{}{}", created.stdout_str(), created.stderr_str());
    let tm_client = if let Some(rest) = stdout.split("07-tendermint-").nth(1) {
        let n: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        if n.is_empty() {
            "07-tendermint-0".into()
        } else {
            format!("07-tendermint-{n}")
        }
    } else {
        "07-tendermint-0".into()
    };
    println!("TM client on A of C: {tm_client}");

    let hdr = ibc_wasm_path::header_json_for_update(a, rev_a).await?;
    ibc_wasm_path::update_wasm_client_json(c, "validator", &client_id, &hdr).await?;

    let _ = ibc_wasm_path::conn_open_init(a, "validator", &tm_client, &client_id).await?;
    wait_for_blocks(a, 1).await?;
    let src_conn = ibc_wasm_path::newest_connection_id(a).await?;
    let c_conn_before = ibc_wasm_path::newest_connection_id(c).await.ok();
    let hdr = ibc_wasm_path::header_json_for_update(a, rev_a).await?;
    ibc_wasm_path::update_wasm_client_json(c, "validator", &client_id, &hdr).await?;
    let proof_h = ibc_wasm_path::status_height(&ibc_wasm_path::query_status_json(a).await?)?.0;
    let _ = ibc_wasm_path::conn_open_try(
        c, a, "validator", &client_id, &tm_client, &src_conn, proof_h, rev_a,
    )
    .await?;
    wait_for_blocks(c, 1).await?;
    let dst_conn = ibc_wasm_path::newest_connection_id(c).await?;
    if c_conn_before.as_ref() == Some(&dst_conn) {
        return Err(format!(
            "ConnOpenTry did not add a connection on C (still {dst_conn})"
        )
        .into());
    }
    println!("A→C connections {src_conn} → {dst_conn}");
    r.update_clients("path-ac").await.ok();
    let proof_c = ibc_wasm_path::status_height(&ibc_wasm_path::query_status_json(c).await?)?.0;
    let rev_c = ibc_wasm_path::revision_of(c.chain_id());
    ibc_wasm_path::conn_open_ack(
        a, c, "validator", &src_conn, &dst_conn, &client_id, proof_c, rev_c, rev_a,
    )
    .await?;
    wait_for_blocks(a, 1).await?;
    let hdr = ibc_wasm_path::header_json_for_update(a, rev_a).await?;
    ibc_wasm_path::update_wasm_client_json(c, "validator", &client_id, &hdr).await?;
    let proof_h = ibc_wasm_path::status_height(&ibc_wasm_path::query_status_json(a).await?)?.0;
    ibc_wasm_path::conn_open_confirm(c, a, "validator", &dst_conn, &src_conn, proof_h, rev_a)
        .await?;

    let _ = ibc_wasm_path::chan_open_init(a, "validator", &src_conn).await?;
    wait_for_blocks(a, 1).await?;
    let src_ch = ibc_wasm_path::newest_channel_id(a).await?;
    let hdr = ibc_wasm_path::header_json_for_update(a, rev_a).await?;
    ibc_wasm_path::update_wasm_client_json(c, "validator", &client_id, &hdr).await?;
    let proof_h = ibc_wasm_path::status_height(&ibc_wasm_path::query_status_json(a).await?)?.0;
    let _ = ibc_wasm_path::chan_open_try(
        c, a, "validator", &dst_conn, &src_ch, proof_h, rev_a,
    )
    .await?;
    wait_for_blocks(c, 1).await?;
    let dst_ch = ibc_wasm_path::newest_channel_id(c).await?;
    let proof_c = ibc_wasm_path::status_height(&ibc_wasm_path::query_status_json(c).await?)?.0;
    ibc_wasm_path::chan_open_ack(a, c, "validator", &src_ch, &dst_ch, proof_c, rev_c).await?;
    wait_for_blocks(a, 1).await?;
    let hdr = ibc_wasm_path::header_json_for_update(a, rev_a).await?;
    ibc_wasm_path::update_wasm_client_json(c, "validator", &client_id, &hdr).await?;
    let proof_h = ibc_wasm_path::status_height(&ibc_wasm_path::query_status_json(a).await?)?.0;
    ibc_wasm_path::chan_open_confirm(c, a, "validator", &dst_ch, &src_ch, proof_h, rev_a).await?;
    wait_for_blocks(c, 1).await?;
    let st_a = ibc_wasm_path::channel_state(a, "transfer", &src_ch).await?;
    let st_c = ibc_wasm_path::channel_state(c, "transfer", &dst_ch).await?;
    println!("A→C channels {src_ch} ({st_a}) → {dst_ch} ({st_c})");
    if !st_a.contains("OPEN") || !st_c.contains("OPEN") {
        return Err(format!("A→C channel not OPEN: A {src_ch}={st_a} C {dst_ch}={st_c}").into());
    }

    let (client_c, note, leg) =
        ac_recv_packets(a, c, dest, n, warmup, &client_id, &src_ch, &dst_ch, None, None).await?;
    Ok((client_c, String::new(), note, leg))
}

async fn update_ac_wasm_client(
    a: &dyn Chain,
    c: &dyn Chain,
    client_id: &str,
    relayer: Option<&dyn Relayer>,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(r) = relayer {
        r.update_wasm_client(c.chain_id(), client_id).await?;
        wait_for_blocks(c, 2).await?;
        return Ok(());
    }
    let rev_a = ibc_wasm_path::revision_of(a.chain_id());
    let hdr = ibc_wasm_path::header_json_for_update(a, rev_a).await?;
    ibc_wasm_path::update_wasm_client_json(c, "validator", client_id, &hdr).await?;
    Ok(())
}

async fn ac_recv_packets(
    a: &dyn Chain,
    c: &dyn Chain,
    dest: &str,
    n: u64,
    warmup: u64,
    client_id: &str,
    src_ch: &str,
    dst_ch: &str,
    relayer: Option<&dyn Relayer>,
    hermes_path: Option<&str>,
) -> Result<(String, String, Option<HasherIbcReport>), Box<dyn std::error::Error>> {
    phase("packets");
    let rev_a = ibc_wasm_path::revision_of(a.chain_id());
    let mut samples = Vec::new();
    for i in 0..n {
        update_ac_wasm_client(a, c, client_id, relayer).await?;
        let tx = a
            .send_ibc_transfer(
                src_ch,
                "user-a",
                &WalletAmount {
                    address: dest.to_string(),
                    denom: "uterp".into(),
                    amount: 1000,
                },
                &TransferOptions::default(),
            )
            .await?;
        if tx.tx_hash.is_empty() {
            return Err("empty A transfer txhash".into());
        }
        wait_for_blocks(a, 2).await?;
        let q = a
            .chain_exec(&["query", "tx", &tx.tx_hash, "--output", "json"])
            .await?;
        let ev: serde_json::Value =
            serde_json::from_str(q.stdout_str().trim()).unwrap_or_default();
        let seq = event_u64(&ev, "packet_sequence").unwrap_or(i + 1);
        let data_hex = event_str(&ev, "packet_data_hex").unwrap_or_default();
        let timeout_ts = event_str(&ev, "packet_timeout_timestamp").unwrap_or_else(|| "0".into());
        let packet = serde_json::json!({
            "sequence": seq.to_string(),
            "source_port": "transfer",
            "source_channel": src_ch,
            "destination_port": "transfer",
            "destination_channel": dst_ch,
            "data": hex_to_b64(&data_hex),
            "timeout_height": {"revision_number": "0", "revision_height": "0"},
            "timeout_timestamp": timeout_ts,
        });
        let h_dst = c.height().await.unwrap_or(1);
        let gas = if let (Some(r), Some(path)) = (relayer, hermes_path) {
            r.flush(path, src_ch).await?;
            wait_for_blocks(c, 2).await?;
            let rec = c
                .chain_exec(&[
                    "query",
                    "ibc",
                    "channel",
                    "packet-receipt",
                    "transfer",
                    dst_ch,
                    &seq.to_string(),
                    "--output",
                    "json",
                ])
                .await?;
            let received = rec.stdout_str().contains("true") || rec.stdout_str().contains("received");
            if !received {
                return Err(format!(
                    "Hermes flush did not receive seq={seq} on {dst_ch}: {}",
                    rec.stdout_str()
                )
                .into());
            }
            let j = query_txs_json(c, h_dst).await?;
            let gas = first_of(&collect_ibc_msgs(&j, h_dst), IbcMsgKind::RecvPacket)
                .map(|m| m.gas)
                .unwrap_or(0);
            if gas == 0 {
                let dump = serde_json::to_string(&j).unwrap_or_default();
                let dump: String = dump.chars().take(1500).collect();
                return Err(format!(
                    "A→C seq={seq} packet-receipt true but no RecvPacket gas_used in query txs (height>={h_dst}): {dump}"
                )
                .into());
            }
            gas
        } else {
            update_ac_wasm_client(a, c, client_id, relayer).await?;
            wait_for_blocks(a, 1).await?;
            let proof_h = match ibc_wasm_path::query_client_latest_height(c, client_id).await {
                Ok(h) if h > 0 => h,
                _ => ibc_wasm_path::status_height(&ibc_wasm_path::query_status_json(a).await?)?.0,
            };
            let recv = ibc_wasm_path::recv_packet(
                c, a, "validator", &packet, "transfer", src_ch, seq, proof_h, rev_a,
            )
            .await?;
            recv.get("gas_used")
                .and_then(|g| g.as_str().and_then(|s| s.parse().ok()).or_else(|| g.as_u64()))
                .unwrap_or(0)
        };
        if gas == 0 {
            return Err(format!("A→C [{i}] RecvPacket gas_used=0 (measurement failed)").into());
        }
        println!("  A→C [{}] RecvPacket gas={gas} (hermes={})", i + 1, hermes_path.is_some());
        if i >= warmup {
            samples.push(TransferSample {
                t_round_trip_ms: 0,
                t_recv_packet_ms: 0,
                t_update_client_ms: 0,
                t_ack_ms: 0,
                gas_recv_packet: gas,
                gas_update_client: 0,
                gas_ack: 0,
                proof_bytes_recv: 0,
            });
        }
    }
    let leg = if samples.is_empty() {
        None
    } else {
        Some(HasherIbcReport::from_samples(
            HasherVariant::F,
            format!("{}:{}", terp_image_repo(), terp_image_version()),
            "08-wasm",
            warmup,
            0,
            0,
            samples,
        ))
    };
    Ok((client_id.to_string(), "ok".into(), leg))
}

fn event_str(tx: &serde_json::Value, key: &str) -> Option<String> {
    fn walk(v: &serde_json::Value, key: &str, out: &mut Option<String>) {
        match v {
            serde_json::Value::Object(m) => {
                if m.get("key").and_then(|k| k.as_str()) == Some(key) {
                    if let Some(s) = m.get("value").and_then(|x| x.as_str()) {
                        *out = Some(s.to_string());
                    }
                }
                for x in m.values() {
                    walk(x, key, out);
                }
            }
            serde_json::Value::Array(a) => {
                for x in a {
                    walk(x, key, out);
                }
            }
            _ => {}
        }
    }
    let mut found = None;
    walk(tx, key, &mut found);
    found
}

fn event_u64(tx: &serde_json::Value, key: &str) -> Option<u64> {
    event_str(tx, key)?.parse().ok()
}

fn hex_to_b64(hex: &str) -> String {
    let t = hex.trim().trim_start_matches("0x");
    let mut out = Vec::new();
    let mut chars = t.chars();
    while let (Some(a), Some(b)) = (chars.next(), chars.next()) {
        if let Ok(byte) = u8::from_str_radix(&format!("{a}{b}"), 16) {
            out.push(byte);
        }
    }
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &out)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    refuse_mock().map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let _ = terp_image_version();
    pin_terp_hermes();

    if pair_mode() {
        return run_native_pairs().await;
    }

    let lab = HasherIbcEnv::from_env();
    let smoke = lab.smoke;
    phase("start");
    println!(
        "=== hasher_ibc_bench smoke={smoke} apps={} spawn={} image={}:{} hermes={}:{} ===",
        lab.apps,
        lab.spawn,
        terp_image_repo(),
        terp_image_version(),
        std::env::var("HERMES_IMAGE_REPO").unwrap_or_default(),
        std::env::var("HERMES_IMAGE_VERSION").unwrap_or_default(),
    );

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let test_name = if smoke {
        "hasher-ibc-smoke"
    } else {
        "hasher-ibc-three"
    };
    let network_id = format!("ict-{test_name}");
    runtime.create_network(&network_id).await?;

    let chain_a = CosmosChain::new(chain_cfg("terp-a", "terp-a-1", "blake3"), 1, 0, runtime.clone());
    let chain_c = CosmosChain::new(chain_cfg("terp-c", "terp-c-1", "sha256"), 1, 0, runtime.clone());
    let relayer = build_relayer(RelayerType::Hermes, runtime.clone(), test_name, &network_id).await?;

    let mut ic = Interchain::new(runtime.clone())
        .add_chain(Box::new(chain_a))
        .add_chain(Box::new(chain_c))
        .add_relayer("hermes", relayer);
    if !smoke {
        let chain_b =
            CosmosChain::new(chain_cfg("terp-b", "terp-b-1", "hybrid"), 1, 0, runtime.clone());
        // B (hybrid, IBC SHA-256) ↔ C first. Smoke skips B entirely.
        ic = ic.add_chain(Box::new(chain_b)).add_link(InterchainLink {
            chain1: "terp-b-1".into(),
            chain2: "terp-c-1".into(),
            relayer: "hermes".into(),
            path: "path-bc".into(),
        });
    }

    phase("chains");
    ic.build(InterchainBuildOptions {
        test_name: test_name.into(),
        skip_path_creation: smoke,
        ..Default::default()
    })
    .await?;

    let result = async {
        let a = ic.get_chain("terp-a-1").ok_or("missing A")?;
        let c = ic.get_chain("terp-c-1").ok_or("missing C")?;
        let b = if smoke {
            None
        } else {
            Some(ic.get_chain("terp-b-1").ok_or("missing B")?)
        };

        phase("store_code");
        let (gas_c, cs_c) = store_wasm_lc(c).await?;
        let (gas_a, cs_a) = store_wasm_lc(a).await?;
        if cs_a != cs_c {
            return Err(format!("08-wasm checksum mismatch A={cs_a} C={cs_c}").into());
        }
        let mut lc_store_gas = gas_a.saturating_add(gas_c);
        let wasm_checksum = cs_c;
        if lab.apps {
            if let Some(b) = b {
                let (gas_b, cs_b) = store_wasm_lc(b).await?;
                if cs_b != wasm_checksum {
                    return Err(format!("08-wasm checksum mismatch B={cs_b} C={wasm_checksum}").into());
                }
                lc_store_gas = lc_store_gas.saturating_add(gas_b);
            }
        }

        a.create_key("user-a").await?;
        let user_a = key_address(a, "user-a").await?;
        let dest = key_address(c, "validator").await?;
        a.send_funds(
            "validator",
            &WalletAmount {
                address: user_a.clone(),
                denom: "uterp".into(),
                amount: 50_000_000_000,
            },
        )
        .await?;
        let user_b = if let Some(b) = b {
            b.create_key("user-b").await?;
            let addr = key_address(b, "user-b").await?;
            b.send_funds(
                "validator",
                &WalletAmount {
                    address: addr.clone(),
                    denom: "uterp".into(),
                    amount: 50_000_000_000,
                },
            )
            .await?;
            addr
        } else {
            String::new()
        };
        wait_for_blocks(a, 2).await?;

        let n = env_u64("HASHER_BENCH_N", if smoke { 1 } else { 20 }).max(1);
        let mut warmup = env_u64("HASHER_BENCH_WARMUP", if smoke { 0 } else { 3 });
        if warmup >= n {
            warmup = 0;
        }

        let skip_tm_ac = std::env::var("HASHER_SKIP_TM_AC")
            .ok()
            .as_deref()
            != Some("0");
        let a_tm_handshake = if skip_tm_ac {
            "skipped (08-wasm Hermes path; set HASHER_SKIP_TM_AC=0 to force)".into()
        } else {
            match ic.get_relayer("hermes") {
            Some(r) => {
                println!("--- A→C 07-tendermint handshake (expect fail if ibc is BLAKE3) ---");
                match r.generate_path("terp-a-1", "terp-c-1", "path-ac").await {
                    Ok(()) => match r
                        .link_path(
                            "path-ac",
                            &ChannelOptions {
                                src_port: "transfer".into(),
                                dst_port: "transfer".into(),
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        Ok(_) => "ok".into(),
                        Err(e) => format!("link_path: {e}"),
                    },
                    Err(e) => format!("generate_path: {e}"),
                }
            }
            None => "no hermes".into(),
            }
        };
        println!("A TM handshake: {a_tm_handshake}");

        let hybrid_inclusion_gas = 0u64;
        let hybrid_exclusion_gas = 0u64;
        let a_inclusion_gas = 0u64;
        let wasm_lc_contract = String::new();

        let mut a_wasm_client = String::new();
        let mut c_wasm_client = String::new();
        let mut actions = Vec::new();
        let mut ac_src_ch = String::new();
        let mut ac_dst_ch = String::new();
        let mut legs = Vec::new();
        if let Some(bchn) = b {
            phase("handshake_bc");
            actions.extend(with_note(
                skip_recv(
                    hasher_apps::lifecycle_rows(bchn, 1, "B→C", "07-tendermint").await?,
                ),
                "TM",
            ));
            actions.extend(with_note(
                skip_recv(
                    hasher_apps::lifecycle_rows(c, 1, "B→C", "07-tendermint").await?,
                ),
                "TM",
            ));
            phase("packets_bc");
            // B-C is the TM path (hybrid IBC SHA-256). C channel-0.
            legs.push(
                run_leg(
                    bchn,
                    c,
                    "user-b",
                    &dest,
                    "channel-0",
                    "channel-0",
                    HasherVariant::H,
                    warmup,
                    n,
                )
                .await?,
            );
        }
        let a_wasm_path = if wasm_checksum.is_empty() {
            return Err("no 08-wasm checksum (store-code skipped)".into());
        } else {
            phase("packets_ac");
            let h_ac_a = a.height().await.unwrap_or(1);
            let h_ac_c = c.height().await.unwrap_or(1);
            match open_ac_wasm_path(
                a,
                c,
                ic.get_relayer("hermes"),
                &wasm_checksum,
                &dest,
                n,
                warmup,
                b.is_none(),
            )
                .await
            {
                Ok((client_c, client_a, path_note, maybe_leg)) => {
                    a_wasm_client = client_c;
                    c_wasm_client = client_a;
                    ac_src_ch = ibc_wasm_path::newest_channel_id(a).await.unwrap_or_default();
                    ac_dst_ch = ibc_wasm_path::newest_channel_id(c).await.unwrap_or_default();
                    if let Some(leg) = maybe_leg {
                        legs.push(leg);
                    }
                    actions.extend(with_note(
                        skip_recv(
                            hasher_apps::lifecycle_rows(a, h_ac_a, "A→C", "08-wasm").await?,
                        ),
                        "08-wasm",
                    ));
                    actions.extend(with_note(
                        skip_recv(
                            hasher_apps::lifecycle_rows(c, h_ac_c, "A→C", "08-wasm").await?,
                        ),
                        "08-wasm",
                    ));
                    path_note
                }
                Err(e) if env_truthy("HASHER_ALLOW_AC_FAIL") => {
                    eprintln!("NOTE: A→C 08-wasm path: {e}");
                    format!("{e}")
                }
                Err(e) => {
                    return Err(e);
                }
            }
        };

        if lab.apps {
            phase("apps");
            let Some(r) = ic.get_relayer("hermes") else {
                return Err("no hermes for apps".into());
            };
            if ac_src_ch.is_empty() {
                ac_src_ch = ibc_wasm_path::newest_channel_id(a).await?;
                ac_dst_ch = ibc_wasm_path::newest_channel_id(c).await?;
            }
            let hooks = hasher_apps::bench_ibc_hooks(
                a,
                c,
                r,
                "path-ac",
                &ac_src_ch,
                &ac_dst_ch,
                "user-a",
                &user_a,
                &dest,
            )
            .await?;
            if !hooks.ok && !env_truthy("HASHER_ALLOW_APP_FAIL") {
                return Err(format!("{}: {}", hooks.action, hooks.note).into());
            }
            actions.push(hooks);
            if let Some(bchn) = b {
                r.generate_path(a.chain_id(), bchn.chain_id(), "path-ab")
                    .await?;
                r.create_clients(
                    "path-ab",
                    &ClientOptions {
                        wasm_checksum: Some(wasm_checksum.clone()),
                        wasm_host_chain_id: None,
                        ..Default::default()
                    },
                )
                .await?;
                r.create_connections("path-ab").await?;
                r.create_channel(
                    "path-ab",
                    &ChannelOptions {
                        src_port: "transfer".into(),
                        dst_port: "transfer".into(),
                        ..Default::default()
                    },
                )
                .await?;
                wait_for_blocks(a, 2).await?;
                let a_ch_ab = ibc_wasm_path::newest_channel_id(a).await?;
                let b_ch_ab = ibc_wasm_path::newest_channel_id(bchn).await?;
                let b_recv = key_address(bchn, "validator").await?;
                let pfm = hasher_apps::bench_pfm(
                    a,
                    bchn,
                    c,
                    r,
                    "path-ab",
                    "path-bc",
                    &a_ch_ab,
                    "channel-0",
                    "channel-0",
                    &b_ch_ab,
                    "user-a",
                    &b_recv,
                    &dest,
                )
                .await?;
                if !pfm.ok && !env_truthy("HASHER_ALLOW_APP_FAIL") {
                    return Err(format!("{}: {}", pfm.action, pfm.note).into());
                }
                actions.push(pfm);
            }
            let poly = hasher_apps::bench_polytone(a, c, r, "path-ac", "user-a").await?;
            if !poly.ok && !env_truthy("HASHER_ALLOW_APP_FAIL") {
                return Err(format!("{}: {}", poly.action, poly.note).into());
            }
            actions.push(poly);
        }

        if let Some(r) = ic.get_relayer("hermes") {
            if !ac_src_ch.is_empty() {
                phase("timeout");
                let trow = hasher_apps::bench_timeout_packet(
                    a,
                    c,
                    r,
                    "path-ac",
                    &ac_src_ch,
                    "user-a",
                    &dest,
                    "08-wasm",
                )
                .await?;
                actions.push(trow);
            }
            phase("warm");
            let mut pin = HasherActionRow::ok(
                "08-wasm Pin()",
                "A+C",
                "08-wasm",
                0,
                1,
            );
            pin.note = "ibc-go store-code already Pin()s checksum; new clients/channels on warm VM"
                .into();
            actions.push(pin);
            let h_w_a = a.height().await.unwrap_or(1);
            let h_w_c = c.height().await.unwrap_or(1);
            let (client_c_w, _client_a_w, src_w, dst_w) =
                hermes_ac_handshake(a, c, r, &wasm_checksum, "path-ac-warm").await?;
            let (_cid, _note, warm_leg) = ac_recv_packets(
                a,
                c,
                &dest,
                1,
                0,
                &client_c_w,
                &src_w,
                &dst_w,
                Some(r),
                Some("path-ac-warm"),
            )
            .await?;
            if let Some(leg) = warm_leg {
                let mut row = HasherActionRow::ok(
                    "RecvPacket",
                    "A→C warm",
                    "08-wasm",
                    leg.median_gas_recv.unwrap_or(0),
                    1,
                );
                row.note = "warm VM after store-code Pin()".into();
                actions.push(row);
            }
            actions.extend(with_note(
                skip_recv(
                    hasher_apps::lifecycle_rows(a, h_w_a, "A→C warm", "08-wasm").await?,
                ),
                "warm",
            ));
            actions.extend(with_note(
                skip_recv(
                    hasher_apps::lifecycle_rows(c, h_w_c, "A→C warm", "08-wasm").await?,
                ),
                "warm",
            ));
        }

        Ok::<_, Box<dyn std::error::Error>>(HasherThreeChainReport {
            timestamp: String::new(),
            arch: String::new(),
            image: format!("{}:{}", terp_image_repo(), terp_image_version()),
            topology: if smoke {
                "A=blake3 C=sha256; 08-wasm A↔C (HASHER_SMOKE)".into()
            } else {
                "A=blake3 B=hybrid C=sha256; TM B→C; 08-wasm A↔C".into()
            },
            lc_store_gas,
            a_tm_handshake,
            hybrid_inclusion_gas,
            hybrid_exclusion_gas,
            a_inclusion_gas,
            wasm_lc_contract,
            a_wasm_client,
            a_wasm_path,
            c_wasm_client,
            actions,
            legs,
        })
    }
    .await;

    if let Err(e) = &result {
        eprintln!("HASHER_FAIL {e}");
    }
    if lab.spawn && !env_truthy("HASHER_KEEP") {
        let _ = ic.close().await;
    } else {
        eprintln!("HASHER_PHASE keep (HASHER_ATTACH/HASHER_KEEP: chains left running)");
    }
    let report = result?;
    phase("done");
    let path = write_three_report(&report, &default_out_dir())?;
    println!("wrote {}", path.display());
    println!("table {}/TABLE.md", default_out_dir().display());
    for leg in &report.legs {
        println!(
            "leg {} median_gas_recv={:?} median_t_round_trip_ms={:?}",
            leg.variant, leg.median_gas_recv, leg.median_t_round_trip_ms
        );
    }
    Ok(())
}
