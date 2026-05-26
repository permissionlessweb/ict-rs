# Multi-Node + Multi-Chain QuickSpawn Extension Plan

## Premise

CosmosChain already supports N validators + M full nodes with full genesis pipeline,
genesis distribution, peer configuration, and sidecar management. The QuickSpawn
layer just doesn't expose it. This plan closes that gap.

Current code evidence:

- `CosmosChain::new(cfg, num_validators, num_full_nodes, runtime)` — cosmos.rs:40-60
- `CosmosChain.create_nodes()` — creates N ChainNode objects at cosmos.rs:100-153
- `CosmosChain.genesis_pipeline()` — runs gentx on each validator, collects on primary
- `CosmosChain.distribute_genesis()` — copies genesis from primary to all other nodes
- `CosmosChain.configure_peers()` — collects node IDs, sets persistent_peers on all
- `CosmosChain.start()` — orchestrates all of the above
- `ValidatorEntry` — already has `index` field, Vec<ValidatorEntry> in ChainSnapshot
- `ChainNode` — has `is_validator`, `full_nodes: Vec<ChainNode>` in CosmosChain

## Step 1 — Extend `ChainSnapshot` for multi-node

Add to `ChainSnapshot`:

```rust
pub struct ChainSnapshot {
    pub metadata: SnapshotMetadata,
    pub genesis_json: serde_json::Value,
    pub validators: Vec<ValidatorEntry>,       // already exists, currently 1 entry
    pub full_nodes: Vec<ValidatorEntry>,       // NEW — full nodes have node_key only
    pub faucet: Option<KeyEntry>,
    pub contracts: Vec<DeployedContract>,
}
```

`ValidatorEntry` already handles full nodes fine — we'll just leave `validator_key_json`
empty and `wallet_mnemonic` None for full nodes. The `index` field identifies position.

Add to `SnapshotMetadata`:

```rust
pub struct SnapshotMetadata {
    // ...existing fields...
    pub num_full_nodes: usize,  // NEW
}
```

No changes needed to `SnapshotStore` trait — it's already generic over `&ChainSnapshot`.
The serialized JSON just gets bigger.

## Step 2 — Extend `bootstrap()` for multi-node

Current: manager.rs:122-127 hardcodes `1` validator, `0` full nodes.

Change signature:

```rust
pub async fn bootstrap(
    &self,
    chain_cfg: ChainConfig,
    genesis_wallets: &[WalletAmount],
    binary_version: &str,
    bootstrap_contracts: Vec<DeployedContract>,
    with_snapshot_height: Option<u64>,
    num_validators: usize,     // NEW — default 1
    num_full_nodes: usize,     // NEW — default 0
) -> Result<(ChainSnapshot, String)>
```

Change CosmosChain creation from:

```rust
let mut chain = CosmosChain::new(chain_cfg, 1, 0, runtime.clone());
```

To:

```rust
let mut chain = CosmosChain::new(chain_cfg, num_validators, num_full_nodes, runtime.clone());
```

Change `collect_validator_keys` from a single call to a loop:

```rust
// After genesis pipeline, collect keys from ALL validators + full nodes
let mut validator_entries = Vec::new();
for (i, v) in chain.validators().iter().enumerate() {
    let entry = Self::collect_validator_keys(v, i).await?;
    validator_entries.push(entry);
}

let mut full_node_entries = Vec::new();
for (i, fn_node) in chain.full_nodes().iter().enumerate() {
    let nk = fn_node.exec_raw(&["cat", &format!("{}/config/node_key.json", fn_node.home_dir)], &[]).await?;
    full_node_entries.push(ValidatorEntry {
        index: i,
        validator_key_json: String::new(), // full nodes have no priv_validator_key
        node_key_json: nk.stdout_str().trim().to_string(),
        wallet_mnemonic: None,
    });
}
```

Update snapshot metadata:

```rust
snapshot.metadata.validator_count = num_validators;
snapshot.metadata.num_full_nodes = num_full_nodes;
```

## Step 3 — New type: `SpawnedChainSet`

Add new type alongside `SpawnedChain`:

```rust
/// A multi-node chain spawned from a snapshot.
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

/// A single node within a spawned chain set.
pub struct SpawnedNode {
    pub node: ChainNode,
    pub host_rpc_port: u16,
    pub host_grpc_port: u16,
    pub host_p2p_port: u16,
}
```

The primary validator (index 0) is accessed via `set.primary()`:

```rust
impl SpawnedChainSet {
    pub fn primary(&self) -> Option<&SpawnedNode> {
        self.validators.first()
    }

    pub fn rpc_url(&self) -> String {
        self.primary().map_or_else(
            || "http://127.0.0.1:26657".to_string(),
            |n| format!("http://127.0.0.1:{}", n.host_rpc_port),
        )
    }

    pub fn grpc_url(&self) -> String {
        self.primary().map_or_else(
            || "localhost:9090".to_string(),
            |n| format!("http://127.0.0.1:{}", n.host_grpc_port),
        )
    }
}
```

## Step 4 — Extend `spawn()` for multi-node

This is the meat. Current spawn() creates 1 container, inits, injects 3 files, starts.
New spawn() iterates over `snapshot.metadata.validator_count + snapshot.metadata.num_full_nodes`,
creating a container for each, then:

1. Create Docker network (reuse existing `runtime.create_network()`)
2. Create N containers (one per validator + full node)
3. Init each container's home dir
4. Inject genesis.json to ALL containers (same genesis for all)
5. Inject each validator's specific priv_validator_key.json + node_key.json
6. Inject each full node's specific node_key.json
7. Configure persistent_peers — this needs node IDs, which means we must either:
   a. Generate them from the injected node_key.json (we know the algorithm)
   b. Or run `comet show-node-id` on each node after inject (simpler)
8. Set config/app.toml overrides (gas prices, API, gRPC enable, block time) — mirroring cosmos.rs:546-590
9. Start chain binary on all nodes
10. Wait for block production from primary

The peer configuration step is the key difference from single-node:

```rust
// After injecting keys and genesis, configure P2P peering
async fn configure_spawned_peers(
    nodes: &[&ChainNode],
) -> Result<()> {
    // Collect node IDs by running `show-node-id` on each node
    let mut peers = Vec::new();
    for node in nodes {
        let output = node.exec_cmd(&["comet", "show-node-id"]).await?;
        let node_id = output.stdout_str().trim().to_string();
        if !node_id.is_empty() {
            peers.push(format!("{}@{}", node_id, node.p2p_address()));
        }
    }
    let peers_str = peers.join(",");

    for node in nodes {
        let config_path = format!("{}/config/config.toml", node.home_dir);
        let cmd = format!(
            "sed -i 's/^persistent_peers = .*/persistent_peers = \"{}\"/' {}",
            peers_str, config_path
        );
        node.exec_raw(&["sh", "-c", &cmd], &[]).await?;
        // ... also apply RPC bind 0.0.0.0, gas prices, gRPC enable, block time
    }
    Ok(())
}
```

This mirrors `cosmos.rs:530-593` (`CosmosChain::configure_peers()`) exactly.

The spawn method signature changes to:

```rust
pub async fn spawn_multi(
    &self,
    snapshot_id: &str,
    chain_cfg: &ChainConfig,
    network_id: &str,
) -> Result<SpawnedChainSet>
```

Keep the old `spawn()` for backward compatibility (single-validator shorthand).

## Step 5 — Stop method for SpawnedChainSet

```rust
impl<S: SnapshotStore> QuickSpawnManager<S> {
    pub async fn stop_set(&self, set: &SpawnedChainSet) -> Result<()> {
        for v in &set.validators {
            self.stop_node(&v.node).await?;
        }
        for fn_node in &set.full_nodes {
            self.stop_node(&fn_node.node).await?;
        }
        self.runtime.remove_network(&set.network_id).await?;
        Ok(())
    }

    async fn stop_node(&self, node: &ChainNode) -> Result<()> {
        if let Some(ref id) = node.container_id {
            let _ = node.runtime.stop_container(id).await;
            let _ = node.runtime.remove_container(id).await;
        }
        Ok(())
    }
}
```

## Step 6 — Multi-chain support (ChainSet)

For IBC and cross-chain testing, add:

```rust
/// A snapshot of multiple chains with full node state.
pub struct ChainSetSnapshot {
    pub chains: Vec<ChainSnapshot>,
    pub metadata: ChainSetMetadata,
}

pub struct ChainSetMetadata {
    pub created_at: i64,
    pub chain_count: usize,
}
```

Add to `QuickSpawnManager`:

```rust
pub async fn bootstrap_all(
    &self,
    chain_configs: Vec<(ChainConfig, Vec<WalletAmount>, usize, usize)>,
    // ^ (config, wallets, num_validators, num_full_nodes) per chain
    binary_version: &str,
    bootstrap_contracts: Vec<Vec<DeployedContract>>,
) -> Result<ChainSetSnapshot>
```

Each chain gets its own Docker network (name derived from chain_id). This matches
how CosmosChain handles isolation already.

And for spawning:

```rust
pub async fn spawn_all(
    &self,
    set_snapshot: &ChainSetSnapshot,
    chain_configs: &[ChainConfig],
) -> Result<Vec<SpawnedChainSet>>
```

Each spawn gets an independent Docker network. The callers then connect them
via a relayer for IBC testing.

## Step 7 — QuickSpawnEnv multi-node variant

Add `QuickSpawnEnvSet` to tests/src/quickspawn_env.rs:

```rust
pub struct QuickSpawnEnvSet {
    /// One Daemon per chain in the snapshot.
    pub daemons: Vec<Daemon>,
    /// Spawned chain sets (one per chain).
    pub spawned_sets: Vec<SpawnedChainSet>,
    pub manager: Arc<QuickSpawnManager<LocalFsStore>>,
}
```

Each `Daemon` connects to the primary node of its respective chain. Tests can
then construct IBC channels between the daemons.

## Risk Assessment

**Snapshot size**: A 4-validator snapshot is ~4x the data. Still trivial — each
validator key is ~1KB of JSON. The bottleneck is genesis.json size, which is the
same regardless of node count. No practical issue.

**Spawn time**: 4 containers instead of 1. Each needs init (~200ms) + inject
(~100ms) + peer config (~100ms) + start (~500ms). Sequential adds ~3.5s per
validator. Parallelize the init + inject phase using tokio::join! to keep total
spawn under 4s for a 4-validator chain.

**Deterministic node IDs**: The node_key.json contains a fixed peer ID. Same
node_key.json in snapshot → same node ID every spawn. So the peer configuration
can be pre-computed and stored in the snapshot, avoiding `comet show-node-id`
calls at spawn time. We capture node IDs during bootstrap and store them.

Add to ValidatorEntry:

```rust
pub struct ValidatorEntry {
    pub index: usize,
    pub validator_key_json: String,
    pub node_key_json: String,
    pub wallet_mnemonic: Option<String>,
    pub node_id: Option<String>,  // NEW — captured during bootstrap, avoids show-node-id at spawn
}
```

## Implementation Order

1. Extend data types (ChainSnapshot, ValidatorEntry, new SpawnedChainSet)
2. Extend bootstrap() to accept num_validators + collect all keys + compute node IDs
3. Implement spawn_multi() with multi-container creation + key injection + peer config
4. Add stop_set() cleanup
5. Add ChainSetSnapshot + bootstrap_all/spawn_all for multi-chain
6. Update QuickSpawnEnv for multi-node/multi-chain
7. Old spawn() delegates to spawn_multi() for 1-validator case (backward compat)

Steps 1-4 cover multi-node. Steps 5-6 cover multi-chain (IBC). Step 7 ensures
existing code doesn't break — the old single-validator API still works.