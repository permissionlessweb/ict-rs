# NostrDocker Harness + NIP-01 WS Client + On-Chain Calendar Record TDD Plan

> For Hermes subagent / ict-rs worker on t_8bc72503. Use TDD skill: tests first, RED-GREEN-REFACTOR. writing-plans + systematic-debug enforced. Pre-edit: QMD + trailmark done (see Obsidian note).

**Goal:** Reusable testing library extension for Docker-managed local Nostr relayer (NIP-01), client WS (tokio-tungstenite), event webhooks that record Nostr (NOSR) events on-chain via ict-rs-cw-orch bridge to DAO calendar module (for Terp/DAO orchestration suites).

**Architecture:**
- Reuse existing `RuntimeBackend` (bollard Docker) + `ContainerOptions` for spinning any Nostr relay container (e.g. "strfry:latest" or "nostr-rs-relay").
- New module: `ict-rs/src/nostr/` or `testing/nostr.rs` with `NostrDocker` struct (lifecycle: new, start, stop, ws_client).
- WS client: minimal NIP-01 over tokio-tungstenite (connect, send_event, subscribe, recv).
- Integration: from test harness, after event, use `daemon_builder_from_chain` (from ict-rs-cw-orch) + cw-orchestrator Daemon to execute dao-calendar::create_event or similar.
- Feature flag: "nostr" in Cargo.toml (optional tokio-tungstenite, serde_json for events).
- Example: `examples/nostr-lifecycle.rs` or `tests/nostr-relay-test.rs` : spin -> WS event -> on-chain record -> cleanup.
- Mirrors: DockerRelayer pattern, TestChain harness, ExampleRelayer mock.

**Tech Stack:**
- Rust, tokio, tokio-tungstenite = "0.20" (with native-tls or rustls)
- serde_json for NIP-01 messages
- Existing: bollard, tracing, async_trait, ict-rs-cw-orch, cw-orch-daemon, dao-contracts calendar types (via cw-orch)
- Docker images: public nostr relay (strfry recommended for simplicity, exposes ws://0.0.0.0:7777)

**Verification:**
- Reproducible local Docker Nostr relay + WS publish + calendar on-chain record (using local ict-rs chain with calendar module deployed).
- All tests pass (mock + docker), no regressions on IBC paths.
- Reusable in terp orchestration suites per [[terp/terp orchestration suites.md]]

## Task 1: Cargo.toml updates + feature (TDD)

**Objective:** Add optional "nostr" feature + tokio-tungstenite dep. Verify build.

**Files:**
- Create/Modify: /Users/returniflost/abstract/ict-rs/ict-rs/Cargo.toml (add to [features], [dependencies.optional])
- Test: /Users/returniflost/abstract/ict-rs/ict-rs/Cargo.toml (cargo check --features nostr)

**Step 1: Write failing test (RED)**
```toml
# In [features]
nostr = ["dep:tokio-tungstenite"]

# In [dependencies]
tokio-tungstenite = { version = "0.20", optional = true, features = ["native-tls"] }
# or rustls for no openssl
```

Run: `cargo check --features nostr` (expect fail on missing dep? No, this is config; the test is cargo check passes after edit? For TDD on config, the "test" is the check command.

Since config, the RED is "cargo check --features nostr fails before dep".

But practically: edit, then verify.

**Step 2: Run to verify failure**
`cargo check -p ict-rs --features nostr` (if dep missing, fails)

**Step 3: Minimal impl (GREEN)**
Add the lines to Cargo.toml under correct sections (copy existing tokio pattern).

**Step 4: Run to verify pass**
`cargo check -p ict-rs --features nostr` && `cargo check -p ict-rs` (default no nostr still works)
`cargo test --features nostr` (ensure no break)

**Step 5: Commit**
git add Cargo.toml ; git commit -m "feat(ict-rs): add optional 'nostr' feature + tokio-tungstenite dep for NIP-01 WS client"

## Task 2: Nostr event types + basic WS client module (TDD)

**Objective:** Define NIP-01 Event struct, minimal client connect/send/recv in new nostr/client.rs or mod.

**Files:**
- Create: /Users/returniflost/abstract/ict-rs/ict-rs/src/nostr/mod.rs
- Create: /Users/returniflost/abstract/ict-rs/ict-rs/src/nostr/client.rs (WS using tungstenite)
- Test: /Users/returniflost/abstract/ict-rs/ict-rs/tests/nostr-client-test.rs (failing first)

**Step 1: Write failing test (RED) - first for connect**
```rust
// tests/nostr-client-test.rs
#[tokio::test]
async fn test_nostr_ws_connect_and_send_event() {
    // will use mock or real after
    let client = NostrClient::connect("ws://localhost:7777").await.unwrap();
    let event = NostrEvent::new(...);
    let ok = client.send_event(event).await.unwrap();
    assert!(ok);
}
```
Expect fail: no NostrClient, no mod.

**Step 2: Run test to verify failure**
`cargo test --features nostr --test nostr-client-test test_nostr_ws_connect_and_send_event -- --nocapture`

**Step 3: Minimal code (GREEN)**
In src/nostr/mod.rs:
```rust
pub mod client;
pub use client::NostrClient;
```
In client.rs:
```rust
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use serde_json::json;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NostrEvent { ... } // id, pubkey, created_at, kind, tags, content, sig

pub struct NostrClient {
    ws: ... , // WebSocketStream
}

impl NostrClient {
    pub async fn connect(url: &str) -> Result<Self> { ... use connect_async ... }
    pub async fn send_event(&mut self, ev: NostrEvent) -> Result<bool> {
        let msg = json!(["EVENT", ev]);
        self.ws.send(Message::Text(msg.to_string())).await?;
        // recv OK
        ...
    }
}
```
Stub the types, minimal to make test pass (hardcode or simple echo for now).

**Step 4: Run to pass**
cargo test ... expect green.

**Step 5: Refactor + commit**

## Task 3: NostrDocker harness struct + Docker lifecycle (TDD, reuse runtime)

**Objective:** NostrDocker { runtime, container_id, ws_url, ... } with start (pull/create/start relay container exposing port), stop, client().

Mirror DockerRelayer but simpler (no commander, fixed image for nostr relay).

**Files:**
- src/nostr/docker.rs or in testing/nostr_docker.rs
- Update src/nostr/mod.rs pub use
- Test: extend tests with docker spin test (gated)

**Step 1: Failing test for spin-up**
```rust
#[tokio::test]
#[cfg(feature = "nostr")]
async fn test_nostr_docker_lifecycle() {
    let harness = NostrDocker::new("test-nostr", /*runtime*/).await?;
    harness.start().await?;
    let client = harness.client().await?;
    // send test event
    harness.stop().await?;
}
```

**Step 2: Run fail**

**Step 3: Impl using runtime.create_container with nostr image, port 7777, labels ict.test etc. Start, get host port? (use mapped or fixed), connect WS to ws://127.0.0.1:xxxx**

For port, use PortBinding in options, runtime exposes host port.

**Step 4: Green + refactor**

## Task 4: On-chain calendar record via ict-rs-cw-orch bridge (integration)

**Objective:** From harness, after event, use bridge to get Daemon, instantiate calendar client, execute create_event with nostr data (as external service event).

**Files:**
- In test or example: use ict_rs_cw_orch::daemon_builder_from_chain
- dao calendar types from cw-orch or direct contract call.

**TDD:** failing integration test that assumes running chain + calendar deployed.

**Step 5: Full lifecycle example script**

Create examples/nostr-to-calendar.rs that:
- uses TestChain or direct runtime
- deploys minimal dao with calendar module (or assume pre-deployed)
- spins NostrDocker
- WS publish sample event (kind 1 or calendar specific)
- records via bridge to on-chain
- asserts queryable in calendar

Run as `cargo run --example nostr-to-calendar --features "nostr,testing,docker"`

## Verification Commands
- `cargo test --features "nostr,testing,docker" --test nostr-* -q`
- Full: spin local chain with dao-contracts calendar, run example, verify on-chain event record.
- Cleanup always via harness Drop.

## Risks / Pitfalls
- Docker image for nostr relay: choose one with no auth, ws only, easy config (strfry good).
- Port mapping: runtime may need host port discovery (inspect or fixed 0 for random?).
- NIP-01 exact JSON: follow spec strictly (no extra fields).
- Feature isolation: nostr code behind cfg(feature="nostr"), no impact on IBC.
- Mock runtime for unit tests (extend MockRuntime for nostr?).

**References:** [[nostr-integrations]], [[COSMWASM/Cw-Calendar.md]], ict-rs Obsidian note, relayer/docker patterns from QMD.

This plan makes implementation obvious. Execute task-by-task with TDD.
