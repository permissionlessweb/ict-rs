use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::chain::cosmos::CosmosChain;
use crate::chain::{ChainConfig, ChainType, GenesisStyle, SigningAlgorithm};
use crate::error::{IctError, Result};
use crate::runtime::{DockerImage, RuntimeBackend};

#[cfg(feature = "ethereum")]
use crate::chain::ethereum::AnvilChain;

/// Declarative chain specification, analogous to Go ICT's `ChainSpec`.
///
/// Provides a high-level way to declare which chains to spin up, optionally
/// overriding individual config fields from built-in defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainSpec {
    /// Name of a built-in chain config (e.g., "gaia", "osmosis", "terp").
    pub name: String,
    /// Docker image version override.
    pub version: Option<String>,
    /// Number of validators to run.
    pub num_validators: Option<usize>,
    /// Number of full nodes to run.
    pub num_full_nodes: Option<usize>,
    /// Override chain ID.
    pub chain_id: Option<String>,
    /// Override denom.
    pub denom: Option<String>,
    /// Override bech32 prefix.
    pub bech32_prefix: Option<String>,
    /// Override gas prices.
    pub gas_prices: Option<String>,
}

impl ChainSpec {
    /// Resolve this spec into a full `ChainConfig` by merging with built-in defaults.
    pub fn resolve(&self) -> Result<ChainConfig> {
        let mut cfg = builtin_chain_config(&self.name)?;

        if let Some(ref version) = self.version {
            if let Some(img) = cfg.images.first_mut() {
                img.version = version.clone();
            }
        }
        if let Some(ref chain_id) = self.chain_id {
            cfg.chain_id = chain_id.clone();
        }
        if let Some(ref denom) = self.denom {
            cfg.denom = denom.clone();
        }
        if let Some(ref prefix) = self.bech32_prefix {
            cfg.bech32_prefix = prefix.clone();
        }
        if let Some(ref gp) = self.gas_prices {
            cfg.gas_prices = gp.clone();
        }

        Ok(cfg)
    }

    /// Create a `CosmosChain` from this spec.
    pub fn build_cosmos_chain(&self, runtime: Arc<dyn RuntimeBackend>) -> Result<CosmosChain> {
        let cfg = self.resolve()?;
        let num_vals = self.num_validators.unwrap_or(1);
        let num_fns = self.num_full_nodes.unwrap_or(0);
        Ok(CosmosChain::new(cfg, num_vals, num_fns, runtime))
    }

    /// Create an `AnvilChain` from this spec.
    #[cfg(feature = "ethereum")]
    pub fn build_anvil_chain(&self, runtime: Arc<dyn RuntimeBackend>) -> Result<AnvilChain> {
        let cfg = self.resolve()?;
        Ok(AnvilChain::new(cfg, runtime))
    }
}

/// Look up a built-in chain configuration by name.
pub fn builtin_chain_config(name: &str) -> Result<ChainConfig> {
    match name {
        "gaia" | "cosmoshub" => Ok(ChainConfig {
            chain_type: ChainType::Cosmos,
            name: "gaia".to_string(),
            chain_id: "cosmoshub-test-1".to_string(),
            images: vec![DockerImage {
                repository: "ghcr.io/strangelove-ventures/heighliner/gaia".to_string(),
                version: "v19.0.0".to_string(),
                uid_gid: None,
            }],
            bin: "gaiad".to_string(),
            bech32_prefix: "cosmos".to_string(),
            denom: "uatom".to_string(),
            coin_type: 118,
            signing_algorithm: SigningAlgorithm::Secp256k1,
            gas_prices: "0.025uatom".to_string(),
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
            genesis_style: GenesisStyle::default(),
        }),

        "osmosis" => Ok(ChainConfig {
            chain_type: ChainType::Cosmos,
            name: "osmosis".to_string(),
            chain_id: "osmosis-test-1".to_string(),
            images: vec![DockerImage {
                repository: "ghcr.io/strangelove-ventures/heighliner/osmosis".to_string(),
                version: "v25.0.0".to_string(),
                uid_gid: None,
            }],
            bin: "osmosisd".to_string(),
            bech32_prefix: "osmo".to_string(),
            denom: "uosmo".to_string(),
            coin_type: 118,
            signing_algorithm: SigningAlgorithm::Secp256k1,
            gas_prices: "0.025uosmo".to_string(),
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
            genesis_style: GenesisStyle::default(),
        }),

        "terp" | "terpnetwork" => Ok(ChainConfig {
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
            genesis_style: GenesisStyle::default(),
        }),

        "juno" => Ok(ChainConfig {
            chain_type: ChainType::Cosmos,
            name: "juno".to_string(),
            chain_id: "juno-test-1".to_string(),
            images: vec![DockerImage {
                repository: "ghcr.io/strangelove-ventures/heighliner/juno".to_string(),
                version: "v23.0.0".to_string(),
                uid_gid: None,
            }],
            bin: "junod".to_string(),
            bech32_prefix: "juno".to_string(),
            denom: "ujuno".to_string(),
            coin_type: 118,
            signing_algorithm: SigningAlgorithm::Secp256k1,
            gas_prices: "0.025ujuno".to_string(),
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
            genesis_style: GenesisStyle::default(),
        }),

        "akash" => Ok(ChainConfig {
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
            block_time: "2s".to_string(),
            genesis: None,
            modify_genesis: None,
            pre_genesis: None,
            config_file_overrides: HashMap::new(),
            additional_start_args: Vec::new(),
            env: Vec::new(),
            sidecar_configs: Vec::new(),
            faucet: None,
            genesis_style: GenesisStyle::Modern,
        }),

        "anvil" | "ethereum" => Ok(ChainConfig {
            chain_type: ChainType::Ethereum,
            name: "anvil".to_string(),
            chain_id: "31337".to_string(),
            images: vec![DockerImage {
                repository: "ghcr.io/foundry-rs/foundry".to_string(),
                version: "latest".to_string(),
                uid_gid: None,
            }],
            bin: "anvil".to_string(),
            bech32_prefix: String::new(),
            denom: "wei".to_string(),
            coin_type: 60,
            signing_algorithm: SigningAlgorithm::Secp256k1,
            gas_prices: String::new(),
            gas_adjustment: 1.0,
            trusting_period: String::new(),
            block_time: "2s".to_string(),
            genesis: None,
            modify_genesis: None,
            pre_genesis: None,
            config_file_overrides: HashMap::new(),
            additional_start_args: Vec::new(),
            env: Vec::new(),
            sidecar_configs: Vec::new(),
            faucet: None,
            genesis_style: GenesisStyle::default(),
        }),

        _ => Err(IctError::Config(format!(
            "unknown built-in chain: '{name}'. Available: gaia, osmosis, terp, juno, akash, anvil"
        ))),
    }
}
