//! Data types for the QuickSpawn snapshot-based test environment.
//!
//! A `ChainSnapshot` captures the minimal state needed to reconstruct a
//! Cosmos chain from a given point: genesis.json, validator keys, and
//! deployed contract metadata. This allows test environments to skip the
//! expensive genesis pipeline and start a chain in under 2 seconds.
//!
//! # Multi-node support
//!
//! A snapshot can capture N validators and M full nodes, each with their own
//! node keys. The [`SpawnedChainSet`] type is returned by the multi-node
//! spawn method and provides access to all spawned nodes.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::runtime::NetworkId;
use sha2::{Digest, Sha256};

/// Complete snapshot of a bootstrapped chain.
///
/// This is the primary artifact produced by `QuickSpawnManager::bootstrap`
/// and consumed by `QuickSpawnManager::spawn` / `spawn_multi`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChainSnapshot {
    pub metadata: SnapshotMetadata,
    pub genesis_json: serde_json::Value,
    /// Validator node keys (index 0 = primary).
    pub validators: Vec<ValidatorEntry>,
    /// Full node keys (each has a node key but no priv_validator_key).
    pub full_nodes: Vec<ValidatorEntry>,
    pub faucet: Option<KeyEntry>,
    pub contracts: Vec<DeployedContract>,
}

/// Metadata identifying and describing a snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    /// Chain identifier (e.g. "terp-test-1").
    pub chain_id: String,
    /// Block height at which the snapshot was taken.
    pub height: u64,
    /// UNIX timestamp of snapshot creation.
    pub created_at: i64,
    /// SHA-256 hex digest of genesis.json (for integrity checks).
    pub genesis_hash: String,
    /// The chain binary version used during bootstrap.
    pub binary_version: String,
    /// Number of validators in the chain.
    pub validator_count: usize,
    /// Number of full nodes in the chain.
    pub num_full_nodes: usize,
}

impl SnapshotMetadata {
    /// Derive a deterministic snapshot identifier from chain-id + height + genesis hash prefix.
    ///
    /// Re-bootstrapping the same chain at the same height produces the same ID,
    /// so snapshots are naturally deduplicated.
    pub fn snapshot_id(&self) -> String {
        format!(
            "{}-h{}-{}",
            self.chain_id,
            self.height,
            &self.genesis_hash[..16]
        )
    }
}

/// A validator's identity material: consensus key, node key, and node peer ID.
///
/// These are injected into a node's config directory so the chain can
/// start from a previously-exported genesis without re-running `init`
/// or `gentx`.
///
/// The `node_id` is the CometBFT peer ID derived from `node_key.json`.
/// It is captured during bootstrap and stored so the spawn phase can
/// configure P2P peering without running `comet show-node-id` on each node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorEntry {
    /// 0-based index within the validator set.
    pub index: usize,
    /// JSON content of `config/priv_validator_key.json`.
    /// Empty string for full nodes (they have no consensus key).
    pub validator_key_json: String,
    /// JSON content of `config/node_key.json`.
    pub node_key_json: String,
    /// Optional wallet mnemonic (recovered during bootstrap).
    pub wallet_mnemonic: Option<String>,
    /// CometBFT peer ID (hex-encoded node ID), captured during bootstrap.
    /// Used for persistent_peers configuration at spawn time.
    pub node_id: Option<String>,
}

/// A key entry for a non-validator account (faucet, relayer, etc.).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyEntry {
    /// Account name.
    pub name: String,
    /// Address (bech32-encoded).
    pub address: String,
    /// BIP39 mnemonic (set during genesis).
    pub mnemonic: String,
}

/// A deployed contract recorded during the bootstrap phase.
///
/// The spawn phase uses this to connect to already-deployed contracts
/// without needing to re-upload or re-instantiate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeployedContract {
    /// Human-readable label (e.g. "cw20-token", "voting-module").
    pub name: String,
    /// On-chain code ID.
    pub code_id: u64,
    /// Contract address (bech32-encoded).
    pub address: String,
    /// Optional Instantiate2 salt (if deployed via instantiate2).
    pub salt: Option<String>,
}

impl ChainSnapshot {
    /// Create a new snapshot with metadata computed from the genesis hash.
    pub fn new(
        chain_id: &str,
        height: u64,
        binary_version: &str,
        validator_count: usize,
        num_full_nodes: usize,
        genesis_json: serde_json::Value,
    ) -> Self {
        let genesis_bytes = serde_json::to_vec(&genesis_json)
            .expect("BUG: serde_json::Value must serialize — this is a fundamental invariant violation");
        let genesis_hash = hex::encode(Sha256::digest(&genesis_bytes));

        Self {
            metadata: SnapshotMetadata {
                chain_id: chain_id.to_string(),
                height,
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64,
                genesis_hash,
                binary_version: binary_version.to_string(),
                validator_count,
                num_full_nodes,
            },
            genesis_json,
            validators: Vec::new(),
            full_nodes: Vec::new(),
            faucet: None,
            contracts: Vec::new(),
        }
    }

    /// Look up a deployed contract by name.
    pub fn contract(&self, name: &str) -> Option<&DeployedContract> {
        self.contracts.iter().find(|c| c.name == name)
    }

    /// Encode to JSON for compact storage.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("JSON serialization should not fail")
    }

    /// Decode from JSON.
    pub fn from_json(json: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(serde_json::from_str(json)?)
    }

    /// Encode to pretty-printed JSON for debugging.
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("JSON serialization should not fail")
    }
}

/// Convenience alias for snapshot JSON bytes.
pub type SnapshotBytes = Vec<u8>;

/// A map of contract addresses keyed by name — the runtime view for tests.
#[derive(Clone, Debug, Default)]
pub struct ContractRegistry {
    contracts: HashMap<String, DeployedContract>,
}

impl ContractRegistry {
    pub fn from_snapshot(snapshot: &ChainSnapshot) -> Self {
        let mut contracts = HashMap::new();
        for c in &snapshot.contracts {
            contracts.insert(c.name.clone(), c.clone());
        }
        Self { contracts }
    }

    pub fn get(&self, name: &str) -> Option<&DeployedContract> {
        self.contracts.get(name)
    }

    pub fn address(&self, name: &str) -> Option<&str> {
        self.contracts.get(name).map(|c| c.address.as_str())
    }

    pub fn code_id(&self, name: &str) -> Option<u64> {
        self.contracts.get(name).map(|c| c.code_id)
    }
}

/// A single node within a spawned multi-node chain set.
#[derive(Debug)]
pub struct SpawnedNode {
    /// The ChainNode wrapping the running container.
    pub node: crate::cosmos::node::ChainNode,
    /// Host-mapped RPC port.
    pub host_rpc_port: u16,
    /// Host-mapped gRPC port.
    pub host_grpc_port: u16,
    /// Host-mapped P2P port.
    pub host_p2p_port: u16,
}

/// A multi-node chain spawned from a snapshot.
///
/// Returned by [`super::QuickSpawnManager::spawn_multi`]. Provides
/// access to all validator and full nodes. The primary validator
/// (index 0) is the canonical entry point for cw-orch Daemon connections.
#[derive(Debug)]
pub struct SpawnedChainSet {
    /// All validator nodes.
    pub validators: Vec<SpawnedNode>,
    /// All full nodes.
    pub full_nodes: Vec<SpawnedNode>,
    /// The snapshot used to create this chain.
    pub snapshot: ChainSnapshot,
    /// Docker network ID for cleanup.
    pub network_id: String,
}

impl SpawnedChainSet {
    /// Returns the primary validator (index 0), if any.
    pub fn primary(&self) -> Option<&SpawnedNode> {
        self.validators.first()
    }

    /// RPC URL of the primary validator for cw-orch Daemon config.
    pub fn rpc_url(&self) -> String {
        self.primary().map_or_else(
            || "http://127.0.0.1:26657".to_string(),
            |n| format!("http://127.0.0.1:{}", n.host_rpc_port),
        )
    }

    /// gRPC URL of the primary validator for cw-orch Daemon config.
    pub fn grpc_url(&self) -> String {
        self.primary().map_or_else(
            || "http://127.0.0.1:9090".to_string(),
            |n| format!("http://127.0.0.1:{}", n.host_grpc_port),
        )
    }

    /// Returns the total number of nodes (validators + full nodes).
    pub fn total_nodes(&self) -> usize {
        self.validators.len() + self.full_nodes.len()
    }
}

// ── Drop impls for automatic cleanup ─────────────────────────────────

impl Drop for SpawnedChainSet {
    /// Best-effort cleanup: attempts to stop and remove all containers,
    /// then remove the Docker network.
    ///
    /// This only works when called inside an active tokio runtime
    /// (which `#[tokio::test]` guarantees). If no runtime is active,
    /// containers are leaked with a warning. Call [`QuickSpawnManager::stop_set`]
    /// explicitly in CI for guaranteed cleanup.
    fn drop(&mut self) {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let net_id_str = self.network_id.clone();
                let mut cleanup_tasks: Vec<(crate::runtime::ContainerId, Arc<dyn crate::runtime::RuntimeBackend>)> = Vec::new();
                for node in self.validators.iter().chain(self.full_nodes.iter()) {
                    if let Some(ref id) = node.node.container_id {
                        cleanup_tasks.push((id.clone(), node.node.runtime.clone()));
                    }
                }

                let _ = handle.block_on(async {
                    for (container_id, runtime) in &cleanup_tasks {
                        let _ = runtime.stop_container(container_id).await;
                        let _ = runtime.remove_container(container_id).await;
                    }
                    if !net_id_str.is_empty() {
                        let net_id = NetworkId(net_id_str);
                        if let Some(first_node) = self.validators.first()
                            .or_else(|| self.full_nodes.first())
                        {
                            let _ = first_node.node.runtime.remove_network(&net_id).await;
                        }
                    }
                });
            }
            Err(_) => warn!(
                "SpawnedChainSet dropped outside tokio runtime — \
                 containers and Docker network will leak. \
                 Call stop_set() explicitly in CI."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snapshot_id_deterministic() {
        let genesis = serde_json::json!({"chain_id": "test-1", "app_state": {}});
        let snap = ChainSnapshot::new("test-1", 42, "v1.0.0", 0, 0, genesis);
        let id1 = snap.metadata.snapshot_id();
        assert!(id1.starts_with("test-1-h42-"));
        // Same inputs produce same id
        let genesis2 = serde_json::json!({"chain_id": "test-1", "app_state": {}});
        let snap2 = ChainSnapshot::new("test-1", 42, "v1.0.0", 0, 0, genesis2);
        assert_eq!(id1, snap2.metadata.snapshot_id());
    }

    #[test]
    fn test_snapshot_roundtrip_msgpack() {
        let genesis = serde_json::json!({"chain_id": "roundtrip", "app_state": {}});
        let mut snap = ChainSnapshot::new("roundtrip", 100, "v2.0.0", 1, 1, genesis);
        snap.validators.push(ValidatorEntry {
            index: 0,
            validator_key_json: r#"{"key":"val1"}"#.to_string(),
            node_key_json: r#"{"key":"node1"}"#.to_string(),
            wallet_mnemonic: None,
            node_id: Some("abc123def456".to_string()),
        });
        snap.full_nodes.push(ValidatorEntry {
            index: 0,
            validator_key_json: String::new(),
            node_key_json: r#"{"key":"fn1"}"#.to_string(),
            wallet_mnemonic: None,
            node_id: Some("def789abc012".to_string()),
        });
        snap.contracts.push(DeployedContract {
            name: "my-contract".to_string(),
            code_id: 1,
            address: "terp1contract".to_string(),
            salt: None,
        });

        let bytes = snap.to_json();
        let restored = ChainSnapshot::from_json(&bytes).unwrap();

        assert_eq!(restored.metadata.chain_id, "roundtrip");
        assert_eq!(restored.metadata.height, 100);
        assert_eq!(restored.validators.len(), 1);
        assert_eq!(restored.full_nodes.len(), 1);
        assert_eq!(restored.contracts.len(), 1);
        assert_eq!(restored.contracts[0].name, "my-contract");
        assert_eq!(restored.validators[0].node_id.as_deref(), Some("abc123def456"));
        assert_eq!(restored.full_nodes[0].node_id.as_deref(), Some("def789abc012"));
    }

    #[test]
    fn test_contract_registry() {
        let genesis = serde_json::json!({});
        let mut snap = ChainSnapshot::new("reg-test", 1, "v1", 0, 0, genesis);
        snap.contracts.push(DeployedContract {
            name: "voter".to_string(),
            code_id: 7,
            address: "terp1voter".to_string(),
            salt: None,
        });

        let registry = ContractRegistry::from_snapshot(&snap);
        assert_eq!(registry.address("voter"), Some("terp1voter"));
        assert_eq!(registry.code_id("voter"), Some(7));
        assert!(registry.get("unknown").is_none());
    }

    #[test]
    fn test_spawned_chain_set_urls() {
        // Empty set: default URLs
        let snap = ChainSnapshot::new("empty", 0, "v1", 0, 0, serde_json::json!({}));
        let set = SpawnedChainSet {
            validators: vec![],
            full_nodes: vec![],
            snapshot: snap,
            network_id: "test-net".to_string(),
        };
        assert_eq!(set.rpc_url(), "http://127.0.0.1:26657");
        assert_eq!(set.total_nodes(), 0);
    }
}