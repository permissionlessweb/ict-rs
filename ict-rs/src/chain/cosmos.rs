use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::chain::{Chain, ChainConfig, ChainType, TestContext};
use crate::error::{IctError, Result};
use crate::node::ChainNode;
use crate::runtime::{NetworkId, RuntimeBackend};
use crate::sidecar::SidecarProcess;
use crate::tx::{
    ExecOutput, Packet, PacketAcknowledgement, PacketTimeout, TransferOptions, Tx, WalletAmount,
};
use crate::wallet::{KeyWallet, Wallet};

/// A Cosmos SDK chain running as one or more containerized nodes.
///
/// Mirrors Go ICT's `CosmosChain` with validators, full nodes, genesis pipeline,
/// key management, and IBC support.
pub struct CosmosChain {
    cfg: ChainConfig,
    num_validators: usize,
    num_full_nodes: usize,
    validators: Vec<ChainNode>,
    full_nodes: Vec<ChainNode>,
    runtime: Arc<dyn RuntimeBackend>,
    network_id: Option<NetworkId>,
    test_name: String,
    initialized: bool,
    /// Cached Docker-network-accessible RPC address (set after nodes are created).
    internal_rpc: String,
    /// Cached Docker-network-accessible gRPC address (set after nodes are created).
    internal_grpc: String,
    /// Sidecar processes attached to this chain (chain-level and validator-level).
    pub sidecars: Vec<SidecarProcess>,
}

impl CosmosChain {
    /// Create a new Cosmos chain configuration.
    pub fn new(
        cfg: ChainConfig,
        num_validators: usize,
        num_full_nodes: usize,
        runtime: Arc<dyn RuntimeBackend>,
    ) -> Self {
        assert_eq!(cfg.chain_type, ChainType::Cosmos);
        Self {
            cfg,
            num_validators: if num_validators == 0 { 1 } else { num_validators },
            num_full_nodes,
            validators: Vec::new(),
            full_nodes: Vec::new(),
            runtime,
            network_id: None,
            test_name: String::new(),
            initialized: false,
            internal_rpc: "http://localhost:26657".to_string(),
            internal_grpc: "localhost:9090".to_string(),
            sidecars: Vec::new(),
        }
    }

    /// Get the primary validator node (index 0).
    pub fn primary_node(&self) -> Result<&ChainNode> {
        self.validators.first().ok_or_else(|| IctError::Chain {
            chain_id: self.cfg.chain_id.clone(),
            source: anyhow::anyhow!("no validators available"),
        })
    }

    /// Get a mutable reference to the primary validator node.
    fn primary_node_mut(&mut self) -> Result<&mut ChainNode> {
        self.validators.first_mut().ok_or_else(|| IctError::Chain {
            chain_id: self.cfg.chain_id.clone(),
            source: anyhow::anyhow!("no validators available"),
        })
    }

    /// Get all validator nodes.
    pub fn validators(&self) -> &[ChainNode] {
        &self.validators
    }

    /// Get all full nodes.
    pub fn full_nodes(&self) -> &[ChainNode] {
        &self.full_nodes
    }

    /// Create node objects (does not start containers).
    fn create_nodes(&mut self) {
        let image = self.cfg.images.first().cloned().unwrap_or_else(|| {
            crate::runtime::DockerImage {
                repository: "ghcr.io/strangelove-ventures/heighliner".to_string(),
                version: "latest".to_string(),
                uid_gid: None,
            }
        });

        let network_id = self
            .network_id
            .as_ref()
            .map(|n| n.0.clone())
            .unwrap_or_default();

        let faucet_port = self.cfg.faucet.as_ref().map(|f| f.port);

        // Create validator nodes (faucet port only on primary validator)
        for i in 0..self.num_validators {
            let node = ChainNode::new(
                i,
                true,
                &self.cfg.chain_id,
                &self.cfg.bin,
                image.clone(),
                &self.test_name,
                &network_id,
                self.runtime.clone(),
                if i == 0 { faucet_port } else { None },
                self.cfg.genesis_style,
                &self.cfg.gas_prices,
                self.cfg.gas_adjustment,
            );
            self.validators.push(node);
        }

        // Create full nodes
        for i in 0..self.num_full_nodes {
            let node = ChainNode::new(
                i,
                false,
                &self.cfg.chain_id,
                &self.cfg.bin,
                image.clone(),
                &self.test_name,
                &network_id,
                self.runtime.clone(),
                None,
                self.cfg.genesis_style,
                &self.cfg.gas_prices,
                self.cfg.gas_adjustment,
            );
            self.full_nodes.push(node);
        }
    }

    /// Initialize all nodes: create containers, init home dirs.
    async fn init_nodes(&mut self) -> Result<()> {
        // Pull the image once
        if let Some(image) = self.cfg.images.first() {
            self.runtime.pull_image(image).await?;
        }

        // Create containers for all nodes
        let all_nodes = self
            .validators
            .iter_mut()
            .chain(self.full_nodes.iter_mut());

        for node in all_nodes {
            node.create_container().await?;
            node.start_container().await?;
            if let Some(ref id) = node.container_id {
                info!(
                    container = %id.0,
                    hostname = %node.hostname,
                    "Spawned test container"
                );
            }
        }

        // Init home directories
        for (i, node) in self.validators.iter().enumerate() {
            let moniker = format!("{}-val-{}", self.cfg.chain_id, i);
            let output = node.init_home(&moniker).await?;
            if output.exit_code != 0 {
                return Err(IctError::ExecFailed {
                    exit_code: output.exit_code,
                    stderr: output.stderr_str(),
                });
            }
        }

        for (i, node) in self.full_nodes.iter().enumerate() {
            let moniker = format!("{}-fn-{}", self.cfg.chain_id, i);
            let output = node.init_home(&moniker).await?;
            if output.exit_code != 0 {
                return Err(IctError::ExecFailed {
                    exit_code: output.exit_code,
                    stderr: output.stderr_str(),
                });
            }
        }

        Ok(())
    }

    /// Apply config_file_overrides to all nodes' TOML config files.
    ///
    /// Mirrors Go ICT's `ModifyTomlConfigFile` + `RecursiveModifyToml`.
    /// Keys in `config_file_overrides` are relative file paths (e.g. "config/app.toml"),
    /// values are JSON objects that get deep-merged into the existing TOML.
    ///
    /// Called after `init_nodes()` (which runs `chain init`) and before
    /// `genesis_pipeline()`, matching Go ICT's ordering.
    async fn apply_config_overrides(&self) -> Result<()> {
        if self.cfg.config_file_overrides.is_empty() {
            return Ok(());
        }

        let all_nodes: Vec<&ChainNode> = self
            .validators
            .iter()
            .chain(self.full_nodes.iter())
            .collect();

        for (file_path, overrides) in &self.cfg.config_file_overrides {
            for node in &all_nodes {
                let node_abs = if file_path.starts_with('/') {
                    file_path.clone()
                } else {
                    format!("{}/{}", node.home_dir, file_path)
                };

                // Read existing TOML
                let output = node.exec_raw(&["cat", &node_abs], &[]).await?;
                let existing = String::from_utf8_lossy(&output.stdout).to_string();

                // Parse existing TOML into a mutable table
                let mut table: toml::Table = toml::from_str(&existing).map_err(|e| {
                    IctError::Config(format!("failed to parse {}: {}", node_abs, e))
                })?;

                // Deep-merge JSON overrides into the TOML table
                merge_json_into_toml(&mut table, overrides);

                // Serialize back to TOML
                let new_toml = toml::to_string_pretty(&table).map_err(|e| {
                    IctError::Config(format!("failed to serialize {}: {}", node_abs, e))
                })?;

                // Write back via base64 to handle special chars
                let encoded = base64_encode(new_toml.as_bytes());
                let write_cmd = format!(
                    "echo '{}' | base64 -d > {}",
                    encoded, node_abs
                );
                node.exec_raw(&["sh", "-c", &write_cmd], &[]).await?;

                debug!(
                    node = %node.hostname,
                    file = %file_path,
                    "Applied config override"
                );
            }
        }

        info!(
            chain_id = %self.cfg.chain_id,
            files = self.cfg.config_file_overrides.len(),
            "Config file overrides applied"
        );
        Ok(())
    }

    /// Run the genesis pipeline: create keys, add accounts, gentx, collect gentxs.
    ///
    /// Mirrors Go interchaintest's `cosmos_chain.go` Start() method.
    ///
    /// Critical pattern from Go ICT:
    /// 1. Each validator runs InitValidatorGenTx on its OWN node:
    ///    - CreateKey, AddGenesisAccount (locally), Gentx (locally)
    /// 2. Then validator 0 collects all: adds other validators' accounts to
    ///    its own genesis, copies their gentx files, runs collect-gentxs
    /// 3. Global "stake" → denom replacement
    /// 4. ModifyGenesis callback
    async fn genesis_pipeline(&self, genesis_wallets: &[WalletAmount]) -> Result<()> {
        // Run pre_genesis hook if set
        if let Some(ref hook) = self.cfg.pre_genesis {
            hook(self)?;
        }

        // Go ICT uses 10_000_000 * 10^coin_decimals for genesis account,
        // and 5_000_000 * 10^coin_decimals for self-delegation (gentx).
        let coin_decimals: u64 = 6; // standard Cosmos SDK micro-units
        let genesis_amount = 10_000_000u64 * 10u64.pow(coin_decimals as u32);
        let self_delegation = 5_000_000u64 * 10u64.pow(coin_decimals as u32);

        let key_name = "validator";
        let coin_type = self.cfg.coin_type;
        let denom = &self.cfg.denom;

        // Phase 1: InitValidatorGenTx on each validator's OWN node.
        // Each validator creates its key, adds its own genesis account to its own
        // genesis, and generates a gentx — all on its own container.
        // This matches Go ICT's InitValidatorGenTx().
        //
        // CRITICAL: Use the real denom (e.g. "uterp") for amounts and gentx,
        // NOT "stake". Go ICT does this because the sed "stake" → denom
        // replacement happens AFTER collect-gentxs. If we used "stake" in the
        // gentx and then sed-replaced it, the gentx signature would be
        // invalidated (signature was computed over "stake"-denominated tx body).
        for (i, node) in self.validators.iter().enumerate() {
            // 1a. Create key
            let output = node.create_key(key_name, coin_type).await?;
            if output.exit_code != 0 {
                return Err(IctError::ExecFailed {
                    exit_code: output.exit_code,
                    stderr: format!(
                        "create_key failed on validator {}: {}",
                        i,
                        output.stderr_str()
                    ),
                });
            }

            // 1b. Get bech32 address
            let address = node.get_key_address(key_name).await?;
            info!(validator = i, address = %address, "Validator key created");

            if address.is_empty() {
                return Err(IctError::Chain {
                    chain_id: self.cfg.chain_id.clone(),
                    source: anyhow::anyhow!(
                        "validator {} key address is empty after create_key",
                        i
                    ),
                });
            }

            // 1c. Add genesis account on THIS validator's OWN node (real denom)
            let coins = format!("{genesis_amount}{denom}");
            let output = node.add_genesis_account(&address, &coins).await?;
            if output.exit_code != 0 {
                return Err(IctError::ExecFailed {
                    exit_code: output.exit_code,
                    stderr: format!(
                        "add_genesis_account failed on validator {}: {}",
                        i,
                        output.stderr_str()
                    ),
                });
            }

            // 1d. Gentx on THIS validator's OWN node (real denom)
            let staking = format!("{self_delegation}{denom}");
            let output = node
                .gentx(key_name, &staking, &self.cfg.gas_prices, self.cfg.gas_adjustment)
                .await?;
            if output.exit_code != 0 {
                return Err(IctError::ExecFailed {
                    exit_code: output.exit_code,
                    stderr: format!(
                        "gentx failed on validator {}: {}",
                        i,
                        output.stderr_str()
                    ),
                });
            }
        }

        // Phase 2: Collect everything onto validator 0 (primary).
        let primary = self.primary_node()?;

        if self.validators.len() > 1 {
            let gentx_dir = format!("{}/config/gentx", primary.home_dir);
            for (i, node) in self.validators.iter().enumerate().skip(1) {
                // Add other validators' accounts to primary's genesis (real denom)
                let address = node.get_key_address(key_name).await?;
                let coins = format!("{genesis_amount}{denom}");
                let output = primary.add_genesis_account(&address, &coins).await?;
                if output.exit_code != 0 {
                    return Err(IctError::ExecFailed {
                        exit_code: output.exit_code,
                        stderr: format!(
                            "add_genesis_account on primary for validator {} failed: {}",
                            i,
                            output.stderr_str()
                        ),
                    });
                }

                // Copy gentx files from validator N to primary
                let src_dir = format!("{}/config/gentx", node.home_dir);
                let cmd =
                    format!("cp {src_dir}/*.json {gentx_dir}/ 2>/dev/null || true");
                primary.exec_raw(&["sh", "-c", &cmd], &[]).await?;
                debug!(validator = i, "Copied gentx to primary");
            }
        }

        // Add extra genesis wallets (test users, faucets, etc.)
        for wallet in genesis_wallets {
            let coins = format!("{}{}", wallet.amount, denom);
            primary
                .add_genesis_account(&wallet.address, &coins)
                .await?;
        }

        // Create faucet key and genesis account if faucet is configured
        if let Some(ref faucet_cfg) = self.cfg.faucet {
            // If a mnemonic is provided, recover the key from it (deterministic
            // address). Otherwise create a fresh random key.
            if let Some(ref mnemonic) = faucet_cfg.mnemonic {
                let output = primary.recover_key(&faucet_cfg.key_name, mnemonic).await?;
                if output.exit_code != 0 {
                    return Err(IctError::ExecFailed {
                        exit_code: output.exit_code,
                        stderr: format!(
                            "recover_key for faucet failed: {}",
                            output.stderr_str()
                        ),
                    });
                }
            } else {
                let output = primary.create_key(&faucet_cfg.key_name, coin_type).await?;
                if output.exit_code != 0 {
                    return Err(IctError::ExecFailed {
                        exit_code: output.exit_code,
                        stderr: format!(
                            "create_key for faucet failed: {}",
                            output.stderr_str()
                        ),
                    });
                }
            }
            let faucet_address = primary.get_key_address(&faucet_cfg.key_name).await?;
            info!(address = %faucet_address, "Faucet key created");

            // Use custom coins if provided, otherwise default genesis amount.
            let faucet_coins = faucet_cfg
                .coins
                .clone()
                .unwrap_or_else(|| format!("{genesis_amount}{denom}"));
            let output = primary.add_genesis_account(&faucet_address, &faucet_coins).await?;
            if output.exit_code != 0 {
                return Err(IctError::ExecFailed {
                    exit_code: output.exit_code,
                    stderr: format!(
                        "add_genesis_account for faucet failed: {}",
                        output.stderr_str()
                    ),
                });
            }
        }

        // Collect gentxs on primary node
        let output = primary.collect_gentxs().await?;
        if output.exit_code != 0 {
            return Err(IctError::ExecFailed {
                exit_code: output.exit_code,
                stderr: format!("collect-gentxs failed: {}", output.stderr_str()),
            });
        }

        // Phase 3: Replace "stake" with configured denom throughout genesis.
        // This is what Go ICT does at cosmos_chain.go:994.
        {
            let genesis_path = format!("{}/config/genesis.json", primary.home_dir);
            let cmd = format!(
                "sed -i 's/\"stake\"/\"{}\"/g' {}",
                self.cfg.denom, genesis_path
            );
            primary.exec_raw(&["sh", "-c", &cmd], &[]).await?;
        }

        // Apply genesis modifications if configured
        if let Some(ref modify) = self.cfg.modify_genesis {
            let output = primary
                .exec_raw(
                    &["cat", &format!("{}/config/genesis.json", primary.home_dir)],
                    &[],
                )
                .await?;
            let genesis_bytes = output.stdout;
            let modified = modify(&self.cfg, genesis_bytes)?;
            let genesis_b64 = base64_encode(&modified);
            let cmd = format!(
                "echo '{}' | base64 -d > {}/config/genesis.json",
                genesis_b64, primary.home_dir
            );
            primary.exec_raw(&["sh", "-c", &cmd], &[]).await?;
        }

        info!(chain_id = %self.cfg.chain_id, "Genesis pipeline complete");
        Ok(())
    }

    /// Copy genesis from the primary validator to all other nodes.
    ///
    /// Uses base64 encoding to safely transfer the genesis JSON through shell
    /// commands without issues from special characters in the JSON.
    async fn distribute_genesis(&self) -> Result<()> {
        let primary = self.primary_node()?;
        let genesis_path = format!("{}/config/genesis.json", primary.home_dir);

        // Read genesis from primary
        let output = primary.exec_raw(&["cat", &genesis_path], &[]).await?;
        let genesis_b64 = base64_encode(&output.stdout);

        // Distribute to other validators and full nodes
        let other_nodes = self
            .validators
            .iter()
            .skip(1)
            .chain(self.full_nodes.iter());

        for node in other_nodes {
            let target_path = format!("{}/config/genesis.json", node.home_dir);
            let cmd = format!(
                "echo '{}' | base64 -d > {}",
                genesis_b64, target_path
            );
            node.exec_raw(&["sh", "-c", &cmd], &[]).await?;
        }

        Ok(())
    }

    /// Configure persistent peers across all nodes.
    async fn configure_peers(&self) -> Result<()> {
        // Collect node IDs and P2P addresses
        let mut peers = Vec::new();
        for node in self.validators.iter().chain(self.full_nodes.iter()) {
            let output = node.exec_cmd(&["comet", "show-node-id"]).await?;
            let node_id = output.stdout_str().trim().to_string();
            if !node_id.is_empty() {
                peers.push(format!("{}@{}", node_id, node.p2p_address()));
            }
        }

        let peers_str = peers.join(",");
        debug!(peers = %peers_str, "Configuring persistent peers");

        // Set persistent peers on all nodes
        for node in self.validators.iter().chain(self.full_nodes.iter()) {
            let config_path = format!("{}/config/config.toml", node.home_dir);
            let cmd = format!(
                "sed -i 's/^persistent_peers = .*/persistent_peers = \"{}\"/' {}",
                peers_str, config_path
            );
            node.exec_raw(&["sh", "-c", &cmd], &[]).await?;

            // Bind RPC to 0.0.0.0 so it's accessible within the Docker network
            let rpc_cmd = format!(
                "sed -i 's#laddr = \"tcp://127.0.0.1:26657\"#laddr = \"tcp://0.0.0.0:26657\"#' {}",
                config_path
            );
            node.exec_raw(&["sh", "-c", &rpc_cmd], &[]).await?;

            // Also set minimum gas prices in app.toml
            let app_config = format!("{}/config/app.toml", node.home_dir);
            let gas_cmd = format!(
                "sed -i 's/^minimum-gas-prices = .*/minimum-gas-prices = \"{}\"/' {}",
                self.cfg.gas_prices, app_config
            );
            node.exec_raw(&["sh", "-c", &gas_cmd], &[]).await?;

            // Enable API server on 0.0.0.0
            let api_cmd = format!(
                "sed -i '/\\[api\\]/,/\\[/ s/enable = false/enable = true/' {} && \
                 sed -i '/\\[api\\]/,/\\[/ s#address = \"tcp://localhost:1317\"#address = \"tcp://0.0.0.0:1317\"#' {}",
                app_config, app_config
            );
            node.exec_raw(&["sh", "-c", &api_cmd], &[]).await?;

            // Enable gRPC server on 0.0.0.0:9090
            let grpc_cmd = format!(
                "sed -i '/\\[grpc\\]/,/\\[/ s/enable = false/enable = true/' {} && \
                 sed -i '/\\[grpc\\]/,/\\[/ s#address = \"localhost:9090\"#address = \"0.0.0.0:9090\"#' {}",
                app_config, app_config
            );
            node.exec_raw(&["sh", "-c", &grpc_cmd], &[]).await?;

            // Set consensus block time (timeout_commit and timeout_propose)
            let consensus_cmd = format!(
                "sed -i 's/^timeout_commit = .*/timeout_commit = \"{}\"/' {} && \
                 sed -i 's/^timeout_propose = .*/timeout_propose = \"{}\"/' {}",
                self.cfg.block_time, config_path, self.cfg.block_time, config_path
            );
            node.exec_raw(&["sh", "-c", &consensus_cmd], &[]).await?;
        }

        Ok(())
    }

    /// Initialize sidecar processes from config.
    ///
    /// Chain-level sidecars (`validator_process == false`) get one instance (index 0).
    /// Validator-level sidecars (`validator_process == true`) get one per validator.
    fn initialize_sidecars(&mut self) {
        let network_id = match &self.network_id {
            Some(id) => id.clone(),
            None => return,
        };

        for sc_cfg in &self.cfg.sidecar_configs {
            if sc_cfg.validator_process {
                // One sidecar per validator
                for i in 0..self.num_validators {
                    let sp = SidecarProcess::new(
                        sc_cfg.clone(),
                        i,
                        &self.cfg.chain_id,
                        &self.test_name,
                        self.runtime.clone(),
                        network_id.clone(),
                    );
                    self.sidecars.push(sp);
                }
            } else {
                // One sidecar per chain
                let sp = SidecarProcess::new(
                    sc_cfg.clone(),
                    0,
                    &self.cfg.chain_id,
                    &self.test_name,
                    self.runtime.clone(),
                    network_id.clone(),
                );
                self.sidecars.push(sp);
            }
        }

        if !self.sidecars.is_empty() {
            info!(
                chain_id = %self.cfg.chain_id,
                count = self.sidecars.len(),
                "Initialized sidecars"
            );
        }
    }

    /// Start all sidecars (or only pre-start / post-start depending on the flag).
    pub async fn start_all_sidecars(&mut self) -> Result<()> {
        for sidecar in &mut self.sidecars {
            sidecar.create_container().await?;
            sidecar.start_container().await?;
        }
        Ok(())
    }

    /// Start only sidecars matching the `pre_start` flag.
    pub async fn start_sidecars_filtered(&mut self, pre_start: bool) -> Result<()> {
        for sidecar in &mut self.sidecars {
            if sidecar.config.pre_start == pre_start {
                sidecar.create_container().await?;
                sidecar.start_container().await?;
            }
        }
        Ok(())
    }

    /// Stop all sidecars.
    pub async fn stop_all_sidecars(&mut self) -> Result<()> {
        for sidecar in &mut self.sidecars {
            sidecar.stop_container().await?;
        }
        Ok(())
    }

    /// Collect all node volume names.
    pub fn volume_names(&self) -> Vec<String> {
        self.validators
            .iter()
            .chain(self.full_nodes.iter())
            .map(|n| n.volume_name.clone())
            .collect()
    }

    /// Collect all container IDs.
    pub fn container_ids(&self) -> Vec<crate::runtime::ContainerId> {
        self.validators
            .iter()
            .chain(self.full_nodes.iter())
            .filter_map(|n| n.container_id.clone())
            .collect()
    }

    /// Expose the network ID.
    pub fn network_id(&self) -> Option<&NetworkId> {
        self.network_id.as_ref()
    }

    /// Get a reference to the runtime backend.
    pub fn runtime(&self) -> &Arc<dyn RuntimeBackend> {
        &self.runtime
    }

    /// Read the genesis.json from the primary node.
    pub async fn read_genesis(&self) -> Result<serde_json::Value> {
        self.primary_node()?.read_genesis().await
    }

    /// Validate genesis.json against the chain config.
    ///
    /// Checks that bond_denom matches configured denom, genesis accounts exist,
    /// and gentx entries are present. Returns the parsed genesis on success.
    pub async fn validate_genesis(&self) -> Result<serde_json::Value> {
        let genesis = self.read_genesis().await?;

        // Check bond_denom matches configured denom
        let bond_denom = genesis["app_state"]["staking"]["params"]["bond_denom"]
            .as_str()
            .unwrap_or("");
        if bond_denom != self.cfg.denom {
            return Err(IctError::Chain {
                chain_id: self.cfg.chain_id.clone(),
                source: anyhow::anyhow!(
                    "genesis bond_denom '{}' != configured denom '{}'",
                    bond_denom,
                    self.cfg.denom
                ),
            });
        }

        // Check genesis accounts exist
        let accounts = genesis["app_state"]["auth"]["accounts"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        if accounts == 0 {
            return Err(IctError::Chain {
                chain_id: self.cfg.chain_id.clone(),
                source: anyhow::anyhow!("genesis has no auth accounts"),
            });
        }

        // Check gentx entries exist
        let gen_txs = genesis["app_state"]["genutil"]["gen_txs"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        if gen_txs == 0 {
            return Err(IctError::Chain {
                chain_id: self.cfg.chain_id.clone(),
                source: anyhow::anyhow!("genesis has no gentx entries"),
            });
        }

        // Log genesis hash for debugging
        if let Ok(hash) = self.primary_node()?.genesis_hash().await {
            info!(
                chain_id = %self.cfg.chain_id,
                bond_denom = %bond_denom,
                accounts = accounts,
                gen_txs = gen_txs,
                genesis_hash = %hash,
                "Genesis validated"
            );
        }

        Ok(genesis)
    }

    /// Parse a tx response from JSON output.
    ///
    /// Delegates to [`crate::cli::parse_tx_response`].
    fn parse_tx_response(output: &ExecOutput) -> Result<Tx> {
        crate::cli::parse_tx_response(output)
    }

    // -- Upgrade Methods --

    /// Stop all node containers without removing volumes or network.
    ///
    /// Use this before [`upgrade_version`] + [`start_all_nodes`] for chain upgrades.
    pub async fn stop_all_nodes(&self) -> Result<()> {
        info!(chain_id = %self.cfg.chain_id, "Stopping all nodes for upgrade");
        for node in self.validators.iter().chain(self.full_nodes.iter()) {
            node.stop_container().await?;
        }
        Ok(())
    }

    /// Update the Docker image on all nodes. Call after [`stop_all_nodes`].
    pub fn upgrade_version(&mut self, repo: &str, version: &str) {
        let uid_gid = self.cfg.images.first().and_then(|i| i.uid_gid.clone());
        let new_image = crate::runtime::DockerImage {
            repository: repo.to_string(),
            version: version.to_string(),
            uid_gid,
        };
        // Update chain config
        if let Some(img) = self.cfg.images.first_mut() {
            *img = new_image.clone();
        }
        // Update all node images
        for node in self.validators.iter_mut().chain(self.full_nodes.iter_mut()) {
            node.image = new_image.clone();
        }
        info!(
            chain_id = %self.cfg.chain_id,
            image = %new_image,
            "Upgraded version on all nodes"
        );
    }

    /// Recreate containers with the current image and restart the chain.
    ///
    /// Call after [`upgrade_version`]. Preserves data volumes so chain state
    /// from the previous version carries over.
    pub async fn start_all_nodes(&mut self) -> Result<()> {
        // Pull the (possibly new) image
        if let Some(image) = self.cfg.images.first() {
            self.runtime.pull_image(image).await?;
        }

        // Remove old stopped containers, create new ones preserving volumes
        for node in self.validators.iter_mut().chain(self.full_nodes.iter_mut()) {
            node.remove_container().await?;
            node.create_container_for_upgrade().await?;
            node.start_container().await?;
        }

        // Start chain binary on all nodes
        for node in self.validators.iter().chain(self.full_nodes.iter()) {
            node.exec_start_chain().await?;
        }

        // Wait for chain to resume producing blocks
        let primary = self.primary_node()?;
        let mut attempts = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            match primary.query_height().await {
                Ok(h) if h > 0 => {
                    info!(height = h, "Chain resumed after upgrade");
                    break;
                }
                _ => {
                    attempts += 1;
                    if attempts > 60 {
                        return Err(IctError::Chain {
                            chain_id: self.cfg.chain_id.clone(),
                            source: anyhow::anyhow!(
                                "chain did not resume after upgrade within 60 seconds"
                            ),
                        });
                    }
                }
            }
        }

        Ok(())
    }

    /// Vote on a governance proposal from all validators.
    ///
    /// Each validator votes from its own node using the "validator" key name.
    /// Mirrors Go ICT's `VoteOnProposalAllValidators`.
    pub async fn vote_on_proposal_all_validators(
        &self,
        proposal_id: u64,
        option: &str,
    ) -> Result<()> {
        let prop_id_str = proposal_id.to_string();

        for (i, node) in self.validators.iter().enumerate() {
            let opts = node.default_tx_opts().from("validator");
            let output = node
                .exec_tx_with(
                    &["tx", "gov", "vote", &prop_id_str, option],
                    opts,
                )
                .await?;

            if output.exit_code != 0 {
                return Err(IctError::ExecFailed {
                    exit_code: output.exit_code,
                    stderr: format!(
                        "validator {} vote failed: {}",
                        i,
                        output.stderr_str()
                    ),
                });
            }
            info!(validator = i, proposal_id = proposal_id, "Voted {}", option);
        }
        Ok(())
    }
}

/// Deep-merge a `serde_json::Value` (JSON object) into a `toml::Table`.
///
/// Mirrors Go ICT's `RecursiveModifyToml`: nested objects are merged
/// recursively, leaf values are overwritten.
fn merge_json_into_toml(table: &mut toml::Table, json: &serde_json::Value) {
    if let serde_json::Value::Object(map) = json {
        for (key, value) in map {
            match value {
                serde_json::Value::Object(_) => {
                    // Recurse into nested section
                    let sub = table
                        .entry(key.clone())
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                    if let toml::Value::Table(sub_table) = sub {
                        merge_json_into_toml(sub_table, value);
                    }
                }
                _ => {
                    // Leaf value — convert JSON → TOML value
                    if let Some(tv) = json_value_to_toml(value) {
                        table.insert(key.clone(), tv);
                    }
                }
            }
        }
    }
}

/// Convert a leaf `serde_json::Value` to a `toml::Value`.
fn json_value_to_toml(v: &serde_json::Value) -> Option<toml::Value> {
    match v {
        serde_json::Value::Bool(b) => Some(toml::Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml::Value::Integer(i))
            } else if let Some(f) = n.as_f64() {
                Some(toml::Value::Float(f))
            } else {
                None
            }
        }
        serde_json::Value::String(s) => Some(toml::Value::String(s.clone())),
        serde_json::Value::Array(arr) => {
            let items: Vec<toml::Value> = arr.iter().filter_map(json_value_to_toml).collect();
            Some(toml::Value::Array(items))
        }
        serde_json::Value::Object(_) => {
            let mut t = toml::Table::new();
            merge_json_into_toml(&mut t, v);
            Some(toml::Value::Table(t))
        }
        serde_json::Value::Null => None,
    }
}

/// Simple base64 encoding without pulling in another crate.
fn base64_encode(data: &[u8]) -> String {
    use std::fmt::Write;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;

        let _ = write!(result, "{}", CHARS[(n >> 18 & 0x3F) as usize] as char);
        let _ = write!(result, "{}", CHARS[(n >> 12 & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            let _ = write!(result, "{}", CHARS[(n >> 6 & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            let _ = write!(result, "{}", CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[async_trait]
impl Chain for CosmosChain {
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

        // Create a Docker network for this chain.
        // Use ctx.network_id if provided (allows callers to guarantee uniqueness),
        // otherwise derive from test_name + chain_id.
        let network_name = if ctx.network_id.is_empty() {
            format!("ict-{}-{}", ctx.test_name, self.cfg.chain_id)
        } else {
            ctx.network_id.clone()
        };
        // Pre-cleanup: remove stale containers from a previous failed run.
        {
            let prefix = format!("ict-{}-{}", ctx.test_name, self.cfg.chain_id);
            let node_types = ["val", "fn"];
            for nt in &node_types {
                for i in 0..std::cmp::max(self.num_validators, self.num_full_nodes) {
                    let stale = crate::runtime::ContainerId(
                        format!("{}-{}-{}", prefix, nt, i),
                    );
                    let _ = self.runtime.stop_container(&stale).await;
                    let _ = self.runtime.remove_container(&stale).await;
                }
            }
            // Remove ALL orphaned ict-rs networks, not just this test's.
            // Safe because integration tests run sequentially (--test-threads=1),
            // so no other test should have an active network.
            let _ = self.runtime.remove_networks_by_prefix("ict-").await;
        }
        let network_id = self.runtime.create_network(&network_name).await?;
        self.network_id = Some(network_id);

        // Create node structs
        self.create_nodes();

        // Cache Docker-network-accessible addresses from the primary validator node
        if let Some(primary) = self.validators.first() {
            self.internal_rpc = primary.rpc_address();
            self.internal_grpc = primary.grpc_address();
        }

        // Initialize all nodes (create containers, init home dirs)
        self.init_nodes().await?;

        // Initialize sidecar processes from config
        self.initialize_sidecars();

        self.initialized = true;
        info!(chain_id = %self.cfg.chain_id, "Chain initialized");
        Ok(())
    }

    async fn start(&mut self, genesis_wallets: &[WalletAmount]) -> Result<()> {
        if !self.initialized {
            return Err(IctError::Chain {
                chain_id: self.cfg.chain_id.clone(),
                source: anyhow::anyhow!("chain not initialized, call initialize() first"),
            });
        }

        // Apply config file overrides (app.toml, config.toml, etc.)
        // Applied after init (which creates default configs) and before genesis
        // pipeline, matching Go ICT's ordering.
        self.apply_config_overrides().await?;

        // Run genesis pipeline
        self.genesis_pipeline(genesis_wallets).await?;

        // Distribute genesis to all nodes
        self.distribute_genesis().await?;

        // Configure persistent peers
        self.configure_peers().await?;

        // Start pre-start sidecars (before the chain binary)
        self.start_sidecars_filtered(true).await?;

        // Start the chain binary in each container (runs in background via nohup).
        for node in self.validators.iter().chain(self.full_nodes.iter()) {
            node.exec_start_chain().await?;
        }

        // Wait for the chain to produce its first block.
        // Checks for early crash: if the chain binary panicked or exited with
        // an error, fail immediately instead of waiting for the full timeout.
        let primary = self.primary_node()?;
        let mut attempts = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            match primary.query_height().await {
                Ok(h) if h > 0 => {
                    info!(height = h, "Chain is producing blocks");
                    break;
                }
                _ => {
                    attempts += 1;

                    // Early crash detection: check if chain binary has crashed
                    // by reading the log file for panic/fatal indicators.
                    // Only check every 3 attempts to avoid exec overhead.
                    if attempts % 3 == 0 {
                        let chain_log = primary.read_chain_log(50).await;
                        if !chain_log.is_empty() {
                            let has_panic = chain_log.contains("panic:")
                                || chain_log.contains("goroutine ")
                                || chain_log.contains("Error:")
                                || chain_log.contains("FATAL");
                            if has_panic {
                                // Dump full log for all nodes
                                for node in self.validators.iter().chain(self.full_nodes.iter()) {
                                    let log = node.read_chain_log(200).await;
                                    if !log.is_empty() {
                                        tracing::error!(
                                            node = %node.hostname,
                                            "Chain binary crashed:\n{log}"
                                        );
                                    }
                                }
                                return Err(IctError::Chain {
                                    chain_id: self.cfg.chain_id.clone(),
                                    source: anyhow::anyhow!(
                                        "chain binary crashed during startup (see logs above)"
                                    ),
                                });
                            }
                        }
                    }

                    if attempts > 60 {
                        for node in self.validators.iter().chain(self.full_nodes.iter()) {
                            let chain_log = node.read_chain_log(200).await;
                            if !chain_log.is_empty() {
                                tracing::error!(
                                    node = %node.hostname,
                                    "Chain binary log on block-wait timeout:\n{chain_log}"
                                );
                            } else {
                                tracing::error!(
                                    node = %node.hostname,
                                    "No chain binary log available on block-wait timeout"
                                );
                            }
                        }
                        return Err(IctError::Chain {
                            chain_id: self.cfg.chain_id.clone(),
                            source: anyhow::anyhow!(
                                "chain did not produce blocks within 60 seconds"
                            ),
                        });
                    }
                }
            }
        }

        // Start post-start sidecars (after chain is producing blocks)
        self.start_sidecars_filtered(false).await?;

        // Start in-container faucet if configured and has a start command.
        // When start_cmd is empty, the faucet is genesis-only (no server).
        if let Some(ref faucet_cfg) = self.cfg.faucet {
            if faucet_cfg.start_cmd.is_empty() {
                info!("Faucet configured as genesis-only (no server to start)");
            } else {
                let primary = self.primary_node()?;
                let env_str = faucet_cfg
                    .env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let cmd_str = faucet_cfg.start_cmd.join(" ");
                let faucet_cmd = format!("{env_str} setsid {cmd_str} &");
                info!(cmd = %faucet_cmd, "Starting faucet");
                primary
                    .exec_raw(&["sh", "-c", &faucet_cmd], &[])
                    .await?;

                // Brief wait then health-check
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let health_cmd = format!(
                    "curl -sf http://localhost:{}/status || echo 'faucet_not_ready'",
                    faucet_cfg.port
                );
                let output = primary
                    .exec_raw(&["sh", "-c", &health_cmd], &[])
                    .await?;
                if output.stdout_str().contains("faucet_not_ready") {
                    warn!("Faucet health-check failed, it may still be starting up");
                } else {
                    info!("Faucet is ready");
                }
            }
        }

        info!(
            chain_id = %self.cfg.chain_id,
            validators = self.validators.len(),
            full_nodes = self.full_nodes.len(),
            sidecars = self.sidecars.len(),
            "Chain started"
        );
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!(chain_id = %self.cfg.chain_id, "Stopping chain");

        // Stop sidecars before stopping chain nodes
        self.stop_all_sidecars().await?;

        // Collect volume names before mutating nodes
        let volume_names: Vec<String> = self
            .validators
            .iter()
            .chain(self.full_nodes.iter())
            .map(|n| n.volume_name.clone())
            .collect();

        for node in self.validators.iter_mut().chain(self.full_nodes.iter_mut()) {
            if let Err(e) = node.stop_container().await {
                warn!(node = %node.hostname, error = %e, "Failed to stop node");
            }
            if let Err(e) = node.remove_container().await {
                warn!(node = %node.hostname, error = %e, "Failed to remove node");
            }
        }

        // Remove named volumes
        for vol in &volume_names {
            if let Err(e) = self.runtime.remove_volume(vol).await {
                warn!(volume = %vol, error = %e, "Failed to remove volume");
            }
        }

        if let Some(network_id) = self.network_id.take() {
            if let Err(e) = self.runtime.remove_network(&network_id).await {
                warn!(error = %e, "Failed to remove network");
            }
        }

        Ok(())
    }

    async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> Result<ExecOutput> {
        self.primary_node()?.exec_raw(cmd, env).await
    }

    fn rpc_address(&self) -> &str {
        &self.internal_rpc
    }

    fn grpc_address(&self) -> &str {
        &self.internal_grpc
    }

    fn host_rpc_address(&self) -> String {
        self.validators
            .first()
            .and_then(|n| n.host_rpc_address())
            .unwrap_or_else(|| "http://localhost:26657".to_string())
    }

    fn host_grpc_address(&self) -> String {
        self.validators
            .first()
            .and_then(|n| n.host_grpc_address())
            .unwrap_or_else(|| "http://localhost:9090".to_string())
    }

    fn home_dir(&self) -> &str {
        self.validators
            .first()
            .map(|n| n.home_dir.as_str())
            .unwrap_or("/home/heighliner")
    }

    async fn create_key(&self, key_name: &str) -> Result<()> {
        let output = self
            .primary_node()?
            .create_key(key_name, self.cfg.coin_type)
            .await?;
        if output.exit_code != 0 {
            return Err(IctError::ExecFailed {
                exit_code: output.exit_code,
                stderr: output.stderr_str(),
            });
        }
        Ok(())
    }

    async fn recover_key(&self, name: &str, mnemonic: &str) -> Result<()> {
        let output = self.primary_node()?.recover_key(name, mnemonic).await?;
        if output.exit_code != 0 {
            return Err(IctError::ExecFailed {
                exit_code: output.exit_code,
                stderr: output.stderr_str(),
            });
        }
        Ok(())
    }

    async fn get_address(&self, key_name: &str) -> Result<Vec<u8>> {
        let addr_str = self.primary_node()?.get_key_address(key_name).await?;
        // Decode bech32 to get raw bytes
        let (_hrp, data) = bech32::decode(&addr_str).map_err(|e| IctError::Wallet(e.to_string()))?;
        Ok(data)
    }

    async fn build_wallet(
        &self,
        key_name: &str,
        mnemonic: &str,
    ) -> Result<Box<dyn Wallet>> {
        // Recover key on the node
        self.recover_key(key_name, mnemonic).await?;

        // Get the address
        let addr_str = self.primary_node()?.get_key_address(key_name).await?;
        let (_hrp, data) =
            bech32::decode(&addr_str).map_err(|e| IctError::Wallet(e.to_string()))?;

        Ok(Box::new(KeyWallet {
            key_name: key_name.to_string(),
            address_bytes: data,
            bech32_address: addr_str,
            mnemonic_phrase: mnemonic.to_string(),
        }))
    }

    async fn send_funds(&self, key_name: &str, amount: &WalletAmount) -> Result<String> {
        let coins = format!("{}{}", amount.amount, amount.denom);
        let output = self
            .primary_node()?
            .bank_send(key_name, &amount.address, &coins, &self.cfg.gas_prices)
            .await?;

        if output.exit_code != 0 {
            return Err(IctError::ExecFailed {
                exit_code: output.exit_code,
                stderr: output.stderr_str(),
            });
        }

        let tx = Self::parse_tx_response(&output)?;
        Ok(tx.tx_hash)
    }

    async fn get_balance(&self, address: &str, denom: &str) -> Result<u128> {
        self.primary_node()?.query_balance(address, denom).await
    }

    async fn send_ibc_transfer(
        &self,
        channel_id: &str,
        key_name: &str,
        amount: &WalletAmount,
        options: &TransferOptions,
    ) -> Result<Tx> {
        let coins = format!("{}{}", amount.amount, amount.denom);
        let output = self
            .primary_node()?
            .ibc_transfer(
                channel_id,
                key_name,
                &amount.address,
                &coins,
                &self.cfg.gas_prices,
                options.memo.as_deref(),
            )
            .await?;

        if output.exit_code != 0 {
            return Err(IctError::ExecFailed {
                exit_code: output.exit_code,
                stderr: output.stderr_str(),
            });
        }

        Self::parse_tx_response(&output)
    }

    async fn height(&self) -> Result<u64> {
        self.primary_node()?.query_height().await
    }

    async fn export_state(&self, height: u64) -> Result<String> {
        self.primary_node()?.export_state(height).await
    }

    async fn acknowledgements(&self, height: u64) -> Result<Vec<PacketAcknowledgement>> {
        let height_str = height.to_string();
        let output = self
            .primary_node()?
            .exec_cmd(&[
                "query",
                "ibc",
                "channel",
                "packet-ack",
                "--height",
                &height_str,
                "--output",
                "json",
            ])
            .await?;

        // Parse acknowledgements from JSON
        if output.exit_code != 0 || output.stdout.is_empty() {
            return Ok(Vec::new());
        }

        let v: serde_json::Value = serde_json::from_str(output.stdout_str().trim())
            .unwrap_or(serde_json::Value::Null);

        let acks = v["acknowledgements"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| {
                        Some(PacketAcknowledgement {
                            packet: Packet {
                                sequence: a["packet"]["sequence"]
                                    .as_str()
                                    .and_then(|s| s.parse().ok())
                                    .unwrap_or(0),
                                source_port: a["packet"]["source_port"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                source_channel: a["packet"]["source_channel"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                dest_port: a["packet"]["destination_port"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                dest_channel: a["packet"]["destination_channel"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                data: Vec::new(),
                                timeout_height: a["packet"]["timeout_height"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                timeout_timestamp: a["packet"]["timeout_timestamp"]
                                    .as_str()
                                    .and_then(|s| s.parse().ok())
                                    .unwrap_or(0),
                            },
                            acknowledgement: a["acknowledgement"]
                                .as_str()
                                .map(|s| s.as_bytes().to_vec())
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(acks)
    }

    async fn timeouts(&self, height: u64) -> Result<Vec<PacketTimeout>> {
        let height_str = height.to_string();
        let output = self
            .primary_node()?
            .exec_cmd(&[
                "query",
                "ibc",
                "channel",
                "unreceived-packets",
                "--height",
                &height_str,
                "--output",
                "json",
            ])
            .await?;

        if output.exit_code != 0 || output.stdout.is_empty() {
            return Ok(Vec::new());
        }

        // Simplified parsing — full implementation would be more thorough
        Ok(Vec::new())
    }

    async fn start_sidecars(&mut self) -> Result<()> {
        self.start_all_sidecars().await
    }

    async fn stop_sidecars(&mut self) -> Result<()> {
        self.stop_all_sidecars().await
    }

    async fn exec_sidecar(
        &self,
        sidecar_name: &str,
        cmd: &[&str],
        env: &[(&str, &str)],
    ) -> Result<ExecOutput> {
        for sc in &self.sidecars {
            if sc.config.name == sidecar_name {
                return sc.exec(cmd, env).await;
            }
        }
        Err(IctError::Config(format!(
            "no sidecar '{}' on chain {}",
            sidecar_name, self.cfg.chain_id
        )))
    }

    fn sidecar_hostname(&self, sidecar_name: &str) -> Option<String> {
        self.sidecars
            .iter()
            .find(|sc| sc.config.name == sidecar_name)
            .map(|sc| sc.hostname())
    }
}
