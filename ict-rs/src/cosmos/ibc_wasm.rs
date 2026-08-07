//! IBC **08-wasm** light-client bytecode lifecycle via governance.
//!
//! Production path (ibc-go v10+):
//! ```text
//! terpd tx ibc-wasm store-code <wasm> --title … --summary … --deposit …
//! terpd tx gov vote <id> yes
//! # wait PROPOSAL_STATUS_PASSED
//! terpd q ibc-wasm checksums
//! ```
//!
//! Authority defaults to the gov module account (MsgStoreCode.signer = gov).
//! This is **not** CosmWasm `tx wasm store` (app contracts) — it stores LC
//! bytecode into the 08-wasm client module for `08-wasm-*` clients.

use crate::chain::Chain;
use crate::cli::QUERY_DEFAULT_FLAGS;
use crate::cosmwasm::extract_event_attr;
use crate::error::{IctError, Result};
use crate::governance::{status, GovernanceExt};
use crate::tx::Tx;

use async_trait::async_trait;

/// Options for an IBC 08-wasm store-code governance proposal.
#[derive(Debug, Clone)]
pub struct IbcWasmStoreProposal {
    /// Path **inside the node container** (copy with `copy_file_from_host` first).
    pub wasm_path: String,
    pub title: String,
    pub summary: String,
    /// e.g. `10000000uterp` (must meet chain min_deposit).
    pub deposit: String,
    /// Optional authority override (default: gov module account).
    pub authority: Option<String>,
    /// Optional metadata string for the proposal.
    pub metadata: Option<String>,
}

impl Default for IbcWasmStoreProposal {
    fn default() -> Self {
        Self {
            wasm_path: String::new(),
            title: "Store IBC 08-wasm light client".into(),
            summary: "Upload wasm light-client bytecode via gov".into(),
            deposit: "10000000uterp".into(),
            authority: None,
            metadata: None,
        }
    }
}

/// Result of a full store-code proposal → vote → pass cycle.
#[derive(Debug, Clone)]
pub struct IbcWasmStoreResult {
    pub proposal_id: u64,
    pub tx_hash: String,
    /// Checksums known after pass (may be empty if query shape differs).
    pub checksums: Vec<String>,
}

/// Extension trait for IBC 08-wasm light-client governance ops.
#[async_trait]
pub trait IbcWasmExt: Chain + GovernanceExt {
    /// Submit `tx ibc-wasm store-code` (governance proposal). Returns proposal id + txhash.
    async fn submit_ibc_wasm_store_code(
        &self,
        key_name: &str,
        prop: &IbcWasmStoreProposal,
    ) -> Result<(u64, String)> {
        if prop.wasm_path.is_empty() {
            return Err(IctError::Config("ibc-wasm store-code: empty wasm_path".into()));
        }

        let adj = (self.config().gas_adjustment * 1.5).max(2.5);
        let mut opts = self
            .default_tx_opts()
            .from(key_name)
            .gas_adjustment(adj)
            .flag("--title", &prop.title)
            .flag("--summary", &prop.summary)
            .flag("--deposit", &prop.deposit);

        if let Some(ref meta) = prop.metadata {
            opts = opts.flag("--metadata", meta);
        }
        if let Some(ref auth) = prop.authority {
            opts = opts.flag("--authority", auth);
        }

        let output = self
            .chain_exec_tx_with(
                &["tx", "ibc-wasm", "store-code", &prop.wasm_path],
                opts,
            )
            .await?;

        let stdout = output.stdout_str();
        let stderr = output.stderr_str();
        if output.exit_code != 0 {
            return Err(IctError::Config(format!(
                "ibc-wasm store-code failed (exit {}): stdout={} stderr={}",
                output.exit_code,
                trunc(&stdout, 800),
                trunc(&stderr, 800)
            )));
        }

        let raw = if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            IctError::Config(format!(
                "ibc-wasm store-code JSON: {e}\nraw={}",
                trunc(&raw, 500)
            ))
        })?;

        if let Some(code) = v["code"].as_u64() {
            if code != 0 {
                return Err(IctError::Config(format!(
                    "ibc-wasm store-code rejected code={code}: {}",
                    v["raw_log"].as_str().unwrap_or("unknown")
                )));
            }
        }

        let txhash = v["txhash"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if txhash.is_empty() {
            return Err(IctError::Config("ibc-wasm store-code: no txhash".into()));
        }

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;

        // Resolve proposal_id from query tx events
        let proposal_id = self
            .proposal_id_from_tx(&txhash)
            .await?
            .unwrap_or(1);

        Ok((proposal_id, txhash))
    }

    /// Vote yes + wait for PASSED (needs fast voting_period in genesis for local e2e).
    async fn pass_ibc_wasm_store_code(
        &self,
        key_name: &str,
        proposal_id: u64,
        timeout_secs: u64,
    ) -> Result<()> {
        let _ = self
            .vote_on_proposal(key_name, proposal_id, "yes")
            .await?;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        self.poll_for_proposal_status(proposal_id, status::PASSED, timeout_secs)
            .await
    }

    /// Full local loop: propose store-code → vote yes → wait PASSED → list checksums.
    async fn gov_store_ibc_wasm_lc(
        &self,
        key_name: &str,
        prop: &IbcWasmStoreProposal,
        timeout_secs: u64,
    ) -> Result<IbcWasmStoreResult> {
        let (proposal_id, tx_hash) = self.submit_ibc_wasm_store_code(key_name, prop).await?;
        self.pass_ibc_wasm_store_code(key_name, proposal_id, timeout_secs)
            .await?;
        let checksums = self.query_ibc_wasm_checksums().await.unwrap_or_default();
        Ok(IbcWasmStoreResult {
            proposal_id,
            tx_hash,
            checksums,
        })
    }

    /// `query ibc-wasm checksums`
    async fn query_ibc_wasm_checksums(&self) -> Result<Vec<String>> {
        let mut args: Vec<String> = vec![
            "query".into(),
            "ibc-wasm".into(),
            "checksums".into(),
        ];
        for flag in QUERY_DEFAULT_FLAGS {
            args.push(flag.to_string());
        }
        let refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let out = self.chain_exec(&refs).await?;
        let raw = out.stdout_str();
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }
        let v: serde_json::Value = serde_json::from_str(raw.trim())
            .map_err(|e| IctError::Config(format!("ibc-wasm checksums JSON: {e}")))?;

        let mut out_v = Vec::new();
        // shapes: { "checksums": ["hex", ...] } or nested
        if let Some(arr) = v.get("checksums").and_then(|c| c.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    out_v.push(s.to_string());
                } else if let Some(s) = item.get("checksum").and_then(|x| x.as_str()) {
                    out_v.push(s.to_string());
                }
            }
        }
        Ok(out_v)
    }

    /// Extract proposal_id from a gov submit tx.
    async fn proposal_id_from_tx(&self, txhash: &str) -> Result<Option<u64>> {
        let tx_output = self
            .chain_exec(&["query", "tx", txhash, "--output", "json"])
            .await?;
        let raw = tx_output.stdout_str();
        if raw.trim().is_empty() {
            return Ok(None);
        }
        let tx_json: serde_json::Value = serde_json::from_str(raw.trim())
            .map_err(|e| IctError::Config(format!("query tx: {e}")))?;

        if let Some(s) = extract_event_attr(&tx_json, "submit_proposal", "proposal_id")
            .or_else(|| extract_event_attr(&tx_json, "proposal_deposit", "proposal_id"))
            .or_else(|| extract_event_attr(&tx_json, "active_proposal", "proposal_id"))
        {
            if let Ok(id) = s.parse::<u64>() {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Convenience: vote yes (re-export shape for callers that only import this trait).
    async fn ibc_wasm_vote_yes(&self, key_name: &str, proposal_id: u64) -> Result<Tx> {
        self.vote_on_proposal(key_name, proposal_id, "yes").await
    }
}

impl<T: Chain + ?Sized> IbcWasmExt for T {}

fn trunc(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.len() <= n {
        t.to_string()
    } else {
        format!("{}…", &t[..n])
    }
}
