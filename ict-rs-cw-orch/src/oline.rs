//! O-Line suite for ict-rs-cw-orch.
//!
//! Feature-gated by `oline`. Wraps O-Line's test infrastructure
//! (`OLineTestEnv`, `OLineBinary`, `IctAkashNetwork`) for use in
//! cw-orchestrator test suites.
//!
//! # Usage
//!
//! ```ignore
//! use ict_rs_cw_orch::oline;
//!
//! // Spawn a local chain + Akash test provider, return a cw-orch Daemon
//! let daemon = oline::spawn_and_daemon("my-test-1", "chain-123").await?;
//!
//! // Or manage the lifecycle yourself:
//! let env = oline::OLineTestEnv::spawn("my-chain").await?;
//! let daemon = oline::daemon_from_oline(&env)?;
//! let binary = oline::OLineBinary::build()?;
//! binary.deploy(hashmap!{ "KEY" => "val" })?;
//! ```

// ── Re-exports from o-line-sdl ────────────────────────────────────
pub use o_line_sdl::interface::{OLineBinary, OLineTestEnv};
pub use o_line_sdl::testing::ict_network::IctAkashNetwork;

use crate::error::BridgeError;

/// Build a cw-orch `DaemonBuilder` from an `OLineTestEnv`.
///
/// Matches the pattern of `daemon_builder_from_chain` — converts
/// the O-Line test environment's chain info into a cw-orch builder.
pub fn daemon_builder_from_oline(
    env: &OLineTestEnv,
) -> Result<cw_orch_daemon::DaemonBuilder, BridgeError> {
    use cw_orch_core::environment::{ChainInfoOwned, ChainKind, NetworkInfoOwned};

    let grpc = if env.grpc_url.is_empty() {
        return Err(BridgeError::NoGrpcAddress);
    } else {
        env.grpc_url.clone()
    };

    let chain_info = ChainInfoOwned {
        chain_id: env.chain_id.clone(),
        gas_denom: "uterp".into(),
        gas_price: 0.025,
        grpc_urls: vec![grpc],
        lcd_url: None,
        fcd_url: None,
        network_info: NetworkInfoOwned {
            chain_name: "terp".into(),
            pub_address_prefix: "terp".into(),
            coin_type: 118,
        },
        kind: ChainKind::Local,
    };

    let mut builder = cw_orch_daemon::DaemonBuilder::new(chain_info);
    builder.is_test(true);
    Ok(builder)
}

/// Convenience: spawn an `OLineTestEnv` and return a ready-to-use `DaemonBuilder`.
///
/// The `OLineTestEnv` is kept alive for the `DaemonBuilder`'s lifetime.
/// For full lifecycle control (e.g., custom mnemonic, pre-fund addresses),
/// use [`OLineTestEnv::spawn`] + [`daemon_builder_from_oline`] directly.
pub async fn spawn_and_builder(
    chain_id: &str,
) -> Result<(OLineTestEnv, cw_orch_daemon::DaemonBuilder), Box<dyn std::error::Error>> {
    let env = OLineTestEnv::spawn(chain_id).await?;
    let builder = daemon_builder_from_oline(&env)?;
    Ok((env, builder))
}