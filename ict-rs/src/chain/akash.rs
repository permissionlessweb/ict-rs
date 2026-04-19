//! Akash Network chain support for ict-rs.
//!
//! Provides Akash-specific genesis modifications and a convenience
//! [`spawn_akash_chain`] helper that returns a fully configured local
//! Akash node ready for deployment testing.
//!
//! Feature-gated by `akash`.

use std::collections::HashMap;

use serde_json::json;

use crate::chain::{Chain, ChainConfig, ChainType, GenesisStyle, SigningAlgorithm};
use crate::error::Result;
use crate::runtime::DockerImage;
#[cfg(feature = "testing")]
use crate::testing::TestChain;
#[cfg(feature = "testing")]
use tracing::info;

/// Modify raw Akash genesis JSON for local testing.
///
/// Sets Akash-specific module params: staking/mint denoms, fast governance,
/// deployment module defaults, and market/provider params.
pub fn modify_akash_genesis(_cfg: &ChainConfig, raw: Vec<u8>) -> Result<Vec<u8>> {
    let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| crate::error::IctError::Config(format!("parse genesis: {}", e)))?;

    // ── Staking ──────────────────────────────────────────────────────────
    if let Some(params) = genesis.pointer_mut("/app_state/staking/params") {
        params["bond_denom"] = json!("uakt");
        // Fast unbonding for tests.
        params["unbonding_time"] = json!("120s");
    }

    // ── Mint ─────────────────────────────────────────────────────────────
    if let Some(params) = genesis.pointer_mut("/app_state/mint/params") {
        params["mint_denom"] = json!("uakt");
    }

    // ── Governance (fast voting for tests) ───────────────────────────────
    if let Some(params) = genesis.pointer_mut("/app_state/gov/params") {
        params["min_deposit"] = json!([{"denom": "uakt", "amount": "10000000"}]);
        params["voting_period"] = json!("90s");
        params["max_deposit_period"] = json!("90s");
    }
    // v1beta1 fallback
    if let Some(dp) = genesis.pointer_mut("/app_state/gov/deposit_params") {
        dp["min_deposit"] = json!([{"denom": "uakt", "amount": "10000000"}]);
        dp["max_deposit_period"] = json!("90s");
    }
    if let Some(vp) = genesis.pointer_mut("/app_state/gov/voting_params") {
        vp["voting_period"] = json!("90s");
    }

    // ── Crisis ────────────────────────────────────────────────────────────
    if let Some(fee) = genesis.pointer_mut("/app_state/crisis/constant_fee") {
        fee["denom"] = json!("uakt");
    }

    // ── Akash deployment module ──────────────────────────────────────────
    // BME upgrade: deployments now accept both uact and uakt deposits.
    if let Some(params) = genesis.pointer_mut("/app_state/deployment/params") {
        params["min_deposits"] = json!([
            {"denom": "uact", "amount": "5000000"},
            {"denom": "uakt", "amount": "5000000"}
        ]);
    }

    // ── Akash market module ──────────────────────────────────────────────
    // BME: bid deposits are denominated in uact (matches order pricing).
    if let Some(params) = genesis.pointer_mut("/app_state/market/params") {
        params["order_max_bids"] = json!(20);
        params["bid_min_deposit"] = json!({"denom": "uact", "amount": "5000000"});
    }

    // ── BME module ───────────────────────────────────────────────────────
    // Only override params that differ from defaults for testing.
    // The default genesis from `akash init` already has correct BME structure;
    // adding unknown fields or wrong types causes InitGenesis panics.
    // min_epoch_blocks = 2 → ACT mints process every ~10s for fast tests.
    if let Some(bme) = genesis.pointer_mut("/app_state/bme") {
        if let Some(params) = bme.pointer_mut("/params") {
            // Very fast epochs for testing (must remain string type)
            params["min_epoch_blocks"] = json!("2");
        }
    }

    // ── Find validator address (used by oracle + bank pre-funding) ──────
    // modify_genesis runs AFTER collect-gentxs, so auth/accounts is populated.
    let validator_addr = genesis
        .pointer("/app_state/auth/accounts")
        .and_then(|a| a.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|acct| {
                acct.pointer("/address")
                    .and_then(|v| v.as_str())
                    .filter(|a| a.starts_with("akash1"))
                    .map(|a| a.to_string())
            })
        });

    // ── Oracle module ──────────────────────────────────────────────────────
    // Register the validator as an oracle source so it can feed prices.
    // Effectively disable staleness — we test the deployment workflow, not
    // the oracle module.
    if let Some(ref addr) = validator_addr {
        if let Some(params) = genesis.pointer_mut("/app_state/oracle/params") {
            params["sources"] = json!([addr]);
            // gogoproto.stdduration → "{seconds}s"
            params["max_price_staleness_period"] = json!("999999999s");
        }
    }

    let output = serde_json::to_vec_pretty(&genesis)
        .map_err(|e| crate::error::IctError::Config(format!("serialize genesis: {}", e)))?;
    Ok(output)
}

/// Inject a pre-funded account directly into genesis JSON (bank balances + auth accounts).
///
/// Used for accounts that need uact at genesis because BME's SendRestrictionFn
/// prevents runtime bank transfers of uact.
fn inject_genesis_account(genesis: &mut serde_json::Value, address: &str, uact: u64, uakt: u64) {
    // ── bank/balances ──────────────────────────────────────────────────
    let mut coins = Vec::new();
    if uact > 0 {
        coins.push(json!({"denom": "uact", "amount": uact.to_string()}));
    }
    if uakt > 0 {
        coins.push(json!({"denom": "uakt", "amount": uakt.to_string()}));
    }
    if coins.is_empty() {
        return;
    }

    let balance_entry = json!({
        "address": address,
        "coins": coins,
    });

    if let Some(balances) = genesis.pointer_mut("/app_state/bank/balances") {
        if let Some(arr) = balances.as_array_mut() {
            arr.push(balance_entry);
        }
    }

    // ── bank/supply ────────────────────────────────────────────────────
    // Update total supply to include the new coins.
    if let Some(supply) = genesis.pointer_mut("/app_state/bank/supply") {
        if let Some(arr) = supply.as_array_mut() {
            for (denom, amount) in [("uact", uact), ("uakt", uakt)] {
                if amount == 0 {
                    continue;
                }
                if let Some(entry) = arr.iter_mut().find(|c| c["denom"] == denom) {
                    let current: u64 = entry["amount"]
                        .as_str()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    entry["amount"] = json!((current + amount).to_string());
                } else {
                    arr.push(json!({"denom": denom, "amount": amount.to_string()}));
                }
            }
        }
    }

    // ── auth/accounts ──────────────────────────────────────────────────
    // Register the address as a BaseAccount so the chain accepts it.
    let account_entry = json!({
        "@type": "/cosmos.auth.v1beta1.BaseAccount",
        "address": address,
        "pub_key": null,
        "account_number": "0",
        "sequence": "0",
    });
    if let Some(accounts) = genesis.pointer_mut("/app_state/auth/accounts") {
        if let Some(arr) = accounts.as_array_mut() {
            arr.push(account_entry);
        }
    }

    tracing::info!(address, uact, uakt, "injected genesis account");
}

/// Build a full Akash [`ChainConfig`] with genesis modifier and fast block times.
pub fn akash_chain_config() -> ChainConfig {
    let mut config_overrides = HashMap::new();
    // Enable API + gRPC in app.toml.
    config_overrides.insert(
        "config/app.toml".into(),
        json!({
            "api": { "enable": true, "address": "tcp://0.0.0.0:1317" },
            "grpc": { "enable": true, "address": "0.0.0.0:9090" },
        }),
    );
    // Fast blocks.
    config_overrides.insert(
        "config/config.toml".into(),
        json!({
            "consensus": {
                "timeout_propose": "200ms",
                "timeout_propose_delta": "200ms",
                "timeout_prevote": "200ms",
                "timeout_prevote_delta": "200ms",
                "timeout_precommit": "200ms",
                "timeout_precommit_delta": "200ms",
                "timeout_commit": "200ms",
            }
        }),
    );

    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "akash".to_string(),
        chain_id: "akash-local-1".to_string(),
        images: vec![DockerImage {
            repository: "ghcr.io/akash-network/node".to_string(),
            version: "latest".to_string(),
            uid_gid: None,
        }],
        bin: "akash".to_string(),
        bech32_prefix: "akash".to_string(),
        denom: "uakt".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.025uakt".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "336h".to_string(),
        block_time: "200ms".to_string(),
        genesis: None,
        modify_genesis: Some(Box::new(modify_akash_genesis)),
        pre_genesis: None,
        config_file_overrides: config_overrides,
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: GenesisStyle::Modern,
    }
}

/// A spawned Akash chain with host-accessible endpoints.
#[cfg(feature = "testing")]
pub struct SpawnedAkashChain {
    pub tc: TestChain,
    pub rpc: String,
    pub grpc: String,
    pub rest: String,
    pub chain_id: String,
    pub faucet_mnemonic: String,
}

/// Well-known test mnemonic for faucet/deployer accounts.
#[cfg(feature = "testing")]
pub const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

/// Spawn a single-validator Akash chain for testing.
///
/// Uses a default test name. For parallel tests, use [`spawn_akash_chain_named`]
/// with unique names to avoid Docker container collisions.
#[cfg(feature = "testing")]
pub async fn spawn_akash_chain() -> Result<SpawnedAkashChain> {
    spawn_akash_chain_named("akash-local", TEST_MNEMONIC).await
}

/// Spawn a single-validator Akash chain with a caller-chosen `test_name`.
///
/// Each invocation must use a **unique** `test_name` to avoid Docker container
/// name collisions (container name = `ict-{test_name}-{chain_id}-val-0`).
///
/// The faucet key is recovered from `faucet_mnemonic` during genesis and
/// pre-funded with both uakt and uact so it can serve as a faucet for
/// deployment tests (uact is the BME payment token and cannot be
/// bank-sent at runtime).
#[cfg(feature = "testing")]
pub async fn spawn_akash_chain_named(
    test_name: &str,
    faucet_mnemonic: &str,
) -> Result<SpawnedAkashChain> {
    spawn_akash_chain_with_accounts(test_name, faucet_mnemonic, &[]).await
}

/// Like [`spawn_akash_chain_named`] but also pre-funds additional accounts at
/// genesis with uact + uakt.  Use this to give the test-provider its own uact
/// balance (BME SendRestrictionFn prevents runtime bank transfers of uact).
///
/// Each entry is `(bech32_address, uact_amount, uakt_amount)`.
#[cfg(feature = "testing")]
pub async fn spawn_akash_chain_with_accounts(
    test_name: &str,
    faucet_mnemonic: &str,
    extra_accounts: &[(&str, u64, u64)],
) -> Result<SpawnedAkashChain> {
    use crate::chain::FaucetConfig;
    use crate::testing::{TestChain, TestChainConfig};

    let mut config = akash_chain_config();
    let chain_id = config.chain_id.clone();

    // Register the faucet as a genesis account with both uakt and uact.
    // uact cannot be bank-sent at runtime (BME SendRestrictionFn), so it
    // must be present in the account from genesis block 0.
    config.faucet = Some(FaucetConfig {
        key_name: "faucet".to_string(),
        mnemonic: Some(faucet_mnemonic.to_string()),
        coins: Some("10000000000uact,100000000000uakt".to_string()),
        port: 5000,       // not used (no faucet server), but must be valid for Docker
        start_cmd: Vec::new(), // empty = don't start a faucet server
        env: Vec::new(),
    });

    // Extra genesis accounts (e.g. test-provider) that need uact pre-funded.
    // Inject via modify_genesis since add_genesis_account requires key names
    // and these are raw addresses.
    let extra_accounts_owned: Vec<(String, u64, u64)> = extra_accounts
        .iter()
        .map(|(addr, uact, uakt)| (addr.to_string(), *uact, *uakt))
        .collect();

    let prev_modify = config.modify_genesis.take();
    config.modify_genesis = Some(Box::new(move |cfg: &ChainConfig, raw: Vec<u8>| {
        // Run the existing modify_genesis first (e.g. modify_akash_genesis).
        let raw = if let Some(ref f) = prev_modify {
            f(cfg, raw)?
        } else {
            raw
        };
        // Parse, inject extra accounts, re-serialize.
        let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
            .map_err(|e| crate::error::IctError::Config(format!("parse genesis: {}", e)))?;
        for (addr, uact, uakt) in &extra_accounts_owned {
            inject_genesis_account(&mut genesis, addr, *uact, *uakt);
        }
        serde_json::to_vec_pretty(&genesis)
            .map_err(|e| crate::error::IctError::Config(format!("serialize genesis: {}", e)))
    }));

    let tc = TestChain::setup(
        test_name,
        TestChainConfig {
            chain_config: config,
            num_validators: 1,
            num_full_nodes: 0,
            genesis_wallets: Vec::new(),
        },
    )
    .await?;

    // The faucet key was recovered from the mnemonic during the genesis
    // pipeline — just read its address for logging.
    let primary = tc.chain.primary_node()?;
    let faucet_address = primary.get_key_address("faucet").await?;

    // Start continuous oracle price feeder — the market module needs
    // oracle prices for bid matching during the deployment workflow.
    info!("Starting oracle price feeder for market module...");
    super::akash_oracle::start_oracle_price_feeder(&tc.chain).await?;

    let rpc = tc.chain.host_rpc_address();
    let grpc = tc.chain.host_grpc_address();
    let rest = primary
        .host_api_port
        .map(|p| format!("http://localhost:{p}"))
        .unwrap_or_else(|| "http://localhost:1317".to_string());

    info!(
        chain_id = %chain_id,
        test_name = %test_name,
        rpc = %rpc,
        grpc = %grpc,
        rest = %rest,
        faucet_address = %faucet_address,
        "Akash chain spawned"
    );

    Ok(SpawnedAkashChain {
        tc,
        rpc,
        grpc,
        rest,
        chain_id,
        faucet_mnemonic: faucet_mnemonic.to_string(),
    })
}
