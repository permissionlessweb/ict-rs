//! Docker-based IBC relayer infrastructure.
//!
//! `DockerRelayer` manages a relayer's container lifecycle and delegates
//! CLI-specific command generation to a `RelayerCommander` trait. This mirrors
//! Go ICT's `DockerRelayer` + `RelayerCommander` pattern.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::info;

use crate::chain::ChainConfig;
use crate::error::Result;
use crate::ibc::{ChannelOptions, ChannelOutput, ClientOptions, ConnectionOutput};
use crate::relayer::Relayer;
use crate::runtime::{
    ContainerId, ContainerOptions, DockerImage, NetworkId, RuntimeBackend, VolumeMount,
};
use crate::tx::ExecOutput;
use crate::wallet::Wallet;

/// Trait for relayer-specific CLI command generation.
///
/// Each relayer implementation (CosmosRly, Hermes) provides its own commander
/// that knows how to build CLI arguments and parse output for that particular
/// relayer binary.
pub trait RelayerCommander: Send + Sync {
    /// Human-readable name (e.g., "cosmos-relayer", "hermes").
    fn name(&self) -> &str;

    /// Default Docker image for this relayer.
    fn default_image(&self) -> DockerImage;

    /// Docker user as "uid:gid".
    fn docker_user(&self) -> &str;

    /// Home directory inside the container.
    fn home_dir(&self) -> &str;

    // -- Initialization --

    /// Optional init command to run on first setup.
    fn init_cmd(&self, home_dir: &str) -> Option<Vec<String>>;

    // -- Config generation --

    /// Generate per-chain config content (JSON for rly, TOML for hermes).
    fn config_content(
        &self,
        cfg: &ChainConfig,
        key_name: &str,
        rpc_addr: &str,
        grpc_addr: &str,
    ) -> Result<Vec<u8>>;

    // -- Command generation (all return Vec<String>) --

    fn add_chain_cmd(&self, config_file_path: &str, home_dir: &str) -> Vec<String>;

    fn add_key_cmd(
        &self,
        chain_id: &str,
        key_name: &str,
        coin_type: u32,
        signing_algo: &str,
        home_dir: &str,
    ) -> Vec<String>;

    fn restore_key_cmd(
        &self,
        chain_id: &str,
        key_name: &str,
        coin_type: u32,
        signing_algo: &str,
        mnemonic: &str,
        home_dir: &str,
    ) -> Vec<String>;

    fn generate_path_cmd(
        &self,
        src: &str,
        dst: &str,
        path: &str,
        home_dir: &str,
    ) -> Vec<String>;

    fn link_path_cmd(
        &self,
        path: &str,
        home_dir: &str,
        ch_opts: &ChannelOptions,
        cl_opts: &ClientOptions,
    ) -> Vec<String>;

    fn create_clients_cmd(
        &self,
        path: &str,
        opts: &ClientOptions,
        home: &str,
    ) -> Vec<String>;

    fn create_connections_cmd(&self, path: &str, home: &str) -> Vec<String>;

    fn create_channel_cmd(
        &self,
        path: &str,
        opts: &ChannelOptions,
        home: &str,
    ) -> Vec<String>;

    fn update_clients_cmd(&self, path: &str, home: &str) -> Vec<String>;

    fn start_cmd(&self, home: &str, paths: &[&str]) -> Vec<String>;

    fn flush_cmd(&self, path: &str, channel_id: &str, home: &str) -> Vec<String>;

    fn get_channels_cmd(&self, chain_id: &str, home: &str) -> Vec<String>;

    fn get_connections_cmd(&self, chain_id: &str, home: &str) -> Vec<String>;

    // -- Output parsing --

    fn parse_add_key_output(&self, stdout: &str, stderr: &str) -> Result<Box<dyn Wallet>>;

    fn parse_channels_output(&self, stdout: &str) -> Result<Vec<ChannelOutput>>;

    fn parse_connections_output(&self, stdout: &str) -> Result<Vec<ConnectionOutput>>;
}

/// A Docker-based IBC relayer that delegates CLI specifics to a `RelayerCommander`.
pub struct DockerRelayer {
    commander: Box<dyn RelayerCommander>,
    runtime: Arc<dyn RuntimeBackend>,
    network_id: String,
    test_name: String,
    volume_name: String,
    home_dir: String,
    wallets: Mutex<HashMap<String, Box<dyn Wallet>>>,
    /// Background relayer container (started by start(), stopped by stop()).
    bg_container_id: Mutex<Option<ContainerId>>,
}

impl std::fmt::Debug for DockerRelayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerRelayer")
            .field("name", &self.commander.name())
            .field("test_name", &self.test_name)
            .field("volume_name", &self.volume_name)
            .finish_non_exhaustive()
    }
}

impl DockerRelayer {
    /// Create a new DockerRelayer.
    pub async fn new(
        commander: Box<dyn RelayerCommander>,
        runtime: Arc<dyn RuntimeBackend>,
        test_name: &str,
        network_id: &str,
    ) -> Result<Self> {
        let volume_name = format!("{}-{}-relayer", test_name, commander.name());
        let home_dir = commander.home_dir().to_string();

        let relayer = Self {
            commander,
            runtime,
            network_id: network_id.to_string(),
            test_name: test_name.to_string(),
            volume_name,
            home_dir,
            wallets: Mutex::new(HashMap::new()),
            bg_container_id: Mutex::new(None),
        };

        // Run init command if the commander provides one
        if let Some(init_cmd) = relayer.commander.init_cmd(&relayer.home_dir) {
            relayer.exec_oneoff(&init_cmd, &[]).await?;
        }

        Ok(relayer)
    }

    /// Get the commander (for subclasses like HermesRelayer that need direct access).
    pub fn commander(&self) -> &dyn RelayerCommander {
        self.commander.as_ref()
    }

    /// Get the runtime.
    pub fn runtime(&self) -> &Arc<dyn RuntimeBackend> {
        &self.runtime
    }

    /// Get the volume name.
    pub fn volume_name(&self) -> &str {
        &self.volume_name
    }

    /// Get the network ID.
    pub fn network_id(&self) -> &str {
        &self.network_id
    }

    /// Get the test name.
    pub fn test_name(&self) -> &str {
        &self.test_name
    }

    /// Execute a one-off command in a temporary container that shares the relayer volume.
    ///
    /// Creates an idle container, execs the command inside it via `exec_in_container`,
    /// then removes the container. This ensures compatibility with the mock runtime
    /// (which routes commands through `exec_in_container`'s smart response matching).
    pub async fn exec_oneoff(
        &self,
        cmd: &[String],
        env: &[(&str, &str)],
    ) -> Result<ExecOutput> {
        let image = self.commander.default_image();

        let opts = ContainerOptions {
            image: image.clone(),
            name: format!(
                "ict-{}-{}-exec-{}",
                self.test_name,
                self.commander.name(),
                rand::random::<u32>()
            ),
            network_id: Some(NetworkId(self.network_id.clone())),
            env: Vec::new(),
            // Idle entrypoint — we exec the real command separately
            cmd: vec![
                "-c".to_string(),
                "trap 'exit 0' TERM; while true; do sleep 1; done".to_string(),
            ],
            entrypoint: Some(vec!["/bin/sh".to_string()]),
            ports: Vec::new(),
            volumes: vec![VolumeMount {
                source: self.volume_name.clone(),
                target: self.home_dir.clone(),
                read_only: false,
            }],
            labels: vec![
                ("ict.test".to_string(), self.test_name.clone()),
                (
                    "ict.relayer".to_string(),
                    self.commander.name().to_string(),
                ),
            ],
            hostname: None,
        };

        // Pull image first
        self.runtime.pull_image(&image).await?;

        let container_id = self.runtime.create_container(&opts).await?;
        self.runtime.start_container(&container_id).await?;

        // Exec the actual command inside the running container
        let cmd_refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let result = self
            .runtime
            .exec_in_container(&container_id, &cmd_refs, env)
            .await;

        // Clean up the temporary container
        let _ = self.runtime.stop_container(&container_id).await;
        let _ = self.runtime.remove_container(&container_id).await;

        result
    }

    /// Write a file into the relayer volume via a temporary container.
    ///
    /// Uses `printf '%s'` with shell-escaped content rather than `xxd` or `base64`,
    /// because minimal container images (e.g. Hermes) may not have those tools.
    /// This is POSIX-compliant and works in any `/bin/sh`.
    pub async fn write_file(&self, path: &str, content: &[u8]) -> Result<()> {
        let text = std::str::from_utf8(content).map_err(|e| {
            crate::error::IctError::Runtime(anyhow::anyhow!(
                "write_file: non-UTF8 content: {e}"
            ))
        })?;

        // Shell-escape single quotes: replace ' with '\'' (end quote, literal quote, resume quote)
        let escaped = text.replace('\'', "'\\''");

        // Build: mkdir -p "$(dirname '<path>')" && printf '%s' '<escaped>' > '<path>'
        let mut shell_cmd =
            String::with_capacity(escaped.len() + path.len() * 2 + 80);
        shell_cmd.push_str("mkdir -p \"$(dirname '");
        shell_cmd.push_str(path);
        shell_cmd.push_str("')\" && printf '%s' '");
        shell_cmd.push_str(&escaped);
        shell_cmd.push_str("' > '");
        shell_cmd.push_str(path);
        shell_cmd.push('\'');

        let cmd = vec![
            "sh".to_string(),
            "-c".to_string(),
            shell_cmd,
        ];
        let output = self.exec_oneoff(&cmd, &[]).await?;

        if output.exit_code != 0 {
            return Err(crate::error::IctError::Runtime(anyhow::anyhow!(
                "write_file '{}' failed (exit {}): {}",
                path,
                output.exit_code,
                output.stderr_str()
            )));
        }

        Ok(())
    }

    /// Read a file from the relayer volume.
    pub async fn read_file(&self, path: &str) -> Result<String> {
        let cmd = vec!["cat".to_string(), path.to_string()];
        let output = self.exec_oneoff(&cmd, &[]).await?;
        Ok(output.stdout_str())
    }

    /// Store a wallet for a chain.
    fn store_wallet(&self, chain_id: &str, wallet: Box<dyn Wallet>) {
        let mut wallets = self.wallets.lock().unwrap();
        wallets.insert(chain_id.to_string(), wallet);
    }
}

#[async_trait]
impl Relayer for DockerRelayer {
    async fn add_key(&self, chain_id: &str, key_name: &str) -> Result<Box<dyn Wallet>> {
        let cfg_chain_id = chain_id;
        let cmd = self.commander.add_key_cmd(
            chain_id,
            key_name,
            118, // default coin_type
            "secp256k1",
            &self.home_dir,
        );

        info!(relayer = %self.commander.name(), chain = %chain_id, key = %key_name, "Adding key");
        let output = self.exec_oneoff(&cmd, &[]).await?;

        let wallet = self
            .commander
            .parse_add_key_output(&output.stdout_str(), &output.stderr_str())?;

        self.store_wallet(cfg_chain_id, wallet.clone_wallet());
        Ok(wallet)
    }

    async fn restore_key(
        &self,
        chain_id: &str,
        key_name: &str,
        mnemonic: &str,
    ) -> Result<()> {
        let cmd = self.commander.restore_key_cmd(
            chain_id,
            key_name,
            118,
            "secp256k1",
            mnemonic,
            &self.home_dir,
        );

        info!(relayer = %self.commander.name(), chain = %chain_id, key = %key_name, "Restoring key");
        self.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    fn get_wallet(&self, _chain_id: &str) -> Option<&dyn Wallet> {
        // We can't return a reference to data behind a Mutex easily.
        // Return None; callers should use add_key which returns the wallet.
        None
    }

    async fn add_chain_configuration(
        &self,
        config: &ChainConfig,
        key_name: &str,
        rpc_addr: &str,
        grpc_addr: &str,
    ) -> Result<()> {
        let content = self
            .commander
            .config_content(config, key_name, rpc_addr, grpc_addr)?;

        let config_path = format!("{}/chains/{}.json", self.home_dir, config.chain_id);
        self.write_file(&config_path, &content).await?;

        let cmd = self
            .commander
            .add_chain_cmd(&config_path, &self.home_dir);
        info!(relayer = %self.commander.name(), chain = %config.chain_id, "Adding chain configuration");
        self.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    async fn generate_path(
        &self,
        src_chain_id: &str,
        dst_chain_id: &str,
        path_name: &str,
    ) -> Result<()> {
        let cmd =
            self.commander
                .generate_path_cmd(src_chain_id, dst_chain_id, path_name, &self.home_dir);
        info!(relayer = %self.commander.name(), path = %path_name, "Generating path");
        self.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    async fn link_path(&self, path_name: &str, opts: &ChannelOptions) -> Result<()> {
        let cl_opts = ClientOptions::default();
        let cmd =
            self.commander
                .link_path_cmd(path_name, &self.home_dir, opts, &cl_opts);
        info!(relayer = %self.commander.name(), path = %path_name, "Linking path");
        self.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    async fn create_clients(&self, path_name: &str, opts: &ClientOptions) -> Result<()> {
        let cmd = self
            .commander
            .create_clients_cmd(path_name, opts, &self.home_dir);
        info!(relayer = %self.commander.name(), path = %path_name, "Creating clients");
        self.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    async fn create_connections(&self, path_name: &str) -> Result<()> {
        let cmd = self
            .commander
            .create_connections_cmd(path_name, &self.home_dir);
        info!(relayer = %self.commander.name(), path = %path_name, "Creating connections");
        self.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    async fn create_channel(&self, path_name: &str, opts: &ChannelOptions) -> Result<()> {
        let cmd = self
            .commander
            .create_channel_cmd(path_name, opts, &self.home_dir);
        info!(relayer = %self.commander.name(), path = %path_name, "Creating channel");
        self.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    async fn update_clients(&self, path_name: &str) -> Result<()> {
        let cmd = self
            .commander
            .update_clients_cmd(path_name, &self.home_dir);
        info!(relayer = %self.commander.name(), path = %path_name, "Updating clients");
        self.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    async fn start(&self, path_names: &[&str]) -> Result<()> {
        let cmd = self.commander.start_cmd(&self.home_dir, path_names);
        let image = self.commander.default_image();

        info!(relayer = %self.commander.name(), paths = ?path_names, "Starting relayer");

        // Pull image
        self.runtime.pull_image(&image).await?;

        // Go interchaintest sets the start command as the Entrypoint (not Cmd).
        // This overrides the image's default ENTRYPOINT (e.g. /usr/bin/hermes)
        // so the command runs directly: `hermes --config ... start`
        let opts = ContainerOptions {
            image,
            name: format!("ict-{}-{}-bg", self.test_name, self.commander.name()),
            network_id: Some(NetworkId(self.network_id.clone())),
            env: Vec::new(),
            cmd: Vec::new(),
            entrypoint: Some(cmd),
            ports: Vec::new(),
            volumes: vec![VolumeMount {
                source: self.volume_name.clone(),
                target: self.home_dir.clone(),
                read_only: false,
            }],
            labels: vec![
                ("ict.test".to_string(), self.test_name.clone()),
                ("ict.relayer".to_string(), self.commander.name().to_string()),
                ("ict.role".to_string(), "relayer-bg".to_string()),
            ],
            hostname: Some(format!("{}-relayer", self.commander.name())),
        };

        let container_id = self.runtime.create_container(&opts).await?;
        self.runtime.start_container(&container_id).await?;

        let mut bg = self.bg_container_id.lock().unwrap();
        *bg = Some(container_id);

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let container_id = {
            let mut bg = self.bg_container_id.lock().unwrap();
            bg.take()
        };

        if let Some(id) = container_id {
            info!(relayer = %self.commander.name(), "Stopping relayer");
            let _ = self.runtime.stop_container(&id).await;
            let _ = self.runtime.remove_container(&id).await;
        }

        // Clean up volume
        let _ = self.runtime.remove_volume(&self.volume_name).await;

        Ok(())
    }

    async fn flush(&self, path_name: &str, channel_id: &str) -> Result<()> {
        let cmd = self
            .commander
            .flush_cmd(path_name, channel_id, &self.home_dir);
        info!(relayer = %self.commander.name(), path = %path_name, channel = %channel_id, "Flushing");
        self.exec_oneoff(&cmd, &[]).await?;
        Ok(())
    }

    async fn get_channels(&self, chain_id: &str) -> Result<Vec<ChannelOutput>> {
        let cmd = self
            .commander
            .get_channels_cmd(chain_id, &self.home_dir);
        let output = self.exec_oneoff(&cmd, &[]).await?;
        self.commander
            .parse_channels_output(&output.stdout_str())
    }

    async fn get_connections(&self, chain_id: &str) -> Result<Vec<ConnectionOutput>> {
        let cmd = self
            .commander
            .get_connections_cmd(chain_id, &self.home_dir);
        let output = self.exec_oneoff(&cmd, &[]).await?;
        self.commander
            .parse_connections_output(&output.stdout_str())
    }

    async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> Result<ExecOutput> {
        let cmd_owned: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
        self.exec_oneoff(&cmd_owned, env).await
    }
}

/// Helper trait for cloning boxed wallets.
trait CloneWallet {
    fn clone_wallet(&self) -> Box<dyn Wallet>;
}

impl<T: Wallet + Clone + 'static> CloneWallet for T {
    fn clone_wallet(&self) -> Box<dyn Wallet> {
        Box::new(self.clone())
    }
}

impl CloneWallet for Box<dyn Wallet> {
    fn clone_wallet(&self) -> Box<dyn Wallet> {
        // We can't clone a trait object directly. Return a simple wrapper.
        Box::new(crate::wallet::KeyWallet {
            key_name: self.key_name().to_string(),
            address_bytes: self.address().to_vec(),
            bech32_address: self.formatted_address(),
            mnemonic_phrase: self.mnemonic().to_string(),
        })
    }
}
