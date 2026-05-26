//! QuickSpawn — snapshot-based test environments for Cosmos chains.
//!
//! The QuickSpawn suite captures a bootstrapped chain's minimal state
//! (genesis, validator keys, contract addresses) and stores it for reuse.
//! Test environments can then spawn from snapshot in ~2 seconds instead of
//! running the full genesis pipeline.
//!
//! # QuickStart
//!
//! ```ignore
//! use ict_rs::quickspawn::{QuickSpawnManager, LocalFsStore, ChainSnapshot, SpawnedChainSet};
//! use std::sync::Arc;
//!
//! let store = Arc::new(LocalFsStore::new("/tmp/snapshots"));
//! let manager = QuickSpawnManager::new(store);
//!
//! // Bootstrap (one-time): 1 validator, 0 full nodes
//! let (snap, id) = manager.bootstrap(config, wallets, "v1.0", vec![], None, 1, 0).await.unwrap();
//!
//! // Spawn single-node (backward compatible):
//! let spawned = manager.spawn(&id, &config, "test-net").await.unwrap();
//!
//! // Spawn multi-node (new API):
//! let chain_set = manager.spawn_multi(&id, &config, "test-net").await.unwrap();
//! for val in &chain_set.validators {
//!     println!("val-{}: rpc={}", val.node.index, val.host_rpc_port);
//! }
//! ```

mod snapshot;
mod store;
mod manager;

pub use snapshot::*;
pub use store::*;
pub use manager::*;