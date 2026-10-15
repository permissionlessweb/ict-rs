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
        let trimmed = json_str.trim();
        if trimmed.is_empty() {
            return Err(IctError::Config(format!(
                "empty store_code stdout exit={} stderr={}",
                output.exit_code,
                output.stderr_str().trim()
            )));
        }
        let v: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| IctError::Config(format!("invalid store_code JSON: {e}")))?;
        if let Some(code) = v.get("code").and_then(|c| c.as_u64()) {
            if code != 0 {
                return Err(IctError::Config(format!("store_code tx code={code}: {trimmed}")));
            }
        }
        if let Some(id) = code_id_field(&v) {
            return Ok(id);
        }
        let Some(hash) = v.get("txhash").and_then(|h| h.as_str()).filter(|h| !h.is_empty()) else {
            return Err(IctError::Config(format!(
                "store_code response had no code_id or txhash: {trimmed}"
            )));
        };
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let q = self
                .chain_exec(&["query", "tx", hash, "--output", "json"])
                .await?;
            if q.exit_code != 0 {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(q.stdout_str().trim()) else {
                continue;
            };
            let tx = tx_body(&parsed);
            if let Some(id) = code_id_from_events(&tx) {
                return Ok(id);
            }
        }
        Err(IctError::Config(format!(
            "timed out waiting for store_code tx {hash}"
        )))
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
        let trimmed = json_str.trim();
        if trimmed.is_empty() {
            return Err(IctError::Config(format!(
                "empty instantiate stdout exit={} stderr={}",
                output.exit_code,
                output.stderr_str().trim()
            )));
        }
        let v: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| IctError::Config(format!("invalid instantiate JSON: {e} body={trimmed}")))?;

        // Try top-level contract_address first (mock runtime)
        if let Some(addr) = v["contract_address"].as_str() {
            if addr != "terp1mockcontract" {
                return Ok(addr.to_string());
            }
        }

        // For real chains: get txhash, wait for inclusion, query tx, parse from events
        let txhash = v["txhash"]
            .as_str()
            .ok_or_else(|| IctError::Config("no txhash in instantiate response".into()))?;

        let txhash = txhash.to_string();
        let mut tx_json = serde_json::Value::Null;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let tx_output = self
                .chain_exec(&["query", "tx", &txhash, "--output", "json"])
                .await?;
            if tx_output.exit_code != 0 {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(tx_output.stdout_str().trim())
            else {
                continue;
            };
            let body = tx_body(&parsed);
            if !tx_included(&body) {
                continue;
            }
            tx_json = body;
            break;
        }
        if tx_json.is_null() {
            return Err(IctError::Config(format!(
                "timed out waiting for instantiate tx {txhash}"
            )));
        }

        // Parse _contract_address from events
        if let Some(events) = tx_json["events"].as_array() {
            for event in events {
                if event["type"].as_str() == Some("instantiate") {
                    if let Some(attrs) = event["attributes"].as_array() {
                        for attr in attrs {
                            if attr["key"].as_str() == Some("_contract_address") {
                                if let Some(addr) = attr["value"].as_str() {
                                    return Ok(addr.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        // Fallback: check logs[].events[] (older SDK format)
        if let Some(logs) = tx_json["logs"].as_array() {
            for log in logs {
                if let Some(events) = log["events"].as_array() {
                    for event in events {
                        if event["type"].as_str() == Some("instantiate") {
                            if let Some(attrs) = event["attributes"].as_array() {
                                for attr in attrs {
                                    if attr["key"].as_str() == Some("_contract_address") {
                                        if let Some(addr) = attr["value"].as_str() {
                                            return Ok(addr.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Err(IctError::Config(format!(
            "contract address not found in tx {txhash} events"
        )))
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
        let mut output = self.chain_exec(&arg_refs).await?;
        for _ in 0..8 {
            if output.exit_code == 0 && !output.stdout_str().trim().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            output = self.chain_exec(&arg_refs).await?;
        }
        let json_str = output.stdout_str();
        let trimmed = json_str.trim();
        if trimmed.is_empty() {
            return Err(IctError::Config(format!(
                "empty contract query exit={} stderr={}",
                output.exit_code,
                output.stderr_str().trim()
            )));
        }
        let v: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| IctError::Config(format!("invalid query JSON: {e} body={trimmed}")))?;
        Ok(v)
    }

    /// Pay yearly circuit-upload coverage (`terpd tx wasm pay-circuit-deposit`).
    async fn pay_circuit_deposit(&self, key_name: &str, years: u32) -> Result<Tx> {
        let opts = self.default_tx_opts().from(key_name);
        let years_s = years.to_string();
        let output = self
            .chain_exec_tx_with(
                &["tx", "wasm", "pay-circuit-deposit", &years_s],
                opts,
            )
            .await?;
        if output.exit_code != 0 || output.stdout_str().trim().is_empty() {
            return Err(IctError::Config(format!(
                "pay-circuit-deposit failed (exit {}): stdout={} stderr={}",
                output.exit_code,
                output.stdout_str().trim(),
                output.stderr_str().trim()
            )));
        }
        parse_tx_response(&output)
    }

    /// Query circuit-upload coverage (`terpd query wasm circuit-deposit`).
    async fn query_circuit_deposit(&self, address: &str) -> Result<serde_json::Value> {
        let mut args: Vec<String> = vec![
            "query".to_string(),
            "wasm".to_string(),
            "circuit-deposit".to_string(),
            address.to_string(),
        ];
        for flag in QUERY_DEFAULT_FLAGS {
            args.push(flag.to_string());
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.chain_exec(&arg_refs).await?;
        serde_json::from_str(output.stdout_str().trim())
            .map_err(|e| IctError::Config(format!("invalid circuit-deposit JSON: {e}")))
    }
}

impl<T: Chain + ?Sized> CosmWasmExt for T {}

/// Extract an attribute value from tx events (logs.events or top-level events).
/// Used by ibc_wasm proposal flows and examples.
pub fn extract_event_attr(
    tx_json: &serde_json::Value,
    event_type: &str,
    attr_key: &str,
) -> Option<String> {
    if let Some(logs) = tx_json["logs"].as_array() {
        for log in logs {
            if let Some(events) = log["events"].as_array() {
                for event in events {
                    if event["type"].as_str() == Some(event_type) {
                        if let Some(attrs) = event["attributes"].as_array() {
                            for attr in attrs {
                                if attr["key"].as_str() == Some(attr_key) {
                                    return attr["value"].as_str().map(|s| s.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if let Some(events) = tx_json["events"].as_array() {
        for event in events {
            if event["type"].as_str() == Some(event_type) {
                if let Some(attrs) = event["attributes"].as_array() {
                    for attr in attrs {
                        if attr["key"].as_str() == Some(attr_key) {
                            return attr["value"].as_str().map(|s| s.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

fn tx_body(parsed: &serde_json::Value) -> serde_json::Value {
    parsed
        .get("tx_response")
        .cloned()
        .unwrap_or_else(|| parsed.clone())
}

fn tx_included(body: &serde_json::Value) -> bool {
    match &body["height"] {
        serde_json::Value::String(s) => !s.is_empty() && s != "0",
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(0) > 0,
        _ => false,
    }
}

fn code_id_field(v: &serde_json::Value) -> Option<String> {
    v.get("code_id").and_then(|id| {
        id.as_str()
            .map(|s| s.to_string())
            .or_else(|| id.as_u64().map(|n| n.to_string()))
    })
}

fn code_id_from_events(tx: &serde_json::Value) -> Option<String> {
    extract_event_attr(tx, "store_code", "code_id")
}
