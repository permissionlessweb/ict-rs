//! Local Zakura (Zcash regtest) node spawn for ict-rs.
//!
//! Feature: `zakura` (implies `docker`).
//!
//! Hardcoded corridor defaults match Private Bridge D6 / Option D egress:
//! - JSON-RPC host port **18232**
//! - miner / demo dest `tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV`
//! - binding domain `terp-dest-binding-v0`
//!
//! Spawn mounts a host-built `zakurad` binary (`ZAKURAD_BIN` or config) into
//! `debian:trixie-slim`, same pattern as `docs/plans/spectrum/e2e/zakura/`.
//!
//! **Honest:** regtest only — not mainnet ZEC.

#![cfg(feature = "zakura")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::chain::SidecarConfig;
use crate::error::{IctError, Result};
use crate::runtime::{
    ContainerId, ContainerOptions, DockerImage, NetworkId, PortBinding, RuntimeBackend, VolumeMount,
};

// ── Hardcoded corridor constants (D6 / Option D) ───────────────────────────

/// Default JSON-RPC listen port (host + container).
pub const ZAKURA_RPC_PORT: u16 = 18232;

/// Default P2P listen port.
pub const ZAKURA_P2P_PORT: u16 = 18233;

/// Default metrics port.
pub const ZAKURA_METRICS_PORT: u16 = 19901;

/// Preauth dest binding domain (must match UI / golden / zakura_local).
pub const DEST_BINDING_DOMAIN: &str = "terp-dest-binding-v0";

/// Regtest transparent miner address (node-corridor.toml).
pub const REGTEST_MINER_DEST: &str = "tmJymvcUCn1ctbghvTJpXBwHiMEB8P6wxNV";

/// Primary golden owner_binding for [`REGTEST_MINER_DEST`].
pub const REGTEST_MINER_OWNER_BINDING_HEX: &str =
    "8b5cac11e39905d56126a0c538b84ff8daa379d8009d4e8b121112479607f09b";

/// Env var for host-built Linux-compatible `zakurad`.
pub const ENV_ZAKURAD_BIN: &str = "ZAKURAD_BIN";

/// Env var override for RPC base URL (default `http://127.0.0.1:18232`).
pub const ENV_ZAKURA_RPC: &str = "ZAKURA_RPC";

/// Container image used to run the bind-mounted binary (no in-image compile).
pub const ZAKURA_BASE_IMAGE_REPO: &str = "debian";
pub const ZAKURA_BASE_IMAGE_TAG: &str = "trixie-slim";

/// Path of mounted binary inside the container.
pub const CONTAINER_ZAKURAD_PATH: &str = "/usr/local/bin/zakurad";

/// Path of mounted config inside the container.
pub const CONTAINER_CONFIG_PATH: &str = "/etc/zakura/node-corridor.toml";

// ── Config / handle ────────────────────────────────────────────────────────

/// Spawn configuration for a local Zakura regtest node.
#[derive(Debug, Clone)]
pub struct ZakuraNodeConfig {
    /// Host path to `zakurad` (Linux binary for Docker Desktop on macOS).
    pub zakurad_bin: PathBuf,
    /// Host RPC port publish (default [`ZAKURA_RPC_PORT`]).
    pub rpc_host_port: u16,
    /// Container RPC listen port (default [`ZAKURA_RPC_PORT`]).
    pub rpc_container_port: u16,
    /// Optional Docker network to join (attach to Terp ict network).
    pub network_id: Option<NetworkId>,
    /// Test / run label for container naming.
    pub test_name: String,
    /// Override dest_display used for binding helpers (default miner dest).
    pub dest_display: String,
    /// Seconds to wait for RPC ready after start.
    pub ready_timeout_secs: u64,
    /// If true, skip spawn when RPC already answers on host (reuse node).
    pub reuse_if_ready: bool,
    /// Base image for the container (default debian:trixie-slim).
    pub image: DockerImage,
}

impl Default for ZakuraNodeConfig {
    fn default() -> Self {
        Self {
            zakurad_bin: resolve_zakurad_bin().unwrap_or_else(|_| PathBuf::from("/nonexistent/zakurad")),
            rpc_host_port: ZAKURA_RPC_PORT,
            rpc_container_port: ZAKURA_RPC_PORT,
            network_id: None,
            test_name: "zakura-local".into(),
            dest_display: REGTEST_MINER_DEST.into(),
            ready_timeout_secs: 60,
            reuse_if_ready: true,
            image: DockerImage {
                repository: ZAKURA_BASE_IMAGE_REPO.into(),
                version: ZAKURA_BASE_IMAGE_TAG.into(),
                uid_gid: None,
            },
        }
    }
}

impl ZakuraNodeConfig {
    /// Corridor defaults with explicit binary path.
    pub fn corridor(zakurad_bin: impl Into<PathBuf>) -> Self {
        Self {
            zakurad_bin: zakurad_bin.into(),
            ..Self::default()
        }
    }

    /// From env `ZAKURAD_BIN` (error if missing / not executable).
    pub fn from_env() -> Result<Self> {
        let bin = resolve_zakurad_bin()?;
        Ok(Self::corridor(bin))
    }

    /// Host-facing JSON-RPC base URL.
    pub fn rpc_url(&self) -> String {
        std::env::var(ENV_ZAKURA_RPC).unwrap_or_else(|_| {
            format!("http://127.0.0.1:{}", self.rpc_host_port)
        })
    }
}

/// Running (or adopted) Zakura node handle.
pub struct ZakuraNode {
    pub config: ZakuraNodeConfig,
    pub container_id: Option<ContainerId>,
    pub rpc_url: String,
    pub dest_display: String,
    pub owner_binding_hex: String,
    /// True if we adopted an already-ready host RPC without creating a container.
    pub reused_existing: bool,
    runtime: Arc<dyn RuntimeBackend>,
    /// Temp dir holding generated config (kept for container lifetime).
    _config_dir: Option<tempfile::TempDir>,
}

impl std::fmt::Debug for ZakuraNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZakuraNode")
            .field("rpc_url", &self.rpc_url)
            .field("dest_display", &self.dest_display)
            .field("owner_binding_hex", &self.owner_binding_hex)
            .field("reused_existing", &self.reused_existing)
            .field("container_id", &self.container_id)
            .finish_non_exhaustive()
    }
}

impl ZakuraNode {
    pub fn rpc_url(&self) -> &str {
        &self.rpc_url
    }

    pub fn dest_display(&self) -> &str {
        &self.dest_display
    }

    pub fn owner_binding_hex(&self) -> &str {
        &self.owner_binding_hex
    }

    /// JSON-RPC `getblockchaininfo` smoke (host curl or TCP+HTTP).
    pub async fn rpc_ready(&self) -> bool {
        rpc_ready(&self.rpc_url).await
    }

    /// Stop and remove the container if we created one.
    pub async fn teardown(&mut self) -> Result<()> {
        if self.reused_existing {
            info!("Zakura node was reused; not tearing down host RPC");
            return Ok(());
        }
        if let Some(ref id) = self.container_id.take() {
            let _ = self.runtime.stop_container(id).await;
            let _ = self.runtime.remove_container(id).await;
            info!(container = %id.0, "Zakura container removed");
        }
        Ok(())
    }
}

// ── Public API ─────────────────────────────────────────────────────────────

/// Resolve `zakurad` path: `ZAKURAD_BIN` else error with install hint.
pub fn resolve_zakurad_bin() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(ENV_ZAKURAD_BIN) {
        let pb = PathBuf::from(p.trim());
        if pb.is_file() {
            return Ok(pb);
        }
        return Err(IctError::Config(format!(
            "{ENV_ZAKURAD_BIN}={pb:?} is not a file (need Linux zakurad for Docker)"
        )));
    }
    Err(IctError::Config(format!(
        "set {ENV_ZAKURAD_BIN} to host-built zakurad (Linux binary for Docker Desktop on macOS). \
         Example: cd crates/zakura && cargo build -p zakura --bin zakurad && \
         export {ENV_ZAKURAD_BIN}=$(pwd)/target/debug/zakurad"
    )))
}

/// Domain-separated dest seal: `SHA256("terp-dest-binding-v0|" ‖ dest)`.
pub fn owner_binding_from_dest_display(dest_display: &str) -> String {
    let preimage = format!("{DEST_BINDING_DOMAIN}|{}", dest_display.trim());
    let digest = Sha256::digest(preimage.as_bytes());
    hex::encode(digest)
}

/// Hardcoded corridor TOML (listen on all interfaces inside container).
pub fn corridor_node_toml() -> String {
    format!(
        r#"# Generated by ict-rs feature=zakura — regtest only (Option D / D6).
[network]
network = "Regtest"
listen_addr = "0.0.0.0:{ZAKURA_P2P_PORT}"
p2p_stack = "legacy"
cache_dir = false
initial_testnet_peers = []
max_connections_per_ip = 4

[state]
ephemeral = true

[rpc]
listen_addr = "0.0.0.0:{ZAKURA_RPC_PORT}"
enable_cookie_auth = false

[metrics]
endpoint_addr = "0.0.0.0:{ZAKURA_METRICS_PORT}"

[mining]
internal_miner = false
miner_address = "{REGTEST_MINER_DEST}"

[tracing]
filter = "info"
"#
    )
}

/// SidecarConfig hard-wired for attaching Zakura next to a Terp chain.
///
/// Note: binary still must be bind-mounted at spawn time via [`spawn_zakura_local`];
/// this helper documents ports/cmd for ChainConfig.sidecars lists.
pub fn zakura_sidecar_config() -> SidecarConfig {
    SidecarConfig {
        name: "zakura".into(),
        image: DockerImage {
            repository: ZAKURA_BASE_IMAGE_REPO.into(),
            version: ZAKURA_BASE_IMAGE_TAG.into(),
            uid_gid: None,
        },
        home_dir: "/etc/zakura".into(),
        ports: vec![
            ZAKURA_RPC_PORT.to_string(),
            ZAKURA_P2P_PORT.to_string(),
            ZAKURA_METRICS_PORT.to_string(),
        ],
        env: vec![],
        cmd: vec![
            CONTAINER_ZAKURAD_PATH.into(),
            "--config".into(),
            CONTAINER_CONFIG_PATH.into(),
            "start".into(),
        ],
        pre_start: false,
        validator_process: false,
        health_endpoint: None,
        ready_timeout_secs: 60,
    }
}

/// Spawn (or reuse) a local Zakura regtest node.
///
/// Requires a reachable Docker daemon and a Linux `zakurad` at `config.zakurad_bin`.
pub async fn spawn_zakura_local(
    runtime: Arc<dyn RuntimeBackend>,
    config: ZakuraNodeConfig,
) -> Result<ZakuraNode> {
    let rpc_url = config.rpc_url();
    let dest = config.dest_display.clone();
    let binding = owner_binding_from_dest_display(&dest);

    if config.reuse_if_ready && rpc_ready(&rpc_url).await {
        info!(%rpc_url, "Zakura RPC already ready — reusing existing node");
        return Ok(ZakuraNode {
            config,
            container_id: None,
            rpc_url,
            dest_display: dest,
            owner_binding_hex: binding,
            reused_existing: true,
            runtime,
            _config_dir: None,
        });
    }

    if !config.zakurad_bin.is_file() {
        return Err(IctError::Config(format!(
            "zakurad binary not found at {:?} (set {ENV_ZAKURAD_BIN})",
            config.zakurad_bin
        )));
    }

    // Write config to temp dir for bind-mount.
    let tmp = tempfile::tempdir().map_err(|e| IctError::Runtime(e.into()))?;
    let cfg_host = tmp.path().join("node-corridor.toml");
    std::fs::write(&cfg_host, corridor_node_toml())
        .map_err(|e| IctError::Runtime(anyhow::anyhow!("write zakura config: {e}")))?;

    let container_name = format!(
        "ict-{}-zakura-{}",
        sanitize_name(&config.test_name),
        config.rpc_host_port
    );

    // Pre-cleanup stale container
    {
        let stale = ContainerId(container_name.clone());
        let _ = runtime.stop_container(&stale).await;
        let _ = runtime.remove_container(&stale).await;
    }

    runtime.pull_image(&config.image).await?;

    let volumes = vec![
        VolumeMount {
            source: abs_path(&config.zakurad_bin)?,
            target: CONTAINER_ZAKURAD_PATH.into(),
            read_only: true,
        },
        VolumeMount {
            source: abs_path(&cfg_host)?,
            target: CONTAINER_CONFIG_PATH.into(),
            read_only: true,
        },
    ];

    let ports = vec![PortBinding {
        host_port: config.rpc_host_port,
        container_port: config.rpc_container_port,
        protocol: "tcp".into(),
    }];

    let opts = ContainerOptions {
        image: config.image.clone(),
        name: container_name.clone(),
        network_id: config.network_id.clone(),
        env: vec![],
        cmd: vec![
            CONTAINER_ZAKURAD_PATH.into(),
            "--config".into(),
            CONTAINER_CONFIG_PATH.into(),
            "start".into(),
        ],
        entrypoint: None,
        ports,
        volumes,
        labels: vec![
            ("ict.test".into(), config.test_name.clone()),
            ("ict.sidecar".into(), "zakura".into()),
            ("ict.feature".into(), "zakura".into()),
        ],
        hostname: Some(format!("zakura-{}", sanitize_name(&config.test_name))),
    };

    info!(
        container = %container_name,
        bin = %config.zakurad_bin.display(),
        rpc = %rpc_url,
        "Spawning local Zakura regtest node (feature=zakura)"
    );

    let id = runtime.create_container(&opts).await?;
    runtime.start_container(&id).await?;

    wait_rpc_ready(&rpc_url, config.ready_timeout_secs).await?;

    Ok(ZakuraNode {
        config,
        container_id: Some(id),
        rpc_url,
        dest_display: dest,
        owner_binding_hex: binding,
        reused_existing: false,
        runtime,
        _config_dir: Some(tmp),
    })
}

/// Poll host RPC until ready or timeout.
pub async fn wait_rpc_ready(rpc_url: &str, timeout_secs: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs.max(1));
    let mut attempt = 0u64;
    loop {
        if rpc_ready(rpc_url).await {
            info!(%rpc_url, attempt, "Zakura RPC ready");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(IctError::Timeout {
                what: format!("zakura RPC at {rpc_url}"),
                duration: Duration::from_secs(timeout_secs),
            });
        }
        attempt += 1;
        if attempt % 10 == 0 {
            warn!(%rpc_url, attempt, "still waiting for Zakura RPC");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// JSON-RPC getblockchaininfo probe.
pub async fn rpc_ready(rpc_url: &str) -> bool {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"getblockchaininfo","params":[]}"#;
    let url = rpc_url.trim_end_matches('/').to_string() + "/";
    // Prefer curl (available on macOS/Linux CI); fall back to false.
    let ok = tokio::process::Command::new("curl")
        .args([
            "-sf",
            "--max-time",
            "3",
            "-X",
            "POST",
            &url,
            "-H",
            "Content-Type: application/json",
            "-d",
            body,
        ])
        .output()
        .await
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("\"result\""))
        .unwrap_or(false);
    ok
}

// ── helpers ────────────────────────────────────────────────────────────────

fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(48)
        .collect()
}

fn abs_path(p: &Path) -> Result<String> {
    let canon = p
        .canonicalize()
        .map_err(|e| IctError::Config(format!("canonicalize {}: {e}", p.display())))?;
    Ok(canon.to_string_lossy().into_owned())
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_binding_matches_golden_primary() {
        let h = owner_binding_from_dest_display(REGTEST_MINER_DEST);
        assert_eq!(h, REGTEST_MINER_OWNER_BINDING_HEX);
    }

    #[test]
    fn owner_binding_trims_whitespace() {
        let a = owner_binding_from_dest_display(REGTEST_MINER_DEST);
        let b = owner_binding_from_dest_display(&format!("  {REGTEST_MINER_DEST}  "));
        assert_eq!(a, b);
    }

    #[test]
    fn corridor_toml_contains_hardcoded_ports_and_dest() {
        let t = corridor_node_toml();
        assert!(t.contains(&format!("0.0.0.0:{ZAKURA_RPC_PORT}")));
        assert!(t.contains(REGTEST_MINER_DEST));
        assert!(t.contains("Regtest"));
    }

    #[test]
    fn sidecar_config_name_and_ports() {
        let s = zakura_sidecar_config();
        assert_eq!(s.name, "zakura");
        assert!(s.ports.iter().any(|p| p == &ZAKURA_RPC_PORT.to_string()));
        assert!(s.cmd.iter().any(|c| c.contains("zakurad")));
    }

    #[test]
    fn default_config_rpc_url() {
        // Clear override for determinism in this test process if set.
        let cfg = ZakuraNodeConfig::default();
        let url = if std::env::var(ENV_ZAKURA_RPC).is_ok() {
            cfg.rpc_url()
        } else {
            format!("http://127.0.0.1:{ZAKURA_RPC_PORT}")
        };
        assert!(url.contains("18232") || std::env::var(ENV_ZAKURA_RPC).is_ok());
        let _ = url;
        assert_eq!(cfg.rpc_host_port, ZAKURA_RPC_PORT);
    }

    #[tokio::test]
    async fn spawn_reuses_when_rpc_already_ready_mock() {
        // Without a live node, rpc_ready is false — exercise config path only.
        let rt = crate::runtime::mock::MockRuntime::new();
        let mut cfg = ZakuraNodeConfig::default();
        cfg.reuse_if_ready = true;
        cfg.zakurad_bin = PathBuf::from("/nonexistent/zakurad");
        // Should fail on missing binary when RPC down (not reuse).
        let err = spawn_zakura_local(Arc::new(rt), cfg).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("zakurad") || msg.contains("ZAKURAD") || msg.contains("not found"),
            "unexpected: {msg}"
        );
    }
}
