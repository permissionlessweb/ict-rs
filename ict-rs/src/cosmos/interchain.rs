use std::collections::HashMap;
use std::sync::Arc;

use tracing::{info, warn};

use crate::chain::{Chain, TestContext};
use crate::error::{IctError, Result};
use crate::ibc::ChannelOptions;
use crate::relayer::Relayer;
use crate::runtime::{IctRuntime, RuntimeBackend};
use crate::tx::WalletAmount;

/// A link between two chains via a relayer (an IBC path).
#[derive(Debug, Clone)]
pub struct InterchainLink {
    pub chain1: String,
    pub chain2: String,
    pub relayer: String,
    pub path: String,
}

/// Options for building the interchain environment.
#[derive(Debug, Clone)]
pub struct InterchainBuildOptions {
    pub test_name: String,
    pub skip_path_creation: bool,
    pub genesis_wallets: HashMap<String, Vec<WalletAmount>>,
}

impl Default for InterchainBuildOptions {
    fn default() -> Self {
        Self {
            test_name: "ict-test".to_string(),
            skip_path_creation: false,
            genesis_wallets: HashMap::new(),
        }
    }
}

/// The top-level orchestrator for multi-chain test environments.
///
/// Uses a builder pattern matching Go ICT's `Interchain` struct:
///
/// ```ignore
/// let runtime = Arc::new(DockerBackend::new(Default::default()).await?);
///
/// let mut ic = Interchain::new(runtime)
///     .add_chain(cosmos_chain)
///     .add_relayer("hermes", hermes_relayer)
///     .add_link(InterchainLink {
///         chain1: "chain-a".into(),
///         chain2: "chain-b".into(),
///         relayer: "hermes".into(),
///         path: "transfer".into(),
///     });
///
/// ic.build(InterchainBuildOptions {
///     test_name: "my_test".into(),
///     ..Default::default()
/// }).await?;
///
/// // ... run tests ...
///
/// ic.close().await?;
/// ```
pub struct Interchain {
    chains: HashMap<String, Box<dyn Chain>>,
    relayers: HashMap<String, Box<dyn Relayer>>,
    links: Vec<InterchainLink>,
    runtime: Arc<dyn RuntimeBackend>,
    built: bool,
}

impl Interchain {
    /// Create a new interchain orchestrator with the given runtime backend.
    pub fn new(runtime: Arc<dyn RuntimeBackend>) -> Self {
        Self {
            chains: HashMap::new(),
            relayers: HashMap::new(),
            links: Vec::new(),
            runtime,
            built: false,
        }
    }

    /// Create from an `IctRuntime` enum (convenience constructor).
    pub async fn from_runtime(runtime: IctRuntime) -> Result<Self> {
        let backend = runtime.into_backend().await?;
        Ok(Self::new(backend))
    }

    /// Add a chain to the environment.
    pub fn add_chain(mut self, chain: Box<dyn Chain>) -> Self {
        let chain_id = chain.chain_id().to_string();
        self.chains.insert(chain_id, chain);
        self
    }

    /// Register a relayer with a name.
    pub fn add_relayer(mut self, name: &str, relayer: Box<dyn Relayer>) -> Self {
        self.relayers.insert(name.to_string(), relayer);
        self
    }

    /// Add an IBC link between two chains.
    pub fn add_link(mut self, link: InterchainLink) -> Self {
        self.links.push(link);
        self
    }

    /// Build and start all chains, configure relayers, create IBC paths.
    ///
    /// This is the main orchestration method that:
    /// 1. Initializes all chains (creates containers, volumes, configs)
    /// 2. Starts all chains with genesis-funded wallets
    /// 3. Configures relayers with chain connection info
    /// 4. Creates IBC clients, connections, and channels for each link
    /// 5. Starts all relayers
    pub async fn build(&mut self, opts: InterchainBuildOptions) -> Result<()> {
        if self.built {
            return Err(IctError::Config(
                "interchain environment already built".to_string(),
            ));
        }

        info!(
            test = %opts.test_name,
            chains = self.chains.len(),
            relayers = self.relayers.len(),
            links = self.links.len(),
            "Building interchain environment"
        );

        // Validate links reference existing chains and relayers
        for link in &self.links {
            if !self.chains.contains_key(&link.chain1) {
                return Err(IctError::Config(format!(
                    "link references unknown chain: {}",
                    link.chain1
                )));
            }
            if !self.chains.contains_key(&link.chain2) {
                return Err(IctError::Config(format!(
                    "link references unknown chain: {}",
                    link.chain2
                )));
            }
            if !self.relayers.contains_key(&link.relayer) {
                return Err(IctError::Config(format!(
                    "link references unknown relayer: {}",
                    link.relayer
                )));
            }
        }

        // Phase 1: Initialize all chains
        let ctx = TestContext {
            test_name: opts.test_name.clone(),
            network_id: format!("ict-{}", opts.test_name),
        };

        for (chain_id, chain) in &mut self.chains {
            info!(chain_id = %chain_id, "Initializing chain");
            chain.initialize(&ctx).await?;
        }

        // Phase 2: Start all chains with genesis wallets
        for (chain_id, chain) in &mut self.chains {
            let wallets = opts
                .genesis_wallets
                .get(chain_id)
                .map(|w| w.as_slice())
                .unwrap_or(&[]);
            info!(chain_id = %chain_id, genesis_wallets = wallets.len(), "Starting chain");
            chain.start(wallets).await?;
        }

        // Phase 3: Configure relayers
        if !opts.skip_path_creation {
            self.configure_relayers().await?;
            self.create_ibc_paths().await?;
        }

        self.built = true;
        info!(test = %opts.test_name, "Interchain environment ready");
        Ok(())
    }

    /// Configure all relayers with chain connection information.
    ///
    /// For each relayer-chain pair:
    /// 1. Generates a key on the relayer for the chain
    /// 2. Funds the relayer wallet from the chain's validator
    /// 3. Configures the chain on the relayer (RPC/gRPC endpoints)
    async fn configure_relayers(&self) -> Result<()> {
        // Collect which chains each relayer needs to know about
        let mut relayer_chains: HashMap<&str, Vec<&str>> = HashMap::new();
        for link in &self.links {
            let entry = relayer_chains.entry(&link.relayer).or_default();
            if !entry.contains(&link.chain1.as_str()) {
                entry.push(&link.chain1);
            }
            if !entry.contains(&link.chain2.as_str()) {
                entry.push(&link.chain2);
            }
        }

        for (relayer_name, chain_ids) in &relayer_chains {
            let relayer = self.relayers.get(*relayer_name).ok_or_else(|| {
                IctError::Config(format!("relayer not found: {relayer_name}"))
            })?;

            for chain_id in chain_ids {
                let chain = self.chains.get(*chain_id).ok_or_else(|| {
                    IctError::Config(format!("chain not found: {chain_id}"))
                })?;

                let config = chain.config();
                let rpc = chain.rpc_address();
                let grpc = chain.grpc_address();

                info!(
                    relayer = %relayer_name,
                    chain_id = %chain_id,
                    "Adding chain to relayer"
                );

                // Configure the chain on the relayer FIRST.
                // Hermes requires chain config before keys can be added.
                let key_name = format!("relayer-{relayer_name}-{chain_id}");
                relayer
                    .add_chain_configuration(config, &key_name, rpc, grpc)
                    .await?;

                // Add a key for the relayer on this chain (now that config exists)
                let wallet = relayer.add_key(chain_id, &key_name).await?;

                // Fund the relayer wallet from the validator so it can submit
                // IBC transactions (create clients, connections, channels, relay packets).
                let relayer_addr = wallet.formatted_address();
                if !relayer_addr.is_empty() && !relayer_addr.contains("unknown") {
                    let fund = WalletAmount {
                        address: relayer_addr.clone(),
                        denom: config.denom.clone(),
                        amount: 100_000_000, // 100M micro-units
                    };
                    info!(
                        relayer = %relayer_name,
                        chain_id = %chain_id,
                        address = %relayer_addr,
                        amount = fund.amount,
                        "Funding relayer wallet"
                    );
                    if let Err(e) = chain.send_funds("validator", &fund).await {
                        warn!(
                            relayer = %relayer_name,
                            chain_id = %chain_id,
                            error = %e,
                            "Failed to fund relayer (may be mock mode)"
                        );
                    }
                }
            }
        }

        // Wait for funding txs to confirm on all chains
        for chain in self.chains.values() {
            if let Err(e) = wait_for_blocks(chain.as_ref(), 2).await {
                warn!(error = %e, "Failed to wait for blocks (may be mock mode)");
            }
        }

        Ok(())
    }

    /// Create IBC paths (clients, connections, channels) for all links.
    async fn create_ibc_paths(&self) -> Result<()> {
        for link in &self.links {
            let relayer = self.relayers.get(&link.relayer).ok_or_else(|| {
                IctError::Config(format!("relayer not found: {}", link.relayer))
            })?;

            info!(
                path = %link.path,
                chain1 = %link.chain1,
                chain2 = %link.chain2,
                "Creating IBC path"
            );

            // Generate path
            relayer
                .generate_path(&link.chain1, &link.chain2, &link.path)
                .await?;

            // Link path (creates clients + connections + channel)
            let channel_opts = ChannelOptions {
                src_port: "transfer".to_string(),
                dst_port: "transfer".to_string(),
                ..Default::default()
            };
            relayer.link_path(&link.path, &channel_opts).await?;

            info!(path = %link.path, "IBC path created");
        }

        // Start all relayers
        let path_names: Vec<&str> = self.links.iter().map(|l| l.path.as_str()).collect();
        for (name, relayer) in &self.relayers {
            info!(relayer = %name, "Starting relayer");
            relayer.start(&path_names).await?;
        }

        Ok(())
    }

    /// Shut down all chains, relayers, and clean up resources.
    pub async fn close(&mut self) -> Result<()> {
        info!("Shutting down interchain environment");

        // Stop relayers first
        for (name, relayer) in &self.relayers {
            if let Err(e) = relayer.stop().await {
                warn!(relayer = %name, error = %e, "Failed to stop relayer");
            }
        }

        // Stop sidecars before stopping chains
        for (chain_id, chain) in &mut self.chains {
            if let Err(e) = chain.stop_sidecars().await {
                warn!(chain_id = %chain_id, error = %e, "Failed to stop sidecars");
            }
        }

        // Stop chains
        for (chain_id, chain) in &mut self.chains {
            if let Err(e) = chain.stop().await {
                warn!(chain_id = %chain_id, error = %e, "Failed to stop chain");
            }
        }

        self.built = false;
        info!("Interchain environment shut down");
        Ok(())
    }

    /// Get a reference to a chain by chain ID.
    pub fn get_chain(&self, chain_id: &str) -> Option<&dyn Chain> {
        self.chains.get(chain_id).map(|c| c.as_ref())
    }

    /// Get a mutable reference to a chain by chain ID.
    pub fn get_chain_mut(&mut self, chain_id: &str) -> Option<&mut Box<dyn Chain>> {
        self.chains.get_mut(chain_id)
    }

    /// Get a reference to a relayer by name.
    pub fn get_relayer(&self, name: &str) -> Option<&dyn Relayer> {
        self.relayers.get(name).map(|r| r.as_ref())
    }

    /// Get the runtime backend.
    pub fn runtime(&self) -> &dyn RuntimeBackend {
        self.runtime.as_ref()
    }

    /// Get the runtime backend as an Arc (for sharing with chains).
    pub fn runtime_arc(&self) -> Arc<dyn RuntimeBackend> {
        self.runtime.clone()
    }

    /// Check if the environment has been built.
    pub fn is_built(&self) -> bool {
        self.built
    }
}

/// Helper: create and fund test users on a chain.
pub async fn get_and_fund_test_users(
    chain: &dyn Chain,
    count: usize,
    key_prefix: &str,
    amount: u128,
) -> Result<Vec<crate::wallet::KeyWallet>> {
    let mut wallets = Vec::with_capacity(count);

    for i in 0..count {
        let key_name = format!("{key_prefix}-{i}");
        let mnemonic = crate::auth::generate_mnemonic();

        let wallet = chain.build_wallet(&key_name, &mnemonic).await?;

        // Fund the wallet from the first validator's key
        let fund_amount = WalletAmount {
            address: wallet.formatted_address(),
            denom: chain.config().denom.clone(),
            amount,
        };
        chain.send_funds("validator-0", &fund_amount).await?;

        wallets.push(crate::wallet::KeyWallet {
            key_name,
            address_bytes: wallet.address().to_vec(),
            bech32_address: wallet.formatted_address(),
            mnemonic_phrase: mnemonic,
        });
    }

    Ok(wallets)
}

/// Wait for a chain to produce a given number of blocks.
///
/// Polls `chain.height()` every 500ms until the height has advanced by
/// at least `num_blocks` from the height observed at call time.
///
/// Exits early if the height hasn't changed after 5 consecutive polls
/// (indicates mock mode or stalled chain). Times out after 120 seconds.
pub async fn wait_for_blocks(chain: &dyn crate::chain::Chain, num_blocks: u64) -> Result<()> {
    let start = chain.height().await?;
    let target = start + num_blocks;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut last_height = start;
    let mut stale_count = 0u32;

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let current = chain.height().await?;
        if current >= target {
            return Ok(());
        }
        // Detect stale height (mock mode or stalled chain)
        if current == last_height {
            stale_count += 1;
            if stale_count >= 5 {
                return Ok(()); // Height not advancing — likely mock mode
            }
        } else {
            stale_count = 0;
            last_height = current;
        }
        if std::time::Instant::now() > deadline {
            warn!(
                start_height = start,
                target_height = target,
                current_height = current,
                "wait_for_blocks timed out"
            );
            return Ok(());
        }
    }
}
