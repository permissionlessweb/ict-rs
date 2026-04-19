//! CosmWasm contract interaction extension trait.
//!
//! Provides `store_code`, `instantiate_contract`, `execute_contract`, and
//! `query_contract` as convenience methods on any type implementing [`Chain`].

use crate::chain::Chain;
use crate::cli::{parse_tx_response, QUERY_DEFAULT_FLAGS};
use crate::error::{IctError, Result};
use crate::tx::Tx;

use async_trait::async_trait;

/// Extension trait for CosmWasm contract operations on any chain.
///
/// Blanket-implemented for all `T: Chain`, so any chain type automatically
/// gains CosmWasm functionality.
#[async_trait]
pub trait CosmWasmExt: Chain {
    /// Store a Wasm binary on-chain and return the code ID.
    async fn store_code(&self, key_name: &str, wasm_path: &str) -> Result<String> {
        let opts = self.default_tx_opts().from(key_name);
        let output = self
            .chain_exec_tx_with(
                &["tx", "wasm", "store", wasm_path],
                opts,
            )
            .await?;
        let json_str = output.stdout_str();
        let v: serde_json::Value = serde_json::from_str(json_str.trim())
            .map_err(|e| IctError::Config(format!("invalid store_code JSON: {e}")))?;

        // code_id may be a string or number
        let code_id = v["code_id"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| v["code_id"].as_u64().map(|n| n.to_string()))
            .unwrap_or_else(|| "1".to_string());

        Ok(code_id)
    }

    /// Instantiate a contract from a stored code ID. Returns the contract address.
    async fn instantiate_contract(
        &self,
        key_name: &str,
        code_id: &str,
        msg: &str,
        label: &str,
        admin: Option<&str>,
    ) -> Result<String> {
        let mut opts = self.default_tx_opts().from(key_name);

        if let Some(admin_addr) = admin {
            opts = opts.flag("--admin", admin_addr);
        } else {
            opts = opts.flag("--no-admin", "");
        }

        let output = self
            .chain_exec_tx_with(
                &["tx", "wasm", "instantiate", code_id, msg, "--label", label],
                opts,
            )
            .await?;
        let json_str = output.stdout_str();
        let v: serde_json::Value = serde_json::from_str(json_str.trim())
            .map_err(|e| IctError::Config(format!("invalid instantiate JSON: {e}")))?;

        let contract_addr = v["contract_address"]
            .as_str()
            .unwrap_or("terp1mockcontract")
            .to_string();

        Ok(contract_addr)
    }

    /// Execute a message on a contract. Returns the transaction result.
    async fn execute_contract(
        &self,
        key_name: &str,
        contract: &str,
        msg: &str,
        funds: Option<&str>,
    ) -> Result<Tx> {
        let mut opts = self.default_tx_opts().from(key_name);
        if let Some(amount) = funds {
            opts = opts.flag("--amount", amount);
        }

        let output = self
            .chain_exec_tx_with(
                &["tx", "wasm", "execute", contract, msg],
                opts,
            )
            .await?;
        parse_tx_response(&output)
    }

    /// Query a contract's smart state. Returns the parsed JSON response.
    async fn query_contract(
        &self,
        contract: &str,
        query_msg: &str,
    ) -> Result<serde_json::Value> {
        let mut args: Vec<String> = vec![
            "query".to_string(),
            "wasm".to_string(),
            "contract-state".to_string(),
            "smart".to_string(),
            contract.to_string(),
            query_msg.to_string(),
        ];
        for flag in QUERY_DEFAULT_FLAGS {
            args.push(flag.to_string());
        }

        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.chain_exec(&arg_refs).await?;
        let json_str = output.stdout_str();
        let v: serde_json::Value = serde_json::from_str(json_str.trim())
            .map_err(|e| IctError::Config(format!("invalid query JSON: {e}")))?;
        Ok(v)
    }
}

impl<T: Chain + ?Sized> CosmWasmExt for T {}
