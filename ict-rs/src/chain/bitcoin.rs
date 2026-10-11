//! Bitcoin Core **regtest** sibling sidecar for ict-rs (SVG mint BTC rail).
//!
//! Feature: `bitcoin` (implies `docker`). Not a child of Zakura — joins the
//! same Docker network as Terp / Anvil / DREGG / Zakura.

#![cfg(feature = "bitcoin")]

use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use crate::error::{IctError, Result};
use crate::runtime::{
    ContainerId, ContainerOptions, DockerImage, NetworkId, PortBinding, RuntimeBackend,
};

pub const BTC_RPC_PORT: u16 = 18443;
pub const BTC_RPC_HOST_PORT: u16 = 18453;
pub const BTC_RPC_USER: &str = "ict";
pub const BTC_RPC_PASS: &str = "ict";
pub const BTC_IMAGE_REPO: &str = "bitcoin/bitcoin";
pub const BTC_IMAGE_TAG: &str = "29";

#[derive(Debug, Clone)]
pub struct BitcoinNodeConfig {
    pub rpc_host_port: u16,
    pub network_id: Option<NetworkId>,
    pub test_name: String,
    pub ready_timeout_secs: u64,
    pub image: DockerImage,
}

impl Default for BitcoinNodeConfig {
    fn default() -> Self {
        Self {
            rpc_host_port: BTC_RPC_HOST_PORT,
            network_id: None,
            test_name: "btc-regtest".into(),
            ready_timeout_secs: 90,
            image: DockerImage {
                repository: BTC_IMAGE_REPO.into(),
                version: BTC_IMAGE_TAG.into(),
                uid_gid: None,
            },
        }
    }
}

pub struct BitcoinNode {
    pub config: BitcoinNodeConfig,
    pub container_id: Option<ContainerId>,
    runtime: Arc<dyn RuntimeBackend>,
}

impl BitcoinNode {
    pub fn rpc_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.config.rpc_host_port)
    }

    /// Docker DNS RPC as seen by sibling containers (DREGG watchers).
    pub fn docker_dns_rpc_url(&self) -> String {
        let host = format!("bitcoind-{}", sanitize(&self.config.test_name));
        format!("http://{BTC_RPC_USER}:{BTC_RPC_PASS}@{host}:{BTC_RPC_PORT}")
    }

    pub async fn cli(&self, args: &[&str]) -> Result<String> {
        let id = self.container_id.as_ref().ok_or_else(|| {
            IctError::Config("bitcoin container not started".into())
        })?;
        let mut cmd = vec![
            "bitcoin-cli".into(),
            "-regtest".into(),
            format!("-rpcuser={BTC_RPC_USER}"),
            format!("-rpcpassword={BTC_RPC_PASS}"),
            format!("-rpcport={BTC_RPC_PORT}"),
        ];
        if args.first() != Some(&"createwallet") && args.first() != Some(&"loadwallet") {
            cmd.push("-rpcwallet=mint".into());
        }
        cmd.extend(args.iter().map(|s| s.to_string()));
        let cmd_ref: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
        let out = self.runtime.exec_in_container(id, &cmd_ref, &[]).await?;
        if out.exit_code != 0 {
            return Err(IctError::Config(format!(
                "bitcoin-cli {}: {}",
                args.join(" "),
                out.stderr_str().trim()
            )));
        }
        Ok(out.stdout_str().trim().to_string())
    }

    /// Wallet + 101 blocks + send `sats` to a new address. Returns txid.
    pub async fn pay_sats(&self, sats: u64) -> Result<(String, String)> {
        let _ = self.cli(&["createwallet", "mint"]).await;
        let _ = self.cli(&["loadwallet", "mint"]).await;
        let miner = self.cli(&["getnewaddress"]).await?;
        let _ = self.cli(&["generatetoaddress", "101", &miner]).await?;
        let dest = self.cli(&["getnewaddress"]).await?;
        let btc = format!("{:.8}", sats as f64 / 100_000_000.0);
        let txid = self.cli(&["sendtoaddress", &dest, &btc]).await?;
        if txid.len() < 32 || txid.contains("error") {
            return Err(IctError::Config(format!("sendtoaddress did not return txid: {txid}")));
        }
        let _ = self.cli(&["generatetoaddress", "1", &miner]).await?;
        Ok((txid, dest))
    }

    /// Send `sats` to an HD dest DREGG issued (not a reused bitcoind address).
    pub async fn pay_sats_to(&self, dest: &str, sats: u64) -> Result<String> {
        let _ = self.cli(&["createwallet", "mint"]).await;
        let _ = self.cli(&["loadwallet", "mint"]).await;
        let v = self.cli(&["validateaddress", dest]).await?;
        if !v.contains("\"isvalid\": true") && !v.contains("\"isvalid\":true") {
            return Err(IctError::Config(format!(
                "HD dest not Core-valid: {dest} validateaddress={v}"
            )));
        }
        let miner = self.cli(&["getnewaddress"]).await?;
        let _ = self.cli(&["generatetoaddress", "101", &miner]).await?;
        let btc = format!("{:.8}", sats as f64 / 100_000_000.0);
        let txid = self.cli(&["sendtoaddress", dest, &btc]).await?;
        if txid.len() < 32 || txid.contains("error") {
            return Err(IctError::Config(format!("sendtoaddress {dest}: {txid}")));
        }
        let _ = self.cli(&["generatetoaddress", "1", &miner]).await?;
        Ok(txid.trim().to_string())
    }

    /// Refund `sats` to `dest` (must be a chain tx). Returns refund txid.
    pub async fn refund_sats(&self, dest: &str, sats: u64) -> Result<String> {
        let miner = self.cli(&["getnewaddress"]).await?;
        let btc = format!("{:.8}", sats as f64 / 100_000_000.0);
        let txid = self.cli(&["sendtoaddress", dest, &btc]).await?;
        if txid.len() < 32 || txid.contains("error") {
            return Err(IctError::Config(format!("refund sendtoaddress: {txid}")));
        }
        let _ = self.cli(&["generatetoaddress", "1", &miner]).await?;
        Ok(txid.trim().to_string())
    }

    pub async fn teardown(&mut self) -> Result<()> {
        if let Some(id) = self.container_id.take() {
            let _ = self.runtime.stop_container(&id).await;
            let _ = self.runtime.remove_container(&id).await;
        }
        Ok(())
    }
}

pub async fn spawn_bitcoind_regtest(
    runtime: Arc<dyn RuntimeBackend>,
    config: BitcoinNodeConfig,
) -> Result<BitcoinNode> {
    let container_name = format!(
        "ict-{}-bitcoind-{}",
        sanitize(&config.test_name),
        config.rpc_host_port
    );
    {
        let stale = ContainerId(container_name.clone());
        let _ = runtime.stop_container(&stale).await;
        let _ = runtime.remove_container(&stale).await;
    }
    runtime.pull_image(&config.image).await?;

    let opts = ContainerOptions {
        image: config.image.clone(),
        name: container_name.clone(),
        network_id: config.network_id.clone(),
        env: vec![],
        cmd: vec![
            "-regtest".into(),
            "-server".into(),
            "-printtoconsole".into(),
            format!("-rpcuser={BTC_RPC_USER}"),
            format!("-rpcpassword={BTC_RPC_PASS}"),
            "-rpcbind=0.0.0.0".into(),
            "-rpcallowip=0.0.0.0/0".into(),
            format!("-rpcport={BTC_RPC_PORT}"),
            "-fallbackfee=0.0001".into(),
        ],
        entrypoint: Some(vec!["bitcoind".into()]),
        ports: vec![PortBinding {
            host_port: config.rpc_host_port,
            container_port: BTC_RPC_PORT,
            protocol: "tcp".into(),
        }],
        volumes: vec![],
        labels: vec![
            ("ict.test".into(), config.test_name.clone()),
            ("ict.sidecar".into(), "bitcoind".into()),
        ],
        hostname: Some(format!("bitcoind-{}", sanitize(&config.test_name))),
    };

    info!(
        container = %container_name,
        rpc = %format!("http://127.0.0.1:{}", config.rpc_host_port),
        "Spawning bitcoind regtest sibling sidecar"
    );
    let id = runtime.create_container(&opts).await?;
    runtime.start_container(&id).await?;

    let node = BitcoinNode {
        config,
        container_id: Some(id),
        runtime,
    };
    wait_rpc(&node).await?;
    Ok(node)
}

async fn wait_rpc(node: &BitcoinNode) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(node.config.ready_timeout_secs);
    loop {
        if node.cli(&["getblockchaininfo"]).await.is_ok() {
            info!("bitcoind RPC ready");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(IctError::Timeout {
                what: "bitcoind getblockchaininfo".into(),
                duration: Duration::from_secs(node.config.ready_timeout_secs),
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}
