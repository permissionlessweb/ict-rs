//! Snapshot storage backends.
//!
//! The `SnapshotStore` trait abstracts where snapshots are persisted.
//! Default implementations:
//!
//! - [`LocalFsStore`] — local filesystem (offline-testable, no external deps)
//! - `S3Store` — S3 bucket (planned, needs `aws-sdk-s3` or `reqwest`)

use std::path::PathBuf;

use async_trait::async_trait;

use super::snapshot::ChainSnapshot;

/// Abstract snapshot storage: save and load full snapshots.
///
/// Implementations handle serialization, compression, and transport.
/// The `save` method stores both the compact JSON and the standalone
/// genesis.json + metadata.json for human inspection.
#[async_trait]
pub trait SnapshotStore: Send + Sync {
    /// Save a snapshot and return its identifier.
    async fn save(&self, snapshot: &ChainSnapshot) -> Result<String, anyhow::Error>;

    /// Load a snapshot by identifier.
    async fn load(&self, snapshot_id: &str) -> Result<ChainSnapshot, anyhow::Error>;

    /// Check if a snapshot exists.
    async fn exists(&self, snapshot_id: &str) -> bool;
}

/// Local filesystem snapshot store.
///
/// Stores snapshots under a root directory with structure:
///
/// ```text
/// {root}/
///   {snapshot_id}/
///     snapshot.json       # full ChainSnapshot (compact JSON)
///     genesis.json        # standalone genesis for debugging
///     metadata.json       # human-readable metadata
/// ```
pub struct LocalFsStore {
    root: PathBuf,
}

impl LocalFsStore {
    /// Create a store rooted at `root`.
    /// The directory is created if it doesn't exist.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
        }
    }

    fn snapshot_dir(&self, snapshot_id: &str) -> PathBuf {
        self.root.join(snapshot_id)
    }

    fn file_path(&self, snapshot_id: &str, filename: &str) -> PathBuf {
        self.snapshot_dir(snapshot_id).join(filename)
    }
}

#[async_trait]
impl SnapshotStore for LocalFsStore {
    async fn save(&self, snapshot: &ChainSnapshot) -> Result<String, anyhow::Error> {
        let snapshot_id = snapshot.metadata.snapshot_id();
        let dir = self.snapshot_dir(&snapshot_id);

        tokio::fs::create_dir_all(&dir).await?;

        // Primary artifact: full snapshot
        let json = snapshot.to_json();
        tokio::fs::write(self.file_path(&snapshot_id, "snapshot.json"), &json).await?;

        // Standalone genesis for debugging
        let genesis_bytes =
            serde_json::to_vec_pretty(&snapshot.genesis_json)
                .map_err(|e| anyhow::anyhow!("Failed to serialize genesis: {e}"))?;
        tokio::fs::write(self.file_path(&snapshot_id, "genesis.json"), &genesis_bytes).await?;

        // Human-readable metadata
        let meta = serde_json::json!({
            "snapshot_id": snapshot_id,
            "chain_id": snapshot.metadata.chain_id,
            "height": snapshot.metadata.height,
            "created_at": snapshot.metadata.created_at,
            "genesis_hash": snapshot.metadata.genesis_hash,
            "binary_version": snapshot.metadata.binary_version,
            "validator_count": snapshot.metadata.validator_count,
            "contract_count": snapshot.contracts.len(),
        });
        let meta_bytes =
            serde_json::to_vec_pretty(&meta)
                .map_err(|e| anyhow::anyhow!("Failed to serialize metadata: {e}"))?;
        tokio::fs::write(self.file_path(&snapshot_id, "metadata.json"), &meta_bytes).await?;

        Ok(snapshot_id)
    }

    async fn load(&self, snapshot_id: &str) -> Result<ChainSnapshot, anyhow::Error> {
        let path = self.file_path(snapshot_id, "snapshot.json");
        let json = tokio::fs::read_to_string(&path).await
            .map_err(|e| anyhow::anyhow!("Failed to read snapshot {snapshot_id}: {e}"))?;
        ChainSnapshot::from_json(&json)
            .map_err(|e| anyhow::anyhow!("Failed to parse snapshot {snapshot_id}: {e}"))
    }

    async fn exists(&self, snapshot_id: &str) -> bool {
        tokio::fs::try_exists(self.file_path(snapshot_id, "snapshot.json"))
            .await
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quickspawn::ChainSnapshot;

    #[tokio::test]
    async fn test_local_fs_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalFsStore::new(dir.path());

        let genesis = serde_json::json!({"chain_id": "store-test", "app_state": {"foo": "bar"}});
        let mut snap = ChainSnapshot::new("store-test", 55, "v1.0.0", 3, 0, genesis);
        snap.contracts.push(crate::quickspawn::DeployedContract {
            name: "token".to_string(),
            code_id: 1,
            address: "terp1token".to_string(),
            salt: None,
        });

        let id = store.save(&snap).await.unwrap();
        assert!(id.starts_with("store-test-h55-"));

        assert!(store.exists(&id).await);

        let loaded = store.load(&id).await.unwrap();
        assert_eq!(loaded.metadata.height, 55);
        assert_eq!(loaded.contracts.len(), 1);
        assert_eq!(loaded.contracts[0].name, "token");
        assert_eq!(
            loaded.genesis_json["app_state"]["foo"],
            serde_json::json!("bar")
        );

        // Verify auxiliary files exist
        let dir = store.snapshot_dir(&id);
        assert!(tokio::fs::try_exists(dir.join("genesis.json")).await.unwrap());
        assert!(tokio::fs::try_exists(dir.join("metadata.json")).await.unwrap());
    }

    #[tokio::test]
    async fn test_local_fs_not_exists() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalFsStore::new(dir.path());
        assert!(!store.exists("nonexistent").await);
    }
}