//! Governance extension trait for chain upgrade proposals and voting.
//!
//! Provides `submit_software_upgrade_proposal`, `vote_on_proposal`,
//! `query_proposal`, and `poll_for_proposal_status` as convenience methods
//! on any type implementing [`Chain`].

use crate::chain::Chain;
use crate::cli::{parse_tx_response, QUERY_DEFAULT_FLAGS};
use crate::error::{IctError, Result};
use crate::tx::Tx;

use async_trait::async_trait;

/// Cosmos SDK governance proposal status strings.
pub mod status {
    pub const DEPOSIT_PERIOD: &str = "PROPOSAL_STATUS_DEPOSIT_PERIOD";
    pub const VOTING_PERIOD: &str = "PROPOSAL_STATUS_VOTING_PERIOD";
    pub const PASSED: &str = "PROPOSAL_STATUS_PASSED";
    pub const REJECTED: &str = "PROPOSAL_STATUS_REJECTED";
    pub const FAILED: &str = "PROPOSAL_STATUS_FAILED";
}

/// Extension trait for governance operations on any chain.
///
/// Blanket-implemented for all `T: Chain`, so any chain type automatically
/// gains governance functionality.
#[async_trait]
pub trait GovernanceExt: Chain {
    /// Submit a software upgrade proposal. Returns the proposal ID.
    async fn submit_software_upgrade_proposal(
        &self,
        key_name: &str,
        upgrade_name: &str,
        height: u64,
        deposit: &str,
    ) -> Result<u64> {
        let height_str = height.to_string();
        let opts = self
            .default_tx_opts()
            .from(key_name)
            .flag("--no-validate", "");

        let output = self
            .chain_exec_tx_with(
                &[
                    "tx", "upgrade", "software-upgrade",
                    upgrade_name,
                    "--title", &format!("Upgrade to {}", upgrade_name),
                    "--summary", &format!("Software upgrade to {}", upgrade_name),
                    "--upgrade-height", &height_str,
                    "--deposit", deposit,
                ],
                opts,
            )
            .await?;
        if output.exit_code != 0 {
            return Err(IctError::ExecFailed {
                exit_code: output.exit_code,
                stderr: format!("software-upgrade submit failed: {}", output.stderr_str()),
            });
        }
        let json_str = output.stdout_str();
        let v: serde_json::Value =
            serde_json::from_str(json_str.trim()).unwrap_or(serde_json::Value::Null);

        let mut proposal_id = extract_proposal_id(&v);
        if proposal_id.is_none() {
            if let Some(hash) = v.get("txhash").and_then(|x| x.as_str()) {
                proposal_id = query_proposal_id_from_tx(self, hash).await?;
            }
        }
        let proposal_id = proposal_id.ok_or_else(|| {
            IctError::Config(format!(
                "software-upgrade tx succeeded but no proposal_id in response: {json_str}"
            ))
        })?;

        Ok(proposal_id)
    }

    /// Vote on a governance proposal.
    async fn vote_on_proposal(
        &self,
        key_name: &str,
        proposal_id: u64,
        option: &str,
    ) -> Result<Tx> {
        let prop_id_str = proposal_id.to_string();
        let opts = self.default_tx_opts().from(key_name);

        let output = self
            .chain_exec_tx_with(
                &["tx", "gov", "vote", &prop_id_str, option],
                opts,
            )
            .await?;
        parse_tx_response(&output)
    }

    /// Query a governance proposal by ID.
    async fn query_proposal(&self, proposal_id: u64) -> Result<serde_json::Value> {
        let prop_id_str = proposal_id.to_string();

        let mut args: Vec<String> = vec![
            "query".into(),
            "gov".into(),
            "proposal".into(),
            prop_id_str,
        ];
        for flag in QUERY_DEFAULT_FLAGS {
            args.push(flag.to_string());
        }

        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.chain_exec(&arg_refs).await?;
        let json_str = output.stdout_str();
        serde_json::from_str(json_str.trim())
            .map_err(|e| IctError::Config(format!("invalid proposal query JSON: {e}")))
    }

    /// Poll until a proposal reaches the target status or timeout.
    async fn poll_for_proposal_status(
        &self,
        proposal_id: u64,
        target_status: &str,
        timeout_secs: u64,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed() > timeout {
                return Err(IctError::Chain {
                    chain_id: self.chain_id().to_string(),
                    source: anyhow::anyhow!(
                        "proposal {} did not reach status '{}' within {}s",
                        proposal_id,
                        target_status,
                        timeout_secs
                    ),
                });
            }

            match self.query_proposal(proposal_id).await {
                Ok(v) => {
                    let status = v["proposal"]["status"]
                        .as_str()
                        .or_else(|| v["status"].as_str())
                        .unwrap_or("");

                    if status == target_status {
                        return Ok(());
                    }
                }
                Err(_) => {} // ignore transient query errors
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

impl<T: Chain + ?Sized> GovernanceExt for T {}


fn extract_proposal_id(v: &serde_json::Value) -> Option<u64> {
    let parse_attr = |evt: &serde_json::Value| -> Option<u64> {
        let typ = evt.get("type")?.as_str()?;
        if typ != "submit_proposal" && typ != "proposal_deposit" && typ != "message" {
            return None;
        }
        evt.get("attributes")?.as_array()?.iter().find_map(|attr| {
            let key = attr.get("key")?.as_str()?;
            if key == "proposal_id" {
                attr.get("value")
                    .and_then(|x| x.as_str().and_then(|s| s.parse().ok()).or_else(|| x.as_u64()))
            } else {
                None
            }
        })
    };

    v.get("proposal_id")
        .and_then(|x| x.as_str().and_then(|s| s.parse().ok()).or_else(|| x.as_u64()))
        .or_else(|| v.get("events")?.as_array()?.iter().find_map(parse_attr))
        .or_else(|| {
            v.get("tx_response")?
                .get("events")?
                .as_array()?
                .iter()
                .find_map(parse_attr)
        })
        .or_else(|| {
            v.get("logs")?.as_array()?.iter().find_map(|log| {
                log.get("events")?.as_array()?.iter().find_map(parse_attr)
            })
        })
}

async fn query_proposal_id_from_tx<C: Chain + ?Sized>(chain: &C, txhash: &str) -> Result<Option<u64>> {
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let out = chain
            .chain_exec(&["query", "tx", txhash, "--output", "json"])
            .await?;
        if out.exit_code != 0 {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(out.stdout_str().trim()).unwrap_or(serde_json::Value::Null);
        if let Some(id) = extract_proposal_id(&v) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}
