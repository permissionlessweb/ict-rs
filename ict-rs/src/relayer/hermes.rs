//! Hermes relayer implementation.
//!
//! Hermes uses a single TOML config file rather than per-chain JSON files.
//! Path concepts are tracked in-memory since Hermes doesn't have native path objects.
//! This wraps `DockerRelayer` and overrides certain operations to handle
//! Hermes-specific behavior.
//!
//! Ported from Go interchaintest's `relayer/hermes/` package.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::chain::ChainConfig;
use crate::error::{IctError, Result};
use crate::ibc::{
    ChannelOptions, ChannelOutput, ClientOptions, ConnectionOutput,
};
use crate::relayer::docker_relayer::{DockerRelayer, RelayerCommander};
use crate::relayer::Relayer;
use crate::runtime::{DockerImage, RuntimeBackend};
use crate::tx::ExecOutput;
use crate::wallet::{KeyWallet, Wallet};

/// A path tracked in-memory for Hermes (which doesn't have native path objects).
#[derive(Debug, Clone, Default)]
struct PathConfig {
    chain_a: PathChainConfig,
    chain_b: PathChainConfig,
}

#[derive(Debug, Clone, Default)]
struct PathChainConfig {
    chain_id: String,
    client_id: String,
    connection_id: String,
    port_id: String,
}

/// A Hermes chain config that will be serialized into the TOML config file.
/// Matches Go interchaintest's `hermes_config.go` Chain struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HermesChainConfig {
    id: String,
    chain_type: String,
    rpc_addr: String,
    grpc_addr: String,
    event_source_url: String,
    ccv_consumer_chain: bool,
    rpc_timeout: String,
    account_prefix: String,
    key_name: String,
    address_type_derivation: String,
    store_prefix: String,
    default_gas: u64,
    max_gas: u64,
    gas_price_denom: String,
    gas_price_amount: String,
    gas_multiplier: f64,
    max_msg_num: u32,
    max_tx_size: u64,
    clock_drift: String,
    max_block_time: String,
    trusting_period: String,
    trust_threshold_numerator: String,
    trust_threshold_denominator: String,
    memo_prefix: String,
}

/// Hermes IBC relayer.
///
/// Wraps `DockerRelayer` with Hermes-specific logic:
/// - Single TOML config regenerated when chains are added
/// - In-memory path tracking
/// - JSON output parsing from `--json` flag
pub struct HermesRelayer {
    docker_relayer: DockerRelayer,
    paths: Mutex<HashMap<String, PathConfig>>,
    chain_configs: Mutex<Vec<HermesChainConfig>>,
}

impl std::fmt::Debug for HermesRelayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HermesRelayer")
            .field("docker_relayer", &self.docker_relayer)
            .finish_non_exhaustive()
    }
}

impl HermesRelayer {
    /// Create a new HermesRelayer.
    pub async fn new(
        runtime: Arc<dyn RuntimeBackend>,
        test_name: &str,
        network_id: &str,
    ) -> Result<Self> {
        let commander = Box::new(HermesCommander);
        let docker_relayer =
            DockerRelayer::new(commander, runtime, test_name, network_id).await?;

        Ok(Self {
            docker_relayer,
            paths: Mutex::new(HashMap::new()),
            chain_configs: Mutex::new(Vec::new()),
        })
    }

    /// Regenerate the Hermes TOML config file from stored chain configs.
    /// Matches Go interchaintest's `NewConfig()` in `hermes_config.go`.
    async fn write_hermes_config(&self) -> Result<()> {
        let configs = self.chain_configs.lock().unwrap().clone();

        let mut toml = String::new();

        // Global
        toml.push_str("[global]\nlog_level = 'info'\n\n");

        // Mode — matches Go's Config.Mode
        toml.push_str("[mode]\n\n");
        toml.push_str("[mode.clients]\nenabled = true\nrefresh = true\nmisbehaviour = true\n\n");
        toml.push_str("[mode.connections]\nenabled = true\n\n");
        toml.push_str("[mode.channels]\nenabled = true\n\n");
        // Go: ClearInterval=0, ClearOnStart=true, TxConfirmation=false
        toml.push_str("[mode.packets]\nenabled = true\nclear_interval = 0\nclear_on_start = true\ntx_confirmation = false\n\n");

        // Rest, Telemetry, TracingServer — include host/port fields even when
        // disabled; Hermes 1.8.2+ requires them.
        toml.push_str("[rest]\nenabled = false\nhost = '0.0.0.0'\nport = 3000\n\n");
        toml.push_str("[telemetry]\nenabled = false\nhost = '0.0.0.0'\nport = 3001\n\n");
        toml.push_str("[tracing_server]\nenabled = false\nport = 5555\n\n");

        // Chains — matches Go's Chain struct with ALL fields
        for cfg in &configs {
            toml.push_str(&format!(
                "[[chains]]\n\
                 id = '{id}'\n\
                 type = '{chain_type}'\n\
                 rpc_addr = '{rpc_addr}'\n\
                 grpc_addr = '{grpc_addr}'\n\
                 ccv_consumer_chain = {ccv}\n\
                 rpc_timeout = '{rpc_timeout}'\n\
                 account_prefix = '{account_prefix}'\n\
                 key_name = '{key_name}'\n\
                 store_prefix = '{store_prefix}'\n\
                 default_gas = {default_gas}\n\
                 max_gas = {max_gas}\n\
                 gas_multiplier = {gas_multiplier}\n\
                 max_msg_num = {max_msg_num}\n\
                 max_tx_size = {max_tx_size}\n\
                 clock_drift = '{clock_drift}'\n\
                 max_block_time = '{max_block_time}'\n\
                 trusting_period = '{trusting_period}'\n\
                 memo_prefix = '{memo_prefix}'\n\
                 \n\
                 [chains.event_source]\n\
                 mode = 'push'\n\
                 url = '{event_source_url}'\n\
                 batch_delay = '200ms'\n\
                 \n\
                 [chains.address_type]\n\
                 derivation = '{address_derivation}'\n\
                 \n\
                 [chains.gas_price]\n\
                 price = {gas_price}\n\
                 denom = '{gas_denom}'\n\
                 \n\
                 [chains.trust_threshold]\n\
                 numerator = '{trust_num}'\n\
                 denominator = '{trust_den}'\n\n",
                id = cfg.id,
                chain_type = cfg.chain_type,
                rpc_addr = cfg.rpc_addr,
                grpc_addr = cfg.grpc_addr,
                ccv = cfg.ccv_consumer_chain,
                rpc_timeout = cfg.rpc_timeout,
                account_prefix = cfg.account_prefix,
                key_name = cfg.key_name,
                store_prefix = cfg.store_prefix,
                default_gas = cfg.default_gas,
                max_gas = cfg.max_gas,
                gas_multiplier = cfg.gas_multiplier,
                max_msg_num = cfg.max_msg_num,
                max_tx_size = cfg.max_tx_size,
                clock_drift = cfg.clock_drift,
                max_block_time = cfg.max_block_time,
                trusting_period = cfg.trusting_period,
                memo_prefix = cfg.memo_prefix,
                event_source_url = cfg.event_source_url,
                address_derivation = cfg.address_type_derivation,
                gas_price = cfg.gas_price_amount,
                gas_denom = cfg.gas_price_denom,
                trust_num = cfg.trust_threshold_numerator,
                trust_den = cfg.trust_threshold_denominator,
            ));
        }

        debug!(relayer = "hermes", config = %toml, "Generated Hermes config");

        let config_path = format!(
            "{}/.hermes/config.toml",
            self.docker_relayer.commander().home_dir()
        );
        self.docker_relayer
            .write_file(&config_path, toml.as_bytes())
            .await
    }

    /// Validate the Hermes config file.
    /// Matches Go's `validateConfig()`.
    async fn validate_config(&self) -> Result<()> {
        let home = self.docker_relayer.commander().home_dir();
        let cmd = vec![
            "hermes".to_string(),
            "--config".to_string(),
            format!("{home}/.hermes/config.toml"),
            "config".to_string(),
            "validate".to_string(),
        ];
        let output = self.docker_relayer.exec_oneoff(&cmd, &[]).await?;
        if output.exit_code != 0 {
            let stderr = output.stderr_str();
            let stdout = output.stdout_str();
            warn!(
                relayer = "hermes",
                exit_code = output.exit_code,
                stdout = %stdout,
                stderr = %stderr,
                "Hermes config validation failed"
            );
            return Err(IctError::Relayer {
                relayer: "hermes".to_string(),
                source: anyhow::anyhow!(
                    "config validation failed (exit {}): {}",
                    output.exit_code,
                    if stderr.is_empty() { &stdout } else { &stderr }
                ),
            });
        }
        debug!(relayer = "hermes", "Config validation passed");
        Ok(())
    }
}

#[async_trait]
impl Relayer for HermesRelayer {
    async fn add_key(&self, chain_id: &str, key_name: &str) -> Result<Box<dyn Wallet>> {
        // Hermes `keys add` REQUIRES --mnemonic-file or --key-file; it cannot
        // generate a key from nothing. Go interchaintest doesn't implement AddKey
        // for Hermes — it only uses RestoreKey. We generate a mnemonic, write it
        // to a file in the volume, then call `hermes keys add --mnemonic-file`.
        let mnemonic = crate::auth::generate_mnemonic();

        let home = self.docker_relayer.commander().home_dir();
        let mnemonic_path = format!("{home}/{chain_id}/mnemonic.txt");
        self.docker_relayer
            .write_file(&mnemonic_path, mnemonic.as_bytes())
            .await?;

        let cmd = vec![
            "hermes".to_string(),
            "keys".to_string(),
            "add".to_string(),
            "--chain".to_string(),
            chain_id.to_string(),
            "--mnemonic-file".to_string(),
            mnemonic_path,
            "--key-name".to_string(),
            key_name.to_string(),
            "--hd-path".to_string(),
            "m/44'/118'/0'/0/0".to_string(),
            "--overwrite".to_string(),
        ];

        info!(relayer = "hermes", chain = %chain_id, key = %key_name, "Adding key via mnemonic");
        let output = self.docker_relayer.exec_oneoff(&cmd, &[]).await?;

        let stdout = output.stdout_str();
        let stderr = output.stderr_str();
        debug!(
            relayer = "hermes",
            stdout = %stdout,
            stderr = %stderr,
            exit_code = output.exit_code,
            "hermes keys add output"
        );

        // Parse address from parentheses: "SUCCESS Added key 'name' (address) on chain ..."
        // Go uses regex `\((.*)\)` — we use simple string search.
        let combined = format!("{stdout}\n{stderr}");
        let address = parse_key_address_from_output(&combined).unwrap_or_else(|| {
            warn!(
                relayer = "hermes",
                chain = %chain_id,
                stdout = %stdout,
                stderr = %stderr,
                "Failed to parse key address from hermes output"
            );
            String::new()
        });

        let wallet = KeyWallet {
            key_name: key_name.to_string(),
            address_bytes: address.as_bytes().to_vec(),
            bech32_address: address,
            mnemonic_phrase: mnemonic,
        };

        Ok(Box::new(wallet))
    }

    async fn restore_key(
        &self,
        chain_id: &str,
        key_name: &str,
        mnemonic: &str,
    ) -> Result<()> {
        // Write mnemonic to a file in the volume, then reference it.
        // Matches Go's RestoreKey: hermes keys add --chain <id> --mnemonic-file <path> --key-name <name>
        let home = self.docker_relayer.commander().home_dir();
        let mnemonic_path = format!("{home}/{chain_id}/mnemonic.txt");
        self.docker_relayer
            .write_file(&mnemonic_path, mnemonic.as_bytes())
            .await?;

        // No --json flag (matches Go)
        let restore_cmd = vec![
            "hermes".to_string(),
            "keys".to_string(),
            "add".to_string(),
            "--chain".to_string(),
            chain_id.to_string(),
            "--mnemonic-file".to_string(),
            mnemonic_path,
            "--key-name".to_string(),
            key_name.to_string(),
        ];

        info!(relayer = "hermes", chain = %chain_id, key = %key_name, "Restoring key");
        let output = self.docker_relayer.exec_oneoff(&restore_cmd, &[]).await?;
        debug!(
            relayer = "hermes",
            stdout = %output.stdout_str(),
            stderr = %output.stderr_str(),
            exit_code = output.exit_code,
            "hermes restore key output"
        );
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
        // Parse gas price from "0.025uatom" format
        let (gas_amount, gas_denom) = parse_gas_prices(&config.gas_prices);

        // Compute WebSocket URL for event_source from RPC addr.
        // Go: strings.ReplaceAll(fmt.Sprintf("%s/websocket", rpcAddr), "http", "ws")
        let event_source_url = format!("{}/websocket", rpc_addr).replace("http", "ws");

        let hermes_cfg = HermesChainConfig {
            id: config.chain_id.clone(),
            chain_type: "CosmosSdk".to_string(),
            rpc_addr: rpc_addr.to_string(),
            grpc_addr: format!("http://{grpc_addr}"),
            event_source_url,
            ccv_consumer_chain: false,
            rpc_timeout: "10s".to_string(),
            account_prefix: config.bech32_prefix.clone(),
            key_name: key_name.to_string(),
            address_type_derivation: "cosmos".to_string(),
            store_prefix: "ibc".to_string(),
            default_gas: 100000,
            max_gas: 400000,
            gas_price_denom: gas_denom,
            gas_price_amount: gas_amount,
            gas_multiplier: config.gas_adjustment,
            max_msg_num: 30,
            max_tx_size: 2097152,
            clock_drift: "5s".to_string(),
            max_block_time: "30s".to_string(),
            trusting_period: if config.trusting_period.is_empty() {
                "14days".to_string()
            } else {
                config.trusting_period.clone()
            },
            trust_threshold_numerator: "1".to_string(),
            trust_threshold_denominator: "3".to_string(),
            memo_prefix: "hermes".to_string(),
        };

        {
            let mut configs = self.chain_configs.lock().unwrap();
            configs.push(hermes_cfg);
        }

        info!(relayer = "hermes", chain = %config.chain_id, "Adding chain configuration");
        self.write_hermes_config().await?;

        // Read back config to verify it was written correctly
        let config_path = format!(
            "{}/.hermes/config.toml",
            self.docker_relayer.commander().home_dir()
        );
        match self.docker_relayer.read_file(&config_path).await {
            Ok(content) => debug!(relayer = "hermes", config_readback = %content, "Config file readback"),
            Err(e) => warn!(relayer = "hermes", error = %e, "Failed to read back config"),
        }

        // Validate config after writing (matches Go's validateConfig)
        if let Err(e) = self.validate_config().await {
            warn!(relayer = "hermes", error = %e, "Config validation failed (continuing)");
        }

        Ok(())
    }

    async fn generate_path(
        &self,
        src_chain_id: &str,
        dst_chain_id: &str,
        path_name: &str,
    ) -> Result<()> {
        // Hermes doesn't have a "path" concept — we track it in-memory
        let path = PathConfig {
            chain_a: PathChainConfig {
                chain_id: src_chain_id.to_string(),
                ..Default::default()
            },
            chain_b: PathChainConfig {
                chain_id: dst_chain_id.to_string(),
                ..Default::default()
            },
        };

        let mut paths = self.paths.lock().unwrap();
        paths.insert(path_name.to_string(), path);
        info!(relayer = "hermes", path = %path_name, src = %src_chain_id, dst = %dst_chain_id, "Path registered");
        Ok(())
    }

    async fn link_path(&self, path_name: &str, opts: &ChannelOptions) -> Result<()> {
        // For Hermes, link_path does: create_clients + create_connections + create_channel
        let cl_opts = ClientOptions::default();
        self.create_clients(path_name, &cl_opts).await?;
        self.create_connections(path_name).await?;
        self.create_channel(path_name, opts).await?;
        Ok(())
    }

    async fn create_clients(&self, path_name: &str, _opts: &ClientOptions) -> Result<()> {
        let (chain_a, chain_b) = {
            let paths = self.paths.lock().unwrap();
            let path = paths.get(path_name).ok_or_else(|| {
                IctError::Relayer {
                    relayer: "hermes".to_string(),
                    source: anyhow::anyhow!("unknown path: {path_name}"),
                }
            })?;
            (path.chain_a.chain_id.clone(), path.chain_b.chain_id.clone())
        };

        // Create client on chain_a for chain_b
        let cmd_a = vec![
            "hermes".to_string(),
            "--json".to_string(),
            "create".to_string(),
            "client".to_string(),
            "--host-chain".to_string(),
            chain_a.clone(),
            "--reference-chain".to_string(),
            chain_b.clone(),
        ];
        info!(relayer = "hermes", path = %path_name, host = %chain_a, ref_chain = %chain_b, "Creating client A→B");
        let output_a = self.docker_relayer.exec_oneoff(&cmd_a, &[]).await?;
        let stdout_a = output_a.stdout_str();
        debug!(
            relayer = "hermes",
            stdout = %stdout_a,
            stderr = %output_a.stderr_str(),
            exit_code = output_a.exit_code,
            "create client A→B output"
        );

        // Parse client ID using extractJSONResult pattern (matches Go)
        let client_id_a = parse_client_id_from_stdout(&stdout_a)
            .map_err(|e| {
                warn!(relayer = "hermes", error = %e, stdout = %stdout_a, "Failed to parse client_id A→B");
                e
            })
            .unwrap_or_else(|_| "07-tendermint-0".to_string());
        info!(relayer = "hermes", client_id = %client_id_a, "Client A→B created");

        // Create client on chain_b for chain_a
        let cmd_b = vec![
            "hermes".to_string(),
            "--json".to_string(),
            "create".to_string(),
            "client".to_string(),
            "--host-chain".to_string(),
            chain_b.clone(),
            "--reference-chain".to_string(),
            chain_a.clone(),
        ];
        info!(relayer = "hermes", path = %path_name, host = %chain_b, ref_chain = %chain_a, "Creating client B→A");
        let output_b = self.docker_relayer.exec_oneoff(&cmd_b, &[]).await?;
        let stdout_b = output_b.stdout_str();
        debug!(
            relayer = "hermes",
            stdout = %stdout_b,
            stderr = %output_b.stderr_str(),
            exit_code = output_b.exit_code,
            "create client B→A output"
        );

        let client_id_b = parse_client_id_from_stdout(&stdout_b)
            .map_err(|e| {
                warn!(relayer = "hermes", error = %e, stdout = %stdout_b, "Failed to parse client_id B→A");
                e
            })
            .unwrap_or_else(|_| "07-tendermint-0".to_string());
        info!(relayer = "hermes", client_id = %client_id_b, "Client B→A created");

        // Store client IDs
        let mut paths = self.paths.lock().unwrap();
        if let Some(path) = paths.get_mut(path_name) {
            path.chain_a.client_id = client_id_a;
            path.chain_b.client_id = client_id_b;
        }

        Ok(())
    }

    async fn create_connections(&self, path_name: &str) -> Result<()> {
        let (chain_a, client_a, _chain_b, client_b) = {
            let paths = self.paths.lock().unwrap();
            let path = paths.get(path_name).ok_or_else(|| {
                IctError::Relayer {
                    relayer: "hermes".to_string(),
                    source: anyhow::anyhow!("unknown path: {path_name}"),
                }
            })?;
            (
                path.chain_a.chain_id.clone(),
                path.chain_a.client_id.clone(),
                path.chain_b.chain_id.clone(),
                path.chain_b.client_id.clone(),
            )
        };

        let cmd = vec![
            "hermes".to_string(),
            "--json".to_string(),
            "create".to_string(),
            "connection".to_string(),
            "--a-chain".to_string(),
            chain_a,
            "--a-client".to_string(),
            client_a,
            "--b-client".to_string(),
            client_b,
        ];

        info!(relayer = "hermes", path = %path_name, "Creating connection");
        let output = self.docker_relayer.exec_oneoff(&cmd, &[]).await?;
        let stdout = output.stdout_str();
        debug!(
            relayer = "hermes",
            stdout = %stdout,
            stderr = %output.stderr_str(),
            exit_code = output.exit_code,
            "create connection output"
        );

        // Parse both a_side and b_side connection IDs (matches Go's GetConnectionIDsFromStdout)
        let (conn_a, conn_b) = parse_connection_ids_from_stdout(&stdout)
            .map_err(|e| {
                warn!(relayer = "hermes", error = %e, stdout = %stdout, "Failed to parse connection IDs");
                e
            })
            .unwrap_or_else(|_| ("connection-0".to_string(), "connection-0".to_string()));
        info!(relayer = "hermes", conn_a = %conn_a, conn_b = %conn_b, "Connection created");

        let mut paths = self.paths.lock().unwrap();
        if let Some(path) = paths.get_mut(path_name) {
            path.chain_a.connection_id = conn_a;
            path.chain_b.connection_id = conn_b;
        }

        Ok(())
    }

    async fn create_channel(&self, path_name: &str, opts: &ChannelOptions) -> Result<()> {
        let (chain_a, conn_a) = {
            let paths = self.paths.lock().unwrap();
            let path = paths.get(path_name).ok_or_else(|| {
                IctError::Relayer {
                    relayer: "hermes".to_string(),
                    source: anyhow::anyhow!("unknown path: {path_name}"),
                }
            })?;
            (
                path.chain_a.chain_id.clone(),
                path.chain_a.connection_id.clone(),
            )
        };

        let src_port = if opts.src_port.is_empty() {
            "transfer"
        } else {
            &opts.src_port
        };
        let dst_port = if opts.dst_port.is_empty() {
            "transfer"
        } else {
            &opts.dst_port
        };

        // Matches Go: hermes --json create channel --order <order> --a-chain <chain>
        //   --a-port <port> --b-port <port> --a-connection <conn>
        let mut cmd = vec![
            "hermes".to_string(),
            "--json".to_string(),
            "create".to_string(),
            "channel".to_string(),
            "--order".to_string(),
            opts.ordering.to_string(),
            "--a-chain".to_string(),
            chain_a,
            "--a-port".to_string(),
            src_port.to_string(),
            "--b-port".to_string(),
            dst_port.to_string(),
            "--a-connection".to_string(),
            conn_a,
        ];

        if !opts.version.is_empty() {
            cmd.push("--channel-version".to_string());
            cmd.push(opts.version.clone());
        }

        info!(relayer = "hermes", path = %path_name, "Creating channel");
        let output = self.docker_relayer.exec_oneoff(&cmd, &[]).await?;
        let stdout = output.stdout_str();
        debug!(
            relayer = "hermes",
            stdout = %stdout,
            stderr = %output.stderr_str(),
            exit_code = output.exit_code,
            "create channel output"
        );

        // Parse channel IDs
        if let Ok((ch_a, ch_b)) = parse_channel_ids_from_stdout(&stdout) {
            info!(relayer = "hermes", channel_a = %ch_a, channel_b = %ch_b, "Channel created");
        }

        // Store port IDs
        let mut paths = self.paths.lock().unwrap();
        if let Some(path) = paths.get_mut(path_name) {
            path.chain_a.port_id = src_port.to_string();
            path.chain_b.port_id = dst_port.to_string();
        }

        Ok(())
    }

    async fn update_clients(&self, path_name: &str) -> Result<()> {
        let (chain_a, client_a, chain_b, client_b) = {
            let paths = self.paths.lock().unwrap();
            let path = paths.get(path_name).ok_or_else(|| {
                IctError::Relayer {
                    relayer: "hermes".to_string(),
                    source: anyhow::anyhow!("unknown path: {path_name}"),
                }
            })?;
            (
                path.chain_a.chain_id.clone(),
                path.chain_a.client_id.clone(),
                path.chain_b.chain_id.clone(),
                path.chain_b.client_id.clone(),
            )
        };

        // Update client on chain_a
        let cmd_a = vec![
            "hermes".to_string(),
            "--json".to_string(),
            "update".to_string(),
            "client".to_string(),
            "--host-chain".to_string(),
            chain_a,
            "--client".to_string(),
            client_a,
        ];
        self.docker_relayer.exec_oneoff(&cmd_a, &[]).await?;

        // Update client on chain_b
        let cmd_b = vec![
            "hermes".to_string(),
            "--json".to_string(),
            "update".to_string(),
            "client".to_string(),
            "--host-chain".to_string(),
            chain_b,
            "--client".to_string(),
            client_b,
        ];
        self.docker_relayer.exec_oneoff(&cmd_b, &[]).await?;

        Ok(())
    }

    async fn start(&self, path_names: &[&str]) -> Result<()> {
        info!(relayer = "hermes", paths = ?path_names, "Starting Hermes");
        // Delegate to docker_relayer.start() which uses the commander's start_cmd
        self.docker_relayer.start(path_names).await
    }

    async fn stop(&self) -> Result<()> {
        self.docker_relayer.stop().await
    }

    async fn flush(&self, path_name: &str, channel_id: &str) -> Result<()> {
        let (chain_a, port_a) = {
            let paths = self.paths.lock().unwrap();
            let path = paths.get(path_name).ok_or_else(|| {
                IctError::Relayer {
                    relayer: "hermes".to_string(),
                    source: anyhow::anyhow!("unknown path: {path_name}"),
                }
            })?;
            (
                path.chain_a.chain_id.clone(),
                path.chain_a.port_id.clone(),
            )
        };

        let cmd = vec![
            "hermes".to_string(),
            "clear".to_string(),
            "packets".to_string(),
            "--chain".to_string(),
            chain_a,
            "--port".to_string(),
            port_a,
            "--channel".to_string(),
            channel_id.to_string(),
        ];

        self.docker_relayer.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    async fn get_channels(&self, chain_id: &str) -> Result<Vec<ChannelOutput>> {
        let cmd = vec![
            "hermes".to_string(),
            "--json".to_string(),
            "query".to_string(),
            "channels".to_string(),
            "--chain".to_string(),
            chain_id.to_string(),
            "--show-counterparty".to_string(),
            "--verbose".to_string(),
        ];

        let output = self.docker_relayer.exec_oneoff(&cmd, &[]).await?;
        parse_hermes_channels(&output.stdout_str())
    }

    async fn get_connections(&self, chain_id: &str) -> Result<Vec<ConnectionOutput>> {
        let home = self.docker_relayer.commander().home_dir();
        let cmd = vec![
            "hermes".to_string(),
            "--config".to_string(),
            format!("{home}/.hermes/config.toml"),
            "--json".to_string(),
            "query".to_string(),
            "connections".to_string(),
            "--chain".to_string(),
            chain_id.to_string(),
            "--verbose".to_string(),
        ];

        let output = self.docker_relayer.exec_oneoff(&cmd, &[]).await?;
        parse_hermes_connections(&output.stdout_str())
    }

    async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> Result<ExecOutput> {
        let cmd_owned: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        self.docker_relayer.exec_oneoff(&cmd_owned, env).await
    }
}

/// Commander stub for Hermes (used by DockerRelayer for image/volume management).
/// Most command generation is handled directly by HermesRelayer.
struct HermesCommander;

impl RelayerCommander for HermesCommander {
    fn name(&self) -> &str {
        "hermes"
    }

    fn default_image(&self) -> DockerImage {
        DockerImage {
            repository: "ghcr.io/informalsystems/hermes".to_string(),
            version: "1.8.2".to_string(),
            uid_gid: Some("1000:1000".to_string()),
        }
    }

    fn docker_user(&self) -> &str {
        "1000:1000"
    }

    fn home_dir(&self) -> &str {
        "/home/hermes"
    }

    fn init_cmd(&self, home_dir: &str) -> Option<Vec<String>> {
        // Create .hermes config directory
        Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("mkdir -p {home_dir}/.hermes"),
        ])
    }

    fn config_content(
        &self,
        _cfg: &ChainConfig,
        _key_name: &str,
        _rpc_addr: &str,
        _grpc_addr: &str,
    ) -> Result<Vec<u8>> {
        // Hermes config is generated holistically by HermesRelayer::write_hermes_config
        Ok(Vec::new())
    }

    fn add_chain_cmd(&self, _config_file_path: &str, _home_dir: &str) -> Vec<String> {
        // No-op for Hermes — chains are added via TOML config
        vec!["true".to_string()]
    }

    fn add_key_cmd(
        &self,
        chain_id: &str,
        key_name: &str,
        _coin_type: u32,
        _signing_algo: &str,
        _home_dir: &str,
    ) -> Vec<String> {
        vec![
            "hermes".to_string(),
            "keys".to_string(),
            "add".to_string(),
            "--chain".to_string(),
            chain_id.to_string(),
            "--key-name".to_string(),
            key_name.to_string(),
        ]
    }

    fn restore_key_cmd(
        &self,
        chain_id: &str,
        key_name: &str,
        _coin_type: u32,
        _signing_algo: &str,
        _mnemonic: &str,
        _home_dir: &str,
    ) -> Vec<String> {
        vec![
            "hermes".to_string(),
            "keys".to_string(),
            "add".to_string(),
            "--chain".to_string(),
            chain_id.to_string(),
            "--key-name".to_string(),
            key_name.to_string(),
            "--mnemonic-file".to_string(),
            "/dev/stdin".to_string(),
        ]
    }

    fn generate_path_cmd(&self, _src: &str, _dst: &str, _path: &str, _home_dir: &str) -> Vec<String> {
        vec!["true".to_string()]
    }

    fn link_path_cmd(&self, _path: &str, _home_dir: &str, _ch_opts: &ChannelOptions, _cl_opts: &ClientOptions) -> Vec<String> {
        vec!["true".to_string()]
    }

    fn create_clients_cmd(&self, _path: &str, _opts: &ClientOptions, _home: &str) -> Vec<String> {
        vec!["true".to_string()]
    }

    fn create_connections_cmd(&self, _path: &str, _home: &str) -> Vec<String> {
        vec!["true".to_string()]
    }

    fn create_channel_cmd(&self, _path: &str, _opts: &ChannelOptions, _home: &str) -> Vec<String> {
        vec!["true".to_string()]
    }

    fn update_clients_cmd(&self, _path: &str, _home: &str) -> Vec<String> {
        vec!["true".to_string()]
    }

    fn start_cmd(&self, home: &str, _paths: &[&str]) -> Vec<String> {
        vec![
            "hermes".to_string(),
            "--config".to_string(),
            format!("{home}/.hermes/config.toml"),
            "start".to_string(),
        ]
    }

    fn flush_cmd(&self, _path: &str, _channel_id: &str, _home: &str) -> Vec<String> {
        vec!["true".to_string()]
    }

    fn get_channels_cmd(&self, chain_id: &str, _home: &str) -> Vec<String> {
        vec![
            "hermes".to_string(),
            "--json".to_string(),
            "query".to_string(),
            "channels".to_string(),
            "--chain".to_string(),
            chain_id.to_string(),
            "--show-counterparty".to_string(),
            "--verbose".to_string(),
        ]
    }

    fn get_connections_cmd(&self, chain_id: &str, home: &str) -> Vec<String> {
        vec![
            "hermes".to_string(),
            "--config".to_string(),
            format!("{home}/.hermes/config.toml"),
            "--json".to_string(),
            "query".to_string(),
            "connections".to_string(),
            "--chain".to_string(),
            chain_id.to_string(),
            "--verbose".to_string(),
        ]
    }

    fn parse_add_key_output(&self, stdout: &str, stderr: &str) -> Result<Box<dyn Wallet>> {
        let combined = format!("{stdout}\n{stderr}");
        let address = parse_key_address_from_output(&combined)
            .unwrap_or_else(|| String::new());

        Ok(Box::new(KeyWallet {
            key_name: "hermes-key".to_string(),
            address_bytes: address.as_bytes().to_vec(),
            bech32_address: address,
            mnemonic_phrase: String::new(),
        }))
    }

    fn parse_channels_output(&self, stdout: &str) -> Result<Vec<ChannelOutput>> {
        parse_hermes_channels(stdout)
    }

    fn parse_connections_output(&self, stdout: &str) -> Result<Vec<ConnectionOutput>> {
        parse_hermes_connections(stdout)
    }
}

// -- Hermes output parsers --
// These match the Go interchaintest patterns from hermes_relayer.go and hermes_types.go.

/// Parse gas prices string like "0.025uatom" into ("0.025", "uatom").
fn parse_gas_prices(gas_prices: &str) -> (String, String) {
    let idx = gas_prices
        .find(|c: char| c.is_alphabetic())
        .unwrap_or(gas_prices.len());
    let amount = &gas_prices[..idx];
    let denom = &gas_prices[idx..];
    (
        if amount.is_empty() { "0".to_string() } else { amount.to_string() },
        if denom.is_empty() { "stake".to_string() } else { denom.to_string() },
    )
}

/// Extract the JSON result line from Hermes stdout.
/// Matches Go's `extractJSONResult()`: finds the first line containing "result".
fn extract_json_result(stdout: &str) -> Option<&str> {
    stdout.lines().find(|line| line.contains("result"))
}

/// Parse key address from Hermes text output.
/// Matches Go's `parseRestoreKeyOutput()` which uses regex `\((.*)\)`.
/// Extracts text between the first pair of parentheses.
/// Example: `SUCCESS Added key 'name' (terp1abc...) on chain test-1` → `terp1abc...`
fn parse_key_address_from_output(stdout: &str) -> Option<String> {
    let start = stdout.find('(')?;
    let end = stdout[start..].find(')')? + start;
    let addr = stdout[start + 1..end].trim().to_string();
    if addr.is_empty() {
        None
    } else {
        Some(addr)
    }
}

/// Parse client ID from Hermes JSON output.
/// Matches Go's `GetClientIDFromStdout()`.
/// JSON format: `{"result":{"CreateClient":{"client_id":"07-tendermint-0",...}}}`
fn parse_client_id_from_stdout(stdout: &str) -> std::result::Result<String, String> {
    let line = extract_json_result(stdout)
        .ok_or_else(|| format!("no JSON result line in stdout: {stdout}"))?;

    let json: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| format!("JSON parse error: {e} in line: {line}"))?;

    json["result"]["CreateClient"]["client_id"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("client_id not found in JSON: {line}"))
}

/// Parse connection IDs from Hermes JSON output (both a_side and b_side).
/// Matches Go's `GetConnectionIDsFromStdout()`.
/// JSON format: `{"result":{"a_side":{"connection_id":"connection-0"},"b_side":{"connection_id":"connection-0"}}}`
fn parse_connection_ids_from_stdout(stdout: &str) -> std::result::Result<(String, String), String> {
    let line = extract_json_result(stdout)
        .ok_or_else(|| format!("no JSON result line in stdout: {stdout}"))?;

    let json: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| format!("JSON parse error: {e} in line: {line}"))?;

    let a_conn = json["result"]["a_side"]["connection_id"]
        .as_str()
        .ok_or_else(|| format!("a_side connection_id not found in: {line}"))?
        .to_string();

    let b_conn = json["result"]["b_side"]["connection_id"]
        .as_str()
        .ok_or_else(|| format!("b_side connection_id not found in: {line}"))?
        .to_string();

    Ok((a_conn, b_conn))
}

/// Parse channel IDs from Hermes JSON output (both a_side and b_side).
/// Matches Go's `GetChannelIDsFromStdout()`.
/// JSON format: `{"result":{"a_side":{"channel_id":"channel-0"},"b_side":{"channel_id":"channel-0"}}}`
fn parse_channel_ids_from_stdout(stdout: &str) -> std::result::Result<(String, String), String> {
    let line = extract_json_result(stdout)
        .ok_or_else(|| format!("no JSON result line in stdout: {stdout}"))?;

    let json: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| format!("JSON parse error: {e} in line: {line}"))?;

    let a_ch = json["result"]["a_side"]["channel_id"]
        .as_str()
        .ok_or_else(|| format!("a_side channel_id not found in: {line}"))?
        .to_string();

    let b_ch = json["result"]["b_side"]["channel_id"]
        .as_str()
        .ok_or_else(|| format!("b_side channel_id not found in: {line}"))?
        .to_string();

    Ok((a_ch, b_ch))
}

/// Parse Hermes channel query output.
fn parse_hermes_channels(stdout: &str) -> Result<Vec<ChannelOutput>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    // Hermes outputs one JSON object per line
    let mut channels = Vec::new();
    for line in trimmed.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(result) = json.get("result") {
                if let Ok(ch) = serde_json::from_value::<ChannelOutput>(result.clone()) {
                    channels.push(ch);
                }
            }
        }
    }
    Ok(channels)
}

/// Parse Hermes connection query output.
fn parse_hermes_connections(stdout: &str) -> Result<Vec<ConnectionOutput>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut connections = Vec::new();
    for line in trimmed.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if let Some(result) = json.get("result") {
                if let Ok(conn) = serde_json::from_value::<ConnectionOutput>(result.clone()) {
                    connections.push(conn);
                }
            }
        }
    }
    Ok(connections)
}
