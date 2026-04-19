//! Conversion from ict-rs chain types to cw-orch chain info types.

use cw_orch_core::environment::{ChainInfoOwned, ChainKind, NetworkInfoOwned};
use ict_rs::chain::Chain;
use ict_rs::chain::cosmos::CosmosChain;

use crate::error::BridgeError;

/// Convert a running `CosmosChain` into a cw-orch `ChainInfoOwned`.
///
/// Extracts chain_id, gas config, bech32 prefix, coin type, and the
/// host-mapped gRPC URL from the ict-rs chain state.
pub fn chain_info_from_cosmos(chain: &CosmosChain) -> Result<ChainInfoOwned, BridgeError> {
    let cfg = chain.config();
    let grpc_url = chain.host_grpc_address();

    // Verify we have a real host port, not just the fallback.
    if grpc_url == "http://localhost:9090" {
        // Could be the real port on a non-auto-assigned setup, but warn.
        tracing::warn!(
            "gRPC address is the default fallback — host ports may not be resolved. \
             Ensure the chain is started before calling this function."
        );
    }

    // Parse gas price from the gas_prices string (e.g. "0.025uterp" → 0.025).
    let gas_price = cfg
        .gas_prices
        .trim_end_matches(|c: char| c.is_alphabetic())
        .parse::<f64>()
        .unwrap_or(0.025);

    Ok(ChainInfoOwned {
        chain_id: cfg.chain_id.clone(),
        gas_denom: cfg.denom.clone(),
        gas_price,
        grpc_urls: vec![grpc_url],
        lcd_url: None,
        fcd_url: None,
        network_info: NetworkInfoOwned {
            chain_name: cfg.name.clone(),
            pub_address_prefix: cfg.bech32_prefix.clone(),
            coin_type: cfg.coin_type,
        },
        kind: ChainKind::Local,
    })
}
