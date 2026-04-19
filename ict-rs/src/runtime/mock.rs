//! Mock runtime backend for unit testing without Docker.
//!
//! Provides a `MockRuntime` that implements `RuntimeBackend` by tracking
//! containers, networks, and images in memory. Exec responses can be
//! pre-programmed via `queue_exec_response`, and container logs can be
//! set via `set_container_logs`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::error::{IctError, Result};
use crate::runtime::{
    ContainerId, ContainerOptions, DockerImage, ExitStatus, NetworkId, RuntimeBackend,
};
use crate::tx::ExecOutput;

/// Status of a mock container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockContainerStatus {
    Created,
    Running,
    Stopped,
    Removed,
}

/// A simulated container tracked by the mock runtime.
#[derive(Debug, Clone)]
pub struct MockContainer {
    pub name: String,
    pub image: String,
    pub status: MockContainerStatus,
    pub exec_responses: VecDeque<ExecOutput>,
    pub logs: String,
}

/// In-memory state for the mock runtime.
#[derive(Debug, Default)]
pub struct MockState {
    pub containers: HashMap<String, MockContainer>,
    pub networks: HashMap<String, String>,
    pub pulled_images: Vec<String>,
    /// Log of all exec calls: `(container_id, command_args)`.
    pub exec_log: Vec<(String, Vec<String>)>,
    /// Volumes that have been removed.
    pub volumes_removed: Vec<String>,
    next_container_id: u64,
    next_network_id: u64,
}

/// A mock implementation of `RuntimeBackend` for testing.
///
/// All operations are performed in-memory. No Docker daemon is required.
#[derive(Debug, Clone)]
pub struct MockRuntime {
    state: Arc<Mutex<MockState>>,
}

impl MockRuntime {
    /// Create a new mock runtime with empty state.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MockState::default())),
        }
    }

    /// Get a reference to the internal state for inspection in tests.
    pub fn state(&self) -> Arc<Mutex<MockState>> {
        self.state.clone()
    }

    /// Pre-program an exec response for a given container.
    ///
    /// When `exec_in_container` is called on that container, responses are
    /// dequeued in FIFO order. If the queue is empty, a default successful
    /// `ExecOutput` is returned.
    pub fn queue_exec_response(&self, container_id: &str, response: ExecOutput) {
        let mut state = self.state.lock().unwrap();
        if let Some(container) = state.containers.get_mut(container_id) {
            container.exec_responses.push_back(response);
        }
    }

    /// Set the logs that will be returned for a given container.
    pub fn set_container_logs(&self, container_id: &str, logs: &str) {
        let mut state = self.state.lock().unwrap();
        if let Some(container) = state.containers.get_mut(container_id) {
            container.logs = logs.to_string();
        }
    }

    /// Find a container ID by substring match on the container name.
    ///
    /// Useful in tests to locate specific containers (e.g., validator, sidecar)
    /// after the interchain has been built.
    pub fn find_container_id_by_name(&self, name_contains: &str) -> Option<String> {
        let state = self.state.lock().unwrap();
        state
            .containers
            .iter()
            .find(|(_, c)| c.name.contains(name_contains))
            .map(|(id, _)| id.clone())
    }
}

impl Default for MockRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RuntimeBackend for MockRuntime {
    async fn pull_image(&self, image: &DockerImage) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.pulled_images.push(image.to_string());
        Ok(())
    }

    async fn create_container(&self, opts: &ContainerOptions) -> Result<ContainerId> {
        let mut state = self.state.lock().unwrap();
        let id = state.next_container_id;
        state.next_container_id += 1;
        let container_id = format!("mock-container-{id}");

        let container = MockContainer {
            name: opts.name.clone(),
            image: opts.image.to_string(),
            status: MockContainerStatus::Created,
            exec_responses: VecDeque::new(),
            logs: String::new(),
        };

        state.containers.insert(container_id.clone(), container);
        Ok(ContainerId(container_id))
    }

    async fn start_container(&self, id: &ContainerId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let container = state.containers.get_mut(&id.0).ok_or_else(|| {
            IctError::Runtime(anyhow::anyhow!("container not found: {}", id.0))
        })?;
        container.status = MockContainerStatus::Running;
        Ok(())
    }

    async fn stop_container(&self, id: &ContainerId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let container = state.containers.get_mut(&id.0).ok_or_else(|| {
            IctError::Runtime(anyhow::anyhow!("container not found: {}", id.0))
        })?;
        container.status = MockContainerStatus::Stopped;
        Ok(())
    }

    async fn remove_container(&self, id: &ContainerId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.containers.remove(&id.0).is_none() {
            return Err(IctError::Runtime(anyhow::anyhow!(
                "container not found: {}",
                id.0
            )));
        }
        Ok(())
    }

    async fn exec_in_container(
        &self,
        id: &ContainerId,
        cmd: &[&str],
        _env: &[(&str, &str)],
    ) -> Result<ExecOutput> {
        let mut state = self.state.lock().unwrap();

        // Log the exec call for test verification.
        state.exec_log.push((
            id.0.clone(),
            cmd.iter().map(|s| s.to_string()).collect(),
        ));

        let container = state.containers.get_mut(&id.0).ok_or_else(|| {
            IctError::Runtime(anyhow::anyhow!("container not found: {}", id.0))
        })?;

        // Return a queued response if available.
        if let Some(response) = container.exec_responses.pop_front() {
            return Ok(response);
        }

        // Otherwise generate a smart default based on the command pattern.
        let stdout = mock_command_response(cmd);
        Ok(ExecOutput {
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
            exit_code: 0,
        })
    }

    async fn create_network(&self, name: &str) -> Result<NetworkId> {
        let mut state = self.state.lock().unwrap();
        let id = state.next_network_id;
        state.next_network_id += 1;
        let network_id = format!("mock-network-{id}");
        state
            .networks
            .insert(network_id.clone(), name.to_string());
        Ok(NetworkId(network_id))
    }

    async fn remove_network(&self, id: &NetworkId) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.networks.remove(&id.0).is_none() {
            return Err(IctError::Runtime(anyhow::anyhow!(
                "network not found: {}",
                id.0
            )));
        }
        Ok(())
    }

    async fn exec_in_container_background(
        &self,
        id: &ContainerId,
        cmd: &[&str],
        _env: &[(&str, &str)],
    ) -> Result<()> {
        let state = self.state.lock().unwrap();
        let _container = state.containers.get(&id.0).ok_or_else(|| {
            IctError::Docker(bollard::errors::Error::IOError {
                err: std::io::Error::new(std::io::ErrorKind::NotFound, "container not found"),
            })
        })?;
        let _ = (id, cmd); // suppress unused warnings
        Ok(())
    }

    async fn container_logs(&self, id: &ContainerId) -> Result<String> {
        let state = self.state.lock().unwrap();
        let container = state.containers.get(&id.0).ok_or_else(|| {
            IctError::Runtime(anyhow::anyhow!("container not found: {}", id.0))
        })?;
        Ok(container.logs.clone())
    }

    async fn wait_for_container(&self, _id: &ContainerId) -> Result<ExitStatus> {
        Ok(ExitStatus { code: 0 })
    }

    async fn remove_volume(&self, name: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.volumes_removed.push(name.to_string());
        Ok(())
    }
}

/// Generate realistic mock responses for common Cosmos SDK CLI commands.
///
/// Parses the command array and returns appropriate JSON or text output so that
/// `CosmosChain` / `ChainNode` methods can parse the result without errors.
fn mock_command_response(cmd: &[&str]) -> String {
    // For `sh -c "yes | terpd keys add ..."` style commands, extract the
    // inner command and re-parse it. This handles create_key, recover_key, etc.
    // that pipe stdin through shell wrappers.
    if cmd.len() >= 3 && cmd[0] == "sh" && cmd[1] == "-c" {
        let inner = cmd[2];
        // Strip leading pipe commands (e.g., "yes | ", "echo 'mnemonic' | ")
        let inner = if let Some(pos) = inner.find('|') {
            inner[pos + 1..].trim()
        } else {
            inner.trim()
        };
        // Split the inner command into parts and recurse
        let inner_parts: Vec<&str> = inner.split_whitespace().collect();
        if !inner_parts.is_empty() {
            return mock_command_response(&inner_parts);
        }
    }

    // cmd is typically: [binary, subcommand..., --home, /path]
    // Find meaningful subcommands ignoring the binary name and flags.
    let parts: Vec<&str> = cmd
        .iter()
        .copied()
        .filter(|s| !s.starts_with("--") && !s.starts_with('/'))
        .collect();

    // Match on command patterns
    match parts.as_slice() {
        // `gaiad status` → CometBFT status JSON
        [_, "status", ..] => r#"{"sync_info":{"latest_block_height":"42","latest_block_time":"2024-01-01T00:00:00Z","catching_up":false},"node_info":{"network":"mock-chain"}}"#.to_string(),

        // `gaiad init <moniker>` → init output
        [_, "init", moniker, ..] => format!(
            r#"{{"app_message":{{"genesis_time":"2024-01-01T00:00:00Z"}},"moniker":"{moniker}"}}"#
        ),

        // `gaiad keys add <name>` → key creation
        [_, "keys", "add", name, ..] => format!(
            r#"{{"name":"{name}","type":"local","address":"cosmos1mockaddr{name}","pubkey":"{{\"@type\":\"/cosmos.crypto.secp256k1.PubKey\",\"key\":\"A0mock\"}}","mnemonic":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"}}"#
        ),

        // `gaiad keys show <name> -a` or `gaiad keys show <name>`
        [_, "keys", "show", name, ..] => {
            if cmd.iter().any(|&s| s == "-a") {
                format!("cosmos1mockaddr{name}")
            } else {
                format!(
                    r#"{{"name":"{name}","type":"local","address":"cosmos1mockaddr{name}"}}"#
                )
            }
        }

        // `gaiad query bank balance <addr> <denom>` (SDK v0.47+)
        [_, "query", "bank", "balance", ..] => {
            r#"{"balance":{"denom":"stake","amount":"1000000000"}}"#.to_string()
        }

        // `gaiad query bank balances <addr>` (older SDK format)
        [_, "query", "bank", "balances", ..] => {
            r#"{"balances":[{"denom":"stake","amount":"1000000000"}],"balance":{"denom":"stake","amount":"1000000000"},"amount":"1000000000"}"#.to_string()
        }

        // `gaiad tx bank send ...`
        [_, "tx", "bank", "send", ..] => {
            r#"{"height":"1","txhash":"MOCKTXHASH000000000000000000000000000000000000000000000000000000","gas_used":"50000","code":0}"#.to_string()
        }

        // `gaiad tx ibc-transfer transfer ...`
        [_, "tx", "ibc-transfer", ..] => {
            r#"{"height":"1","txhash":"MOCKIBCTX00000000000000000000000000000000000000000000000000000000","gas_used":"80000","code":0,"packet_sequence":"1"}"#.to_string()
        }

        // `terpd tx wasm store <path>` → store code response
        [_, "tx", "wasm", "store", ..] | ["tx", "wasm", "store", ..] => {
            r#"{"code_id":"1","txhash":"MOCKWASMSTORE00000000000000000000000000000000000000000000000000","gas_used":"200000","code":0}"#.to_string()
        }

        // `terpd tx wasm instantiate <code_id> <msg>` → instantiate response
        [_, "tx", "wasm", "instantiate", code_id, ..] | ["tx", "wasm", "instantiate", code_id, ..] => {
            format!(
                r#"{{"contract_address":"terp1mockcontract{code_id}","txhash":"MOCKWASMINST000000000000000000000000000000000000000000000000000000","gas_used":"150000","code":0}}"#
            )
        }

        // `terpd tx wasm execute <contract> <msg>` → execute response
        [_, "tx", "wasm", "execute", ..] | ["tx", "wasm", "execute", ..] => {
            r#"{"height":"1","txhash":"MOCKWASMEXEC000000000000000000000000000000000000000000000000000000","gas_used":"100000","code":0}"#.to_string()
        }

        // `terpd query wasm contract-state smart <contract> <query>` → smart query response
        [_, "query", "wasm", "contract-state", "smart", ..] | ["query", "wasm", "contract-state", "smart", ..] => {
            r#"{"data":{"count":1,"total_funds":[{"denom":"uterp","amount":"1000"}]}}"#.to_string()
        }

        // `terpd query wasm code-info <code_id>` → code info response
        [_, "query", "wasm", "code-info", ..] | ["query", "wasm", "code-info", ..] => {
            r#"{"code_id":"1","creator":"terp1mockaddrvalidator","data_hash":"ABCDEF","instantiate_permission":{"permission":"Everybody"}}"#.to_string()
        }

        // Tokenfactory tx create-denom
        [_, "tx", "tokenfactory", "create-denom", subdenom, ..] | ["tx", "tokenfactory", "create-denom", subdenom, ..] => {
            format!(
                r#"{{"height":"1","txhash":"MOCKTXHASH000000000000000000000000000000000000000000000000000000","gas_used":"50000","code":0,"new_token_denom":"factory/terp1mockaddr/{subdenom}"}}"#
            )
        }

        // Tokenfactory tx mint
        [_, "tx", "tokenfactory", "mint", ..] | ["tx", "tokenfactory", "mint", ..] => {
            r#"{"height":"2","txhash":"MOCKMINTHASH0000000000000000000000000000000000000000000000000000","gas_used":"60000","code":0}"#.to_string()
        }

        // Tokenfactory tx burn
        [_, "tx", "tokenfactory", "burn", ..] | ["tx", "tokenfactory", "burn", ..] => {
            r#"{"height":"3","txhash":"MOCKBURNHASH0000000000000000000000000000000000000000000000000000","gas_used":"55000","code":0}"#.to_string()
        }

        // Tokenfactory query denom-authority-metadata
        [_, "query", "tokenfactory", "denom-authority-metadata", denom, ..] | ["query", "tokenfactory", "denom-authority-metadata", denom, ..] => {
            format!(
                r#"{{"authority_metadata":{{"admin":"terp1mockadmin"}},"denom":"{denom}"}}"#
            )
        }

        // Tokenfactory query denoms-from-creator
        [_, "query", "tokenfactory", "denoms-from-creator", ..] | ["query", "tokenfactory", "denoms-from-creator", ..] => {
            r#"{"denoms":["factory/terp1mockaddr/testdenom"]}"#.to_string()
        }

        // Tokenfactory query params
        [_, "query", "tokenfactory", "params", ..] | ["query", "tokenfactory", "params", ..] => {
            r#"{"params":{"denom_creation_fee":[{"denom":"uterp","amount":"1000000"}]}}"#.to_string()
        }

        // Staking queries
        [_, "query", "staking", "validators", ..] | ["query", "staking", "validators", ..] => {
            r#"{"validators":[{"operator_address":"terpvaloper1mock","status":"BOND_STATUS_BONDED","tokens":"1000000","description":{"moniker":"validator-0"}}]}"#.to_string()
        }

        // Auth queries
        [_, "query", "auth", "module-address", module, ..] | ["query", "auth", "module-address", module, ..] => {
            format!(
                r#"{{"address":"terp1mock{module}moduleaddr"}}"#
            )
        }

        // Distribution queries
        [_, "query", "distribution", "community-pool", ..] | ["query", "distribution", "community-pool", ..] => {
            r#"{"pool":[{"denom":"uterp","amount":"50000.000000000000000000"}]}"#.to_string()
        }

        // Slashing queries
        [_, "query", "slashing", "params", ..] | ["query", "slashing", "params", ..] => {
            r#"{"params":{"signed_blocks_window":"100","min_signed_per_window":"0.500000000000000000","downtime_jail_duration":"600s","slash_fraction_double_sign":"0.050000000000000000","slash_fraction_downtime":"0.010000000000000000"}}"#.to_string()
        }

        // -- Hashmerchant module --

        // hashmerchant hash-root query (direct, no binary prefix)
        ["query", "hashmerchant", "hash-root", chain_uid, algo, ..] => {
            format!(
                r#"{{"hash_root":{{"chain_uid":"{chain_uid}","algo":"{algo}","height":"0","root":"","attestation_count":"0","block_time":"0"}}}}"#
            )
        }

        // hashmerchant registered-chain query
        ["query", "hashmerchant", "registered-chain", chain_uid, ..] => {
            format!(
                r#"{{"registered_chain":{{"chain_uid":"{chain_uid}","chain_type":"evm","status":"active"}}}}"#
            )
        }

        // hashmerchant registered-chains query
        ["query", "hashmerchant", "registered-chains", ..] => {
            r#"{"registered_chains":[],"pagination":{"total":"0"}}"#.to_string()
        }

        // hashmerchant hash-roots query
        ["query", "hashmerchant", "hash-roots", chain_uid, ..] => {
            format!(
                r#"{{"hash_roots":[],"chain_uid":"{chain_uid}","pagination":{{"total":"0"}}}}"#
            )
        }

        // hashmerchant params query
        ["query", "hashmerchant", "params", ..] => {
            r#"{"params":{"min_attestation_count":"1","vote_extension_timeout":"30s","enabled_chains":["anvil-31337"]}}"#.to_string()
        }

        // hashmerchant tx register-chain
        ["tx", "hashmerchant", "register-chain", ..] => {
            r#"{"height":"1","txhash":"HMREGISTER00000000000000000000000000000000000000000000000000000000","gas_used":"75000","code":0}"#.to_string()
        }

        // hashmerchant tx register-contract
        ["tx", "hashmerchant", "register-contract", ..] => {
            r#"{"height":"2","txhash":"HMCONTRACT00000000000000000000000000000000000000000000000000000000","gas_used":"80000","code":0}"#.to_string()
        }

        // hashmerchant other tx
        ["tx", "hashmerchant", ..] => {
            r#"{"height":"1","txhash":"HMTXHASH0000000000000000000000000000000000000000000000000000000000","gas_used":"50000","code":0}"#.to_string()
        }

        // hashmerchant hash-root query (with binary prefix)
        [_, "query", "hashmerchant", "hash-root", chain_uid, algo, ..] => {
            format!(
                r#"{{"hash_root":{{"chain_uid":"{chain_uid}","algo":"{algo}","height":"0","root":"","attestation_count":"0","block_time":"0"}}}}"#
            )
        }

        // hashmerchant registered-chain query (with binary prefix)
        [_, "query", "hashmerchant", "registered-chain", chain_uid, ..] => {
            format!(
                r#"{{"registered_chain":{{"chain_uid":"{chain_uid}","chain_type":"evm","status":"active"}}}}"#
            )
        }

        // hashmerchant tx (with binary prefix)
        [_, "tx", "hashmerchant", ..] => {
            r#"{"height":"1","txhash":"HMTXHASH0000000000000000000000000000000000000000000000000000000000","gas_used":"50000","code":0}"#.to_string()
        }

        // Generic `binary tx <module> <action>` → mock tx response
        [_, "tx", _module, _action, ..] => {
            r#"{"height":"1","txhash":"MOCKTXHASH000000000000000000000000000000000000000000000000000000","gas_used":"50000","code":0}"#.to_string()
        }

        // Generic `binary query <module> <action>` → mock query response
        [_, "query", _module, _action, ..] => {
            r#"{"result":"mock_query_result"}"#.to_string()
        }

        // Direct `tx <module> <action>` (no binary prefix, from Chain::exec)
        ["tx", _module, _action, ..] => {
            r#"{"height":"1","txhash":"MOCKTXHASH000000000000000000000000000000000000000000000000000000","gas_used":"50000","code":0}"#.to_string()
        }

        // Direct `query <module> <action>` (no binary prefix, from Chain::exec)
        ["query", _module, _action, ..] => {
            r#"{"result":"mock_query_result"}"#.to_string()
        }

        // `gaiad genesis add-genesis-account ...`
        [_, "genesis", "add-genesis-account", ..] => String::new(),

        // `gaiad genesis gentx ...`
        [_, "genesis", "gentx", ..] => {
            r#"Genesis transaction written to gentx file"#.to_string()
        }

        // `gaiad genesis collect-gentxs`
        [_, "genesis", "collect-gentxs", ..] => {
            r#"{"app_message":"collected gentxs"}"#.to_string()
        }

        // `gaiad export`
        [_, "export", ..] => {
            r#"{"app_state":{},"chain_id":"mock-chain","genesis_time":"2024-01-01T00:00:00Z"}"#.to_string()
        }

        // `gaiad comet show-node-id` or `gaiad tendermint show-node-id`
        [_, "comet", "show-node-id", ..] | [_, "tendermint", "show-node-id", ..] => {
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string()
        }

        // `sed` commands (config modifications) → no output
        ["sed", ..] => String::new(),

        // `sha256sum` (used for genesis hash)
        ["sha256sum", path, ..] => {
            format!("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  {path}")
        }

        // `cat` (used for reading genesis files)
        ["cat", ..] => {
            r#"{"app_state":{"auth":{"accounts":[{"@type":"/cosmos.auth.v1beta1.BaseAccount","address":"cosmos1mockaddrvalidator","pub_key":null,"account_number":"0","sequence":"0"},{"@type":"/cosmos.auth.v1beta1.BaseAccount","address":"cosmos1mockaddrfaucet","pub_key":null,"account_number":"1","sequence":"0"}]},"staking":{"params":{"bond_denom":"uterp","unbonding_time":"1814400s","max_validators":100}},"mint":{"params":{"mint_denom":"uterp"}},"genutil":{"gen_txs":[{"body":{"messages":[{"@type":"/cosmos.staking.v1beta1.MsgCreateValidator"}]}}]}},"chain_id":"mock-chain","genesis_time":"2024-01-01T00:00:00Z"}"#.to_string()
        }

        // -- Ethereum/Anvil cast commands --

        // `cast block-number`
        ["cast", "block-number", ..] => "42".to_string(),

        // `cast balance`
        ["cast", "balance", ..] => "10000000000000000000000".to_string(),

        // `cast send` (transaction)
        ["cast", "send", ..] => {
            r#"{"transactionHash":"0xabcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890","blockNumber":"0x2a","status":"0x1"}"#.to_string()
        }

        // `cast block` (get block)
        ["cast", "block", ..] => {
            r#"{"number":"0x2a","hash":"0x1234...","timestamp":"0x60000000","transactions":[]}"#.to_string()
        }

        // `cast receipt`
        ["cast", "receipt", ..] => {
            r#"{"status":"0x1","transactionHash":"0xabc...","contractAddress":"0x5FbDB2315678afecb367f032d93F642f64180aa3","gasUsed":"0x5208"}"#.to_string()
        }

        // `cast call`
        ["cast", "call", ..] => "0x".to_string(),

        // -- IBC Relayer commands --

        // Cosmos Relayer (rly) commands
        ["rly", "config", "init", ..] => String::new(),
        ["rly", "chains", "add", ..] => String::new(),
        ["rly", "keys", "add", ..] => {
            r#"{"address":"cosmos1mockrelayerkey123456789abcdef"}"#.to_string()
        }
        ["rly", "keys", "restore", ..] => String::new(),
        ["rly", "paths", "new", ..] => String::new(),
        ["rly", "tx", "clients", ..] => String::new(),
        ["rly", "tx", "connection", ..] => String::new(),
        ["rly", "tx", "channel", ..] => String::new(),
        ["rly", "tx", "link", ..] => String::new(),
        ["rly", "tx", "update-clients", ..] => String::new(),
        ["rly", "tx", "flush", ..] => String::new(),
        ["rly", "start", ..] => String::new(),
        ["rly", "q", "channels", ..] => {
            r#"[{"state":"STATE_OPEN","ordering":"ORDER_UNORDERED","version":"ics20-1","port_id":"transfer","channel_id":"channel-0","connection_hops":["connection-0"],"counterparty":{"port_id":"transfer","channel_id":"channel-0"}}]"#.to_string()
        }
        ["rly", "q", "connections", ..] => {
            r#"[{"id":"connection-0","client_id":"07-tendermint-0","state":"STATE_OPEN","counterparty_client_id":"07-tendermint-0","counterparty_connection_id":"connection-0"}]"#.to_string()
        }

        // Hermes commands
        ["hermes", "--json", "keys", "add", ..] | ["hermes", "keys", "add", ..] => {
            r#"{"result":{"account":"cosmos1mockhermes123456789"}}"#.to_string()
        }
        ["hermes", "--json", "create", "client", ..] | ["hermes", "create", "client", ..] => {
            r#"{"result":{"CreateClient":{"client_id":"07-tendermint-0"}}}"#.to_string()
        }
        ["hermes", "--json", "create", "connection", ..] | ["hermes", "create", "connection", ..] => {
            r#"{"result":{"a_side":{"connection_id":"connection-0"},"b_side":{"connection_id":"connection-0"}}}"#.to_string()
        }
        ["hermes", "--json", "create", "channel", ..] | ["hermes", "create", "channel", ..] => {
            r#"{"result":{"a_side":{"channel_id":"channel-0"},"b_side":{"channel_id":"channel-0"}}}"#.to_string()
        }
        ["hermes", "--json", "update", ..] | ["hermes", "update", ..] => String::new(),
        ["hermes", "--json", "clear", ..] | ["hermes", "clear", ..] => String::new(),
        ["hermes", "--json", "query", "channels", ..] | ["hermes", "query", "channels", ..] => {
            r#"{"result":{"state":"STATE_OPEN","ordering":"ORDER_UNORDERED","version":"ics20-1","port_id":"transfer","channel_id":"channel-0","connection_hops":["connection-0"],"counterparty":{"port_id":"transfer","channel_id":"channel-0"}}}"#.to_string()
        }
        ["hermes", "--json", "query", "connections", ..] | ["hermes", "query", "connections", ..] => {
            r#"{"result":{"id":"connection-0","client_id":"07-tendermint-0","state":"STATE_OPEN","counterparty_client_id":"07-tendermint-0","counterparty_connection_id":"connection-0"}}"#.to_string()
        }
        ["hermes", ..] => String::new(),

        // Mkdir (used by relayer init)
        ["mkdir", ..] => String::new(),

        // True (no-op command)
        ["true", ..] => String::new(),

        // Default: empty success
        _ => String::new(),
    }
}
