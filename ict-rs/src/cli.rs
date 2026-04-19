//! CLI helpers for generated code and internal use.
//!
//! Provides constants and parsing utilities shared between the derive macros'
//! generated code and `CosmosChain`'s own transaction handling.

pub use async_trait::async_trait;

use crate::error::{IctError, Result};
use crate::tx::{ExecOutput, Tx};

/// Default flags appended to every `tx` CLI command.
pub const TX_DEFAULT_FLAGS: &[&str] = &[
    "--keyring-backend",
    "test",
    "--gas",
    "auto",
    "--gas-adjustment",
    "1.5",
    "--broadcast-mode",
    "sync",
    "--output",
    "json",
    "-y",
];

/// Default flags appended to every `query` CLI command.
pub const QUERY_DEFAULT_FLAGS: &[&str] = &["--output", "json"];

/// Parse a transaction response from raw JSON.
///
/// Extracts `height`, `txhash`, and `gas_used` from the JSON object returned
/// by a Cosmos SDK CLI `tx` command.
pub fn parse_tx_response(output: &ExecOutput) -> Result<Tx> {
    let json_str = output.stdout_str();
    let json_str = json_str.trim();

    let v: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| IctError::Config(format!("invalid tx JSON: {e}")))?;

    Ok(Tx {
        height: v["height"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v["height"].as_u64())
            .unwrap_or(0),
        tx_hash: v["txhash"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        gas_spent: v["gas_used"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v["gas_used"].as_u64())
            .unwrap_or(0),
        packet: None,
    })
}

/// Parse a JSON query response from CLI output.
pub fn parse_query_response(output: &ExecOutput) -> Result<serde_json::Value> {
    let json_str = output.stdout_str();
    let json_str = json_str.trim();

    serde_json::from_str(json_str)
        .map_err(|e| IctError::Config(format!("invalid query JSON: {e}")))
}
