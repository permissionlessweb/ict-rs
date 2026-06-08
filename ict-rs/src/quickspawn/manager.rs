// //! QuickSpawnManager — bootstrap and spawn Cosmos chains from snapshot state.
// //!
// //! ## Bootstrap
// //!
// //! Runs the full genesis pipeline once, deploys contracts via cw-orch, captures
// //! the resulting state (genesis.json + validator keys + contract metadata), and
// //! stores it in a [`SnapshotStore`]. This is the expensive one-time setup.
// //!
// //! ## Spawn
// //!
// //! Loads a saved snapshot and creates a chain from injected state — no genesis
// //! pipeline, no gentx, no waiting for validator set formation. Under 2 seconds
// //! from start to first block for a single node; ~4 seconds for a 4-validator set.
// //!
// //! ## Multi-node
// //!
// //! [`QuickSpawnManager::spawn_multi`] creates N containers (one per validator +
// //! full node), injects each node's specific keys, configures P2P peering between
// //! them, and waits for block production from a majority of validators.

// use std::sync::Arc;

// use base64::Engine;
// use tracing::{info, warn};

// use crate::chain::cosmos::CosmosChain;
// use crate::chain::{Chain, ChainConfig, GenesisStyle, TestContext};
// use crate::cosmos::node::ChainNode;
// use crate::error::{IctError, Result};
// use crate::quickspawn::snapshot::{
//     ChainSnapshot, DeployedContract, SpawnedChainSet, SpawnedNode, ValidatorEntry,
// };
// use crate::quickspawn::store::SnapshotStore;
// use crate::runtime::{ContainerOptions, NetworkId, PortBinding, RuntimeBackend};

// use crate::tx::WalletAmount;

// /// Minimum blocks to produce before exporting state during bootstrap.
// const BOOTSTRAP_MIN_HEIGHT: u64 = 3;
// /// Maximum seconds to wait for bootstrap chain to reach target height.
// const BOOTSTRAP_TIMEOUT_SECS: u64 = 90;

// /// Seconds to wait for the chain to produce its first block during spawn.
// const SPAWN_TIMEOUT_SECS: u64 = 15;

// /// Manage the bootstrap ➝ snapshot ➝ spawn lifecycle for a Cosmos chain.
// ///
// /// Generic over storage backend (`S: SnapshotStore`) so tests can use local
// /// filesystem and production deployments can use S3.
// pub struct QuickSpawnManager<S: SnapshotStore> {
//     store: Arc<S>,
// }

// impl<S: SnapshotStore> QuickSpawnManager<S> {
//     /// Create a new manager that stores/loads snapshots via `store`.
//     pub fn new(store: Arc<S>) -> Self {
//         Self { store }
//     }

//     /// Read a node's consensus key and node key from a running container.
//     ///
//     /// For full nodes, `is_validator = false` so `validator_key_json` is left empty.
//     async fn collect_node_keys(
//         node: &ChainNode,
//         index: usize,
//         is_validator: bool,
//     ) -> Result<ValidatorEntry> {
//         let nk_path = format!("{}/config/node_key.json", node.home_dir);

//         // Read node_key.json
//         let nk_out = node.exec_raw(&["cat", &nk_path], &[]).await?;
//         let node_key_json = nk_out.stdout_str().trim().to_string();

//         // Read priv_validator_key.json (validators only)
//         let validator_key_json = if is_validator {
//             let vk_path = format!("{}/config/priv_validator_key.json", node.home_dir);
//             let vk_out = node.exec_raw(&["cat", &vk_path], &[]).await?;
//             vk_out.stdout_str().trim().to_string()
//         } else {
//             String::new()
//         };

//         // Derive the CometBFT node ID. Hard failure: without a node ID the
//         // persistent_peers config will be silently corrupt.
//         let node_id = node.node_id().await.map_err(|e| IctError::Chain {
//             chain_id: node.chain_id.clone(),
//             source: anyhow::anyhow!("failed to get node_id for validator {index}: {e}"),
//         })?;
//         let node_id = Some(node_id);

//         Ok(ValidatorEntry {
//             index,
//             validator_key_json,
//             node_key_json,
//             wallet_mnemonic: None,
//             node_id,
//         })
//     }

//     /// Bootstrap a chain from scratch, capture state, and store the snapshot.
//     ///
//     /// This is the expensive one-time setup. After it completes, use
//     /// [`spawn`](Self::spawn) or [`spawn_multi`](Self::spawn_multi) for fast
//     /// test environments.
//     pub async fn bootstrap(
//         &self,
//         chain_cfg: ChainConfig,
//         genesis_wallets: &[WalletAmount],
//         binary_version: &str,
//         bootstrap_contracts: Vec<DeployedContract>,
//         with_snapshot_height: Option<u64>,
//         num_validators: usize,
//         num_full_nodes: usize,
//     ) -> Result<(ChainSnapshot, String)> {
//         let chain_id = chain_cfg.chain_id.clone();

//         // ── 1. Create and start the chain ──────────────────────────
//         let runtime = crate::runtime::IctRuntime::Docker(Default::default())
//             .into_backend()
//             .await?;

//         let test_ctx = TestContext {
//             test_name: format!("quickspawn-bootstrap-{}", &chain_id),
//             network_id: String::new(),
//         };

//         let mut chain =
//             CosmosChain::new(chain_cfg, num_validators, num_full_nodes, runtime.clone());

//         chain.initialize(&test_ctx).await?;
//         chain.start(genesis_wallets).await?;

//         // ── 2. Wait for blocks ─────────────────────────────────────
//         let target_height = with_snapshot_height.unwrap_or(BOOTSTRAP_MIN_HEIGHT);
//         let primary = chain.primary_node()?;
//         let deadline =
//             std::time::Instant::now() + std::time::Duration::from_secs(BOOTSTRAP_TIMEOUT_SECS);
//         loop {
//             let height = primary.query_height().await.unwrap_or(0);
//             if height >= target_height {
//                 info!(height, "Bootstrap chain reached target height");
//                 break;
//             }
//             if deadline.elapsed().as_secs() > BOOTSTRAP_TIMEOUT_SECS {
//                 return Err(IctError::Chain {
//                     chain_id: chain_id.clone(),
//                     source: anyhow::anyhow!(
//                         "Bootstrap chain failed to reach height {target_height} within {BOOTSTRAP_TIMEOUT_SECS}s"
//                     ),
//                 });
//             }
//             tokio::time::sleep(std::time::Duration::from_millis(200)).await;
//         }

//         // ── 3. Export state ─────────────────────────────────────────
//         let actual_height = primary.query_height().await.unwrap_or(target_height);
//         let genesis_value = primary.read_genesis().await?;

//         // ── 4. Collect all node keys ─────────────────────────────────
//         let mut validator_entries = Vec::new();
//         for (i, v) in chain.validators().iter().enumerate() {
//             let entry = Self::collect_node_keys(v, i, true).await?;
//             validator_entries.push(entry);
//         }

//         let mut full_node_entries = Vec::new();
//         for (i, fn_node) in chain.full_nodes().iter().enumerate() {
//             let entry = Self::collect_node_keys(fn_node, i, false).await?;
//             full_node_entries.push(entry);
//         }

//         // ── 5. Build snapshot ───────────────────────────────────────
//         let validator_count = validator_entries.len();
//         let full_node_count = full_node_entries.len();
//         let mut snapshot = ChainSnapshot::new(
//             &chain_id,
//             actual_height,
//             binary_version,
//             validator_count,
//             full_node_count,
//             genesis_value,
//         );
//         snapshot.validators = validator_entries;
//         snapshot.full_nodes = full_node_entries;
//         snapshot.contracts = bootstrap_contracts;

//         // ── 6. Store snapshot ───────────────────────────────────────
//         let snapshot_id = self
//             .store
//             .save(&snapshot)
//             .await
//             .map_err(|e| IctError::Chain {
//                 chain_id: chain_id.clone(),
//                 source: e.into(),
//             })?;

//         info!(snapshot_id = %snapshot_id, height = actual_height, "Bootstrap complete");

//         // ── 7. Cleanup: stop the bootstrap chain container ────────────
//         if let Err(e) = chain.stop_all_nodes().await {
//             warn!(error = %e, "Failed to stop bootstrap chain (container may leak)");
//         }
//         if let Err(e) = chain.stop_all_sidecars().await {
//             warn!(error = %e, "Failed to stop bootstrap sidecars (container may leak)");
//         }

//         Ok((snapshot, snapshot_id))
//     }

//     // ── Spawn (single-node, backward compatible) ─────────────────────

//     /// Spawn a chain from a previously saved snapshot (single-node).
//     ///
//     /// Creates exactly one container (the primary validator) from the
//     /// snapshot, with no additional nodes. For multi-node chains use
//     /// [`spawn_multi`] directly.
//     pub async fn spawn(
//         &self,
//         snapshot_id: &str,
//         chain_cfg: &ChainConfig,
//         _network_id: &str,
//     ) -> Result<SpawnedChainSet> {
//         let snapshot = self
//             .store
//             .load(snapshot_id)
//             .await
//             .map_err(|e| IctError::Chain {
//                 chain_id: chain_cfg.chain_id.clone(),
//                 source: e.into(),
//             })?;

//         let primary_entry = snapshot.validators.first().ok_or_else(|| IctError::Chain {
//             chain_id: chain_cfg.chain_id.clone(),
//             source: anyhow::anyhow!("snapshot {snapshot_id} has no validators"),
//         })?;

//         // ── 1. Setup runtime + network ────────────────────────────────
//         let runtime: Arc<dyn RuntimeBackend> =
//             crate::runtime::IctRuntime::Docker(Default::default())
//                 .into_backend()
//                 .await?;

//         let network_name = format!(
//             "qs-spawn-{}-{}",
//             snapshot_id.get(..8).unwrap_or("snap"),
//             chain_cfg.chain_id.get(..8).unwrap_or("chain"),
//         );
//         let net_id = runtime.create_network(&network_name).await?;
//         let net_str = net_id.0.clone();
//         let image = &chain_cfg.images[0];

//         let test_name = format!("qs-spawn-{}", snapshot_id.get(..12).unwrap_or("deadbeef"));

//         // ── 2. Create container for the primary validator ─────────────
//         let spawn_node = create_spawned_node(
//             0,
//             true,
//             chain_cfg,
//             &test_name,
//             &net_str,
//             runtime.clone(),
//             image,
//         )
//         .await?;

//         // ── 3. Init home directory ────────────────────────────────────
//         let moniker = format!("qs-val-{}", spawn_node.node.index);
//         let init_cmd: &[&str] = match chain_cfg.genesis_style {
//             GenesisStyle::Modern => &[
//                 &chain_cfg.bin,
//                 "genesis",
//                 "init",
//                 &moniker,
//                 "--home",
//                 &spawn_node.node.home_dir,
//                 "--chain-id",
//                 &chain_cfg.chain_id,
//             ],
//             GenesisStyle::Legacy => &[
//                 &chain_cfg.bin,
//                 "init",
//                 &moniker,
//                 "--home",
//                 &spawn_node.node.home_dir,
//                 "--chain-id",
//                 &chain_cfg.chain_id,
//             ],
//         };
//         let init_output =
//             spawn_node
//                 .node
//                 .exec_raw(init_cmd, &[])
//                 .await
//                 .map_err(|e| IctError::Chain {
//                     chain_id: chain_cfg.chain_id.clone(),
//                     source: anyhow::anyhow!("node {} init failed: {e}", spawn_node.node.hostname),
//                 })?;
//         if init_output.exit_code != 0 {
//             let stderr = String::from_utf8_lossy(&init_output.stderr);
//             return Err(IctError::Chain {
//                 chain_id: chain_cfg.chain_id.clone(),
//                 source: anyhow::anyhow!(
//                     "node {} init exited with code {}: {}",
//                     spawn_node.node.hostname,
//                     init_output.exit_code,
//                     stderr.trim()
//                 ),
//             });
//         }

//         // ── 4. Inject genesis.json ───────────────────────────────────
//         write_node_file(
//             &spawn_node.node,
//             &format!("{}/config/genesis.json", spawn_node.node.home_dir),
//             &serde_json::to_string_pretty(&snapshot.genesis_json).map_err(|e| IctError::Chain {
//                 chain_id: chain_cfg.chain_id.clone(),
//                 source: e.into(),
//             })?,
//         )
//         .await?;

//         // ── 5. Inject keys ───────────────────────────────────────────
//         write_node_file(
//             &spawn_node.node,
//             &format!("{}/config/node_key.json", spawn_node.node.home_dir),
//             &primary_entry.node_key_json,
//         )
//         .await?;

//         if !primary_entry.validator_key_json.is_empty() {
//             write_node_file(
//                 &spawn_node.node,
//                 &format!(
//                     "{}/config/priv_validator_key.json",
//                     spawn_node.node.home_dir
//                 ),
//                 &primary_entry.validator_key_json,
//             )
//             .await?;
//         }

//         // ── 6. Configure config.toml / app.toml ──────────────────────
//         configure_node_config(&spawn_node, "", chain_cfg).await?;

//         // ── 7. Start chain ───────────────────────────────────────────
//         spawn_node.node.exec_start_chain().await?;

//         // ── 8. Wait for block production ─────────────────────────────
//         let start = std::time::Instant::now();
//         loop {
//             tokio::time::sleep(std::time::Duration::from_millis(200)).await;
//             match spawn_node.node.query_height().await {
//                 Ok(h) if h > 0 => {
//                     info!(
//                         height = h,
//                         host_rpc = spawn_node.host_rpc_port,
//                         host_grpc = spawn_node.host_grpc_port,
//                         "Single-node spawn producing blocks"
//                     );
//                     break;
//                 }
//                 _ => {
//                     if start.elapsed().as_secs() > SPAWN_TIMEOUT_SECS {
//                         return Err(IctError::Chain {
//                             chain_id: chain_cfg.chain_id.clone(),
//                             source: anyhow::anyhow!(
//                                 "Single-node spawn failed to produce first block within {SPAWN_TIMEOUT_SECS}s"
//                             ),
//                         });
//                     }
//                 }
//             }
//         }

//         Ok(SpawnedNode {
//             node: spawn_node.node,
//             host_rpc_port: spawn_node.host_rpc_port,
//             host_grpc_port: spawn_node.host_grpc_port,
//             snapshot,
//             host_p2p_port: todo!(),
//         })
//     }

//     // ── Spawn (multi-node) ──────────────────────────────────────────

//     /// Spawn a chain from a saved snapshot with full multi-node topology.
//     ///
//     /// Creates one Docker container per validator + full node, injects each
//     /// node's specific keys from the snapshot, configures P2P peering between
//     /// all nodes, and waits for block production from a majority of validators.
//     pub async fn spawn_multi(
//         &self,
//         snapshot_id: &str,
//         chain_cfg: &ChainConfig,
//         _network_id: &str,
//     ) -> Result<SpawnedChainSet> {
//         // ── 1. Load the snapshot ────────────────────────────────────
//         let snapshot = self
//             .store
//             .load(snapshot_id)
//             .await
//             .map_err(|e| IctError::Chain {
//                 chain_id: chain_cfg.chain_id.clone(),
//                 source: e.into(),
//             })?;

//         let total_node_count = snapshot.validators.len() + snapshot.full_nodes.len();
//         if total_node_count == 0 {
//             return Err(IctError::Chain {
//                 chain_id: chain_cfg.chain_id.clone(),
//                 source: anyhow::anyhow!("snapshot {snapshot_id} has no validators or full nodes"),
//             });
//         }

//         info!(
//             snapshot_id = %snapshot_id,
//             height = snapshot.metadata.height,
//             validators = snapshot.validators.len(),
//             full_nodes = snapshot.full_nodes.len(),
//             "Loaded snapshot, spawning multi-node chain"
//         );

//         let runtime: Arc<dyn RuntimeBackend> =
//             crate::runtime::IctRuntime::Docker(Default::default())
//                 .into_backend()
//                 .await?;

//         // ── 2. Create Docker network ─────────────────────────────────
//         let network_name = format!(
//             "qs-{}-{}",
//             snapshot_id.get(..8).unwrap_or("snap"),
//             chain_cfg.chain_id.get(..8).unwrap_or("chain"),
//         );
//         let net_id = runtime.create_network(&network_name).await?;
//         let net_str = net_id.0.clone();

//         let image = &chain_cfg.images[0];

//         // ── 3. Create containers for all nodes ───────────────────────
//         let test_name = format!(
//             "quickspawn-spawn-{}",
//             snapshot_id.get(..16).unwrap_or("deadbeef")
//         );

//         // Build all node descriptors: (global_index, is_validator, &ValidatorEntry)
//         // global_index is sequential across validators then full_nodes
//         let mut node_descriptors: Vec<(usize, bool, &ValidatorEntry)> = Vec::new();
//         for v in &snapshot.validators {
//             node_descriptors.push((node_descriptors.len(), true, v));
//         }
//         for fn_entry in &snapshot.full_nodes {
//             node_descriptors.push((node_descriptors.len(), false, fn_entry));
//         }

//         let mut spawned_validators: Vec<SpawnedNode> = Vec::new();
//         let mut spawned_full_nodes: Vec<SpawnedNode> = Vec::new();

//         for (_global_idx, is_validator, entry) in &node_descriptors {
//             // Use the index stored in ValidatorEntry for hostname consistency
//             let node = create_spawned_node(
//                 entry.index,
//                 *is_validator,
//                 chain_cfg,
//                 &test_name,
//                 &net_str,
//                 runtime.clone(),
//                 image,
//             )
//             .await;
//             match node {
//                 Ok(n) => {
//                     if *is_validator {
//                         spawned_validators.push(n);
//                     } else {
//                         spawned_full_nodes.push(n);
//                     }
//                 }
//                 Err(e) => {
//                     // Cleanup already-created containers before propagating
//                     let all_created: Vec<&SpawnedNode> = spawned_validators
//                         .iter()
//                         .chain(spawned_full_nodes.iter())
//                         .collect();
//                     for cleanup_node in &all_created {
//                         if let Some(ref cid) = cleanup_node.node.container_id {
//                             let _ = cleanup_node.node.runtime.stop_container(cid).await;
//                             let _ = cleanup_node.node.runtime.remove_container(cid).await;
//                         }
//                     }
//                     let _ = runtime.remove_network(&net_id).await;
//                     return Err(e);
//                 }
//             }
//         }

//         // ── 4. Init home directories ──────────────────────────────
//         let all_nodes: Vec<&SpawnedNode> = spawned_validators
//             .iter()
//             .chain(spawned_full_nodes.iter())
//             .collect();

//         for spawn_node in &all_nodes {
//             let moniker = format!(
//                 "qs-{}-{}",
//                 if spawn_node.node.is_validator {
//                     "val"
//                 } else {
//                     "fn"
//                 },
//                 spawn_node.node.index,
//             );
//             let init_cmd: &[&str] = match chain_cfg.genesis_style {
//                 GenesisStyle::Modern => &[
//                     &chain_cfg.bin,
//                     "genesis",
//                     "init",
//                     &moniker,
//                     "--home",
//                     &spawn_node.node.home_dir,
//                     "--chain-id",
//                     &chain_cfg.chain_id,
//                 ],
//                 GenesisStyle::Legacy => &[
//                     &chain_cfg.bin,
//                     "init",
//                     &moniker,
//                     "--home",
//                     &spawn_node.node.home_dir,
//                     "--chain-id",
//                     &chain_cfg.chain_id,
//                 ],
//             };
//             let init_output =
//                 spawn_node
//                     .node
//                     .exec_raw(init_cmd, &[])
//                     .await
//                     .map_err(|e| IctError::Chain {
//                         chain_id: chain_cfg.chain_id.clone(),
//                         source: anyhow::anyhow!(
//                             "node {} init failed: {e}",
//                             spawn_node.node.hostname
//                         ),
//                     })?;
//             if init_output.exit_code != 0 {
//                 let stderr = String::from_utf8_lossy(&init_output.stderr);
//                 return Err(IctError::Chain {
//                     chain_id: chain_cfg.chain_id.clone(),
//                     source: anyhow::anyhow!(
//                         "node {} init exited with code {}: {}",
//                         spawn_node.node.hostname,
//                         init_output.exit_code,
//                         stderr.trim()
//                     ),
//                 });
//             }
//         }

//         // ── 5. Inject genesis.json to ALL nodes ──────────────────────
//         let genesis_content =
//             serde_json::to_string_pretty(&snapshot.genesis_json).map_err(|e| IctError::Chain {
//                 chain_id: chain_cfg.chain_id.clone(),
//                 source: e.into(),
//             })?;

//         for spawn_node in &all_nodes {
//             write_node_file(
//                 &spawn_node.node,
//                 &format!("{}/config/genesis.json", spawn_node.node.home_dir),
//                 &genesis_content,
//             )
//             .await?;
//         }

//         // ── 6. Inject per-node keys ───────────────────────────────
//         for (_global_idx, is_validator, entry) in &node_descriptors {
//             let spawn_node_idx = entry.index;
//             let spawn_node = if *is_validator {
//                 spawned_validators
//                     .iter()
//                     .find(|n| n.node.index == spawn_node_idx)
//             } else {
//                 spawned_full_nodes
//                     .iter()
//                     .find(|n| n.node.index == spawn_node_idx)
//             }
//             .ok_or_else(|| IctError::Chain {
//                 chain_id: chain_cfg.chain_id.clone(),
//                 source: anyhow::anyhow!("node at index {} not found in spawned set", entry.index),
//             })?;

//             // Inject node_key.json (all nodes have one)
//             write_node_file(
//                 &spawn_node.node,
//                 &format!("{}/config/node_key.json", spawn_node.node.home_dir),
//                 &entry.node_key_json,
//             )
//             .await?;

//             // Inject priv_validator_key.json (validators only)
//             if *is_validator && !entry.validator_key_json.is_empty() {
//                 write_node_file(
//                     &spawn_node.node,
//                     &format!(
//                         "{}/config/priv_validator_key.json",
//                         spawn_node.node.home_dir
//                     ),
//                     &entry.validator_key_json,
//                 )
//                 .await?;
//             }
//         }

//         // ── 7. Build persistent_peers string ─────────────────────────
//         // Node IDs come from the snapshot (captured at bootstrap time).
//         // Using container_name for the host portion because Docker DNS
//         // resolves by container name in user-defined networks.
//         let mut peers: Vec<String> = Vec::new();
//         for spawn_node in &all_nodes {
//             let node_id = spawn_node.node.container_name();
//             // Find the matching entry in the descriptor for the node_id
//             let entry_id = if spawn_node.node.is_validator {
//                 snapshot
//                     .validators
//                     .iter()
//                     .find(|v| v.index == spawn_node.node.index)
//                     .and_then(|v| v.node_id.as_deref())
//             } else {
//                 snapshot
//                     .full_nodes
//                     .iter()
//                     .find(|v| v.index == spawn_node.node.index)
//                     .and_then(|v| v.node_id.as_deref())
//             }
//             .ok_or_else(|| IctError::Chain {
//                 chain_id: chain_cfg.chain_id.clone(),
//                 source: anyhow::anyhow!(
//                     "missing node_id for {} (snapshot did not capture it)",
//                     spawn_node.node.hostname
//                 ),
//             })?;

//             peers.push(format!(
//                 "{}@{}:{}",
//                 entry_id, node_id, spawn_node.node.ports.p2p
//             ));
//         }
//         let peers_str = peers.join(",");
//         info!(peers = %peers_str, "Configured persistent peers for multi-node spawn");

//         // ── 8. Configure config.toml / app.toml ──────────────────────
//         for spawn_node in &all_nodes {
//             configure_node_config(spawn_node, &peers_str, chain_cfg).await?;
//         }

//         // ── 9. Start chain binary on all nodes ──────────────────────
//         for spawn_node in &all_nodes {
//             spawn_node.node.exec_start_chain().await?;
//         }

//         // ── 10. Wait for block production from majority of validators ──
//         let total_validators = spawned_validators.len();
//         // For N validators, need 2f+1 where f = floor((N-1)/3). Since
//         // test networks are typically small (1-4), we use a pragmatic
//         // threshold: ceil(N*2/3). For N=1 this is 1, N=2 is 2, N=3 is 2,
//         // N=4 is 3.
//         let majority_threshold = (total_validators * 2 + 2) / 3;
//         if majority_threshold == 0 {
//             return Err(IctError::Chain {
//                 chain_id: chain_cfg.chain_id.clone(),
//                 source: anyhow::anyhow!("cannot achieve majority with no validators"),
//             });
//         }

//         // Track last-seen height per validator so we can detect stalls.
//         // A validator is "advancing" only if its height strictly increased
//         // since the last poll cycle (not just height > 0 once).
//         let mut last_heights: Vec<u64> = vec![0; spawned_validators.len()];

//         let start = std::time::Instant::now();
//         loop {
//             tokio::time::sleep(std::time::Duration::from_millis(200)).await;

//             let mut advancing = 0usize;
//             for (i, v) in spawned_validators.iter().enumerate() {
//                 if let Ok(h) = v.node.query_height().await {
//                     if h > last_heights[i] {
//                         advancing += 1;
//                         last_heights[i] = h;
//                     }
//                 }
//             }

//             if advancing >= majority_threshold {
//                 info!(
//                     validators_advancing = advancing,
//                     total_validators = total_validators,
//                     total_nodes = all_nodes.len(),
//                     "Multi-node spawn producing blocks (majority increasing)"
//                 );
//                 break;
//             }

//             if start.elapsed().as_secs() > SPAWN_TIMEOUT_SECS {
//                 return Err(IctError::Chain {
//                     chain_id: chain_cfg.chain_id.clone(),
//                     source: anyhow::anyhow!(
//                         "Multi-node spawn: only {advancing}/{total_validators} validators advancing \
//                          after {SPAWN_TIMEOUT_SECS}s (need {majority_threshold})"
//                     ),
//                 });
//             }
//         }

//         Ok(SpawnedChainSet {
//             validators: spawned_validators,
//             full_nodes: spawned_full_nodes,
//             snapshot,
//             network_id: net_str,
//         })
//     }

//     /// Stop and remove a spawned chain node (single-node).
//     pub async fn stop(&self, spawned: &SpawnedChain) -> Result<()> {
//         if let Some(ref id) = spawned.node.container_id {
//             info!(container_id = %id.0, "Stopping spawn node");
//             let _ = spawned
//                 .node
//                 .runtime
//                 .stop_container(id)
//                 .await
//                 .map_err(|e| warn!("Failed to stop spawn node: {e}"));
//             if let Err(e) = spawned.node.runtime.remove_container(id).await {
//                 warn!("Failed to remove spawn node: {e}");
//                 return Err(e.into());
//             }
//         }
//         Ok(())
//     }

//     /// Stop and remove all nodes in a spawned multi-node chain set.
//     ///
//     /// Stops and removes every container, then removes the Docker network.
//     /// Errors are logged but do not short-circuit — all cleanup is attempted.
//     pub async fn stop_set(&self, set: &SpawnedChainSet) -> Result<()> {
//         let all_nodes: Vec<&SpawnedNode> =
//             set.validators.iter().chain(set.full_nodes.iter()).collect();

//         for spawn_node in &all_nodes {
//             if let Some(ref id) = spawn_node.node.container_id {
//                 let _ = spawn_node
//                     .node
//                     .runtime
//                     .stop_container(id)
//                     .await
//                     .map_err(|e| warn!("Failed to stop {}: {e}", spawn_node.node.hostname));
//                 if let Err(e) = spawn_node.node.runtime.remove_container(id).await {
//                     warn!("Failed to remove {}: {e}", spawn_node.node.hostname);
//                 }
//             }
//         }

//         // Remove the Docker network via any node's runtime reference
//         if let Some(first_node) = all_nodes.first() {
//             let net_id = NetworkId(set.network_id.clone());
//             // Use force removal in case container endpoints are still active
//             let _ = first_node
//                 .node
//                 .runtime
//                 .remove_network(&net_id)
//                 .await
//                 .map_err(|e| warn!("Failed to remove network {}: {e}", set.network_id));
//         }

//         Ok(())
//     }
// }

// // ── Internal helpers ──────────────────────────────────────────────────────

// /// Read a file from inside a container via `cat`.
// async fn read_node_file(node: &ChainNode, path: &str) -> Result<String> {
//     let out = node.exec_raw(&["cat", path], &[]).await?;
//     Ok(out.stdout_str().to_string())
// }

// /// Write a file inside a container via base64-encoded STDIN redirect.
// ///
// /// Content goes through base64 encoding (safe against any byte sequence).
// /// The file path is passed as a separate argv element (`$2`), preventing
// /// shell injection from path characters like `;`, `$`, or backticks.
// async fn write_node_file(node: &ChainNode, path: &str, content: &str) -> Result<()> {
//     let b64 = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
//     // The path is argv[5], separate from the shell script (argv[2]).
//     // Double quotes around "$2" prevent word-splitting and glob expansion
//     // even if path contains spaces, semicolons, or metacharacters.
//     node.exec_raw(
//         &[
//             "sh",
//             "-c",
//             "echo \"$1\" | base64 -d > \"$2\"",
//             "--",
//             &b64,
//             path,
//         ],
//         &[],
//     )
//     .await?;
//     Ok(())
// }

// /// Replace a TOML config line by key prefix.
// ///
// /// Finds the line starting with `{key} = ` and replaces the entire value
// /// portion with `"{new_value}"`. If the key is not found, the content is
// /// returned unchanged (caller should ensure the file has the expected format).
// fn replace_config_line(content: &str, key: &str, new_value: &str) -> String {
//     let prefix = format!("{} = ", key);
//     content
//         .lines()
//         .map(|line| {
//             if line.trim_start().starts_with(&prefix) {
//                 format!("{} = \"{}\"", key, new_value)
//             } else {
//                 line.to_string()
//             }
//         })
//         .collect::<Vec<_>>()
//         .join("\n")
// }

// /// Create a single spawned node container for multi-node spawn.
// async fn create_spawned_node(
//     index: usize,
//     is_validator: bool,
//     chain_cfg: &ChainConfig,
//     test_name: &str,
//     network_id: &str,
//     runtime: Arc<dyn RuntimeBackend>,
//     image: &crate::runtime::DockerImage,
// ) -> Result<SpawnedNode> {
//     let mut node = ChainNode::new(
//         index,
//         is_validator,
//         &chain_cfg.chain_id,
//         &chain_cfg.bin,
//         image.clone(),
//         test_name,
//         network_id,
//         runtime.clone(),
//         None, // no faucet
//         chain_cfg.genesis_style,
//         &chain_cfg.gas_prices,
//         chain_cfg.gas_adjustment,
//     );
//     node.home_dir = format!("/home/.{}", chain_cfg.bin);

//     let container_name = node.container_name();
//     let opts = ContainerOptions {
//         image: image.clone(),
//         name: container_name.clone(),
//         network_id: Some(NetworkId(network_id.to_string())),
//         env: vec![],
//         cmd: vec![],
//         entrypoint: None,
//         ports: vec![
//             PortBinding {
//                 host_port: 0,
//                 container_port: 26657,
//                 protocol: "tcp".to_string(),
//             },
//             PortBinding {
//                 host_port: 0,
//                 container_port: 9090,
//                 protocol: "tcp".to_string(),
//             },
//             PortBinding {
//                 host_port: 0,
//                 container_port: 26656,
//                 protocol: "tcp".to_string(),
//             },
//         ],
//         volumes: vec![],
//         labels: vec![("ict-rs.quickspawn".to_string(), "true".to_string())],
//         hostname: Some(node.hostname.clone()),
//     };

//     let container_id = runtime.create_container(&opts).await?;
//     node.container_id = Some(container_id.clone());
//     runtime.start_container(&container_id).await?;

//     let host_rpc_port = runtime
//         .get_host_port(&container_id, 26657, "tcp")
//         .await?
//         .ok_or_else(|| IctError::Chain {
//             chain_id: chain_cfg.chain_id.clone(),
//             source: anyhow::anyhow!("failed to resolve RPC port for {}", node.hostname),
//         })?;
//     let host_grpc_port = runtime
//         .get_host_port(&container_id, 9090, "tcp")
//         .await?
//         .ok_or_else(|| IctError::Chain {
//             chain_id: chain_cfg.chain_id.clone(),
//             source: anyhow::anyhow!("failed to resolve gRPC port for {}", node.hostname),
//         })?;
//     let host_p2p_port = runtime
//         .get_host_port(&container_id, 26656, "tcp")
//         .await?
//         .ok_or_else(|| IctError::Chain {
//             chain_id: chain_cfg.chain_id.clone(),
//             source: anyhow::anyhow!("failed to resolve P2P port for {}", node.hostname),
//         })?;

//     node.host_rpc_port = Some(host_rpc_port);
//     node.host_grpc_port = Some(host_grpc_port);

//     Ok(SpawnedNode {
//         node,
//         host_rpc_port,
//         host_grpc_port,
//         host_p2p_port,
//     })
// }

// /// Replace a key=value pair only within a specific TOML section.
// ///
// /// Reads lines, tracks active section via `[section]` headers, and only
// /// applies the replacement to lines in the specified section. This prevents
// /// accidentally modifying entries in other sections that share the same key.
// fn replace_in_section(content: &str, section: &str, key: &str, new_value: &str) -> String {
//     let section_header = format!("[{}]", section);
//     let mut in_section = false;
//     let mut result = String::new();
//     let prefix = format!("{} = ", key);

//     for line in content.lines() {
//         let trimmed = line.trim();
//         if trimmed.starts_with('[') && trimmed.ends_with(']') {
//             in_section = trimmed == section_header;
//         }

//         if in_section && trimmed.starts_with(&prefix) {
//             // Replace value in this section
//             result.push_str(&format!("{} = \"{}\"\n", key, new_value));
//         } else {
//             result.push_str(line);
//             result.push('\n');
//         }
//     }

//     result
// }

// /// Apply config.toml and app.toml overrides to a spawned node.
// ///
// /// Reads the files from the container, applies Rust-side line replacements,
// /// and writes them back via base64 encoding. No shell interpolation of
// /// user-controlled values.
// async fn configure_node_config(
//     spawn_node: &SpawnedNode,
//     peers_str: &str,
//     chain_cfg: &ChainConfig,
// ) -> Result<()> {
//     let config_path = format!("{}/config/config.toml", spawn_node.node.home_dir);
//     let app_path = format!("{}/config/app.toml", spawn_node.node.home_dir);
//     let node = &spawn_node.node;
//     let mut config = read_node_file(node, &config_path).await?;
//     if !peers_str.is_empty() {
//         config = replace_config_line(&config, "persistent_peers", peers_str);
//     }
//     config = replace_in_section(&config, "rpc", "laddr", "tcp://0.0.0.0:26657");
//     config = replace_config_line(&config, "timeout_commit", &chain_cfg.block_time);
//     config = replace_config_line(&config, "timeout_propose", &chain_cfg.block_time);
//     write_node_file(node, &config_path, &config).await?;

//     // ── app.toml ────────────────────────────────────────────────────
//     let mut app = read_node_file(node, &app_path).await?;

//     // minimum-gas-prices
//     app = replace_config_line(&app, "minimum-gas-prices", &chain_cfg.gas_prices);

//     // Enable gRPC: only replace inside [grpc] section
//     app = replace_in_section(&app, "grpc", "enable", "true");
//     app = replace_in_section(&app, "grpc", "address", "0.0.0.0:9090");

//     write_node_file(node, &app_path, &app).await?;

//     Ok(())
// }
