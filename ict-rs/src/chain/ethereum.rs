//! Ethereum/Anvil chain implementation.
//!
//! Deploys a Foundry Anvil node via Docker and interacts with it using
//! Foundry's `cast` CLI — matching Go ICT's pattern of no ethers/alloy dependency.

#![cfg(feature = "ethereum")]

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, info};

use crate::chain::{Chain, ChainConfig, TestContext};
use crate::error::{IctError, Result};
use crate::runtime::{
    ContainerId, ContainerOptions, DockerImage, NetworkId, PortBinding, RuntimeBackend, VolumeMount,
};
use crate::tx::{ExecOutput, PacketAcknowledgement, PacketTimeout, Tx, TransferOptions, WalletAmount};
use crate::wallet::{EthWallet, Wallet};

/// Anvil's default mnemonic (deterministic accounts).
pub const ANVIL_MNEMONIC: &str =
    "test test test test test test test test test test test junk";

/// Default chain ID for Anvil.
pub const ANVIL_CHAIN_ID: u64 = 31337;

/// Default RPC port.
pub const ANVIL_RPC_PORT: u16 = 8545;

/// Default block time in seconds.
pub const ANVIL_BLOCK_TIME: u64 = 2;

/// Number of pre-funded accounts.
pub const ANVIL_NUM_ACCOUNTS: usize = 10;

/// Anvil's 10 default pre-funded accounts (address, private_key).
/// Generated from the default mnemonic with 10000 ETH each.
pub const ANVIL_DEFAULT_ACCOUNTS: [(&str, &str); 10] = [
    ("0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266", "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"),
    ("0x70997970C51812dc3A010C7d01b50e0d17dc79C8", "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d"),
    ("0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC", "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a"),
    ("0x90F79bf6EB2c4f870365E785982E1f101E93b906", "0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6"),
    ("0x15d34AAf54267DB7D7c367839AAf71A00a2C6A65", "0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a"),
    ("0x9965507D1a55bcC2695C58ba16FB37d819B0A4dc", "0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba"),
    ("0x976EA74026E726554dB657fA54763abd0C3a0aa9", "0x92db14e403b83dfe3df233f83dfa3a0d7096f21ca9b0d6d6b8d88b2b4ec1564e"),
    ("0x14dC79964da2C08dA15Fd353d30d65197b998505", "0x4bbbf85ce3377467afe5d46f804f221813b2bb87f24d81f60f1fcdbf7cbf4356"),
    ("0x23618e81E3f5cdF7f54C3d65f7FBc0aBf5B21E8f", "0xdbda1821b80551c9d65939329250298aa3472ba22feea921c0cf5d620ea67b97"),
    ("0xa0Ee7A142d267C1f36714E4a8F75612F20a79720", "0x2a871d0798f97d79848a013d4936a73bf4cc922c825d33c1cf7073dff6d409c6"),
];

/// A pre-funded Anvil account.
#[derive(Debug, Clone)]
pub struct AnvilAccount {
    pub address: String,
    pub private_key: String,
    pub index: usize,
}

/// An Ethereum chain backed by Foundry's Anvil.
pub struct AnvilChain {
    cfg: ChainConfig,
    runtime: Arc<dyn RuntimeBackend>,
    container_id: Option<ContainerId>,
    network_id: Option<NetworkId>,
    volume_name: Option<String>,
    test_name: String,
    hostname: String,
    initialized: bool,
    accounts: Vec<AnvilAccount>,
    rpc_port: u16,
}

impl std::fmt::Debug for AnvilChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnvilChain")
            .field("chain_id", &self.cfg.chain_id)
            .field("hostname", &self.hostname)
            .field("initialized", &self.initialized)
            .field("accounts", &self.accounts.len())
            .finish_non_exhaustive()
    }
}

impl AnvilChain {
    /// Create a new AnvilChain (does not start containers).
    pub fn new(cfg: ChainConfig, runtime: Arc<dyn RuntimeBackend>) -> Self {
        let accounts: Vec<AnvilAccount> = ANVIL_DEFAULT_ACCOUNTS
            .iter()
            .enumerate()
            .map(|(i, (addr, key))| AnvilAccount {
                address: addr.to_string(),
                private_key: key.to_string(),
                index: i,
            })
            .collect();

        Self {
            cfg,
            runtime,
            container_id: None,
            network_id: None,
            volume_name: None,
            test_name: String::new(),
            hostname: String::new(),
            initialized: false,
            accounts,
            rpc_port: ANVIL_RPC_PORT,
        }
    }

    /// Get the pre-funded accounts.
    pub fn accounts(&self) -> &[AnvilAccount] {
        &self.accounts
    }

    /// Get an EthWallet for a given account index.
    pub fn wallet_for_account(&self, index: usize) -> Option<EthWallet> {
        self.accounts.get(index).map(|acct| {
            EthWallet::from_anvil_account(acct.index, &acct.private_key, &acct.address)
        })
    }

    /// Get all container IDs (for cleanup).
    pub fn container_ids(&self) -> Vec<ContainerId> {
        self.container_id.iter().cloned().collect()
    }

    /// Get the Docker network ID (for attaching sidecars to the same network).
    pub fn network_id(&self) -> Option<&NetworkId> {
        self.network_id.as_ref()
    }

    // -- Cast CLI helpers --

    /// Execute a `cast` command inside the Anvil container.
    pub async fn exec_cast(&self, args: &[&str]) -> Result<ExecOutput> {
        let id = self.container_id.as_ref().ok_or_else(|| {
            IctError::Chain {
                chain_id: self.cfg.chain_id.clone(),
                source: anyhow::anyhow!("anvil container not started"),
            }
        })?;

        let mut cmd = vec!["cast"];
        cmd.extend_from_slice(args);

        debug!(chain = %self.cfg.chain_id, cmd = ?cmd, "Executing cast command");
        self.runtime.exec_in_container(id, &cmd, &[]).await
    }

    /// Get the ETH balance of an address (in wei).
    pub async fn get_eth_balance(&self, address: &str) -> Result<String> {
        let rpc = self.internal_rpc_url();
        let output = self
            .exec_cast(&["balance", address, "--rpc-url", &rpc])
            .await?;
        Ok(output.stdout_str().trim().to_string())
    }

    /// Send ETH from one account to another.
    pub async fn send_eth(
        &self,
        from_private_key: &str,
        to: &str,
        value: &str,
    ) -> Result<ExecOutput> {
        let rpc = self.internal_rpc_url();
        self.exec_cast(&[
            "send",
            "--private-key",
            from_private_key,
            to,
            "--value",
            value,
            "--rpc-url",
            &rpc,
            "--json",
        ])
        .await
    }

    /// Get the current block number.
    pub async fn block_number(&self) -> Result<u64> {
        let rpc = self.internal_rpc_url();
        let output = self
            .exec_cast(&["block-number", "--rpc-url", &rpc])
            .await?;
        let s = output.stdout_str();
        let s = s.trim();
        // Handle hex (0x...) or decimal
        if let Some(hex_str) = s.strip_prefix("0x") {
            u64::from_str_radix(hex_str, 16)
        } else {
            s.parse::<u64>()
        }
        .map_err(|e| IctError::Chain {
            chain_id: self.cfg.chain_id.clone(),
            source: anyhow::anyhow!("failed to parse block number '{}': {}", s, e),
        })
    }

    /// Get a block by number (JSON output).
    pub async fn get_block_by_number(&self, number: u64) -> Result<String> {
        let rpc = self.internal_rpc_url();
        let num_str = number.to_string();
        let output = self
            .exec_cast(&["block", &num_str, "--json", "--rpc-url", &rpc])
            .await?;
        Ok(output.stdout_str())
    }

    /// Deploy a contract from bytecode. Returns the tx output (contains contract address).
    pub async fn deploy_contract(
        &self,
        from_private_key: &str,
        bytecode: &str,
    ) -> Result<ExecOutput> {
        let rpc = self.internal_rpc_url();
        self.exec_cast(&[
            "send",
            "--private-key",
            from_private_key,
            "--create",
            bytecode,
            "--rpc-url",
            &rpc,
            "--json",
        ])
        .await
    }

    /// Call a contract (read-only, no state change).
    pub async fn call_contract(&self, to: &str, calldata: &str) -> Result<String> {
        let rpc = self.internal_rpc_url();
        let output = self
            .exec_cast(&["call", to, calldata, "--rpc-url", &rpc])
            .await?;
        Ok(output.stdout_str().trim().to_string())
    }

    /// Send a transaction to a contract (state-changing).
    pub async fn send_transaction(
        &self,
        from_private_key: &str,
        to: &str,
        calldata: &str,
        value: Option<&str>,
    ) -> Result<ExecOutput> {
        let rpc = self.internal_rpc_url();
        let mut args = vec![
            "send",
            "--private-key",
            from_private_key,
            to,
            calldata,
            "--rpc-url",
            &rpc,
            "--json",
        ];
        if let Some(v) = value {
            args.push("--value");
            args.push(v);
        }
        self.exec_cast(&args).await
    }

    /// Get transaction receipt.
    pub async fn get_receipt(&self, tx_hash: &str) -> Result<String> {
        let rpc = self.internal_rpc_url();
        let output = self
            .exec_cast(&["receipt", tx_hash, "--json", "--rpc-url", &rpc])
            .await?;
        Ok(output.stdout_str())
    }

    fn internal_rpc_url(&self) -> String {
        format!("http://{}:{}", self.hostname, self.rpc_port)
    }
}

#[async_trait]
impl Chain for AnvilChain {
    fn config(&self) -> &ChainConfig {
        &self.cfg
    }

    fn chain_id(&self) -> &str {
        &self.cfg.chain_id
    }

    async fn initialize(&mut self, ctx: &TestContext) -> Result<()> {
        if self.initialized {
            return Ok(());
        }

        self.test_name = ctx.test_name.clone();
        self.hostname = format!("{}-anvil-0", self.cfg.chain_id);
        let volume_name = format!("{}-{}", ctx.test_name, self.hostname);

        // Pre-cleanup: remove stale containers and network from a previous
        // failed run. Containers must be removed before the network since
        // Docker refuses to remove a network with active endpoints.
        {
            let container_name = format!("ict-{}-{}", ctx.test_name, self.hostname);
            let stale = crate::runtime::ContainerId(container_name);
            let _ = self.runtime.stop_container(&stale).await;
            let _ = self.runtime.remove_container(&stale).await;
            let _ = self.runtime.remove_network(&NetworkId(ctx.network_id.clone())).await;
        }

        info!(chain = %self.cfg.chain_id, "Creating Anvil network");
        let network_id = self.runtime.create_network(&ctx.network_id).await?;

        // Pull the Foundry image
        if let Some(image) = self.cfg.images.first() {
            self.runtime.pull_image(image).await?;
        }

        let image = self
            .cfg
            .images
            .first()
            .cloned()
            .unwrap_or_else(|| DockerImage {
                repository: "ghcr.io/foundry-rs/foundry".to_string(),
                version: "latest".to_string(),
                uid_gid: None,
            });

        let block_time = ANVIL_BLOCK_TIME.to_string();
        let num_accounts = ANVIL_NUM_ACCOUNTS.to_string();

        let opts = ContainerOptions {
            image,
            name: format!("ict-{}-{}", ctx.test_name, self.hostname),
            network_id: Some(network_id.clone()),
            env: Vec::new(),
            cmd: vec![
                "--host".to_string(),
                "0.0.0.0".to_string(),
                "--block-time".to_string(),
                block_time,
                "--accounts".to_string(),
                num_accounts,
            ],
            entrypoint: Some(vec!["anvil".to_string()]),
            ports: vec![PortBinding {
                host_port: 0,
                container_port: self.rpc_port,
                protocol: "tcp".to_string(),
            }],
            volumes: vec![VolumeMount {
                source: volume_name.clone(),
                target: "/anvil-data".to_string(),
                read_only: false,
            }],
            labels: vec![
                ("ict.test".to_string(), ctx.test_name.clone()),
                ("ict.chain_id".to_string(), self.cfg.chain_id.clone()),
                ("ict.chain_type".to_string(), "ethereum".to_string()),
            ],
            hostname: Some(self.hostname.clone()),
        };

        info!(chain = %self.cfg.chain_id, "Creating Anvil container");
        let container_id = self.runtime.create_container(&opts).await?;

        self.container_id = Some(container_id);
        self.network_id = Some(network_id);
        self.volume_name = Some(volume_name);
        self.initialized = true;

        Ok(())
    }

    async fn start(&mut self, _genesis_wallets: &[WalletAmount]) -> Result<()> {
        let container_id = self.container_id.as_ref().ok_or_else(|| {
            IctError::Chain {
                chain_id: self.cfg.chain_id.clone(),
                source: anyhow::anyhow!("anvil not initialized"),
            }
        })?;

        info!(chain = %self.cfg.chain_id, "Starting Anvil container");
        self.runtime.start_container(container_id).await?;

        // Poll until Anvil is ready
        info!(chain = %self.cfg.chain_id, "Waiting for Anvil to be ready");
        let mut attempts = 0;
        loop {
            match self.block_number().await {
                Ok(n) => {
                    info!(chain = %self.cfg.chain_id, block = n, "Anvil is ready");
                    break;
                }
                Err(_) if attempts < 30 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Err(_) => {
                    return Err(IctError::Timeout {
                        what: "Anvil readiness".to_string(),
                        duration: std::time::Duration::from_secs(15),
                    });
                }
            }
        }

        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        if let Some(id) = self.container_id.take() {
            info!(chain = %self.cfg.chain_id, "Stopping Anvil container");
            let _ = self.runtime.stop_container(&id).await;
            let _ = self.runtime.remove_container(&id).await;
        }
        if let Some(ref vol) = self.volume_name {
            let _ = self.runtime.remove_volume(vol).await;
        }
        if let Some(id) = self.network_id.take() {
            let _ = self.runtime.remove_network(&id).await;
        }
        Ok(())
    }

    async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> Result<ExecOutput> {
        let id = self.container_id.as_ref().ok_or_else(|| {
            IctError::Chain {
                chain_id: self.cfg.chain_id.clone(),
                source: anyhow::anyhow!("anvil container not started"),
            }
        })?;
        self.runtime.exec_in_container(id, cmd, env).await
    }

    fn rpc_address(&self) -> &str {
        // We return a static-ish reference; for the internal address we build it dynamically
        // but the trait requires &str. We store it in config name as a workaround.
        // Better: just return the hostname:port pattern
        ""
    }

    fn grpc_address(&self) -> &str {
        "" // N/A for Ethereum
    }

    fn host_rpc_address(&self) -> String {
        format!("http://localhost:{}", self.rpc_port)
    }

    fn host_grpc_address(&self) -> String {
        String::new() // N/A
    }

    fn home_dir(&self) -> &str {
        "/anvil-data"
    }

    async fn create_key(&self, _key_name: &str) -> Result<()> {
        // Anvil uses pre-funded accounts, not keyring
        Ok(())
    }

    async fn recover_key(&self, _name: &str, _mnemonic: &str) -> Result<()> {
        Ok(())
    }

    async fn get_address(&self, key_name: &str) -> Result<Vec<u8>> {
        // Parse index from key_name like "anvil-0"
        if let Some(idx_str) = key_name.strip_prefix("anvil-") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if let Some(acct) = self.accounts.get(idx) {
                    let clean = acct.address.strip_prefix("0x").unwrap_or(&acct.address);
                    return hex::decode(clean).map_err(|e| IctError::Chain {
                        chain_id: self.cfg.chain_id.clone(),
                        source: e.into(),
                    });
                }
            }
        }
        Err(IctError::Wallet(format!("unknown key: {key_name}")))
    }

    async fn build_wallet(
        &self,
        key_name: &str,
        _mnemonic: &str,
    ) -> Result<Box<dyn Wallet>> {
        if let Some(idx_str) = key_name.strip_prefix("anvil-") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if let Some(wallet) = self.wallet_for_account(idx) {
                    return Ok(Box::new(wallet));
                }
            }
        }
        Err(IctError::Wallet(format!("unknown key: {key_name}")))
    }

    async fn send_funds(&self, key_name: &str, amount: &WalletAmount) -> Result<String> {
        // Find the private key for this key_name
        let private_key = if let Some(idx_str) = key_name.strip_prefix("anvil-") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                self.accounts
                    .get(idx)
                    .map(|a| a.private_key.clone())
                    .ok_or_else(|| IctError::Wallet(format!("account index {idx} out of range")))?
            } else {
                return Err(IctError::Wallet(format!("invalid key name: {key_name}")));
            }
        } else {
            // Treat as raw private key
            key_name.to_string()
        };

        let value_str = format!("{}wei", amount.amount);
        let output = self.send_eth(&private_key, &amount.address, &value_str).await?;
        // Parse tx hash from JSON output
        let stdout = output.stdout_str();
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&stdout) {
            if let Some(hash) = json["transactionHash"].as_str() {
                return Ok(hash.to_string());
            }
        }
        Ok(stdout.trim().to_string())
    }

    async fn get_balance(&self, address: &str, _denom: &str) -> Result<u128> {
        let balance_str = self.get_eth_balance(address).await?;
        balance_str
            .parse::<u128>()
            .map_err(|e| IctError::Chain {
                chain_id: self.cfg.chain_id.clone(),
                source: anyhow::anyhow!("failed to parse balance '{}': {}", balance_str, e),
            })
    }

    async fn send_ibc_transfer(
        &self,
        _channel_id: &str,
        _key_name: &str,
        _amount: &WalletAmount,
        _options: &TransferOptions,
    ) -> Result<Tx> {
        Err(IctError::Chain {
            chain_id: self.cfg.chain_id.clone(),
            source: anyhow::anyhow!("IBC transfers not supported on standalone Anvil"),
        })
    }

    async fn height(&self) -> Result<u64> {
        self.block_number().await
    }

    async fn export_state(&self, _height: u64) -> Result<String> {
        Err(IctError::Chain {
            chain_id: self.cfg.chain_id.clone(),
            source: anyhow::anyhow!("state export not supported on Anvil"),
        })
    }

    async fn acknowledgements(&self, _height: u64) -> Result<Vec<PacketAcknowledgement>> {
        Ok(Vec::new())
    }

    async fn timeouts(&self, _height: u64) -> Result<Vec<PacketTimeout>> {
        Ok(Vec::new())
    }
}
