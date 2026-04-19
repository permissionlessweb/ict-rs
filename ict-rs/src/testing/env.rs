//! Environment-based test configuration.
//!
//! Reads `ICT_*` environment variables to configure test behavior.

use std::collections::HashMap;

use crate::chain::{ChainConfig, ChainType, FaucetConfig, GenesisStyle, SigningAlgorithm};
use crate::runtime::DockerImage;

/// Log output mode for test containers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogMode {
    /// Never dump logs.
    Never,
    /// Dump logs only on test failure.
    OnFailure,
    /// Always dump logs.
    Always,
}

/// Test environment configuration derived from `ICT_*` environment variables.
pub struct TestEnv;

impl TestEnv {
    /// Whether to use the mock runtime (no Docker needed).
    ///
    /// Set `ICT_MOCK=1` to enable.
    pub fn is_mock() -> bool {
        std::env::var("ICT_MOCK").map(|v| v == "1").unwrap_or(false)
    }

    /// Whether to keep containers alive after the test ends.
    ///
    /// Set `ICT_KEEP_CONTAINERS=1` to enable.
    pub fn keep_containers() -> bool {
        std::env::var("ICT_KEEP_CONTAINERS")
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    /// How to handle container log output.
    ///
    /// - `ICT_SHOW_LOGS=1` → dump on failure
    /// - `ICT_SHOW_LOGS=always` → always dump
    /// - unset → never dump
    pub fn log_mode() -> LogMode {
        match std::env::var("ICT_SHOW_LOGS").as_deref() {
            Ok("1") => LogMode::OnFailure,
            Ok("always") => LogMode::Always,
            _ => LogMode::Never,
        }
    }

    /// Build a default Terp chain config from environment variables.
    ///
    /// Reads:
    /// - `ICT_IMAGE_REPO` (default: `terpnetwork/terp-core`)
    /// - `ICT_IMAGE_VERSION` (default: `local-zk`)
    pub fn terp_config() -> ChainConfig {
        let repo = std::env::var("ICT_IMAGE_REPO")
            .unwrap_or_else(|_| "terpnetwork/terp-core".to_string());
        let version = std::env::var("ICT_IMAGE_VERSION")
            .unwrap_or_else(|_| "local-zk".to_string());

        ChainConfig {
            chain_type: ChainType::Cosmos,
            name: "terp".to_string(),
            chain_id: "120u-1".to_string(),
            images: vec![DockerImage {
                repository: repo,
                version,
                uid_gid: None,
            }],
            bin: "terpd".to_string(),
            bech32_prefix: "terp".to_string(),
            denom: "uterp".to_string(),
            coin_type: 118,
            signing_algorithm: SigningAlgorithm::Secp256k1,
            gas_prices: "0.025uterp".to_string(),
            gas_adjustment: 1.5,
            trusting_period: "508h".to_string(),
            block_time: "2s".to_string(),
            genesis: None,
            modify_genesis: None,
            pre_genesis: None,
            config_file_overrides: Default::default(),
            additional_start_args: Vec::new(),
            env: Vec::new(),
            sidecar_configs: Vec::new(),
            faucet: None,
            genesis_style: GenesisStyle::default(),
        }
    }

    /// Terp config with localterp faucet enabled.
    ///
    /// Uses the `terpnetwork/terp-core:localterp` image which includes
    /// Node.js and `/code/faucet_server.js`.
    pub fn terp_localterp_config() -> ChainConfig {
        let mut cfg = Self::terp_config();
        cfg.images = vec![DockerImage {
            repository: "terpnetwork/terp-core".into(),
            version: "localterp".into(),
            uid_gid: None,
        }];
        cfg.faucet = Some(FaucetConfig::default());
        cfg
    }

    /// Build a default Anvil chain config from environment variables.
    ///
    /// Reads:
    /// - `ICT_ANVIL_IMAGE` (default: `ghcr.io/foundry-rs/foundry`)
    /// - `ICT_ANVIL_VERSION` (default: `latest`)
    #[cfg(feature = "ethereum")]
    pub fn anvil_config() -> ChainConfig {
        let repo = std::env::var("ICT_ANVIL_IMAGE")
            .unwrap_or_else(|_| "ghcr.io/foundry-rs/foundry".to_string());
        let version = std::env::var("ICT_ANVIL_VERSION")
            .unwrap_or_else(|_| "latest".to_string());

        ChainConfig {
            chain_type: ChainType::Ethereum,
            name: "anvil".to_string(),
            chain_id: "31337".to_string(),
            images: vec![DockerImage {
                repository: repo,
                version,
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
        }
    }
}
