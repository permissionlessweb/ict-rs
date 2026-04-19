//! Cosmos Relayer (rly) commander implementation.
//!
//! Implements `RelayerCommander` for the `rly` binary (github.com/cosmos/relayer).

use crate::chain::ChainConfig;
use crate::error::{IctError, Result};
use crate::ibc::{ChannelOptions, ChannelOutput, ClientOptions, ConnectionOutput};
use crate::relayer::docker_relayer::RelayerCommander;
use crate::runtime::DockerImage;
use crate::wallet::{KeyWallet, Wallet};

/// Commander for the Cosmos Relayer (`rly`) binary.
pub struct CosmosRlyCommander {
    pub extra_start_flags: Vec<String>,
}

impl CosmosRlyCommander {
    pub fn new() -> Self {
        Self {
            extra_start_flags: Vec::new(),
        }
    }

    pub fn with_extra_start_flags(mut self, flags: Vec<String>) -> Self {
        self.extra_start_flags = flags;
        self
    }
}

impl Default for CosmosRlyCommander {
    fn default() -> Self {
        Self::new()
    }
}

impl RelayerCommander for CosmosRlyCommander {
    fn name(&self) -> &str {
        "cosmos-relayer"
    }

    fn default_image(&self) -> DockerImage {
        DockerImage {
            repository: "ghcr.io/cosmos/relayer".to_string(),
            version: "v2.5.2".to_string(),
            uid_gid: Some("100:1000".to_string()),
        }
    }

    fn docker_user(&self) -> &str {
        "100:1000"
    }

    fn home_dir(&self) -> &str {
        "/home/relayer/.relayer"
    }

    fn init_cmd(&self, home_dir: &str) -> Option<Vec<String>> {
        Some(vec![
            "rly".to_string(),
            "config".to_string(),
            "init".to_string(),
            "--home".to_string(),
            home_dir.to_string(),
        ])
    }

    fn config_content(
        &self,
        cfg: &ChainConfig,
        key_name: &str,
        rpc_addr: &str,
        grpc_addr: &str,
    ) -> Result<Vec<u8>> {
        let config = serde_json::json!({
            "type": "cosmos",
            "value": {
                "key": key_name,
                "chain-id": cfg.chain_id,
                "rpc-addr": rpc_addr,
                "grpc-addr": grpc_addr,
                "account-prefix": cfg.bech32_prefix,
                "keyring-backend": "test",
                "gas-adjustment": cfg.gas_adjustment,
                "gas-prices": cfg.gas_prices,
                "debug": true,
                "timeout": "30s",
                "output-format": "json",
                "sign-mode": "direct"
            }
        });

        serde_json::to_vec_pretty(&config).map_err(|e| {
            IctError::Relayer {
                relayer: self.name().to_string(),
                source: e.into(),
            }
        })
    }

    fn add_chain_cmd(&self, config_file_path: &str, home_dir: &str) -> Vec<String> {
        vec![
            "rly".to_string(),
            "chains".to_string(),
            "add".to_string(),
            "-f".to_string(),
            config_file_path.to_string(),
            "--home".to_string(),
            home_dir.to_string(),
        ]
    }

    fn add_key_cmd(
        &self,
        chain_id: &str,
        key_name: &str,
        coin_type: u32,
        signing_algo: &str,
        home_dir: &str,
    ) -> Vec<String> {
        vec![
            "rly".to_string(),
            "keys".to_string(),
            "add".to_string(),
            chain_id.to_string(),
            key_name.to_string(),
            "--coin-type".to_string(),
            coin_type.to_string(),
            "--signing-algorithm".to_string(),
            signing_algo.to_string(),
            "--home".to_string(),
            home_dir.to_string(),
        ]
    }

    fn restore_key_cmd(
        &self,
        chain_id: &str,
        key_name: &str,
        coin_type: u32,
        signing_algo: &str,
        mnemonic: &str,
        home_dir: &str,
    ) -> Vec<String> {
        vec![
            "rly".to_string(),
            "keys".to_string(),
            "restore".to_string(),
            chain_id.to_string(),
            key_name.to_string(),
            mnemonic.to_string(),
            "--coin-type".to_string(),
            coin_type.to_string(),
            "--signing-algorithm".to_string(),
            signing_algo.to_string(),
            "--home".to_string(),
            home_dir.to_string(),
        ]
    }

    fn generate_path_cmd(
        &self,
        src: &str,
        dst: &str,
        path: &str,
        home_dir: &str,
    ) -> Vec<String> {
        vec![
            "rly".to_string(),
            "paths".to_string(),
            "new".to_string(),
            src.to_string(),
            dst.to_string(),
            path.to_string(),
            "--home".to_string(),
            home_dir.to_string(),
        ]
    }

    fn link_path_cmd(
        &self,
        path: &str,
        home_dir: &str,
        ch_opts: &ChannelOptions,
        _cl_opts: &ClientOptions,
    ) -> Vec<String> {
        let mut cmd = vec![
            "rly".to_string(),
            "tx".to_string(),
            "link".to_string(),
            path.to_string(),
        ];

        if !ch_opts.src_port.is_empty() {
            cmd.push("--src-port".to_string());
            cmd.push(ch_opts.src_port.clone());
        }
        if !ch_opts.dst_port.is_empty() {
            cmd.push("--dst-port".to_string());
            cmd.push(ch_opts.dst_port.clone());
        }
        if !ch_opts.version.is_empty() {
            cmd.push("--version".to_string());
            cmd.push(ch_opts.version.clone());
        }

        cmd.push("--home".to_string());
        cmd.push(home_dir.to_string());
        cmd
    }

    fn create_clients_cmd(
        &self,
        path: &str,
        opts: &ClientOptions,
        home: &str,
    ) -> Vec<String> {
        let mut cmd = vec![
            "rly".to_string(),
            "tx".to_string(),
            "clients".to_string(),
            path.to_string(),
        ];

        if let Some(ref tp) = opts.trusting_period {
            cmd.push("--client-tp".to_string());
            cmd.push(tp.clone());
        }

        cmd.push("--home".to_string());
        cmd.push(home.to_string());
        cmd
    }

    fn create_connections_cmd(&self, path: &str, home: &str) -> Vec<String> {
        vec![
            "rly".to_string(),
            "tx".to_string(),
            "connection".to_string(),
            path.to_string(),
            "--home".to_string(),
            home.to_string(),
        ]
    }

    fn create_channel_cmd(
        &self,
        path: &str,
        opts: &ChannelOptions,
        home: &str,
    ) -> Vec<String> {
        let mut cmd = vec![
            "rly".to_string(),
            "tx".to_string(),
            "channel".to_string(),
            path.to_string(),
        ];

        if !opts.src_port.is_empty() {
            cmd.push("--src-port".to_string());
            cmd.push(opts.src_port.clone());
        }
        if !opts.dst_port.is_empty() {
            cmd.push("--dst-port".to_string());
            cmd.push(opts.dst_port.clone());
        }
        cmd.push("--order".to_string());
        cmd.push(opts.ordering.to_string());
        if !opts.version.is_empty() {
            cmd.push("--version".to_string());
            cmd.push(opts.version.clone());
        }

        cmd.push("--home".to_string());
        cmd.push(home.to_string());
        cmd
    }

    fn update_clients_cmd(&self, path: &str, home: &str) -> Vec<String> {
        vec![
            "rly".to_string(),
            "tx".to_string(),
            "update-clients".to_string(),
            path.to_string(),
            "--home".to_string(),
            home.to_string(),
        ]
    }

    fn start_cmd(&self, home: &str, paths: &[&str]) -> Vec<String> {
        let mut cmd = vec![
            "rly".to_string(),
            "start".to_string(),
            "--debug".to_string(),
            "--home".to_string(),
            home.to_string(),
        ];
        for flag in &self.extra_start_flags {
            cmd.push(flag.clone());
        }
        for path in paths {
            cmd.push(path.to_string());
        }
        cmd
    }

    fn flush_cmd(&self, path: &str, channel_id: &str, home: &str) -> Vec<String> {
        vec![
            "rly".to_string(),
            "tx".to_string(),
            "flush".to_string(),
            path.to_string(),
            channel_id.to_string(),
            "--home".to_string(),
            home.to_string(),
        ]
    }

    fn get_channels_cmd(&self, chain_id: &str, home: &str) -> Vec<String> {
        vec![
            "rly".to_string(),
            "q".to_string(),
            "channels".to_string(),
            chain_id.to_string(),
            "--home".to_string(),
            home.to_string(),
        ]
    }

    fn get_connections_cmd(&self, chain_id: &str, home: &str) -> Vec<String> {
        vec![
            "rly".to_string(),
            "q".to_string(),
            "connections".to_string(),
            chain_id.to_string(),
            "--home".to_string(),
            home.to_string(),
        ]
    }

    fn parse_add_key_output(&self, stdout: &str, _stderr: &str) -> Result<Box<dyn Wallet>> {
        // rly keys add outputs JSON: {"address":"cosmos1..."}
        let json: serde_json::Value = serde_json::from_str(stdout.trim()).map_err(|e| {
            IctError::Relayer {
                relayer: self.name().to_string(),
                source: anyhow::anyhow!("failed to parse key output: {}", e),
            }
        })?;

        let address = json["address"]
            .as_str()
            .unwrap_or("cosmos1unknown")
            .to_string();

        Ok(Box::new(KeyWallet {
            key_name: "relayer".to_string(),
            address_bytes: address.as_bytes().to_vec(),
            bech32_address: address,
            mnemonic_phrase: String::new(),
        }))
    }

    fn parse_channels_output(&self, stdout: &str) -> Result<Vec<ChannelOutput>> {
        let trimmed = stdout.trim();
        if trimmed.is_empty() || trimmed == "[]" {
            return Ok(Vec::new());
        }

        serde_json::from_str(trimmed).map_err(|e| {
            IctError::Relayer {
                relayer: self.name().to_string(),
                source: anyhow::anyhow!("failed to parse channels: {}", e),
            }
        })
    }

    fn parse_connections_output(&self, stdout: &str) -> Result<Vec<ConnectionOutput>> {
        let trimmed = stdout.trim();
        if trimmed.is_empty() || trimmed == "[]" {
            return Ok(Vec::new());
        }

        serde_json::from_str(trimmed).map_err(|e| {
            IctError::Relayer {
                relayer: self.name().to_string(),
                source: anyhow::anyhow!("failed to parse connections: {}", e),
            }
        })
    }
}
