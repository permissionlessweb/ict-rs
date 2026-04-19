//! Naming conventions for proto → Rust conversion.

use heck::{ToKebabCase, ToSnakeCase, ToUpperCamelCase};

/// Extract the module name from a protobuf package.
///
/// `osmosis.tokenfactory.v1beta1` → `tokenfactory`
/// `cosmos.bank.v1beta1` → `bank`
///
/// Skips the first segment (project name) and the last segment if it looks
/// like a version (starts with `v` followed by a digit).
pub fn module_from_package(package: &str) -> String {
    let parts: Vec<&str> = package.split('.').collect();

    // Find the "meaningful" part: skip first (project), skip version-like last
    let meaningful: Vec<&&str> = parts
        .iter()
        .skip(1) // skip project name
        .filter(|p| !is_version_segment(p))
        .collect();

    meaningful
        .first()
        .map(|s| s.to_string())
        .unwrap_or_else(|| parts.last().unwrap_or(&"unknown").to_string())
}

/// Check if a package segment looks like a version (`v1`, `v1beta1`, etc.).
fn is_version_segment(s: &str) -> bool {
    s.starts_with('v') && s.chars().nth(1).map_or(false, |c| c.is_ascii_digit())
}

/// Convert an RPC method name to a Rust method name, prefixed with the module.
///
/// `CreateDenom` with module `tokenfactory` → `tokenfactory_create_denom`
pub fn method_name(module: &str, rpc_name: &str) -> String {
    format!("{}_{}", module, rpc_name.to_snake_case())
}

/// Convert an RPC method name to a CLI action (kebab-case).
///
/// `CreateDenom` → `create-denom`
pub fn cli_action(rpc_name: &str) -> String {
    rpc_name.to_kebab_case()
}

/// Convert a module name to a trait name.
///
/// `tokenfactory` → `TokenfactoryMsgExt`
pub fn msg_trait_name(module: &str) -> String {
    format!("{}MsgExt", module.to_upper_camel_case())
}

/// Convert a module name to a query trait name.
///
/// `tokenfactory` → `TokenfactoryQueryExt`
pub fn query_trait_name(module: &str) -> String {
    format!("{}QueryExt", module.to_upper_camel_case())
}

/// Convert a proto field name to a CLI flag.
///
/// `subdenom` → `--subdenom`
/// `mint_to_address` → `--mint-to-address`
pub fn field_to_cli_flag(field_name: &str) -> String {
    format!("--{}", field_name.to_kebab_case())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_from_package() {
        assert_eq!(module_from_package("osmosis.tokenfactory.v1beta1"), "tokenfactory");
        assert_eq!(module_from_package("cosmos.bank.v1beta1"), "bank");
        assert_eq!(module_from_package("cosmos.staking.v1beta1"), "staking");
        assert_eq!(module_from_package("cosmwasm.wasm.v1"), "wasm");
    }

    #[test]
    fn test_method_name() {
        assert_eq!(method_name("tokenfactory", "CreateDenom"), "tokenfactory_create_denom");
        assert_eq!(method_name("bank", "Send"), "bank_send");
        assert_eq!(method_name("staking", "Delegate"), "staking_delegate");
    }

    #[test]
    fn test_cli_action() {
        assert_eq!(cli_action("CreateDenom"), "create-denom");
        assert_eq!(cli_action("Send"), "send");
        assert_eq!(cli_action("MintTo"), "mint-to");
    }

    #[test]
    fn test_msg_trait_name() {
        assert_eq!(msg_trait_name("tokenfactory"), "TokenfactoryMsgExt");
        assert_eq!(msg_trait_name("bank"), "BankMsgExt");
    }

    #[test]
    fn test_query_trait_name() {
        assert_eq!(query_trait_name("tokenfactory"), "TokenfactoryQueryExt");
        assert_eq!(query_trait_name("bank"), "BankQueryExt");
    }

    #[test]
    fn test_field_to_cli_flag() {
        assert_eq!(field_to_cli_flag("subdenom"), "--subdenom");
        assert_eq!(field_to_cli_flag("mint_to_address"), "--mint-to-address");
        assert_eq!(field_to_cli_flag("validator_address"), "--validator-address");
    }
}
