//! Comprehensive integration tests for ict-rs.
//!
//! These tests exercise end-to-end workflows using mock backends so they run
//! without Docker or any external infrastructure.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;

use ict_rs::auth::{generate_mnemonic, Authenticator, KeyringAuthenticator};
use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::error::{IctError, Result};
use ict_rs::genesis::{get_genesis_module_value, set_genesis_module_value};
use ict_rs::ibc::{ChannelOptions, ChannelOutput, ClientOptions, ConnectionOutput};
use ict_rs::interchain::{Interchain, InterchainBuildOptions, InterchainLink};
use ict_rs::node::ChainNode;
use ict_rs::relayer::Relayer;
use ict_rs::reporter::{ExecReport, TestReporter};
use ict_rs::runtime::*;
use ict_rs::spec::{builtin_chain_config, ChainSpec};
use ict_rs::tx::{ExecOutput, TransferOptions, WalletAmount};
use ict_rs::wallet::{KeyWallet, Wallet};

// ---------------------------------------------------------------------------
// MockRuntime
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct MockState {
    containers: HashMap<String, String>, // id -> status
    next_id: u64,
    networks: HashMap<String, String>, // id -> name
    exec_log: Vec<Vec<String>>,        // recorded commands
}

impl Default for MockState {
    fn default() -> Self {
        Self {
            containers: HashMap::new(),
            next_id: 1,
            networks: HashMap::new(),
            exec_log: Vec::new(),
        }
    }
}

struct MockRuntime {
    state: Arc<Mutex<MockState>>,
    /// When set, exec_in_container returns this error instead of a normal response.
    force_error: Arc<Mutex<Option<String>>>,
}

impl MockRuntime {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState::default())),
            force_error: Arc::new(Mutex::new(None)),
        }
    }

    fn new_with_error(msg: &str) -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState::default())),
            force_error: Arc::new(Mutex::new(Some(msg.to_string()))),
        }
    }

    fn container_count(&self) -> usize {
        self.state.lock().unwrap().containers.len()
    }

    fn network_count(&self) -> usize {
        self.state.lock().unwrap().networks.len()
    }

    fn exec_log(&self) -> Vec<Vec<String>> {
        self.state.lock().unwrap().exec_log.clone()
    }
}

/// Determine a mock response based on the command arguments.
///
/// The first element is typically the chain binary (gaiad, terpd, etc.) or a
/// shell utility (cat, sh, sed). We match on subcommand parts to return
/// realistic JSON that the CosmosChain / ChainNode parsing code expects.
fn mock_exec_response(cmd: &[&str]) -> ExecOutput {
    // Flatten into owned strings for easier matching.
    let parts: Vec<&str> = cmd.iter().copied().collect();

    // Helper: find the index of a subcommand ignoring the binary name.
    let has = |needle: &str| parts.iter().any(|p| *p == needle);

    // ---- shell utilities ----
    if parts.first() == Some(&"cat") {
        // Reading genesis.json or any file — return minimal valid genesis JSON.
        return ExecOutput {
            stdout: br#"{"app_state":{}}"#.to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }
    if parts.first() == Some(&"sh") {
        // Shell commands (sed, echo, piped key creation, etc.) — parse inner command.
        let joined = parts.join(" ");
        if joined.contains("keys add") && joined.contains("--recover") {
            return ExecOutput {
                stdout: br#"{"name":"recovered","address":"cosmos1mockrecovered000000000000000000"}"#.to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
            };
        }
        if joined.contains("keys add") {
            // Extract key name: find "keys add <name>" in the sh -c string.
            let key_name = joined
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(3)
                .find(|w| w[0] == "keys" && w[1] == "add")
                .and_then(|w| Some(w[2]))
                .unwrap_or("test");
            let json = format!(
                r#"{{"name":"{}","address":"cosmos1mock{}000000000000000000000000"}}"#,
                key_name, key_name
            );
            return ExecOutput {
                stdout: json.into_bytes(),
                stderr: Vec::new(),
                exit_code: 0,
            };
        }
        return ExecOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }
    if parts.first() == Some(&"sed") {
        return ExecOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    // ---- chain CLI subcommands ----
    // The first element is the chain binary; subcommands start at index 1.

    if has("init") {
        return ExecOutput {
            stdout: b"{}".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("keys") && has("add") {
        // `<bin> keys add <name> ...`
        let key_name = parts
            .iter()
            .position(|p| *p == "add")
            .and_then(|i| parts.get(i + 1))
            .unwrap_or(&"test");
        let json = format!(
            r#"{{"name":"{}","address":"cosmos1mock{}000000000000000000000000"}}"#,
            key_name, key_name
        );
        return ExecOutput {
            stdout: json.into_bytes(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("keys") && has("show") {
        // `<bin> keys show <name> ... -a` — return just the address.
        let key_name = parts
            .iter()
            .position(|p| *p == "show")
            .and_then(|i| parts.get(i + 1))
            .unwrap_or(&"test");
        let addr = format!("cosmos1mock{}000000000000000000000000", key_name);
        return ExecOutput {
            stdout: addr.into_bytes(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("status") {
        return ExecOutput {
            stdout: br#"{"sync_info":{"latest_block_height":"100"}}"#.to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    // SDK v0.47+: `query bank balance <addr> <denom>`
    if has("query") && has("bank") && has("balance") && !has("balances") {
        return ExecOutput {
            stdout: br#"{"balance":{"amount":"1000000","denom":"uatom"}}"#.to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    // Older SDK: `query bank balances <addr> --denom <denom>`
    if has("query") && has("bank") && has("balances") {
        return ExecOutput {
            stdout: br#"{"balance":{"amount":"1000000","denom":"uatom"}}"#.to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("tx") && has("bank") && has("send") {
        return ExecOutput {
            stdout: br#"{"txhash":"AABBCCDD1234567890","height":"101","gas_used":"50000"}"#
                .to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("tx") && has("ibc-transfer") {
        return ExecOutput {
            stdout: br#"{"txhash":"IBCTX0001","height":"102","gas_used":"80000"}"#.to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("genesis") && has("add-genesis-account") {
        return ExecOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("genesis") && has("gentx") {
        return ExecOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("genesis") && has("collect-gentxs") {
        return ExecOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("comet") && has("show-node-id") {
        return ExecOutput {
            stdout: b"abcdef1234567890abcdef1234567890abcdef12".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("export") {
        return ExecOutput {
            stdout: br#"{"app_state":{"bank":{}}}"#.to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    if has("query") && has("ibc") {
        return ExecOutput {
            stdout: br#"{"acknowledgements":[]}"#.to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
    }

    // Default fallback.
    ExecOutput {
        stdout: b"{}".to_vec(),
        stderr: Vec::new(),
        exit_code: 0,
    }
}

#[async_trait]
impl RuntimeBackend for MockRuntime {
    async fn pull_image(&self, _image: &DockerImage) -> Result<()> {
        Ok(())
    }

    async fn create_container(&self, opts: &ContainerOptions) -> Result<ContainerId> {
        let mut state = self.state.lock().unwrap();
        let id = format!("mock-container-{}", state.next_id);
        state.next_id += 1;
        state.containers.insert(id.clone(), "created".to_string());
        Ok(ContainerId(id))
    }

    async fn start_container(&self, id: &ContainerId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(status) = state.containers.get_mut(&id.0) {
            *status = "running".to_string();
        }
        Ok(())
    }

    async fn stop_container(&self, id: &ContainerId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if let Some(status) = state.containers.get_mut(&id.0) {
            *status = "stopped".to_string();
        }
        Ok(())
    }

    async fn remove_container(&self, id: &ContainerId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.containers.remove(&id.0);
        Ok(())
    }

    async fn exec_in_container(
        &self,
        _id: &ContainerId,
        cmd: &[&str],
        _env: &[(&str, &str)],
    ) -> Result<ExecOutput> {
        // Check for forced error.
        if let Some(msg) = self.force_error.lock().unwrap().as_ref() {
            return Err(IctError::Runtime(anyhow::anyhow!(msg.clone())));
        }

        // Record the command.
        {
            let mut state = self.state.lock().unwrap();
            state
                .exec_log
                .push(cmd.iter().map(|s| s.to_string()).collect());
        }

        Ok(mock_exec_response(cmd))
    }

    async fn create_network(&self, name: &str) -> Result<NetworkId> {
        let mut state = self.state.lock().unwrap();
        let id = format!("mock-net-{}", state.next_id);
        state.next_id += 1;
        state.networks.insert(id.clone(), name.to_string());
        Ok(NetworkId(id))
    }

    async fn remove_network(&self, id: &NetworkId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.networks.remove(&id.0);
        Ok(())
    }

    async fn container_logs(&self, _id: &ContainerId) -> Result<String> {
        Ok("mock log output".to_string())
    }

    async fn wait_for_container(&self, _id: &ContainerId) -> Result<ExitStatus> {
        Ok(ExitStatus { code: 0 })
    }

    async fn remove_volume(&self, _name: &str) -> Result<()> {
        Ok(())
    }

    async fn exec_in_container_background(
        &self,
        _id: &ContainerId,
        _cmd: &[&str],
        _env: &[(&str, &str)],
    ) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MockRelayer
// ---------------------------------------------------------------------------

struct MockRelayer {
    calls: Arc<Mutex<Vec<String>>>,
}

impl MockRelayer {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn call_log(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, method: &str) {
        self.calls.lock().unwrap().push(method.to_string());
    }
}

#[async_trait]
impl Relayer for MockRelayer {
    async fn add_key(&self, chain_id: &str, key_name: &str) -> Result<Box<dyn Wallet>> {
        self.record(&format!("add_key({chain_id}, {key_name})"));
        Ok(Box::new(KeyWallet {
            key_name: key_name.to_string(),
            address_bytes: vec![0u8; 20],
            bech32_address: format!("cosmos1relayer{key_name}"),
            mnemonic_phrase: String::new(),
        }))
    }

    async fn restore_key(
        &self,
        chain_id: &str,
        key_name: &str,
        _mnemonic: &str,
    ) -> Result<()> {
        self.record(&format!("restore_key({chain_id}, {key_name})"));
        Ok(())
    }

    fn get_wallet(&self, _chain_id: &str) -> Option<&dyn Wallet> {
        None
    }

    async fn add_chain_configuration(
        &self,
        config: &ChainConfig,
        key_name: &str,
        rpc_addr: &str,
        grpc_addr: &str,
    ) -> Result<()> {
        self.record(&format!(
            "add_chain_configuration({}, {key_name}, {rpc_addr}, {grpc_addr})",
            config.chain_id
        ));
        Ok(())
    }

    async fn generate_path(
        &self,
        src_chain_id: &str,
        dst_chain_id: &str,
        path_name: &str,
    ) -> Result<()> {
        self.record(&format!(
            "generate_path({src_chain_id}, {dst_chain_id}, {path_name})"
        ));
        Ok(())
    }

    async fn link_path(&self, path_name: &str, _opts: &ChannelOptions) -> Result<()> {
        self.record(&format!("link_path({path_name})"));
        Ok(())
    }

    async fn create_clients(&self, path_name: &str, _opts: &ClientOptions) -> Result<()> {
        self.record(&format!("create_clients({path_name})"));
        Ok(())
    }

    async fn create_connections(&self, path_name: &str) -> Result<()> {
        self.record(&format!("create_connections({path_name})"));
        Ok(())
    }

    async fn create_channel(&self, path_name: &str, _opts: &ChannelOptions) -> Result<()> {
        self.record(&format!("create_channel({path_name})"));
        Ok(())
    }

    async fn update_clients(&self, path_name: &str) -> Result<()> {
        self.record(&format!("update_clients({path_name})"));
        Ok(())
    }

    async fn start(&self, path_names: &[&str]) -> Result<()> {
        self.record(&format!("start({:?})", path_names));
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.record("stop()");
        Ok(())
    }

    async fn flush(&self, path_name: &str, channel_id: &str) -> Result<()> {
        self.record(&format!("flush({path_name}, {channel_id})"));
        Ok(())
    }

    async fn get_channels(&self, _chain_id: &str) -> Result<Vec<ChannelOutput>> {
        self.record("get_channels()");
        Ok(Vec::new())
    }

    async fn get_connections(&self, _chain_id: &str) -> Result<Vec<ConnectionOutput>> {
        self.record("get_connections()");
        Ok(Vec::new())
    }

    async fn exec(&self, cmd: &[&str], _env: &[(&str, &str)]) -> Result<ExecOutput> {
        self.record(&format!("exec({:?})", cmd));
        Ok(ExecOutput::default())
    }
}

// ---------------------------------------------------------------------------
// Helper: build a minimal terp ChainConfig for testing.
// ---------------------------------------------------------------------------

fn make_terp_config() -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "terp-test-1".to_string(),
        images: vec![DockerImage {
            repository: "ghcr.io/terpnetwork/terp-core".to_string(),
            version: "latest".to_string(),
            uid_gid: None,
        }],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.025uterp".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "336h".to_string(),
        block_time: "2s".to_string(),
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

fn make_gaia_config() -> ChainConfig {
    builtin_chain_config("gaia").unwrap()
}

fn make_osmosis_config() -> ChainConfig {
    builtin_chain_config("osmosis").unwrap()
}

// ===========================================================================
// Test 1: Single chain lifecycle
// ===========================================================================

#[tokio::test]
async fn test_single_chain_lifecycle() {
    let runtime = Arc::new(MockRuntime::new());
    let mut chain = CosmosChain::new(make_terp_config(), 1, 0, runtime.clone());

    // --- Initialize ---
    let ctx = TestContext {
        test_name: "lifecycle_test".to_string(),
        network_id: "ict-lifecycle".to_string(),
    };
    chain.initialize(&ctx).await.unwrap();

    // A network should have been created.
    assert_eq!(runtime.network_count(), 1, "one network should exist after init");
    // One validator container.
    assert!(
        runtime.container_count() >= 1,
        "at least one container should exist after init"
    );

    // --- Start with genesis wallets ---
    let genesis_wallets = vec![WalletAmount {
        address: "terp1user000000000000000000000000000000".to_string(),
        denom: "uterp".to_string(),
        amount: 500_000_000,
    }];
    chain.start(&genesis_wallets).await.unwrap();

    // The exec log should contain init, keys add, genesis add-genesis-account,
    // gentx, collect-gentxs, and peer-configuration commands.
    let log = runtime.exec_log();
    let flat: Vec<String> = log.iter().map(|v| v.join(" ")).collect();

    assert!(
        flat.iter().any(|c| c.contains("init")),
        "init command should have been executed"
    );
    assert!(
        flat.iter().any(|c| c.contains("keys") && c.contains("add")),
        "keys add should have been executed"
    );
    assert!(
        flat.iter().any(|c| c.contains("add-genesis-account")),
        "add-genesis-account should have been executed"
    );
    assert!(
        flat.iter().any(|c| c.contains("gentx")),
        "gentx should have been executed"
    );
    assert!(
        flat.iter().any(|c| c.contains("collect-gentxs")),
        "collect-gentxs should have been executed"
    );

    // --- Query height ---
    let h = chain.height().await.unwrap();
    assert_eq!(h, 100, "mock should return height 100");

    // --- Get balance ---
    let bal = chain
        .get_balance("terp1user000000000000000000000000000000", "uterp")
        .await
        .unwrap();
    assert_eq!(bal, 1_000_000, "mock should return balance 1000000");

    // --- Stop ---
    chain.stop().await.unwrap();

    // After stop, containers should be removed and network cleaned up.
    assert_eq!(
        runtime.container_count(),
        0,
        "all containers should be removed after stop"
    );
    assert_eq!(
        runtime.network_count(),
        0,
        "network should be removed after stop"
    );
}

// ===========================================================================
// Test 2: ChainSpec resolves to working chain
// ===========================================================================

#[tokio::test]
async fn test_chain_spec_to_chain() {
    let spec = ChainSpec {
        name: "gaia".to_string(),
        version: Some("v20.0.0".to_string()),
        num_validators: Some(2),
        num_full_nodes: Some(1),
        chain_id: Some("my-custom-chain".to_string()),
        denom: Some("mycoin".to_string()),
        bech32_prefix: Some("myprefix".to_string()),
        gas_prices: Some("0.1mycoin".to_string()),
    };

    // --- Verify overrides are applied ---
    let cfg = spec.resolve().unwrap();
    assert_eq!(cfg.chain_id, "my-custom-chain");
    assert_eq!(cfg.denom, "mycoin");
    assert_eq!(cfg.bech32_prefix, "myprefix");
    assert_eq!(cfg.gas_prices, "0.1mycoin");
    assert_eq!(cfg.images[0].version, "v20.0.0");
    assert_eq!(cfg.bin, "gaiad", "binary should remain from builtin");
    assert_eq!(cfg.chain_type, ChainType::Cosmos);

    // --- Build chain from spec ---
    let runtime = Arc::new(MockRuntime::new());
    let mut chain = spec.build_cosmos_chain(runtime.clone()).unwrap();

    let ctx = TestContext {
        test_name: "spec_test".to_string(),
        network_id: "ict-spec".to_string(),
    };
    chain.initialize(&ctx).await.unwrap();

    // With 2 validators + 1 full node we expect 3 containers.
    assert!(
        runtime.container_count() >= 3,
        "expected at least 3 containers for 2 validators + 1 full node, got {}",
        runtime.container_count()
    );

    chain.start(&[]).await.unwrap();

    // Chain ID should match our override.
    assert_eq!(chain.chain_id(), "my-custom-chain");

    chain.stop().await.unwrap();
}

// ===========================================================================
// Test 3: Interchain builder validation
// ===========================================================================

#[tokio::test]
async fn test_interchain_builder_validation() {
    let runtime = Arc::new(MockRuntime::new());

    // --- Link referencing nonexistent chain ---
    {
        let gaia = CosmosChain::new(make_gaia_config(), 1, 0, runtime.clone());
        let relayer = MockRelayer::new();
        let mut ic = Interchain::new(runtime.clone())
            .add_chain(Box::new(gaia))
            .add_relayer("hermes", Box::new(relayer))
            .add_link(InterchainLink {
                chain1: "cosmoshub-test-1".to_string(),
                chain2: "nonexistent-chain".to_string(),
                relayer: "hermes".to_string(),
                path: "transfer".to_string(),
            });

        let result = ic
            .build(InterchainBuildOptions {
                test_name: "validate_chain".to_string(),
                ..Default::default()
            })
            .await;

        assert!(result.is_err(), "should fail with nonexistent chain");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("nonexistent-chain"),
            "error should mention the missing chain: {err_msg}"
        );
    }

    // --- Link referencing nonexistent relayer ---
    {
        let gaia = CosmosChain::new(make_gaia_config(), 1, 0, runtime.clone());
        let osmo = CosmosChain::new(make_osmosis_config(), 1, 0, runtime.clone());
        let mut ic = Interchain::new(runtime.clone())
            .add_chain(Box::new(gaia))
            .add_chain(Box::new(osmo))
            .add_link(InterchainLink {
                chain1: "cosmoshub-test-1".to_string(),
                chain2: "osmosis-test-1".to_string(),
                relayer: "missing-relayer".to_string(),
                path: "transfer".to_string(),
            });

        let result = ic
            .build(InterchainBuildOptions {
                test_name: "validate_relayer".to_string(),
                ..Default::default()
            })
            .await;

        assert!(result.is_err(), "should fail with nonexistent relayer");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("missing-relayer"),
            "error should mention the missing relayer: {err_msg}"
        );
    }

    // --- Double build ---
    {
        let gaia = CosmosChain::new(make_gaia_config(), 1, 0, runtime.clone());
        let mut ic = Interchain::new(runtime.clone()).add_chain(Box::new(gaia));

        ic.build(InterchainBuildOptions {
            test_name: "double_build".to_string(),
            ..Default::default()
        })
        .await
        .unwrap();

        let result = ic
            .build(InterchainBuildOptions {
                test_name: "double_build2".to_string(),
                ..Default::default()
            })
            .await;

        assert!(result.is_err(), "double build should fail");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("already built"),
            "error should mention already built: {err_msg}"
        );
    }
}

// ===========================================================================
// Test 4: Multi-chain IBC workflow
// ===========================================================================

#[tokio::test]
async fn test_multichain_ibc_workflow() {
    let runtime = Arc::new(MockRuntime::new());
    let relayer = MockRelayer::new();
    let relayer_calls = relayer.calls.clone();

    let gaia = CosmosChain::new(make_gaia_config(), 1, 0, runtime.clone());
    let osmo = CosmosChain::new(make_osmosis_config(), 1, 0, runtime.clone());

    let mut ic = Interchain::new(runtime.clone())
        .add_chain(Box::new(gaia))
        .add_chain(Box::new(osmo))
        .add_relayer("hermes", Box::new(relayer))
        .add_link(InterchainLink {
            chain1: "cosmoshub-test-1".to_string(),
            chain2: "osmosis-test-1".to_string(),
            relayer: "hermes".to_string(),
            path: "gaia-osmo".to_string(),
        });

    ic.build(InterchainBuildOptions {
        test_name: "ibc_workflow".to_string(),
        ..Default::default()
    })
    .await
    .unwrap();

    assert!(ic.is_built(), "interchain should be marked as built");

    // Verify both chains are accessible.
    let gaia_chain = ic.get_chain("cosmoshub-test-1");
    assert!(gaia_chain.is_some(), "gaia chain should be accessible");
    assert_eq!(gaia_chain.unwrap().chain_id(), "cosmoshub-test-1");

    let osmo_chain = ic.get_chain("osmosis-test-1");
    assert!(osmo_chain.is_some(), "osmosis chain should be accessible");
    assert_eq!(osmo_chain.unwrap().chain_id(), "osmosis-test-1");

    // Verify relayer call sequence.
    let calls = relayer_calls.lock().unwrap().clone();
    let calls_joined = calls.join("; ");

    // add_key should have been called for both chains.
    assert!(
        calls.iter().any(|c| c.contains("add_key(cosmoshub-test-1")),
        "relayer should have add_key for gaia: {calls_joined}"
    );
    assert!(
        calls.iter().any(|c| c.contains("add_key(osmosis-test-1")),
        "relayer should have add_key for osmosis: {calls_joined}"
    );

    // add_chain_configuration should have been called for both.
    assert!(
        calls
            .iter()
            .any(|c| c.contains("add_chain_configuration(cosmoshub-test-1")),
        "relayer should configure gaia: {calls_joined}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.contains("add_chain_configuration(osmosis-test-1")),
        "relayer should configure osmosis: {calls_joined}"
    );

    // generate_path and link_path should be called.
    assert!(
        calls
            .iter()
            .any(|c| c.contains("generate_path(cosmoshub-test-1, osmosis-test-1, gaia-osmo)")),
        "relayer should generate path: {calls_joined}"
    );
    assert!(
        calls.iter().any(|c| c.contains("link_path(gaia-osmo)")),
        "relayer should link path: {calls_joined}"
    );

    // start should be called.
    assert!(
        calls.iter().any(|c| c.contains("start(")),
        "relayer should have been started: {calls_joined}"
    );

    // --- Close ---
    ic.close().await.unwrap();

    let calls_after = relayer_calls.lock().unwrap().clone();
    assert!(
        calls_after.iter().any(|c| c == "stop()"),
        "relayer should have been stopped on close"
    );
    assert!(!ic.is_built(), "interchain should no longer be built after close");
}

// ===========================================================================
// Test 5: Wallet lifecycle
// ===========================================================================

#[tokio::test]
async fn test_wallet_lifecycle() {
    // --- Generate mnemonic ---
    let mnemonic = generate_mnemonic();
    let words: Vec<&str> = mnemonic.split_whitespace().collect();
    assert_eq!(words.len(), 24, "should generate 24-word mnemonic");

    // Ensure it parses successfully.
    bip39::Mnemonic::parse(&mnemonic).expect("mnemonic should be valid BIP39");

    // --- Build KeyWallet from mnemonic ---
    let wallet = KeyWallet::from_mnemonic("test-key", &mnemonic, "cosmos", 118).unwrap();
    assert_eq!(wallet.key_name(), "test-key");
    assert!(
        wallet.formatted_address().starts_with("cosmos1"),
        "address should start with cosmos1, got: {}",
        wallet.formatted_address()
    );
    assert_eq!(wallet.address().len(), 20, "raw address should be 20 bytes");
    assert_eq!(wallet.mnemonic(), mnemonic);

    // --- Create KeyringAuthenticator from same mnemonic ---
    let auth = KeyringAuthenticator::new(&mnemonic, 118).unwrap();
    let auth_addr = auth.bech32_address("cosmos").unwrap();

    // Addresses should match.
    assert_eq!(
        wallet.formatted_address(),
        auth_addr,
        "wallet and authenticator addresses should match"
    );

    // Public key should be 33 bytes (compressed SEC1).
    let pubkey = auth.public_key_bytes();
    assert_eq!(pubkey.len(), 33);

    // --- Sign and verify ---
    let message = b"hello interchain test";
    let sig_bytes = auth.sign(message).await.unwrap();
    assert_eq!(sig_bytes.len(), 64, "ECDSA signature should be 64 bytes");

    // Verify using the verifying key.
    use k256::ecdsa::signature::hazmat::PrehashVerifier;
    use k256::ecdsa::Signature;
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(message);
    let signature = Signature::from_slice(&sig_bytes).unwrap();
    auth.verifying_key()
        .verify_prehash(&digest, &signature)
        .expect("signature should verify");

    // --- Different prefix produces different bech32 but same raw bytes ---
    let terp_wallet = KeyWallet::from_mnemonic("test-key", &mnemonic, "terp", 118).unwrap();
    assert!(
        terp_wallet.formatted_address().starts_with("terp1"),
        "terp wallet should have terp1 prefix"
    );
    assert_eq!(
        wallet.address(),
        terp_wallet.address(),
        "raw address bytes should be identical regardless of prefix"
    );
}

// ===========================================================================
// Test 6: Genesis modification pipeline
// ===========================================================================

#[tokio::test]
async fn test_genesis_modification_pipeline() {
    // Build a genesis JSON structure.
    let mut genesis: serde_json::Value = serde_json::json!({
        "app_state": {
            "staking": {
                "params": {
                    "bond_denom": "uatom",
                    "max_validators": 100,
                    "unbonding_time": "1814400s"
                }
            },
            "bank": {
                "params": {
                    "default_send_enabled": true
                },
                "supply": []
            },
            "gov": {
                "params": {
                    "min_deposit": [{"denom": "uatom", "amount": "10000000"}],
                    "voting_period": "172800s"
                }
            }
        }
    });

    // --- Modify staking bond_denom ---
    set_genesis_module_value(
        &mut genesis,
        &["app_state", "staking", "params", "bond_denom"],
        serde_json::json!("uterp"),
    )
    .unwrap();

    let bond_denom = get_genesis_module_value(
        &genesis,
        &["app_state", "staking", "params", "bond_denom"],
    )
    .unwrap();
    assert_eq!(bond_denom, "uterp");

    // --- Modify nested value: max_validators ---
    set_genesis_module_value(
        &mut genesis,
        &["app_state", "staking", "params", "max_validators"],
        serde_json::json!(50),
    )
    .unwrap();

    let max_vals = get_genesis_module_value(
        &genesis,
        &["app_state", "staking", "params", "max_validators"],
    )
    .unwrap();
    assert_eq!(max_vals, 50);

    // --- Modify governance voting period ---
    set_genesis_module_value(
        &mut genesis,
        &["app_state", "gov", "params", "voting_period"],
        serde_json::json!("60s"),
    )
    .unwrap();

    let voting_period = get_genesis_module_value(
        &genesis,
        &["app_state", "gov", "params", "voting_period"],
    )
    .unwrap();
    assert_eq!(voting_period, "60s");

    // --- Verify original values not clobbered ---
    let send_enabled = get_genesis_module_value(
        &genesis,
        &["app_state", "bank", "params", "default_send_enabled"],
    )
    .unwrap();
    assert_eq!(send_enabled, true);

    // --- Reading a non-existent path returns None ---
    let missing = get_genesis_module_value(&genesis, &["app_state", "does_not_exist"]);
    assert!(missing.is_none(), "missing path should return None");

    // --- Setting a path where intermediate key doesn't exist returns error ---
    let result = set_genesis_module_value(
        &mut genesis,
        &["app_state", "nonexistent_module", "params", "key"],
        serde_json::json!("value"),
    );
    assert!(result.is_err(), "setting through missing intermediate should fail");
}

// ===========================================================================
// Test 7: Fund test users workflow
// ===========================================================================

#[tokio::test]
async fn test_fund_test_users_workflow() {
    let runtime = Arc::new(MockRuntime::new());
    let mut chain = CosmosChain::new(make_terp_config(), 1, 0, runtime.clone());

    let ctx = TestContext {
        test_name: "fund_users_test".to_string(),
        network_id: "ict-fund".to_string(),
    };
    chain.initialize(&ctx).await.unwrap();
    chain.start(&[]).await.unwrap();

    // Now exercise the send_funds + get_balance pattern.
    // Create a key and send funds.
    chain.create_key("user-0").await.unwrap();

    let amount = WalletAmount {
        address: "terp1mockuser-0000000000000000000000".to_string(),
        denom: "uterp".to_string(),
        amount: 1_000_000,
    };
    let tx_hash = chain.send_funds("validator-0", &amount).await.unwrap();
    assert!(
        !tx_hash.is_empty(),
        "send_funds should return a tx hash"
    );
    assert_eq!(tx_hash, "AABBCCDD1234567890");

    // Query balance.
    let bal = chain
        .get_balance("terp1mockuser-0000000000000000000000", "uterp")
        .await
        .unwrap();
    assert_eq!(bal, 1_000_000);

    // Create a second user and fund them too.
    chain.create_key("user-1").await.unwrap();
    let amount2 = WalletAmount {
        address: "terp1mockuser-1000000000000000000000".to_string(),
        denom: "uterp".to_string(),
        amount: 2_000_000,
    };
    let tx_hash2 = chain.send_funds("validator-0", &amount2).await.unwrap();
    assert!(!tx_hash2.is_empty());

    // Verify the exec log contains bank send commands for both users.
    let log = runtime.exec_log();
    let flat: Vec<String> = log.iter().map(|v| v.join(" ")).collect();
    let send_cmds: Vec<&String> = flat.iter().filter(|c| c.contains("bank") && c.contains("send")).collect();
    assert!(
        send_cmds.len() >= 2,
        "should have at least 2 bank send commands, got {}",
        send_cmds.len()
    );

    chain.stop().await.unwrap();
}

// ===========================================================================
// Test 8: Chain node exec commands
// ===========================================================================

#[tokio::test]
async fn test_chain_node_exec_commands() {
    let runtime = Arc::new(MockRuntime::new());

    // Create a ChainNode directly.
    let mut node = ChainNode::new(
        0,
        true,
        "test-chain-1",
        "gaiad",
        DockerImage {
            repository: "gaia".to_string(),
            version: "v19.0.0".to_string(),
            uid_gid: None,
        },
        "node_exec_test",
        "mock-net-1",
        runtime.clone(),
        None,
        Default::default(),
        "0.025uatom",
        1.5,
    );

    // Create and start the container.
    node.create_container().await.unwrap();
    assert!(node.container_id.is_some(), "container should be created");

    node.start_container().await.unwrap();

    // --- init_home ---
    let out = node.init_home("test-moniker").await.unwrap();
    assert_eq!(out.exit_code, 0);

    // --- create_key ---
    let out = node.create_key("mykey", 118).await.unwrap();
    assert_eq!(out.exit_code, 0);
    let key_json: serde_json::Value =
        serde_json::from_str(out.stdout_str().trim()).unwrap();
    assert_eq!(key_json["name"], "mykey");

    // --- get_key_address ---
    let addr = node.get_key_address("mykey").await.unwrap();
    assert!(
        addr.contains("cosmos1mock"),
        "address should be a mock cosmos address: {addr}"
    );

    // --- recover_key ---
    let test_mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let out = node.recover_key("recovered", test_mnemonic).await.unwrap();
    assert_eq!(out.exit_code, 0);

    // --- add_genesis_account ---
    let out = node
        .add_genesis_account("cosmos1someaddr", "100000000uatom")
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);

    // --- gentx ---
    let out = node.gentx("validator-0", "50000000uatom", "0.025uatom", 1.5).await.unwrap();
    assert_eq!(out.exit_code, 0);

    // --- collect_gentxs ---
    let out = node.collect_gentxs().await.unwrap();
    assert_eq!(out.exit_code, 0);

    // --- query_balance ---
    let bal = node.query_balance("cosmos1someaddr", "uatom").await.unwrap();
    assert_eq!(bal, 1_000_000);

    // --- query_height ---
    let height = node.query_height().await.unwrap();
    assert_eq!(height, 100);

    // --- bank_send ---
    let out = node
        .bank_send("validator-0", "cosmos1dest", "1000uatom", "0.025uatom")
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);
    assert!(out.stdout_str().contains("txhash"));

    // --- ibc_transfer ---
    let out = node
        .ibc_transfer(
            "channel-0",
            "validator-0",
            "osmo1dest",
            "500uatom",
            "0.025uatom",
            Some("test memo"),
        )
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);

    // --- export_state ---
    let state = node.export_state(100).await.unwrap();
    assert!(state.contains("app_state"));

    // --- Verify command arguments were recorded ---
    let log = runtime.exec_log();
    let flat: Vec<String> = log.iter().map(|v| v.join(" ")).collect();

    assert!(flat.iter().any(|c| c.contains("init") && c.contains("test-moniker")));
    assert!(flat.iter().any(|c| c.contains("keys") && c.contains("add") && c.contains("mykey")));
    assert!(flat.iter().any(|c| c.contains("add-genesis-account") && c.contains("cosmos1someaddr")));
    assert!(flat.iter().any(|c| c.contains("gentx") && c.contains("validator-0")));
    assert!(flat.iter().any(|c| c.contains("collect-gentxs")));
    assert!(flat.iter().any(|c| c.contains("bank") && c.contains("send")));
    assert!(flat.iter().any(|c| c.contains("ibc-transfer") && c.contains("channel-0")));
    assert!(flat.iter().any(|c| c.contains("export") && c.contains("100")));

    // --- Stop and remove ---
    node.stop_container().await.unwrap();
    node.remove_container().await.unwrap();
    assert!(node.container_id.is_none(), "container should be removed");
}

// ===========================================================================
// Test 9: Reporter tracking
// ===========================================================================

#[tokio::test]
async fn test_reporter_tracking() {
    let mut reporter = TestReporter::new();
    assert!(reporter.reports().is_empty(), "reporter should start empty");

    let now = Instant::now();

    // Record some reports.
    reporter.record(ExecReport {
        container_name: "ict-gaia-val-0".to_string(),
        command: vec!["gaiad".to_string(), "init".to_string(), "moniker".to_string()],
        stdout: "{}".to_string(),
        stderr: String::new(),
        exit_code: 0,
        started_at: now,
        duration: Duration::from_millis(50),
    });

    reporter.record(ExecReport {
        container_name: "ict-gaia-val-0".to_string(),
        command: vec![
            "gaiad".to_string(),
            "keys".to_string(),
            "add".to_string(),
            "validator-0".to_string(),
        ],
        stdout: r#"{"name":"validator-0","address":"cosmos1..."}"#.to_string(),
        stderr: String::new(),
        exit_code: 0,
        started_at: now,
        duration: Duration::from_millis(30),
    });

    reporter.record(ExecReport {
        container_name: "ict-gaia-val-0".to_string(),
        command: vec!["gaiad".to_string(), "status".to_string()],
        stdout: r#"{"sync_info":{"latest_block_height":"100"}}"#.to_string(),
        stderr: String::new(),
        exit_code: 0,
        started_at: now,
        duration: Duration::from_millis(15),
    });

    // Verify all recorded.
    assert_eq!(reporter.reports().len(), 3);

    // Verify individual reports.
    let r0 = &reporter.reports()[0];
    assert_eq!(r0.container_name, "ict-gaia-val-0");
    assert_eq!(r0.command, vec!["gaiad", "init", "moniker"]);
    assert_eq!(r0.exit_code, 0);
    assert_eq!(r0.duration, Duration::from_millis(50));

    let r1 = &reporter.reports()[1];
    assert!(r1.command.contains(&"keys".to_string()));
    assert_eq!(r1.duration, Duration::from_millis(30));

    let r2 = &reporter.reports()[2];
    assert!(r2.stdout.contains("latest_block_height"));
    assert_eq!(r2.duration, Duration::from_millis(15));

    // Verify timing data is accessible.
    let total_duration: Duration = reporter.reports().iter().map(|r| r.duration).sum();
    assert_eq!(total_duration, Duration::from_millis(95));
}

// ===========================================================================
// Test 10: Error propagation
// ===========================================================================

#[tokio::test]
async fn test_error_propagation() {
    // --- Runtime error propagates through chain ---
    {
        let runtime = Arc::new(MockRuntime::new_with_error("runtime exploded"));
        let mut chain = CosmosChain::new(make_terp_config(), 1, 0, runtime.clone());

        let ctx = TestContext {
            test_name: "error_test".to_string(),
            network_id: "ict-error".to_string(),
        };
        // Initialize should succeed (create_network, create_container, start_container don't fail)
        // but init_home calls exec_in_container which will fail.
        let result = chain.initialize(&ctx).await;
        assert!(result.is_err(), "init should fail when exec returns errors");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("runtime exploded"),
            "error message should contain runtime error: {err_msg}"
        );
    }

    // --- Invalid chain config: unknown builtin ---
    {
        let result = builtin_chain_config("nonexistent-chain");
        assert!(result.is_err());
        match result.unwrap_err() {
            IctError::Config(msg) => {
                assert!(
                    msg.contains("unknown built-in chain"),
                    "should be a Config error: {msg}"
                );
            }
            other => panic!("expected Config error, got: {other}"),
        }
    }

    // --- Chain not initialized: start fails ---
    {
        let runtime = Arc::new(MockRuntime::new());
        let mut chain = CosmosChain::new(make_terp_config(), 1, 0, runtime.clone());
        let result = chain.start(&[]).await;
        assert!(result.is_err(), "start without initialize should fail");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("not initialized"),
            "error should mention not initialized: {err_msg}"
        );
    }

    // --- IctError variants ---
    {
        let err = IctError::ExecFailed {
            exit_code: 1,
            stderr: "command not found".to_string(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("exit 1"));
        assert!(msg.contains("command not found"));

        let err = IctError::Timeout {
            what: "block height".to_string(),
            duration: Duration::from_secs(30),
        };
        let msg = format!("{err}");
        assert!(msg.contains("block height"));
        assert!(msg.contains("30"));

        let err = IctError::Ibc("channel not found".to_string());
        let msg = format!("{err}");
        assert!(msg.contains("channel not found"));

        let err = IctError::Wallet("invalid mnemonic".to_string());
        let msg = format!("{err}");
        assert!(msg.contains("invalid mnemonic"));
    }
}

// ===========================================================================
// Additional: Test ChainNode address and hostname generation
// ===========================================================================

#[tokio::test]
async fn test_chain_node_naming_conventions() {
    let runtime = Arc::new(MockRuntime::new());

    let val_node = ChainNode::new(
        0,
        true,
        "gaia-1",
        "gaiad",
        DockerImage {
            repository: "gaia".to_string(),
            version: "v1".to_string(),
            uid_gid: None,
        },
        "my_test",
        "net-1",
        runtime.clone(),
        None,
        Default::default(),
        "0.025uatom",
        1.5,
    );

    assert_eq!(val_node.hostname, "gaia-1-val-0");
    assert_eq!(val_node.container_name(), "ict-my_test-gaia-1-val-0");
    assert_eq!(val_node.volume_name, "my_test-gaia-1-val-0");
    assert_eq!(val_node.home_dir, "/var/cosmos-chain/gaia-1");
    assert!(val_node.rpc_address().contains("gaia-1-val-0"));
    assert!(val_node.rpc_address().contains("26657"));
    assert!(val_node.grpc_address().contains("9090"));
    assert!(val_node.p2p_address().contains("26656"));

    let fn_node = ChainNode::new(
        2,
        false,
        "osmo-1",
        "osmosisd",
        DockerImage {
            repository: "osmosis".to_string(),
            version: "v1".to_string(),
            uid_gid: None,
        },
        "another_test",
        "net-2",
        runtime.clone(),
        None,
        Default::default(),
        "0.025uosmo",
        1.5,
    );

    assert_eq!(fn_node.hostname, "osmo-1-fn-2");
    assert_eq!(fn_node.container_name(), "ict-another_test-osmo-1-fn-2");
    assert!(fn_node.is_validator == false);
    assert_eq!(fn_node.home_dir, "/var/cosmos-chain/osmo-1");
}

// ===========================================================================
// Additional: Test IBC transfer through chain trait
// ===========================================================================

#[tokio::test]
async fn test_ibc_transfer_through_chain() {
    let runtime = Arc::new(MockRuntime::new());
    let mut chain = CosmosChain::new(make_gaia_config(), 1, 0, runtime.clone());

    let ctx = TestContext {
        test_name: "ibc_test".to_string(),
        network_id: "ict-ibc".to_string(),
    };
    chain.initialize(&ctx).await.unwrap();
    chain.start(&[]).await.unwrap();

    let amount = WalletAmount {
        address: "osmo1destination000000000000000000000".to_string(),
        denom: "uatom".to_string(),
        amount: 1_000_000,
    };

    let tx = chain
        .send_ibc_transfer(
            "channel-0",
            "validator-0",
            &amount,
            &TransferOptions {
                memo: Some("test ibc memo".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(tx.tx_hash, "IBCTX0001");
    assert_eq!(tx.height, 102);
    assert_eq!(tx.gas_spent, 80000);

    chain.stop().await.unwrap();
}

// ===========================================================================
// Additional: Test multiple chain specs resolve correctly
// ===========================================================================

#[tokio::test]
async fn test_builtin_chain_configs() {
    let gaia = builtin_chain_config("gaia").unwrap();
    assert_eq!(gaia.chain_type, ChainType::Cosmos);
    assert_eq!(gaia.bin, "gaiad");
    assert_eq!(gaia.bech32_prefix, "cosmos");
    assert_eq!(gaia.denom, "uatom");

    let osmo = builtin_chain_config("osmosis").unwrap();
    assert_eq!(osmo.bin, "osmosisd");
    assert_eq!(osmo.bech32_prefix, "osmo");
    assert_eq!(osmo.denom, "uosmo");

    let terp = builtin_chain_config("terp").unwrap();
    assert_eq!(terp.bin, "terpd");
    assert_eq!(terp.bech32_prefix, "terp");
    assert_eq!(terp.denom, "uterp");

    let juno = builtin_chain_config("juno").unwrap();
    assert_eq!(juno.bin, "junod");
    assert_eq!(juno.bech32_prefix, "juno");

    // Also accessible via alias.
    let gaia2 = builtin_chain_config("cosmoshub").unwrap();
    assert_eq!(gaia2.chain_id, gaia.chain_id);

    let terp2 = builtin_chain_config("terpnetwork").unwrap();
    assert_eq!(terp2.chain_id, terp.chain_id);

    // Unknown should error.
    assert!(builtin_chain_config("unknown_chain").is_err());
}

// ===========================================================================
// Additional: Test double initialization is idempotent
// ===========================================================================

#[tokio::test]
async fn test_double_initialize_is_idempotent() {
    let runtime = Arc::new(MockRuntime::new());
    let mut chain = CosmosChain::new(make_terp_config(), 1, 0, runtime.clone());

    let ctx = TestContext {
        test_name: "double_init".to_string(),
        network_id: "ict-double".to_string(),
    };

    chain.initialize(&ctx).await.unwrap();
    let container_count_after_first = runtime.container_count();

    // Second initialize should be a no-op.
    chain.initialize(&ctx).await.unwrap();
    let container_count_after_second = runtime.container_count();

    assert_eq!(
        container_count_after_first, container_count_after_second,
        "second initialize should not create additional containers"
    );

    chain.stop().await.unwrap();
}

// ===========================================================================
// Additional: Test Interchain with skip_path_creation
// ===========================================================================

#[tokio::test]
async fn test_interchain_skip_path_creation() {
    let runtime = Arc::new(MockRuntime::new());
    let relayer = MockRelayer::new();
    let relayer_calls = relayer.calls.clone();

    let gaia = CosmosChain::new(make_gaia_config(), 1, 0, runtime.clone());
    let osmo = CosmosChain::new(make_osmosis_config(), 1, 0, runtime.clone());

    let mut ic = Interchain::new(runtime.clone())
        .add_chain(Box::new(gaia))
        .add_chain(Box::new(osmo))
        .add_relayer("hermes", Box::new(relayer))
        .add_link(InterchainLink {
            chain1: "cosmoshub-test-1".to_string(),
            chain2: "osmosis-test-1".to_string(),
            relayer: "hermes".to_string(),
            path: "gaia-osmo".to_string(),
        });

    ic.build(InterchainBuildOptions {
        test_name: "skip_paths".to_string(),
        skip_path_creation: true,
        ..Default::default()
    })
    .await
    .unwrap();

    let calls = relayer_calls.lock().unwrap().clone();

    // With skip_path_creation, relayer should NOT have generate_path, link_path, or start called.
    assert!(
        !calls.iter().any(|c| c.contains("generate_path")),
        "should not generate path when skip_path_creation is true"
    );
    assert!(
        !calls.iter().any(|c| c.contains("link_path")),
        "should not link path when skip_path_creation is true"
    );
    assert!(
        !calls.iter().any(|c| c.contains("start(")),
        "should not start relayer when skip_path_creation is true"
    );

    ic.close().await.unwrap();
}
