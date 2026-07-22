//! Convenience constructors for common sidecar types.
//!
//! Users can always build `SidecarConfig` directly; these helpers reduce
//! boilerplate for well-known sidecar patterns like hash-market.

use crate::chain::SidecarConfig;
use crate::runtime::DockerImage;

/// Create a `SidecarConfig` for a local Zakura regtest node (feature `zakura`).
///
/// Ports/cmd are hardcoded for Private Bridge corridor (RPC 18232). Actual
/// spawn should use [`crate::chain::zakura::spawn_zakura_local`] so the host
/// `zakurad` binary and config are bind-mounted.
#[cfg(feature = "zakura")]
pub fn zakura_regtest_sidecar_config() -> SidecarConfig {
    crate::chain::zakura::zakura_sidecar_config()
}

/// Create a `SidecarConfig` for the hash-market-server sidecar.
///
/// The server runs alongside a Terp validator, receives transformed hashes
/// from the client, and produces signed vote extensions via ABCI++.
///
/// Mount a config produced by [`hash_market_config_toml`] (or bounds variant)
/// at `/home/sidecar/config.toml` before start.
pub fn hash_market_server_config(
    signing_key: &str,
    chain_id: &str,
    bind_addr: &str,
) -> SidecarConfig {
    SidecarConfig {
        name: "hm-server".into(),
        image: DockerImage {
            repository: "hash-market-server".into(),
            version: "latest".into(),
            uid_gid: None,
        },
        home_dir: "/home/sidecar".into(),
        ports: vec!["9090".into(), "9091".into()],
        env: vec![
            ("BIND".into(), bind_addr.to_string()),
            ("CHAIN_ID".into(), chain_id.to_string()),
            ("SIGNING_KEY".into(), signing_key.to_string()),
        ],
        cmd: vec![
            "hash-market-server".into(),
            "-c".into(),
            "/home/sidecar/config.toml".into(),
        ],
        pre_start: false,
        validator_process: true,
        health_endpoint: Some("/health".into()),
        ready_timeout_secs: 30,
    }
}

/// Default hash-market-server TOML: **state_root VE only** (legacy / default path).
///
/// `signing_key` is hex secp256k1 material — still prefer SFTP in production;
/// local e2e may embed for determinism.
pub fn hash_market_config_toml(bind: &str, chain_id: &str, signing_key_hex: &str) -> String {
    format!(
        r#"bind = "{bind}"
chain_id = "{chain_id}"
signing_key = "{signing_key_hex}"
ve_enabled = true
data_dir = "data"

[[providers]]
name = "eth_mainnet"
chain_uid = "ethereum-mainnet"
algo = "keccak256"
mode = "http_poll"
address = "http://127.0.0.1:8545"
interval_secs = 12
"#
    )
}

/// Elevated sophistication: state_root **plus** multi-source **price_bound** providers.
///
/// Tacit rule: price providers are **bounds only** (never mint). In-process
/// aggregation is covered by `hash-market` `tests::suite` oracle scenarios;
/// this TOML is what Docker e2e mounts for the same matrix on a live sidecar.
///
/// See `hash-market/docs/oracle-connect-bounds.md`.
pub fn hash_market_config_toml_with_oracle_bounds(
    bind: &str,
    chain_id: &str,
    signing_key_hex: &str,
) -> String {
    format!(
        r#"bind = "{bind}"
chain_id = "{chain_id}"
signing_key = "{signing_key_hex}"
ve_enabled = true
data_dir = "data"

[oracle]
bounds_enabled = true
aggregation = "median"
min_sources = 2
max_age_secs = 120

# ── state_root path (interop fabric) ────────────────────────────────────────
[[providers]]
name = "eth_mainnet"
kind = "state_root"
chain_uid = "ethereum-mainnet"
algo = "keccak256"
mode = "http_poll"
address = "http://127.0.0.1:8545"
interval_secs = 12

# ── price_bound path (cUSD-like mid only; multi-source) ─────────────────────
[[providers]]
name = "mock_binance"
kind = "price_bound"
market_id = "ETH/USD"
chain_uid = "price-feeds"
algo = "price_median_v1"
mode = "http_poll"
address = "http://127.0.0.1:18080/ticker/binance"
interval_secs = 5
weight = 1.0

[[providers]]
name = "mock_coinbase"
kind = "price_bound"
market_id = "ETH/USD"
chain_uid = "price-feeds"
algo = "price_median_v1"
mode = "http_poll"
address = "http://127.0.0.1:18080/ticker/coinbase"
interval_secs = 5
weight = 1.0

[[providers]]
name = "mock_okx"
kind = "price_bound"
market_id = "ETH/USD"
chain_uid = "price-feeds"
algo = "price_median_v1"
mode = "http_poll"
address = "http://127.0.0.1:18080/ticker/okx"
interval_secs = 5
weight = 1.0
"#
    )
}

/// Capability labels for modular e2e matrices (shared language with hash-market suite).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashMarketE2eCapability {
    /// Merkle whitelist trees (headstash surface).
    Trees,
    /// Multi-source price bounds (Tacit oracle role).
    OracleBounds,
    /// ABCI++ vote-extension / HashRoot pipeline.
    VoteExtension,
    /// Nostr relay / kind:30070 (optional).
    Nostr,
}

impl HashMarketE2eCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trees => "trees",
            Self::OracleBounds => "oracle_bounds",
            Self::VoteExtension => "vote_extension",
            Self::Nostr => "nostr",
        }
    }

    /// Default modular matrix for ICT-RS hashmerchant e2e evolution.
    pub fn default_matrix() -> &'static [HashMarketE2eCapability] {
        &[
            HashMarketE2eCapability::Trees,
            HashMarketE2eCapability::OracleBounds,
            HashMarketE2eCapability::VoteExtension,
        ]
    }
}

/// Create a `SidecarConfig` for the hash-market-client sidecar.
///
/// The client polls an Ethereum node (e.g., Anvil) for `eth_getProof` data,
/// transforms Keccak hashes to Pallas-friendly form, and streams them to the server.
pub fn hash_market_client_config(
    eth_rpc: &str,
    sidecar_url: &str,
) -> SidecarConfig {
    SidecarConfig {
        name: "hm-client".into(),
        image: DockerImage {
            repository: "hash-market-client".into(),
            version: "latest".into(),
            uid_gid: None,
        },
        home_dir: "/home/sidecar".into(),
        ports: Vec::new(),
        env: vec![
            ("ETH_RPC".into(), eth_rpc.to_string()),
            ("SIDECAR_URL".into(), sidecar_url.to_string()),
        ],
        cmd: vec![
            "hash-market-client".into(),
            "-c".into(),
            "/home/sidecar/client.toml".into(),
        ],
        pre_start: false,
        validator_process: true,
        health_endpoint: None,
        ready_timeout_secs: 10,
    }
}

/// Create a `SidecarConfig` for the nostr-relay sidecar (local NIP-01 relay for testing).
///
/// Treats the local Nostr relayer as a first-class `SidecarProcess` (Docker),
/// exactly mirroring the existing hash-market helpers and SidecarProcess lifecycle
/// in ict-rs for unified experience (no special casing).
///
/// Uses `mattn/nostr-relay:latest` (validated: minimal, WS on 7777/tcp, no volumes/cmd/env needed for basic NIP-01).
#[cfg(feature = "nostr")]
pub fn nostr_relay_config() -> SidecarConfig {
    SidecarConfig {
        name: "nostr-relay".into(),
        image: DockerImage {
            repository: "mattn/nostr-relay".into(),
            version: "latest".into(),
            uid_gid: None,
        },
        home_dir: "/data".into(),
        ports: vec!["7777".into()],
        env: vec![],
        cmd: vec![],
        pre_start: false,
        validator_process: false,
        health_endpoint: None,
        ready_timeout_secs: 10,
    }
}

/// Create a `SidecarConfig` for the akash-provider sidecar (local provider for testing).
///
/// The provider runs alongside a local Akash chain, accepting SDL deployments,
/// and launching containers via the Docker socket. Requires the following
/// environment variables to be set on the provider container:
///
/// - `AKASH_KEY_NAME` — provider wallet key name
/// - `AKASH_KEYRING_BACKEND` — `test` (for local dev)
/// - `AKASH_FROM` — provider wallet address
/// - `AKASH_CHAIN_ID` — local chain ID
/// - `AKASH_NODE` — local chain RPC endpoint
///
/// Mounts `/var/run/docker.sock` so the provider can launch containers.
#[cfg(feature = "akash")]
pub fn akash_provider_config(
    chain_rpc: &str,
    chain_id: &str,
    provider_addr: &str,
) -> SidecarConfig {
    SidecarConfig {
        name: "akash-provider".into(),
        image: DockerImage {
            repository: "ghcr.io/akash-network/provider".into(),
            version: "latest".into(),
            uid_gid: None,
        },
        home_dir: "/home/provider".into(),
        ports: vec![
            "8443".into(),   // provider gRPC
            "8444".into(),   // provider HTTP status
        ],
        env: vec![
            ("AKASH_KEY_NAME".into(), "provider".into()),
            ("AKASH_KEYRING_BACKEND".into(), "test".into()),
            ("AKASH_FROM".into(), provider_addr.into()),
            ("AKASH_CHAIN_ID".into(), chain_id.into()),
            ("AKASH_NODE".into(), format!("tcp://{}", chain_rpc)),
            ("AKASH_GAS".into(), "auto".into()),
            ("AKASH_GAS_ADJUSTMENT".into(), "1.5".into()),
            ("AKASH_GAS_PRICES".into(), "0.025uakt".into()),
        ],
        cmd: vec![
            "provider-services".into(),
            "run".into(),
            "--chain-id".into(),
            chain_id.into(),
            "--from".into(),
            "provider".into(),
            "--home".into(),
            "/home/provider".into(),
            "--keyring-backend".into(),
            "test".into(),
            "--node".into(),
            format!("tcp://{}", chain_rpc),
        ],
        pre_start: false,
        validator_process: false,
        health_endpoint: Some("/health".into()),
        ready_timeout_secs: 60,
    }
}

/// Create a `SidecarConfig` for the oracle price feeder sidecar.
///
/// Runs alongside an Akash chain to continuously post oracle prices so the
/// market module can perform bid matching during deployment workflows.
#[cfg(feature = "akash")]
pub fn oracle_feeder_config(
    chain_rpc: &str,
    chain_id: &str,
) -> SidecarConfig {
    SidecarConfig {
        name: "akash-oracle-feeder".into(),
        image: DockerImage {
            repository: "ghcr.io/akash-network/node".into(),
            version: "latest".into(),
            uid_gid: None,
        },
        home_dir: "/home/feeder".into(),
        ports: Vec::new(),
        env: vec![
            ("CHAIN_ID".into(), chain_id.into()),
            ("CHAIN_RPC".into(), chain_rpc.into()),
        ],
        cmd: vec![
            "akash".into(),
            "tx".into(),
            "oracle".into(),
            "feed".into(),
            "akt".into(),
            "usd".into(),
            "1.000000000000000000".into(),
            "--from".into(),
            "validator".into(),
            "--chain-id".into(),
            chain_id.into(),
            "--node".into(),
            format!("tcp://{}", chain_rpc),
            "--keyring-backend".into(),
            "test".into(),
            "--gas".into(),
            "auto".into(),
            "--gas-prices".into(),
            "0.025uakt".into(),
            "-y".into(),
        ],
        pre_start: false,
        validator_process: false,
        health_endpoint: None,
        ready_timeout_secs: 10,
    }
}

/// Build a deployment SDL for a sentry node with snapshots bootstrapped via
/// minio-ipfs.  Returns the raw SDL YAML as a String suitable for use with
/// `akash tx deployment create`.
///
/// This is the canonical SDL used in the O-Line Phase A / Phase B bootstrap
/// workflow.  Contains a single sentry container that mounts snapshot data
/// from a minio-ipfs sidecar, using the `terpd` binary for chain node operation.
pub fn sentry_deployment_sdl(
    chain_id: &str,
    terp_image: &str,
    minio_endpoint: &str,
    moniker: &str,
) -> String {
    format!(
        r#"---
version: "2.0"
services:
  sentry:
    image: {terp_image}
    expose:
      - port: 26656
        as: 26656
        proto: tcp
        to:
          - global: true
      - port: 26657
        as: 26657
        proto: tcp
        to:
          - global: true
    env:
      - CHAIN_ID={chain_id}
      - MONIKER={moniker}
      - S3_ENDPOINT={minio_endpoint}
      - S3_BUCKET=snapshots
    command:
      - terpd
      - start
profiles:
  compute:
    sentry:
      resources:
        cpu:
          units: 0.5
        memory:
          size: 1Gi
        storage:
          size: 10Gi
  placement:
    dcloud:
      pricing:
        sentry:
          denom: uakt
          amount: 100
"#,
        chain_id = chain_id,
        terp_image = terp_image,
        minio_endpoint = minio_endpoint,
        moniker = moniker,
    )
}
/// Create a `SidecarConfig` for the minio-ipfs container.
///
/// Ports: 9000 (MinIO API), 9002 (Console), 8081 (IPFS Gateway),
/// 9100 (Webhook collector), 9443 (Push daemon).
pub fn minio_ipfs_config(
    root_user: &str,
    root_password: &str,
    webhook_token: &str,
    autopin_buckets: &str,
) -> SidecarConfig {
    SidecarConfig {
        name: "minio-ipfs".into(),
        image: DockerImage {
            repository: "minio-ipfs".into(),
            version: "latest".into(),
            uid_gid: None,
        },
        home_dir: "/data".into(),
        ports: vec![
            "9000".into(),
            "9002".into(),
            "8081".into(),
            "9100".into(),
            "9443".into(),
        ],
        env: vec![
            ("MINIO_ROOT_USER".into(), root_user.into()),
            ("MINIO_ROOT_PASSWORD".into(), root_password.into()),
            ("WEBHOOK_AUTH_TOKEN".into(), webhook_token.into()),
            ("AUTOPIN_BUCKETS".into(), autopin_buckets.into()),
        ],
        cmd: vec![],
        pre_start: false,
        validator_process: false,
        health_endpoint: Some("/minio/health/live".into()),
        ready_timeout_secs: 30,
    }
}
