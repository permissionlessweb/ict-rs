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
                    "tx", "gov", "submit-legacy-proposal", "software-upgrade",
                    upgrade_name,
                    "--title", &format!("Upgrade to {}", upgrade_name),
                    "--description", &format!("Software upgrade to {}", upgrade_name),
                    "--upgrade-height", &height_str,
                    "--deposit", deposit,
                ],
                opts,
            )
            .await?;
        let json_str = output.stdout_str();
        let v: serde_json::Value =
            serde_json::from_str(json_str.trim()).unwrap_or(serde_json::Value::Null);

        // Extract proposal_id from response — check multiple locations
        let proposal_id = v["proposal_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| v["proposal_id"].as_u64())
            // Check in tx logs/events
            .or_else(|| {
                v["logs"].as_array()?.iter().find_map(|log| {
                    log["events"].as_array()?.iter().find_map(|evt| {
                        if evt["type"].as_str()? == "submit_proposal" {
                            evt["attributes"].as_array()?.iter().find_map(|attr| {
                                if attr["key"].as_str()? == "proposal_id" {
                                    attr["value"].as_str()?.parse().ok()
                                } else {
                                    None
                                }
                            })
                        } else {
                            None
                        }
                    })
                })
            })
            .unwrap_or(1); // Default to 1 for the first proposal

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
