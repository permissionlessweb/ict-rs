//! Convenience constructors for common sidecar types.
//!
//! Users can always build `SidecarConfig` directly; these helpers reduce
//! boilerplate for well-known sidecar patterns like hash-market.

use crate::chain::SidecarConfig;
use crate::runtime::DockerImage;

/// Create a `SidecarConfig` for the hash-market-server sidecar.
///
/// The server runs alongside a Terp validator, receives transformed hashes
/// from the client, and produces signed vote extensions via ABCI++.
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
