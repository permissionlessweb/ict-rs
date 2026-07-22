# ict-rs — Interchain Test for Rust

A Rust re-implementation of [Interchain Test](https://github.com/strangelove-ventures/interchaintest)
for spinning up Cosmos chain environments in tests and CI.

## Modules

| Module | Feature | Purpose |
|---|---|---|
| `chain` | — | Chain abstraction: create, start, stop, query Cosmos nodes |
| `chain::zakura` | `zakura` | Local Zakura (Zcash regtest) node spawn — Private Bridge D6 / Option D |
| `cosmos` | — | Node, tx, IBC, governance, genesis, modules, cosmwasm |
| `runtime` | `docker` | Pluggable backend: Docker (default) or Kuasar sandboxes |
| `quickspawn` | `docker` | Snapshot-based chain bootstrap + ~2s replay |
| `nostr` | `nostr` | Local Nostr relay server lifecycle (Docker sidecar) |
| `relayer` | — | IBC relayer management (Hermes, CosmosRly) |
| `sidecar` | — | Generic sidecar process lifecycle for auxiliary services |
| `wallet` | — | Key management, mnemonic derivation, BIP32 |
| `auth` | — | Pluggable signing backends (keyring, KMS, Ledger) |
| `spec` | — | ChainSpec for network topology and genesis configuration |

## Feature flags

| Feature | Default | Enables |
|---|---|---|
| `full` | yes | docker + ethereum + testing + terp |
| `docker` | via full | Bollard Docker backend |
| `zakura` | no | Local Zakura regtest spawn (`spawn_zakura_local`); implies `docker` |
| `nostr` | no | WebSocket nostr relay sidecar (tokio-tungstenite) |
| `testing` | via full | Test helpers, mock runtime |
| `terp` | via full | Terp-specific chain modules (tokenfactory, feeshare, etc.) |

### Zakura local (`feature = "zakura"`)

Hardcoded corridor defaults (RPC **:18232**, miner dest, `terp-dest-binding-v0` seal).

```rust
// Cargo.toml: ict-rs = { features = ["zakura"] }  // pulls docker
use ict_rs::prelude::*;
use std::sync::Arc;

async fn bring_up_zakura(rt: Arc<dyn IctRuntime /* RuntimeBackend */>) {
    // export ZAKURAD_BIN=/path/to/linux/zakurad
    let cfg = ZakuraNodeConfig::from_env().expect("ZAKURAD_BIN");
    let mut node = spawn_zakura_local(rt, cfg).await.expect("spawn");
    assert!(node.rpc_ready().await);
    // node.owner_binding_hex() == REGTEST_MINER_OWNER_BINDING_HEX for default dest
    node.teardown().await.ok();
}
```

Regtest only — not mainnet ZEC.

```toml
# Minimal test environment (no Ethereum, no Terp)
ict-rs = { features = ["docker", "nostr"] }
```

## QuickStart: classic Interchain

```ignore
use ict_rs::prelude::*;

#[tokio::test]
async fn test_ibc_transfer() {
    let ic = Interchain::new(IctRuntime::Docker(Default::default()))
        .unwrap()
        .add_chain(cosmos_chain)
        .add_relayer("hermes", hermes)
        .add_link(InterchainLink {
            chain1: "chain-a".into(),
            chain2: "chain-b".into(),
            relayer: "hermes".into(),
            path: "transfer".into(),
        });

    ic.build(InterchainBuildOptions {
        test_name: "test_ibc_transfer".into(),
        skip_path_creation: false,
    }).await.unwrap();

    // ... test logic ...

    ic.close().await.unwrap();
}
```

---

## QuickSpawn — snapshot-based chain environments

QuickSpawn solves the slow test-teardown cycle: genesis pipeline + gentx +
validator set formation takes 15–45 seconds per test. Snapshot-based spawn
is ~2 seconds.

### Architecture

```
                bootstrap()                          spawn()
  ┌──────────────────────────┐         ┌──────────────────────────────┐
  │ Cosmos chain (full)      │         │ New container (empty)        │
  │ → terpd init             │         │ → terpd init (new moniker)   │
  │ → genesis + gentx + start│         │ → inject genesis.json        │
  │ → deploy contracts       │         │   (base64, no shell inject)  │
  │ → reach target height    │         │ → inject validator keys      │
  │ → export state           │         │ → start chain daemon         │
  │ → stop & remove chain    │         │ → wait 1st block (~2s)       │
  └──────────┬───────────────┘         └──────────┬───────────────────┘
             │ save                               │ load
             ▼                                    ▼
      SnapshotStore ───────────────────────► SnapshotStore
      (LocalFsStore or future S3)
```

**Data flow:**

`ChainSnapshot` (genesis.json, validator keys, contract metadata)
→ `SnapshotStore::save()` → filesystem directory:
```
{root}/{chain_id}-h{height}-{hash[:8]}/
  snapshot.json    # full ChainSnapshot (compact JSON)
  genesis.json     # standalone genesis for debugging
  metadata.json    # human-readable metadata
```

### Key types

```rust
pub struct ChainSnapshot {
    pub metadata: SnapshotMetadata,    // chain_id, height, genesis_hash, etc.
    pub genesis_json: serde_json::Value,
    pub validators: Vec<ValidatorEntry>, // priv_validator_key + node_key
    pub faucet: Option<KeyEntry>,
    pub contracts: Vec<DeployedContract>,  // code_id + address per contract
}

pub struct SpawnedChain {
    pub node: ChainNode,               // running container handle
    pub host_rpc_port: u16,            // host-mapped RPC
    pub host_grpc_port: u16,           // host-mapped gRPC
    pub snapshot: ChainSnapshot,
}
```

### Bootstrap (one-time)

```ignore
use ict_rs::quickspawn::{QuickSpawnManager, LocalFsStore};
use std::sync::Arc;

let store = Arc::new(LocalFsStore::new("/tmp/snapshots"));
let manager = QuickSpawnManager::new(store);

let (snapshot, id) = manager
    .bootstrap(config, wallets, "v1.0.0", vec![])
    .await?;
// bootstrap container is stopped and removed automatically
```

The bootstrap container is cleaned up after snapshot export. No orphan
containers. If the chain never reaches target height, the bootstrap
fails after `BOOTSTRAP_TIMEOUT_SECS` (90s) with a descriptive error.

### Spawn (repeatable, ~2s)

```ignore
let (spawned, contract_registry) = manager
    .spawn(&snapshot_id, &chain_config, "test-net")
    .await?;

let daemon = cw_orch::daemon::Daemon::builder()
    .chain(spawned.to_chain_info())
    .build()?;
```

The spawn method:
1. Creates a fresh container
2. Runs `terpd init` (checked: failure propagates immediately)
3. Injects genesis.json via base64 (zero shell injection risk)
4. Injects validator priv_validator_key and node_key files
5. Starts the chain binary in background
6. Polls for first block (200ms intervals, 15s timeout)
7. Maps host ports and returns `SpawnedChain`

### Cleanup

Every spawned container must be stopped:

```ignore
manager.stop(&spawned).await?;
```

If you use `QuickSpawnEnv` (see below), the Drop impl calls `stop()`
automatically via `block_on`.

### `SnapshotStore` trait

```ignore
#[async_trait]
pub trait SnapshotStore: Send + Sync {
    async fn save(&self, snapshot: &ChainSnapshot) -> Result<String>;
    async fn load(&self, snapshot_id: &str) -> Result<ChainSnapshot>;
    async fn exists(&self, snapshot_id: &str) -> bool;
}
```

Implementations:
- `LocalFsStore` — filesystem-backed, no external deps (included)
- `S3Store` — planned, reuses existing snapshot distribution infra

### Snapshot IDs

Deterministic: `{chain_id}-h{height}-{genesis_hash[:8]}`

Rebootstrapping the same chain at the same height overwrites the same
snapshot. No deduplication logic needed; it falls out of the hash.

### QuickSpawnEnv — cw-orch test environment wrapper

`QuickSpawnEnv` wraps `QuickSpawnManager + Daemon` and implements
`Environment<Daemon>`, making it a drop-in replacement for any cw-orch
test that needs a `CwEnv`:

```ignore
use ict_rs::quickspawn::LocalFsStore;

let env = QuickSpawnEnv::spawn(&snapshot_id, &config).await?;

// Use as any CwEnv:
let contract = Cw20Contract::new("token", env.environment());
let balance = contract.balance(&addr)?;
```

**Cleanup on drop:**

```ignore
// In #[tokio::test]:
impl Drop for QuickSpawnEnv {
    fn drop(&mut self) {
        // Uses tokio::runtime::Handle::block_on to call
        // manager.stop(&spawned).await — safe inside #[tokio::test].
    }
}
```

Explicit cleanup is also available:

```ignore
env.stop().await; // explicit async teardown
```

---

## Nostr relayer sidecar

The `nostr` module manages a local Nostr relay server as a Docker sidecar.
It integrates with cw-orch for end-to-end testing of chain-to-Nostr events.

```ignore
use ict_rs::nostr::NostrRelayerManager;
use ict_rs::runtime::IctRuntime;

let relayer = NostrRelayerManager::new(
    Arc::new(IctRuntime::Docker(Default::default()).into_backend().await?),
    "nostr-test",
);

relayer.start().await?;
// relay is running at ws://127.0.0.1:{host_port}
relayer.stop().await?;
```

### NostrTestEnv — combined chain + relay test environment

`NostrTestEnv` wraps a `Daemon` + chain container + Nostr relay sidecar +
`ChainEventWatcher`. All three components run independently with separate
lifecycle management.

```ignore
use ict_rs::chain::SidecarConfig;
use ict_rs::nostr::NostrRelayerManager;

let env = NostrTestEnv::start(
    &chain_config,
    SidecarConfig::default(),
    "test-env",
).await?;

let daemon: &Daemon = env.environment();
let client = env.nostr_client().await?;
let watcher = env.event_watcher().await?;

env.stop().await?;
```

Events flow: chain → event_watcher (poll) → nostr client (publish) →
nostr relay (serve) → test asserts (subscribe).

---

## Design principles

1. **Delegation, not reimplementation** — `Environment<Daemon>` delegation
   means all existing cw-orch traits (`TxHandler`, `QueryHandler`,
   `ChainState`, `CwEnv`) work with no new code.

2. **Separate lifecycles** — chain, Nostr relay, and any sidecar have
   independent containers with independent cleanup. `stop()` methods are
   async and explicit. Drop impls are best-effort backups only.

3. **Feature gates match module boundaries** — `quickspawn` is behind
   `#[cfg(feature = "docker")]`; `nostr` is behind `#[cfg(feature = "nostr")]`.
   Building without a feature doesn't include dead Docker/Nostr code.

4. **Error propagation, not defaults** — Host port lookup returns an error
   (not a silent fallback). Genesis serialization uses `.expect()` on a
   known-safe operation. Init failures propagate immediately. No unwrap_or
   on critical paths.

5. **No shell injection** — File injection into containers uses base64
   encoding/decoding, never shell heredocs. The base64 alphabet cannot
   contain quotes, backticks, or heredoc terminators.