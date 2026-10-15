//! Chain upgrade E2E using Docker — mirrors `tests/interchaintest/chain_upgrade_test.go`.
//!
//! Starts Terp on the **pre-v6** image, submits `MsgSoftwareUpgrade` plan **`v6`**
//! (override with `ICT_UPGRADE_NAME=v6.1` for the IAVL dual-store upgrade),
//! (SDK 0.54 + ibc-go v11.1 + official 08-wasm v11.1.0 + zk-wasmvm), votes,
//! waits for halt, swaps to the post-upgrade image, restarts, then checks
//! tokenfactory / wasm / upgrade module_versions and block production.
//!
//! Interchaintest (Go) still needs a 0.54-capable fork; this rust example is
//! the runnable upgrade harness until that lands.
//!
//! ## Images
//!
//! `TERP_IMAGE_REPO` and `TERP_IMAGE_VERSION` are the final image.
//! `TERP_UPGRADE_FROM` is the halt image (it must not already register the
//! plan). `TERP_UPGRADE_MID` is the image that executes the plan and, on
//! v6.3, arms the next one. `local` and `local-zk` are refused.
//!
//! ```sh
//! TERP_IMAGE_REPO=registry.terp.network/terp-core \
//! TERP_IMAGE_VERSION=v6.4.0 TERP_UPGRADE_FROM=v6.2.1 TERP_UPGRADE_MID=v6.3.0 \
//!   cargo run --example cosmos_upgrade --features docker
//! ```
//!
//! `ICT_UPGRADE_WORKFLOWS` is a comma list (default `core`):
//! - `core` — tokenfactory params, wasm params, upgrade module_versions
//! - `ibc`  — client/connection/channel counts (load-bearing IBC surface)
//! - `ibc-app` — transfer params, denom-traces, wasm codes (ICS-20 + wasm)
//! - `tf-tx` — post-upgrade `tokenfactory create-denom` (needs funded user)
//! - `iavl`  — post-upgrade `query upgrade applied <plan>` (v6.1 dual-store / IAVL v2 TSH is `make tsh-upgrade-v61`)

use ict_rs::cli::parse_query_response;
use ict_rs::prelude::*;

/// Blocks ahead of now to schedule the halt (matches Go ICT).
const HALT_HEIGHT_DELTA: u64 = 12;
const BLOCKS_AFTER_UPGRADE: u64 = 8;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

fn nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

fn refuse_retired(label: &str, tag: &str) {
    if tag == "local" || tag == "local-zk" {
        panic!(
            "{label}={tag} is retired. Set TERP_IMAGE_VERSION (and TERP_UPGRADE_FROM only when the halt image is a different real tag)."
        );
    }
}

fn start_version() -> String {
    let tag = nonempty("ICT_UPGRADE_FROM").or_else(|| nonempty("TERP_UPGRADE_FROM")).unwrap_or_else(|| {
        panic!(
            "set TERP_UPGRADE_FROM (or ICT_UPGRADE_FROM) to the pre-upgrade tag. \
             The SDK rejects a plan whose handler is already linked in the running binary \
             (BINARY UPDATED BEFORE TRIGGER). TERP_IMAGE_VERSION is the final image, not the halt image."
        );
    });
    refuse_retired("TERP_UPGRADE_FROM", &tag);
    tag
}

fn plan_name_from_tag(version: &str) -> String {
    let bare = version.trim_start_matches('v');
    let mut parts: Vec<&str> = bare.split('.').collect();
    if parts.len() >= 3 && parts.last() == Some(&"0") {
        parts.pop();
    }
    if version.starts_with('v') {
        format!("v{}", parts.join("."))
    } else {
        parts.join(".")
    }
}

fn upgrade_name() -> String {
    if let Some(name) = nonempty("ICT_UPGRADE_NAME").or_else(|| nonempty("TERP_UPGRADE_NAME")) {
        return name;
    }
    plan_name_from_tag(&first_hop_version())
}

fn upgrade_repo() -> String {
    nonempty("ICT_UPGRADE_REPO")
        .or_else(|| nonempty("TERP_UPGRADE_REPO"))
        .unwrap_or_else(terp_image_repo)
}

fn final_version() -> String {
    let tag = terp_image_version();
    refuse_retired("TERP_IMAGE_VERSION", &tag);
    tag
}

/// Image that contains the handler for [`upgrade_name`]. When this differs
/// from [`final_version`], the chain is expected to arm the next plan and halt
/// again (v6.3 arms v6.4). The second swap uses `TERP_IMAGE_VERSION`.
fn first_hop_version() -> String {
    let tag = nonempty("TERP_UPGRADE_MID")
        .or_else(|| nonempty("ICT_UPGRADE_MID"))
        .or_else(|| nonempty("ICT_UPGRADE_TO"))
        .or_else(|| nonempty("TERP_UPGRADE_TO"))
        .unwrap_or_else(final_version);
    refuse_retired("TERP_UPGRADE_MID", &tag);
    tag
}

fn upgrade_version() -> String {
    first_hop_version()
}

fn modify_v6_genesis(_cfg: &ChainConfig, raw: Vec<u8>) -> IctResult<Vec<u8>> {
    let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| IctError::Config(format!("parse genesis: {e}")))?;

    if let Some(params) = genesis.pointer_mut("/app_state/staking/params") {
        params["bond_denom"] = serde_json::json!("uterp");
    }
    if let Some(params) = genesis.pointer_mut("/app_state/mint/params") {
        params["mint_denom"] = serde_json::json!("uterp");
    }
    if let Some(params) = genesis.pointer_mut("/app_state/gov/params") {
        params["min_deposit"] = serde_json::json!([{"denom": "uterp", "amount": "10000000"}]);
        params["voting_period"] = serde_json::json!("15s");
        params["expedited_voting_period"] = serde_json::json!("5s");
        params["max_deposit_period"] = serde_json::json!("15s");
    }
    if let Some(vp) = genesis.pointer_mut("/app_state/gov/voting_params") {
        vp["voting_period"] = serde_json::json!("15s");
    }

    serde_json::to_vec(&genesis).map_err(|e| IctError::Config(format!("encode genesis: {e}")))
}

fn terp_upgrade_config(version: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "120u-1".to_string(),
        images: vec![DockerImage {
            repository: upgrade_repo(),
            version: version.to_string(),
            uid_gid: None,
        }],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0uterp".to_string(),
        gas_adjustment: 2.0,
        trusting_period: "112h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: Some(Box::new(modify_v6_genesis)),
        pre_genesis: None,
        config_file_overrides: std::collections::HashMap::new(),
        additional_start_args: vec!["--wasm.skip_wasmvm_version_check".to_string()],
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

async fn query_json(chain: &CosmosChain, args: &[&str]) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let mut cmd = vec!["query"];
    cmd.extend_from_slice(args);
    cmd.extend_from_slice(&["--output", "json"]);
    let out = chain.chain_exec(&cmd).await?;
    if out.exit_code != 0 {
        return Err(format!(
            "query {:?} failed: stdout={} stderr={}",
            args,
            out.stdout_str().trim(),
            out.stderr_str().trim()
        )
        .into());
    }
    Ok(parse_query_response(&out)?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpgradePhase {
    Pre,
    Post,
}

impl UpgradePhase {
    fn label(self) -> &'static str {
        match self {
            Self::Pre => "pre-upgrade",
            Self::Post => "post-upgrade",
        }
    }
}

fn enabled_workflows() -> Vec<String> {
    env_or("ICT_UPGRADE_WORKFLOWS", "core")
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn workflow_on(enabled: &[String], name: &str) -> bool {
    enabled.iter().any(|w| w == name || w == "all")
}

fn json_len(v: &serde_json::Value, keys: &[&str]) -> usize {
    let mut cur = v;
    for k in keys {
        cur = match cur.get(*k) {
            Some(x) => x,
            None => return 0,
        };
    }
    cur.as_array().map(|a| a.len()).unwrap_or(0)
}

async fn workflow_core(
    chain: &CosmosChain,
    phase: UpgradePhase,
) -> Result<(), Box<dyn std::error::Error>> {
    let tf = query_json(chain, &["tokenfactory", "params"]).await?;
    let wasm = query_json(chain, &["wasm", "params"]).await?;
    let versions = query_json(chain, &["upgrade", "module_versions"]).await?;
    println!(
        "  [{}] tokenfactory={} wasm={} module_versions={}",
        phase.label(),
        !tf.is_null(),
        !wasm.is_null(),
        !versions.is_null()
    );
    if tf.is_null() || wasm.is_null() {
        return Err(format!("{}: tokenfactory/wasm params missing", phase.label()).into());
    }
    if phase == UpgradePhase::Post && versions.is_null() {
        return Err("post-upgrade: module_versions empty".into());
    }
    Ok(())
}

async fn workflow_ibc(
    chain: &CosmosChain,
    phase: UpgradePhase,
) -> Result<IbcSnapshot, Box<dyn std::error::Error>> {
    let clients = query_json(chain, &["ibc", "client", "states"]).await.unwrap_or(serde_json::json!({}));
    let conns = query_json(chain, &["ibc", "connection", "connections"]).await.unwrap_or(serde_json::json!({}));
    let chans = query_json(chain, &["ibc", "channel", "channels"]).await.unwrap_or(serde_json::json!({}));
    let snap = IbcSnapshot {
        clients: json_len(&clients, &["client_states"]),
        connections: json_len(&conns, &["connections"]),
        channels: json_len(&chans, &["channels"]),
    };
    println!(
        "  [{}] ibc clients={} connections={} channels={}",
        phase.label(),
        snap.clients,
        snap.connections,
        snap.channels
    );
    Ok(snap)
}

#[derive(Clone, Debug, Default)]
struct IbcSnapshot {
    clients: usize,
    connections: usize,
    channels: usize,
}

#[derive(Clone, Debug, Default)]
struct IbcAppSnapshot {
    transfer_ok: bool,
    denom_traces: usize,
    wasm_codes: usize,
}

async fn workflow_ibc_app(
    chain: &CosmosChain,
    phase: UpgradePhase,
) -> Result<IbcAppSnapshot, Box<dyn std::error::Error>> {
    let transfer = query_json(chain, &["ibc-transfer", "params"])
        .await
        .unwrap_or(serde_json::json!({}));
    let traces = query_json(chain, &["ibc-transfer", "denom-traces"])
        .await
        .unwrap_or(serde_json::json!({}));
    let codes = query_json(chain, &["wasm", "list-code"])
        .await
        .unwrap_or(serde_json::json!({}));

    let snap = IbcAppSnapshot {
        transfer_ok: !transfer.is_null()
            && (transfer.get("params").is_some()
                || transfer.get("send_enabled").is_some()
                || transfer.as_object().map(|o| !o.is_empty()).unwrap_or(false)),
        denom_traces: json_len(&traces, &["denom_traces"]),
        wasm_codes: json_len(&codes, &["code_infos"]),
    };
    println!(
        "  [{}] ibc-app transfer_ok={} denom_traces={} wasm_codes={}",
        phase.label(),
        snap.transfer_ok,
        snap.denom_traces,
        snap.wasm_codes
    );
    if !snap.transfer_ok {
        return Err(format!("{}: ibc-transfer params missing", phase.label()).into());
    }
    Ok(snap)
}

async fn workflow_tf_tx(
    chain: &CosmosChain,
    key: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let sub = format!("v6{}", &uuidish());
    let out = chain
        .exec(
            &[
                "tx",
                "tokenfactory",
                "create-denom",
                &sub,
                "--from",
                key,
                "--gas",
                "auto",
                "--gas-adjustment",
                "1.5",
                "--fees",
                "1000uterp",
                "-y",
                "--output",
                "json",
            ],
            &[],
        )
        .await?;
    if out.exit_code != 0 {
        return Err(format!("create-denom failed: {}", out.stderr_str()).into());
    }
    println!("  [post-upgrade] tokenfactory create-denom {sub} ok");
    Ok(())
}

fn uuidish() -> String {
    format!(
        "{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    )
}

async fn run_workflows(
    chain: &CosmosChain,
    phase: UpgradePhase,
    pre_ibc: Option<&IbcSnapshot>,
    user_key: &str,
) -> Result<Option<IbcSnapshot>, Box<dyn std::error::Error>> {
    let enabled = enabled_workflows();
    println!("\n--- {} workflows ({}) ---", phase.label(), enabled.join(","));
    if workflow_on(&enabled, "core") {
        workflow_core(chain, phase).await?;
    }
    let mut ibc_snap = None;
    if workflow_on(&enabled, "ibc") {
        let snap = workflow_ibc(chain, phase).await?;
        if phase == UpgradePhase::Post {
            if let Some(pre) = pre_ibc {
                if snap.clients < pre.clients
                    || snap.connections < pre.connections
                    || snap.channels < pre.channels
                {
                    return Err(format!(
                        "ibc surface shrank across upgrade: pre={pre:?} post={snap:?}"
                    )
                    .into());
                }
            }
            if snap.clients == 0 && snap.channels == 0 {
                println!(
                    "  [{}] ibc empty (fresh genesis — expected; statesync/mainnet state would be non-zero)",
                    phase.label()
                );
            }
        }
        ibc_snap = Some(snap);
    }
    if workflow_on(&enabled, "ibc-app") {
        let app = workflow_ibc_app(chain, phase).await?;
        if phase == UpgradePhase::Post && app.denom_traces == 0 && app.wasm_codes == 0 {
            println!(
                "  [{}] ibc-app empty traces/codes (fresh genesis — expected; A.sh statesync would be non-zero)",
                phase.label()
            );
        }
    }
    if phase == UpgradePhase::Post && workflow_on(&enabled, "tf-tx") {
        workflow_tf_tx(chain, user_key).await?;
    }
    if phase == UpgradePhase::Post && workflow_on(&enabled, "iavl") {
        workflow_iavl(chain).await?;
    }
    Ok(ibc_snap)
}

async fn workflow_iavl(chain: &CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let plan = upgrade_name();
    let applied = query_json(chain, &["upgrade", "applied", &plan]).await?;
    let height = applied.get("height").cloned().unwrap_or(serde_json::Value::Null);
    if height.is_null() || height == serde_json::json!("0") || height == serde_json::json!(0) {
        return Err(format!("iavl: upgrade {plan} not applied (got {applied})").into());
    }
    println!("  [post-upgrade] iavl: applied {plan} at height {height}");
    println!("  [post-upgrade] iavl: CMS remains IAVL v1; v2 ingest is TSH tsh-upgrade-v61 / app/iavlv2");
    Ok(())
}

async fn swap_image(chain: &mut CosmosChain, version: &str) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    println!("\n--- Stopping nodes for upgrade ---");
    chain.stop_all_nodes().await?;
    let repo = upgrade_repo();
    println!("\n--- Upgrading image to {repo}:{version} ---");
    chain.upgrade_version(&repo, version);
    println!("\n--- Starting upgraded nodes ---");
    chain.start_all_nodes().await?;
    Ok(())
}

/// `target` set: return once height reaches it or RPC dies.
/// `target` unset: return once height stops advancing or RPC dies (armed next plan).
async fn wait_until_halt(
    chain: &CosmosChain,
    target: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut last = chain.height().await.unwrap_or(0);
    let mut frozen = 0u32;
    for _ in 0..180 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        match chain.height().await {
            Ok(h) if target.is_some_and(|want| h >= want) => {
                println!("Chain reached halt height: {h}");
                return Ok(());
            }
            Ok(h) if h == last => {
                frozen += 1;
                if target.is_none() && frozen >= 20 {
                    println!("Chain height frozen at {h}");
                    return Ok(());
                }
            }
            Ok(h) => {
                last = h;
                frozen = 0;
            }
            Err(_) => {
                println!("Chain halted (RPC error — expected at halt height).");
                return Ok(());
            }
        }
    }
    Err(format!("timed out waiting for halt (last height {last})").into())
}

async fn run_test(chain: &mut CosmosChain, num_validators: usize) -> Result<(), Box<dyn std::error::Error>> {
    let plan = upgrade_name();

    println!("\n--- Initializing chain ---");
    let ctx = TestContext {
        test_name: "cosmos-upgrade-v6".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started and producing blocks.");

    println!("\n--- Funding test user ---");
    chain.create_key("testuser").await?;
    let user_addr = chain.primary_node()?.get_key_address("testuser").await?;
    chain
        .send_funds(
            "validator",
            &WalletAmount {
                address: user_addr.clone(),
                denom: "uterp".to_string(),
                amount: 10_000_000_000,
            },
        )
        .await?;
    println!("Funded user: {user_addr}");

    let pre_ibc = run_workflows(chain, UpgradePhase::Pre, None, "testuser").await?;

    let current_height = chain.height().await?;
    let halt_height = current_height + HALT_HEIGHT_DELTA;
    println!("\n--- Submitting software upgrade `{plan}` ---");
    println!("Current height: {current_height}");
    println!("Halt height:    {halt_height}");

    let proposal_id = chain
        .submit_software_upgrade_proposal("testuser", &plan, halt_height, "500000000uterp")
        .await?;
    println!("Proposal submitted: ID={proposal_id}");

    println!("\n--- Voting on proposal ---");
    chain.vote_on_proposal_all_validators(proposal_id, "yes").await?;
    println!("All {num_validators} validators voted yes.");

    println!("\n--- Waiting for proposal to pass ---");
    chain.poll_for_proposal_status(proposal_id, status::PASSED, 90).await?;
    println!("Proposal PASSED.");

    println!("\n--- Waiting for chain halt at height {halt_height} ---");
    wait_until_halt(chain, Some(halt_height)).await?;

    let hop = first_hop_version();
    let done = final_version();
    swap_image(chain, &hop).await?;

    if hop != done {
        println!("\n--- Waiting for the armed next plan to halt before {done} ---");
        wait_until_halt(chain, None).await?;
        swap_image(chain, &done).await?;
    }

    println!("\n--- Verifying post-upgrade block production ---");
    let post_upgrade_height = chain.height().await?;
    println!("Post-upgrade height: {post_upgrade_height}");

    wait_for_blocks(chain, BLOCKS_AFTER_UPGRADE).await?;
    let final_height = chain.height().await?;
    println!(
        "Chain produced {} blocks after upgrade (height: {final_height})",
        final_height - post_upgrade_height
    );
    if final_height < post_upgrade_height + BLOCKS_AFTER_UPGRADE {
        return Err("chain did not produce enough blocks after upgrade".into());
    }

    run_workflows(chain, UpgradePhase::Post, pre_ibc.as_ref(), "testuser").await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let from = start_version();
    let hop = first_hop_version();
    let done = final_version();
    let plan = upgrade_name();
    if from == hop {
        panic!("TERP_UPGRADE_FROM={from} already is the image that handles plan {plan}");
    }
    println!("=== Terp chain upgrade ({from} → {hop} → {done}, plan `{plan}`) ===\n");
    println!("sdk 0.54 + ibc-go v11.1 + 08-wasm v11.1.0 + zk-wasmvm\n");

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    println!("Docker runtime connected.");

    let config = terp_upgrade_config(&from);
    let num_validators = 2;
    let num_full_nodes = 0;
    let mut chain = CosmosChain::new(config, num_validators, num_full_nodes, runtime);

    println!(
        "Chain: {} (from {}:{}, hop {}:{}, final {}:{})",
        chain.chain_id(),
        upgrade_repo(),
        from,
        upgrade_repo(),
        hop,
        upgrade_repo(),
        done
    );
    println!("Validators: {num_validators}");

    let result = run_test(&mut chain, num_validators).await;

    println!("\n--- Shutdown ---");
    if let Err(e) = chain.stop().await {
        eprintln!("Warning: cleanup error: {e}");
    }

    match result {
        Ok(()) => {
            println!("Chain upgrade test PASSED ({from} → {hop} → {done}, plan `{plan}`)");
            Ok(())
        }
        Err(e) => {
            eprintln!("Test FAILED: {e}");
            Err(e)
        }
    }
}
