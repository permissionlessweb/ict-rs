//! Derive macros for ict-rs.
//!
//! Provides `ExecuteFns` and `QueryFns` derive macros that auto-generate
//! typed async extension traits on `Chain` from enum definitions.
//!
//! # Example
//!
//! ```ignore
//! use ict_rs::prelude::*;
//!
//! #[derive(ExecuteFns)]
//! #[ict(module = "tokenfactory")]
//! pub enum TokenfactoryMsg {
//!     CreateDenom { sender: String, subdenom: String },
//!     MintTo { sender: String, amount: String, mint_to_address: String },
//! }
//!
//! // Generates `TokenfactoryMsgExt` trait with:
//! //   async fn tokenfactory_create_denom(&self, key_name: &str, subdenom: &str) -> Result<Tx>
//! //   async fn tokenfactory_mint_to(&self, key_name: &str, amount: &str, mint_to_address: &str) -> Result<Tx>
//! ```

mod attrs;
mod execute;
mod query;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Derive typed execute functions from a message enum.
///
/// Generates an async extension trait on `ict_rs::chain::Chain` with one method
/// per enum variant. The sender field (detected by `#[ict(sender)]` attribute
/// or common names like `sender`, `authority`, `creator`) becomes the
/// `key_name: &str` parameter. All other fields become `&str` parameters.
///
/// # Attributes
///
/// - `#[ict(module = "name")]` (required on enum) — CLI module name
/// - `#[ict(sender)]` (on field) — marks the sender/signer field
/// - `#[ict(skip)]` (on variant) — skip this variant
#[proc_macro_derive(ExecuteFns, attributes(ict))]
pub fn derive_execute_fns(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    execute::expand(&input).into()
}

/// Derive typed query functions from a query message enum.
///
/// Generates an async extension trait on `ict_rs::chain::Chain` with one method
/// per enum variant. All fields become `&str` parameters. Returns
/// `serde_json::Value`.
///
/// # Attributes
///
/// - `#[ict(module = "name")]` (required on enum) — CLI module name
/// - `#[returns(Type)]` (on variant) — return type (reserved for future use)
/// - `#[ict(skip)]` (on variant) — skip this variant
#[proc_macro_derive(QueryFns, attributes(ict, returns))]
pub fn derive_query_fns(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    query::expand(&input).into()
}
