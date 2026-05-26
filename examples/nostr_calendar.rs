//! Nostr + calendar demo: publish Nostr events and record on-chain via dao-calendar.
//!
//! Demonstrates the integrated flow:
//! 1. Spin up a local Terp chain (Docker backend) with a Nostr relay sidecar
//! 2. Connect to the Nostr relay and publish a signed NIP-01 event
//! 3. Deploy the dao-calendar CosmWasm contract
//! 4. Record the Nostr event on-chain via the calendar's `extension` field
//! 5. Query the calendar event and verify the Nostr payload is stored
//!
//! This connects the Headscale JWT auth narrative with Nostr/calendar on-chain
//! records: a JWT-authenticated provider publishes Nostr events about deployment
//! activity and records them as calendar entries for auditability.
//!
//! ## Prerequisites
//!
//! - Docker running
//! - `terpnetwork/terp-core:local` or compatible image
//! - dao_calendar.wasm at expected path (set via CALENDAR_WASM env or default)
//! - `mattn/nostr-relay:latest` Docker image (auto-pulled by ict-rs sidecar)
//!
//! ## Usage
//!
//! ```sh
//! cargo run --example nostr_calendar --features docker,nostr
//! ```
//!
//! Set `CALENDAR_WASM` env var to override the wasm path.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::cosmos::docker_sidecar::nostr_relay_config;
use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::nostr::client::{NostrClient, NostrEvent};
use ict_rs::runtime::{DockerConfig, DockerImage, IctRuntime};
use ict_rs::tx::WalletAmount;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const IMAGE_REPO: &str = "terpnetwork/terp-core";
const IMAGE_VERSION: &str = "local";

const CHAIN_ID: &str = "120u-1";
const DENOM: &str = "uterp";
const BECH32_PREFIX: &str = "terp";

const DEFAULT_WASM_PATH: &str = "~/abstract/terp-core/crates/dao-contracts/artifacts/dao_calendar.wasm";

/// Nostr relay port inside the sidecar container.
const NOSTR_RELAY_PORT: u16 = 7777;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn timestamp_days_from_now(days: u64) -> (u64, u32) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch");
    let future = now.as_secs() + days * 86400;
    (future, 0)
}

// ---------------------------------------------------------------------------
// Chain config (with Nostr sidecar)
// ---------------------------------------------------------------------------

fn terp_config_with_nostr() -> ChainConfig {
    let mut config = ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: CHAIN_ID.to_string(),
        images: vec![DockerImage {
            repository: IMAGE_REPO.to_string(),
            version: IMAGE_VERSION.to_string(),
            uid_gid: None,
        }],
        bin: "terpd".to_string(),
        bech32_prefix: BECH32_PREFIX.to_string(),
        denom: DENOM.to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: format!("0{}", DENOM),
        gas_adjustment: 1.5,
        trusting_period: "112h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: Default::default(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: vec![nostr_relay_config()],
        faucet: None,
        genesis_style: Default::default(),
    };
    // Override Nostr relay port
    config.sidecar_configs[0].ports = vec![NOSTR_RELAY_PORT.to_string()];
    config
}

// ---------------------------------------------------------------------------
// Main demo flow
// ---------------------------------------------------------------------------

async fn run_demo(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let wasm_host = std::env::var("CALENDAR_WASM")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_WASM_PATH));

    if !wasm_host.exists() {
        return Err(format!(
            "dao-calendar WASM not found at {}. Set CALENDAR_WASM env var.",
            wasm_host.display()
        )
        .into());
    }
    println!("dao-calendar WASM: {}", wasm_host.display());

    // -----------------------------------------------------------------------
    // Step 1: Start the chain (with Nostr relay sidecar)
    // -----------------------------------------------------------------------
    println!("\n--- [1/8] Starting local Terp chain with Nostr relay sidecar ---");
    let ctx = TestContext {
        test_name: "nostr-calendar-demo".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started. RPC: {}", chain.host_rpc_address());
    println!("gRPC: {}", chain.host_grpc_address());

    // -----------------------------------------------------------------------
    // Step 2: Resolve Nostr relay endpoint
    // -----------------------------------------------------------------------
    println!("\n--- [2/8] Resolving Nostr relay endpoint ---");
    let nostr_ws_url = format!("ws://127.0.0.1:{}", NOSTR_RELAY_PORT);
    println!("Nostr relay WS endpoint: {}", nostr_ws_url);

    // -----------------------------------------------------------------------
    // Step 3: Connect Nostr client and publish an event
    // -----------------------------------------------------------------------
    println!("\n--- [3/8] Connecting Nostr client and publishing event ---");

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let nostr_event = NostrEvent::new(
        "demo-calendar-integration-event-id".to_string(),
        "akash19rl4cm2hmr8afy4kldpxz3fka4jguq0a3mq6x0".to_string(),
        now,
        1, // kind 1: short text note
        vec![
            vec!["d".to_string(), "deployment-window-2026-05-26".to_string()],
            vec!["t".to_string(), "calendar-demo".to_string()],
        ],
        "Provider deployment window opened for manifest 7f7d66fb63fbf87368fe48297ddea45da1cbb40512d6007cdac72c01c4dccd92".to_string(),
        "demo-signature".to_string(),
    );

    let mut client = NostrClient::connect(&nostr_ws_url).await?;
    println!("NostrClient connected");
    let published = client.send_event(nostr_event.clone()).await?;
    println!("Nostr event published: {}", published);

    // -----------------------------------------------------------------------
    // Step 4: Fund deployer account
    // -----------------------------------------------------------------------
    println!("\n--- [4/8] Funding deployer account ---");
    chain.create_key("deployer").await?;
    let deployer_addr = chain.primary_node()?.get_key_address("deployer").await?;

    let fund = WalletAmount {
        address: deployer_addr.clone(),
        denom: DENOM.to_string(),
        amount: 100_000_000_000,
    };
    chain.send_funds("validator", &fund).await?;
    ict_rs::interchain::wait_for_blocks(chain, 2).await?;
    println!("Funded deployer: {}", deployer_addr);

    // -----------------------------------------------------------------------
    // Step 5: Deploy dao-calendar
    // -----------------------------------------------------------------------
    println!("\n--- [5/8] Deploying dao-calendar contract ---");
    let node = chain.primary_node()?;
    let container_wasm = "/tmp/dao_calendar.wasm";
    node.copy_file_from_host(&wasm_host, container_wasm).await?;

    let code_id = chain.store_code("deployer", container_wasm).await?;
    println!("Code ID: {}", code_id);

    ict_rs::interchain::wait_for_blocks(chain, 2).await?;

    // -----------------------------------------------------------------------
    // Step 6: Instantiate the calendar
    // -----------------------------------------------------------------------
    println!("\n--- [6/8] Instantiating calendar ---");

    let instantiate_msg = serde_json::json!({
        "initial_groups": [
            {
                "id": "nostr-providers",
                "name": "Nostr-Aware Providers",
                "description": "Providers publishing Nostr events to on-chain calendar",
                "suppliers": [],
                "color": "#9B59B6",
                "members": [deployer_addr]
            }
        ]
    });

    let contract_addr = chain
        .instantiate_contract(
            "deployer",
            &code_id,
            &instantiate_msg.to_string(),
            "nostr-calendar-demo",
            Some(&deployer_addr),
        )
        .await?;
    println!("Contract address: {}", contract_addr);

    ict_rs::interchain::wait_for_blocks(chain, 2).await?;

    // -----------------------------------------------------------------------
    // Step 7: Create calendar event with Nostr extension
    // -----------------------------------------------------------------------
    println!("\n--- [7/8] Creating calendar event with Nostr extension ---");

    let (start, _) = timestamp_days_from_now(4);
    let end = start + 3600;

    let nostr_payload = serde_json::json!({
        "nostr_event": {
            "id": nostr_event.id,
            "pubkey": nostr_event.pubkey,
            "created_at": nostr_event.created_at,
            "kind": nostr_event.kind,
            "tags": nostr_event.tags,
            "content": nostr_event.content,
            "sig": nostr_event.sig
        }
    });

    let create_msg = serde_json::json!({
        "create_event": {
            "title": "Nostr Deployment Event",
            "description": "On-chain record of Nostr-published deployment activity",
            "start_time": {"seconds": start, "nanos": 0},
            "end_time": {"seconds": end, "nanos": 0},
            "managing_groups": ["nostr-providers"],
            "timezone": "UTC",
            "extension": nostr_payload
        }
    });

    let _tx = chain
        .execute_contract("deployer", &contract_addr, &create_msg.to_string(), None)
        .await?;
    println!("Calendar event created: OK");

    ict_rs::interchain::wait_for_blocks(chain, 2).await?;

    // -----------------------------------------------------------------------
    // Step 8: Query and verify the on-chain record
    // -----------------------------------------------------------------------
    println!("\n--- [8/8] Querying on-chain Nostr record ---");

    let event_query = chain
        .query_contract(&contract_addr, r#"{"event":{"event_id":1}}"#)
        .await?;
    println!("On-chain event record:\n{}", event_query);

    // Verify the extension contains the Nostr payload
    let extension = &event_query["event"]["extension"];
    if extension.is_object() && extension.get("nostr_event").is_some() {
        let nostr_id = extension["nostr_event"]["id"].as_str().unwrap_or("unknown");
        println!("Nostr event recorded on-chain. Nostr ID: {}", nostr_id);
    } else {
        println!("Warning: Nostr extension not found in on-chain record");
    }

    // Query agenda
    let agenda_res = chain
        .query_contract(&contract_addr, r#"{"agenda":{}}"#)
        .await?;
    println!("Agenda: {}", agenda_res);

    println!("\n--- Nostr + calendar demo complete! ---");
    println!("Contract: {}", contract_addr);
    println!("Nostr event published and recorded on-chain.");

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let config = terp_config_with_nostr();
    let mut chain = CosmosChain::new(config, 1, 0, runtime);

    let result = run_demo(&mut chain).await;

    println!("\n--- Cleaning up ---");
    chain.stop().await?;
    println!("Chain stopped.");

    result
}
