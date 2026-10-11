//! Local DREGG federation node spawn for ict-rs (SVG mint cell runtime).
//!
//! Feature: `dregg-node` (implies `docker`).
//!
//! Bind-mounts a host Linux `dregg-node` (`DREGG_NODE_BIN`) into debian:trixie-slim
//! and publishes the query HTTP port (**8421**). Same corridor as Zakura: never
//! bind-mount a macOS Mach-O into Debian.
//!
//! Honest: this is the **federation runtime sidecar**, not a Lean crown.

#![cfg(feature = "dregg-node")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use crate::error::{IctError, Result};
use crate::runtime::{
    ContainerId, ContainerOptions, DockerImage, NetworkId, PortBinding, RuntimeBackend, VolumeMount,
};

pub const DREGG_QUERY_PORT: u16 = 8421;
pub const ENV_DREGG_NODE_BIN: &str = "DREGG_NODE_BIN";
pub const ENV_DREGG_NODE_URL: &str = "DREGG_NODE_URL";
pub const CONTAINER_DREGG_PATH: &str = "/usr/local/bin/dregg-node";
pub const DREGG_BASE_IMAGE_REPO: &str = "debian";
pub const DREGG_BASE_IMAGE_TAG: &str = "trixie-slim";

#[derive(Debug, Clone)]
pub struct DreggNodeConfig {
    pub dregg_bin: PathBuf,
    pub query_host_port: u16,
    pub query_container_port: u16,
    pub network_id: Option<NetworkId>,
    pub test_name: String,
    pub ready_timeout_secs: u64,
    pub reuse_if_ready: bool,
    pub image: DockerImage,
    /// Extra args after the binary (default: empty — binary's own listen).
    pub extra_args: Vec<String>,
    /// Host paths mounted beside the desk binary (wallet dir, wallet ELF, admin file).
    pub extra_volumes: Vec<VolumeMount>,
}

impl Default for DreggNodeConfig {
    fn default() -> Self {
        Self {
            dregg_bin: resolve_dregg_bin().unwrap_or_else(|_| PathBuf::from("/nonexistent/dregg-node")),
            query_host_port: DREGG_QUERY_PORT,
            query_container_port: DREGG_QUERY_PORT,
            network_id: None,
            test_name: "dregg-local".into(),
            ready_timeout_secs: 60,
            reuse_if_ready: true,
            image: DockerImage {
                repository: DREGG_BASE_IMAGE_REPO.into(),
                version: DREGG_BASE_IMAGE_TAG.into(),
                uid_gid: None,
            },
            extra_args: vec![],
            extra_volumes: vec![],
        }
    }
}

impl DreggNodeConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            dregg_bin: resolve_dregg_bin()?,
            ..Self::default()
        })
    }

    pub fn query_url(&self) -> String {
        std::env::var(ENV_DREGG_NODE_URL).unwrap_or_else(|_| {
            format!("http://127.0.0.1:{}", self.query_host_port)
        })
    }
}

pub struct DreggNode {
    pub config: DreggNodeConfig,
    pub container_id: Option<ContainerId>,
    pub query_url: String,
    pub reused_existing: bool,
    runtime: Arc<dyn RuntimeBackend>,
}

impl DreggNode {
    pub fn query_url(&self) -> &str {
        &self.query_url
    }

    pub async fn teardown(&mut self) -> Result<()> {
        if self.reused_existing {
            info!("DREGG node was reused; not tearing down host process");
            return Ok(());
        }
        if let Some(id) = self.container_id.take() {
            let _ = self.runtime.stop_container(&id).await;
            let _ = self.runtime.remove_container(&id).await;
        }
        Ok(())
    }
}

pub async fn spawn_dregg_local(
    runtime: Arc<dyn RuntimeBackend>,
    config: DreggNodeConfig,
) -> Result<DreggNode> {
    let query_url = config.query_url();
    if config.reuse_if_ready && query_ready(&query_url).await {
        info!(%query_url, "DREGG query already ready — reusing");
        return Ok(DreggNode {
            config,
            container_id: None,
            query_url,
            reused_existing: true,
            runtime,
        });
    }

    if !config.dregg_bin.is_file() {
        return Err(IctError::Config(format!(
            "dregg-node binary not found at {:?} (set {ENV_DREGG_NODE_BIN})",
            config.dregg_bin
        )));
    }
    if !is_linux_elf(&config.dregg_bin) {
        return Err(IctError::Config(format!(
            "dregg-node at {:?} is not a Linux ELF (set {ENV_DREGG_NODE_BIN} on groot-wan)",
            config.dregg_bin
        )));
    }

    let container_name = format!(
        "ict-{}-dregg-{}",
        sanitize_name(&config.test_name),
        config.query_host_port
    );
    {
        let stale = ContainerId(container_name.clone());
        let _ = runtime.stop_container(&stale).await;
        let _ = runtime.remove_container(&stale).await;
    }
    runtime.pull_image(&config.image).await?;

    let mut cmd = vec![CONTAINER_DREGG_PATH.to_string()];
    cmd.extend(config.extra_args.clone());

    let opts = ContainerOptions {
        image: config.image.clone(),
        name: container_name.clone(),
        network_id: config.network_id.clone(),
        env: {
            // DREGG_MINTER_FUNDED is not a funded proof — do not copy it into the sidecar.
            let mut e: Vec<(String, String)> = Vec::new();
            for k in [
                "DREGG_MINTER_LCD",
                "DREGG_MINTER_ADDR",
                "DREGG_ADMIN_TOKEN",
                "DREGG_ADMIN_ZID_PK_PATH",
                "DREGG_ADMIN_TERP1_PATH",
                "DREGG_WITHDRAW_ZEC_UA",
                "DREGG_EVM_REFUND_FROM",
                "DREGG_BTC_REFUND_TO",
                "ETH_RPC",
                "BASE_RPC",
                "BTC_RPC",
                "GAS_DENOM",
                "ZCASH_PUBLIC_RPC",
                "ZCASH_PRIVATE_RPC",
                "ZCASH_TIP_MAX_DRIFT",
                "ZCASH_TIP_CACHE_SECS",
                "ZCASH_DEVTOOL_BIN",
                "ZCASH_DEVTOOL_WALLET",
                "ZCASH_DEVTOOL_SERVER",
                "ZCASH_DEVTOOL_NETWORK",
                "ZCASH_DEVTOOL_ACTIVATION_HEIGHTS",
                "SVG_MINT_ZRT_BIN",
                "SVG_MINT_ZRT_DIR",
                "SVG_MINT_ZRT_LAB",
                "SVG_MINT_SKIP_ZRT_WATCH",
                "DREGG_ADMIN_TERP1_PATH",
            ] {
                if let Ok(v) = std::env::var(k) {
                    if !v.is_empty() {
                        e.push((k.to_string(), v));
                    }
                }
            }
            e
        },
        cmd,
        entrypoint: None,
        ports: vec![PortBinding {
            host_port: config.query_host_port,
            container_port: config.query_container_port,
            protocol: "tcp".into(),
        }],
        volumes: {
            let mut volumes = vec![VolumeMount {
                source: abs_path(&config.dregg_bin)?,
                target: CONTAINER_DREGG_PATH.into(),
                read_only: true,
            }];
            volumes.extend(config.extra_volumes.clone());
            volumes
        },
        labels: vec![
            ("ict.test".into(), config.test_name.clone()),
            ("ict.sidecar".into(), "dregg".into()),
        ],
        hostname: Some(format!("dregg-{}", sanitize_name(&config.test_name))),
    };

    info!(
        container = %container_name,
        bin = %config.dregg_bin.display(),
        query = %query_url,
        "Spawning local DREGG node (feature=dregg-node)"
    );
    let id = runtime.create_container(&opts).await?;
    runtime.start_container(&id).await?;
    wait_query_ready(&query_url, config.ready_timeout_secs).await?;
    Ok(DreggNode {
        config,
        container_id: Some(id),
        query_url,
        reused_existing: false,
        runtime,
    })
}

pub async fn wait_query_ready(url: &str, timeout_secs: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs.max(1));
    loop {
        if query_ready(url).await {
            info!(%url, "DREGG query ready");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(IctError::Timeout {
                what: format!("dregg query at {url}"),
                duration: Duration::from_secs(timeout_secs),
            });
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub async fn query_ready(url: &str) -> bool {
    let probe = url.trim_end_matches('/').to_string();
    tokio::task::spawn_blocking(move || http_get_ok(&probe))
        .await
        .unwrap_or(false)
}

/// std HTTP/1.0 GET (no host curl), same shape as svg-mint `httpx`.
fn http_get_ok(url: &str) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (hp, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{path}")
    };
    let (host, port): (String, u16) = if let Some((h, p)) = hp.rsplit_once(':') {
        (h.to_string(), p.parse().unwrap_or(80))
    } else {
        (hp.to_string(), 80)
    };
    let Ok(mut stream) = TcpStream::connect((host.as_str(), port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
        if buf.len() > 64_000 {
            break;
        }
    }
    let s = String::from_utf8_lossy(&buf);
    s.starts_with("HTTP/1.") && s.contains(" 200")
}

pub fn resolve_dregg_bin() -> Result<PathBuf> {
    let p = std::env::var(ENV_DREGG_NODE_BIN)
        .map_err(|_| IctError::Config(format!("{ENV_DREGG_NODE_BIN} unset")))?;
    Ok(PathBuf::from(p))
}

fn is_linux_elf(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    bytes.len() >= 5 && bytes[0..4] == [0x7f, b'E', b'L', b'F']
}

fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn abs_path(p: &Path) -> Result<String> {
    let c = p
        .canonicalize()
        .map_err(|e| IctError::Config(format!("canonicalize {}: {e}", p.display())))?;
    Ok(c.to_string_lossy().into_owned())
}
