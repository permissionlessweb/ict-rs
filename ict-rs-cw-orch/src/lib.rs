//! # ict-rs-cw-orch
//!
//! Bridge between **ict-rs** (local chain infrastructure) and **cw-orchestrator**
//! (CosmWasm scripting). Spawn Docker-backed chains via ict-rs, then interact
//! with them through cw-orch's `Daemon` abstraction.
//!
//! ## Architecture
//!
//! ```text
//! ┌───────────────┐      ┌──────────────────┐      ┌──────────────┐
//! │   ict-rs      │      │  ict-rs-cw-orch  │      │  cw-orch     │
//! │ (Docker infra)│─────▶│  (bridge/glue)   │─────▶│  (Daemon)    │
//! │               │      │                  │      │  (gRPC conn) │
//! └───────────────┘      └──────────────────┘      └──────────────┘
//! ```
//!
//! ict-rs handles container lifecycle, genesis, and validator setup.
//! This crate converts ict-rs chain state into cw-orch `ChainInfoOwned` and
//! constructs `Daemon` instances that connect via the host-mapped gRPC port.

mod convert;
mod error;

pub use convert::chain_info_from_cosmos;
pub use error::BridgeError;

use cw_orch_daemon::DaemonBuilder;
use ict_rs::chain::cosmos::CosmosChain;
use tracing::info;

/// Build a cw-orch `DaemonBuilder` from a running ict-rs `CosmosChain`.
///
/// The chain must already be initialized and started (i.e. containers are
/// running and the gRPC port is mapped to the host).
///
/// # Arguments
///
/// * `chain` — A running `CosmosChain` with host ports resolved.
/// * `mnemonic` — Optional mnemonic for the cw-orch wallet. If `None`, the
///   builder will generate a new key. To interact with pre-funded accounts,
///   pass the same mnemonic used in ict-rs genesis (the validator key mnemonic).
///
/// # Example
///
/// ```ignore
/// use ict_rs::prelude::*;
/// use ict_rs_cw_orch::daemon_builder_from_chain;
///
/// let daemon = daemon_builder_from_chain(&chain, Some("abandon ... about"))
///     .unwrap()
///     .build()
///     .unwrap();
///
/// // Now use `daemon` as a normal cw-orch Daemon.
/// ```
pub fn daemon_builder_from_chain(
    chain: &CosmosChain,
    mnemonic: Option<&str>,
) -> Result<DaemonBuilder, BridgeError> {
    let chain_info = chain_info_from_cosmos(chain)?;

    info!(
        chain_id = %chain_info.chain_id,
        grpc = ?chain_info.grpc_urls,
        "Building cw-orch DaemonBuilder from ict-rs chain"
    );

    let mut builder = DaemonBuilder::new(chain_info);
    builder.is_test(true);

    if let Some(m) = mnemonic {
        builder.mnemonic(m);
    }

    Ok(builder)
}
