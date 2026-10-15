//! IBC v2 client path: `tx ibc client create` + `add-counterparty`.
//!
//! This is the shipped CLI used by `trustless_builder` (not Hermes v1
//! `create client`). Packet relay can still be Hermes; the v2 *client*
//! is this terpd path.

use async_trait::async_trait;
use serde_json::Value;

use crate::chain::cosmos::CosmosChain;
use crate::chain::Chain;
use crate::cosmwasm::extract_event_attr;
use crate::error::{IctError, Result};

/// `terpd tx ibc client create <client_state.json> <cons_state.json>`
pub fn ibc_v2_create_client_tx_args(
    client_state_path: &str,
    cons_state_path: &str,
) -> Vec<String> {
    vec![
        "tx".into(),
        "ibc".into(),
        "client".into(),
        "create".into(),
        client_state_path.into(),
        cons_state_path.into(),
    ]
}

/// `terpd tx ibc client add-counterparty <client> <counterparty> <merkle_prefix>`
pub fn ibc_v2_add_counterparty_tx_args(
    client_id: &str,
    counterparty_client_id: &str,
    merkle_prefix: &str,
) -> Vec<String> {
    vec![
        "tx".into(),
        "ibc".into(),
        "client".into(),
        "add-counterparty".into(),
        client_id.into(),
        counterparty_client_id.into(),
        merkle_prefix.into(),
    ]
}

const CLIENT_STATE_PATH: &str = "/tmp/ibc_v2_client_state.json";
const CONS_STATE_PATH: &str = "/tmp/ibc_v2_cons_state.json";
/// Base64 of `ibc` — same merkle prefix as trustless_builder.
pub const IBC_V2_MERKLE_PREFIX: &str = "aWJj";

/// IBC v2 tendermint client + RegisterCounterparty (ibc-go v10 channel/v2).
#[async_trait]
pub trait IbcV2Ext: Chain {
    async fn create_ibc_v2_tendermint_path(&self, from: &str) -> Result<String>;
}

#[async_trait]
impl IbcV2Ext for CosmosChain {
    async fn create_ibc_v2_tendermint_path(&self, from: &str) -> Result<String> {
        create_ibc_v2_tendermint_path(self, from).await
    }
}

/// Create a 07-tendermint client from this chain's self header, then
/// `add-counterparty` so IBC v2 packets can commit.
pub async fn create_ibc_v2_tendermint_path(
    terp: &CosmosChain,
    from: &str,
) -> Result<String> {
    let status = terp
        .chain_exec(&["status", "--output", "json"])
        .await
        .map_err(|e| IctError::Config(format!("status: {e}")))?;
    let status_j: Value = serde_json::from_str(status.stdout_str().trim())
        .or_else(|_| serde_json::from_str(status.stderr_str().trim()))
        .map_err(|e| {
            IctError::Config(format!("status json: {e}\n{}", status.stdout_str()))
        })?;
    let height = latest_height(&status_j)?;

    let cons = terp
        .chain_exec(&[
            "query",
            "ibc",
            "client",
            "self-consensus-state",
            "--output",
            "json",
        ])
        .await
        .map_err(|e| IctError::Config(format!("self-consensus-state: {e}")))?;
    let mut cons_j: Value = serde_json::from_str(cons.stdout_str().trim()).map_err(|e| {
        IctError::Config(format!(
            "self-consensus-state json: {e}\n{}",
            cons.stdout_str()
        ))
    })?;
    if cons_j.get("@type").is_none() {
        if let Some(inner) = cons_j.get("consensus_state").cloned() {
            cons_j = inner;
        }
    }
    if cons_j.get("@type").is_none() {
        if let Some(o) = cons_j.as_object_mut() {
            o.insert(
                "@type".into(),
                Value::String("/ibc.lightclients.tendermint.v1.ConsensusState".into()),
            );
        }
    }

    let client_state = serde_json::json!({
        "@type": "/ibc.lightclients.tendermint.v1.ClientState",
        "chain_id": terp.chain_id(),
        "trust_level": { "numerator": "1", "denominator": "3" },
        "trusting_period": "80s",
        "unbonding_period": "120s",
        "max_clock_drift": "20s",
        "frozen_height": { "revision_number": "0", "revision_height": "0" },
        "latest_height": { "revision_number": "1", "revision_height": height },
        // IBC store stays on SHA-256 IAVL. An empty list fails client Validate
        // ("proof specs cannot be nil"). These are ics23 IavlSpec + TendermintSpec.
        "proof_specs": [
            {
                "leaf_spec": {
                    "hash": "SHA256",
                    "prehash_value": "SHA256",
                    "prehash_key": "NO_HASH",
                    "length": "VAR_PROTO",
                    "prefix": "AA=="
                },
                "inner_spec": {
                    "child_order": [0, 1],
                    "child_size": 33,
                    "min_prefix_length": 4,
                    "max_prefix_length": 12,
                    "hash": "SHA256"
                }
            },
            {
                "leaf_spec": {
                    "hash": "SHA256",
                    "prehash_value": "SHA256",
                    "prehash_key": "NO_HASH",
                    "length": "VAR_PROTO",
                    "prefix": "AA=="
                },
                "inner_spec": {
                    "child_order": [0, 1],
                    "child_size": 32,
                    "min_prefix_length": 1,
                    "max_prefix_length": 1,
                    "hash": "SHA256"
                }
            }
        ],
        "upgrade_path": ["upgrade", "upgradedIBCState"],
        "allow_update_after_expiry": false,
        "allow_update_after_misbehaviour": false
    });

    let node = terp.primary_node()?;
    node.write_file(client_state.to_string().as_bytes(), CLIENT_STATE_PATH)
        .await?;
    node.write_file(cons_j.to_string().as_bytes(), CONS_STATE_PATH)
        .await?;

    let create_args = ibc_v2_create_client_tx_args(CLIENT_STATE_PATH, CONS_STATE_PATH);
    let create_refs: Vec<&str> = create_args.iter().map(|s| s.as_str()).collect();
    let created = ibc_tx_json(terp, from, &create_refs).await?;
    let client_id = extract_event_attr(&created, "create_client", "client_id")
        .or_else(|| extract_event_attr(&created, "message", "client_id"))
        .ok_or_else(|| {
            IctError::Config(format!("no client_id in create client tx: {created}"))
        })?;
    if !client_id.starts_with("07-tendermint-") {
        return Err(IctError::Config(format!(
            "expected 07-tendermint client, got {client_id}"
        )));
    }

    let cp_args =
        ibc_v2_add_counterparty_tx_args(&client_id, "08-wasm-0", IBC_V2_MERKLE_PREFIX);
    let cp_refs: Vec<&str> = cp_args.iter().map(|s| s.as_str()).collect();
    ibc_tx_json(terp, from, &cp_refs).await?;
    Ok(client_id)
}

fn latest_height(status_j: &Value) -> Result<String> {
    status_j
        .pointer("/sync_info/latest_block_height")
        .or_else(|| status_j.pointer("/SyncInfo/latest_block_height"))
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .ok_or_else(|| IctError::Config(format!("no latest_block_height in status: {status_j}")))
}

async fn ibc_tx_json(terp: &CosmosChain, from: &str, args: &[&str]) -> Result<Value> {
    let opts = terp.default_tx_opts().from(from);
    let out = terp.chain_exec_tx_with(args, opts).await?;
    if out.exit_code != 0 {
        return Err(IctError::ExecFailed {
            exit_code: out.exit_code,
            stderr: out.stderr_str(),
        });
    }
    let j: Value = serde_json::from_str(out.stdout_str().trim()).unwrap_or(Value::Null);
    if j["code"].as_u64().unwrap_or(1) != 0 {
        return Err(IctError::Config(format!(
            "tx {} code!=0: {}",
            args.join(" "),
            out.stdout_str()
        )));
    }
    // sync broadcast often returns height 0 and no events. Wait for inclusion.
    let included = j["height"].as_str().unwrap_or("0") != "0"
        && j["height"].as_str().unwrap_or("0") != "";
    if included {
        return Ok(j);
    }
    let hash = j["txhash"]
        .as_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| {
            IctError::Config(format!(
                "tx {} accepted with no hash: {}",
                args.join(" "),
                out.stdout_str()
            ))
        })?
        .to_string();
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let q = match terp
            .chain_exec(&["query", "tx", &hash, "--output", "json"])
            .await
        {
            Ok(q) if q.exit_code == 0 => q,
            _ => continue,
        };
        let Ok(parsed) = serde_json::from_str::<Value>(q.stdout_str().trim()) else {
            continue;
        };
        let tv = if parsed.get("tx_response").is_some() {
            parsed["tx_response"].clone()
        } else {
            parsed
        };
        let height = tv["height"].as_str().unwrap_or("0");
        if height == "0" || height.is_empty() {
            continue;
        }
        if tv["code"].as_u64().unwrap_or(1) != 0 {
            return Err(IctError::Config(format!(
                "tx {hash} code!=0: {}",
                q.stdout_str()
            )));
        }
        return Ok(tv);
    }
    Err(IctError::Config(format!(
        "timed out waiting for tx {hash}"
    )))
}
