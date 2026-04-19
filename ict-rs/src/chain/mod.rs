pub mod cosmos;
#[cfg(feature = "ethereum")]
pub mod ethereum;
#[cfg(feature = "akash")]
pub mod akash;
#[cfg(feature = "akash")]
pub mod akash_oracle;
#[cfg(feature = "terp")]
pub mod terp;
pub mod penumbra;

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::auth::Authenticator;
use crate::error::{IctError, Result};
use crate::runtime::DockerImage;
use crate::tx::{ExecOutput, PacketAcknowledgement, PacketTimeout, Tx, TransferOptions, TxOptions, WalletAmount};
use crate::tx_builder::TxBuilder;
use crate::wallet::Wallet;

/// Supported chain ecosystems.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChainType {
    Cosmos,
    Ethereum,
    Penumbra,
    Polkadot,
    Thorchain,
    Utxo,
    Namada,
}

/// Signing algorithm used by the chain.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum SigningAlgorithm {
    #[default]
    Secp256k1,
    Ed25519,
}

/// How genesis commands are structured in the chain binary.
///
/// Cosmos SDK 0.50+ moved `init`, `add-genesis-account`, and `collect-gentxs`
/// under the `genesis` subcommand and shortened some names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GenesisStyle {
    /// Traditional: `init`, `add-genesis-account`, `collect-gentxs`
    #[default]
    Legacy,
    /// Cosmos SDK 0.50+: `genesis init`, `genesis add-account`, `genesis collect`
    Modern,
}

/// Sidecar process configuration (e.g., oracle, price feeder, hash-market).
///
/// Mirrors Go ICT's `SidecarConfig` in `ibc/types.go` with health-check support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarConfig {
    /// Process name (Go: `ProcessName`). Used in container naming.
    pub name: String,
    /// Docker image to run.
    pub image: DockerImage,
    /// Home directory inside the container (for config injection).
    pub home_dir: String,
    /// Container ports to expose.
    pub ports: Vec<String>,
    /// Environment variables.
    pub env: Vec<(String, String)>,
    /// Start command (Go: `StartCmd`).
    pub cmd: Vec<String>,
    /// If true, start this sidecar *before* the chain starts.
    /// If false (default), start after the chain is producing blocks.
    pub pre_start: bool,
    /// If true, one sidecar per validator node.
    /// If false (default), one sidecar per chain.
    pub validator_process: bool,
    /// Optional HTTP health endpoint (e.g., "/health") on the first exposed port.
    /// When set, the framework polls this endpoint until it returns 200 or the timeout expires.
    pub health_endpoint: Option<String>,
    /// Seconds to wait for the sidecar to become ready (default: 30).
    pub ready_timeout_secs: u64,
}

/// Optional in-container faucet configuration.
///
/// When set, the framework creates a funded faucet key during genesis,
/// exposes the faucet port from chain containers, and starts the faucet
/// process after the chain begins producing blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaucetConfig {
    /// Key name for the faucet account (created during genesis). Default: "faucet".
    pub key_name: String,
    /// Container port the faucet listens on. Default: 5000.
    pub port: u16,
    /// Command to start the faucet (run in background inside the container).
    /// Default: ["node", "/code/faucet_server.js"]
    pub start_cmd: Vec<String>,
    /// Environment variables for the faucet process.
    pub env: Vec<(String, String)>,
    /// Optional BIP39 mnemonic. When set, the key is recovered from this
    /// mnemonic instead of generating a random one. Useful when the test
    /// needs a deterministic faucet address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mnemonic: Option<String>,
    /// Optional coins string for the genesis account (e.g. `"100uakt,50uact"`).
    /// When set, overrides the default `{genesis_amount}{denom}` formula.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coins: Option<String>,
}

impl Default for FaucetConfig {
    fn default() -> Self {
        Self {
            key_name: "faucet".to_string(),
            port: 5000,
            start_cmd: vec!["node".to_string(), "/code/faucet_server.js".to_string()],
            env: vec![
                ("FAUCET_WALLET_NAME".to_string(), "faucet".to_string()),
                ("FAUCET_AMOUNT".to_string(), "1000000000".to_string()),
                ("DENOMS".to_string(), "uterp".to_string()),
            ],
            mnemonic: None,
            coins: None,
        }
    }
}

/// Genesis-level configuration overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GenesisConfig {
    pub accounts: Vec<GenesisAccount>,
    pub module_overrides: HashMap<String, serde_json::Value>,
}

/// A genesis account with initial balance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisAccount {
    pub address: String,
    pub coins: Vec<WalletAmount>,
}

/// Full configuration for a chain instance.
///
/// Mirrors Go ICT's `ibc.ChainConfig` with Rust-native types.
pub struct ChainConfig {
    pub chain_type: ChainType,
    pub name: String,
    pub chain_id: String,
    pub images: Vec<DockerImage>,
    pub bin: String,
    pub bech32_prefix: String,
    pub denom: String,
    pub coin_type: u32,
    pub signing_algorithm: SigningAlgorithm,
    pub gas_prices: String,
    pub gas_adjustment: f64,
    pub trusting_period: String,
    /// CometBFT block time (timeout_commit & timeout_propose). Default: "2s".
    pub block_time: String,
    pub genesis: Option<GenesisConfig>,
    /// Modify raw genesis JSON after initial generation.
    pub modify_genesis: Option<Box<dyn Fn(&ChainConfig, Vec<u8>) -> Result<Vec<u8>> + Send + Sync>>,
    /// Hook called before gentx (e.g., to add custom genesis state).
    pub pre_genesis: Option<Box<dyn Fn(&dyn Chain) -> Result<()> + Send + Sync>>,
    pub config_file_overrides: HashMap<String, serde_json::Value>,
    pub additional_start_args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub sidecar_configs: Vec<SidecarConfig>,
    /// Optional in-container faucet. See [`FaucetConfig`].
    pub faucet: Option<FaucetConfig>,
    /// Genesis command style. See [`GenesisStyle`].
    pub genesis_style: GenesisStyle,
}

impl std::fmt::Debug for ChainConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChainConfig")
            .field("chain_type", &self.chain_type)
            .field("name", &self.name)
            .field("chain_id", &self.chain_id)
            .field("bin", &self.bin)
            .field("denom", &self.denom)
            .finish_non_exhaustive()
    }
}

/// Context passed to chain initialization.
pub struct TestContext {
    pub test_name: String,
    pub network_id: String,
}

/// The core blockchain abstraction. Every chain type (Cosmos, Ethereum, etc.)
/// implements this trait.
///
/// Mirrors Go ICT's `ibc.Chain` interface.
#[async_trait]
pub trait Chain: Send + Sync {
    /// Get the chain's configuration.
    fn config(&self) -> &ChainConfig;

    /// Get the chain ID.
    fn chain_id(&self) -> &str;

    // -- Lifecycle --

    /// Initialize the chain (create containers, volumes, configs).
    async fn initialize(&mut self, ctx: &TestContext) -> Result<()>;

    /// Start the chain with optional genesis-funded wallets.
    async fn start(&mut self, genesis_wallets: &[WalletAmount]) -> Result<()>;

    /// Stop all chain processes.
    async fn stop(&mut self) -> Result<()>;

    // -- Execution --

    /// Execute a command on the chain's primary node.
    async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> Result<ExecOutput>;

    /// Execute a chain CLI command (prepends chain binary and appends --home).
    ///
    /// Equivalent to `ChainNode::exec_cmd()` but callable from the `Chain` trait.
    /// Use this for extension traits (CosmWasmExt, GovernanceExt, etc.) that need
    /// the chain binary prefix — `exec()` passes raw commands without it.
    async fn chain_exec(&self, args: &[&str]) -> Result<ExecOutput> {
        let bin = self.config().bin.clone();
        let home = self.home_dir().to_string();
        let mut cmd: Vec<String> = Vec::with_capacity(args.len() + 3);
        cmd.push(bin);
        cmd.extend(args.iter().map(|s| s.to_string()));
        cmd.push("--home".to_string());
        cmd.push(home);
        let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        self.exec(&cmd_refs, &[]).await
    }

    /// Build default [`TxOptions`] from this chain's config.
    fn default_tx_opts(&self) -> TxOptions {
        let cfg = self.config();
        TxOptions::new(&cfg.chain_id, &cfg.gas_prices)
            .gas_adjustment(cfg.gas_adjustment)
    }

    /// Execute a `tx` subcommand with default tx options appended.
    ///
    /// Convenience wrapper: `chain_exec(args ++ default_tx_opts().to_flags())`.
    async fn chain_exec_tx(&self, args: &[&str]) -> Result<ExecOutput> {
        self.chain_exec_tx_with(args, self.default_tx_opts()).await
    }

    /// Execute a `tx` subcommand with custom [`TxOptions`].
    async fn chain_exec_tx_with(&self, args: &[&str], opts: TxOptions) -> Result<ExecOutput> {
        let flags = opts.to_flags();
        let mut full: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        full.extend(flags);
        let refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
        self.chain_exec(&refs).await
    }

    /// Build a [`TxBuilder`] for programmatic transaction construction.
    ///
    /// Uses the provided [`Authenticator`] for signing and this chain's
    /// host RPC endpoint for broadcasting.
    fn tx_builder<'a>(&'a self, signer: &'a dyn Authenticator) -> TxBuilder<'a> {
        TxBuilder::new(self.config(), signer, &self.host_rpc_address())
    }

    // -- Endpoints --

    /// RPC address accessible from within the Docker network.
    fn rpc_address(&self) -> &str;

    /// gRPC address accessible from within the Docker network.
    fn grpc_address(&self) -> &str;

    /// RPC address accessible from the host machine.
    fn host_rpc_address(&self) -> String;

    /// gRPC address accessible from the host machine.
    fn host_grpc_address(&self) -> String;

    /// Chain home directory inside the container.
    fn home_dir(&self) -> &str;

    // -- Keys & Wallets --

    /// Create a new key with the given name.
    async fn create_key(&self, key_name: &str) -> Result<()>;

    /// Recover a key from a BIP39 mnemonic.
    async fn recover_key(&self, name: &str, mnemonic: &str) -> Result<()>;

    /// Get the raw address bytes for a named key.
    async fn get_address(&self, key_name: &str) -> Result<Vec<u8>>;

    /// Build a wallet from a key name and optional mnemonic.
    async fn build_wallet(
        &self,
        key_name: &str,
        mnemonic: &str,
    ) -> Result<Box<dyn Wallet>>;

    // -- Funds --

    /// Send tokens from one account to another. Returns the tx hash.
    async fn send_funds(&self, key_name: &str, amount: &WalletAmount) -> Result<String>;

    /// Query the balance of an address for a given denom.
    async fn get_balance(&self, address: &str, denom: &str) -> Result<u128>;

    // -- IBC --

    /// Send an IBC transfer and return the resulting transaction.
    async fn send_ibc_transfer(
        &self,
        channel_id: &str,
        key_name: &str,
        amount: &WalletAmount,
        options: &TransferOptions,
    ) -> Result<Tx>;

    // -- State --

    /// Get the current block height.
    async fn height(&self) -> Result<u64>;

    /// Export chain state at a given height as JSON.
    async fn export_state(&self, height: u64) -> Result<String>;

    /// Get packet acknowledgements at a given height.
    async fn acknowledgements(&self, height: u64) -> Result<Vec<PacketAcknowledgement>>;

    /// Get packet timeouts at a given height.
    async fn timeouts(&self, height: u64) -> Result<Vec<PacketTimeout>>;

    // -- Sidecars --

    /// Start all sidecars attached to this chain. Default no-op.
    async fn start_sidecars(&mut self) -> Result<()> {
        Ok(())
    }

    /// Stop all sidecars attached to this chain. Default no-op.
    async fn stop_sidecars(&mut self) -> Result<()> {
        Ok(())
    }

    /// Execute a command inside a named sidecar. Default returns error.
    async fn exec_sidecar(
        &self,
        sidecar_name: &str,
        _cmd: &[&str],
        _env: &[(&str, &str)],
    ) -> Result<ExecOutput> {
        Err(IctError::Config(format!(
            "no sidecar '{}' on this chain",
            sidecar_name
        )))
    }

    /// Get the hostname of a named sidecar for Docker network DNS. Default returns None.
    fn sidecar_hostname(&self, _sidecar_name: &str) -> Option<String> {
        None
    }
}
