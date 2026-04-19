/// Genesis modification pipeline.
///
/// Provides helpers for modifying chain genesis JSON, analogous to Go ICT's
/// module_*.go helpers (auth, bank, staking, gov, cosmwasm, etc.).
///
/// Full implementation in Phase 2.
use serde_json::Value;

use crate::error::Result;

/// Apply a JSON patch to a genesis document at the given module path.
///
/// # Example
/// ```ignore
/// set_genesis_module_value(&mut genesis, &["app_state", "staking", "params", "bond_denom"], json!("uterp"))?;
/// ```
pub fn set_genesis_module_value(
    genesis: &mut Value,
    path: &[&str],
    value: Value,
) -> Result<()> {
    let mut current = genesis;
    for &key in &path[..path.len() - 1] {
        current = current
            .get_mut(key)
            .ok_or_else(|| crate::error::IctError::Config(format!("missing genesis key: {key}")))?;
    }
    let last_key = path.last().ok_or_else(|| {
        crate::error::IctError::Config("empty path".to_string())
    })?;
    current[*last_key] = value;
    Ok(())
}

/// Read a value from the genesis document at the given path.
pub fn get_genesis_module_value<'a>(genesis: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = genesis;
    for &key in path {
        current = current.get(key)?;
    }
    Some(current)
}
