# Nostr Sidecar + Robust Client + DAO Calendar Integration TDD Plan (Phase 2)

> Background agent for t_8bc72503. Follow test-driven-development + writing-plans. QMD/Trailmark + sidecar/relayer analysis complete (see Obsidian note). Core Rule: no stubs. Validate Docker image first (done: mattn/nostr-relay works on 7777). Update Obsidian + `hermes kanban comment t_8bc72503` after every step.

**Goal:** Local Nostr relay as first-class `SidecarProcess` (Docker), robust `NostrClient` (persistent bidi, proper NIP-01 handling not fragile .next()), integration points for tests: publish via client, record on-chain via ict-rs-cw-orch Daemon + DAO calendar `CreateEvent` (using extension for Nostr payload).

**Architecture (mirrors existing):**
- New: `cosmos/docker_nostr_sidecar.rs` with `nostr_relay_config()` -> `SidecarConfig` (image=mattn/nostr-relay, ports=["7777"], no validator_process, health=None).
- `src/nostr/client.rs` enhanced: `NostrClient` with internal reader task (tokio::spawn), mpsc channels for send/recv, enum `NostrMessage` (Event, Ok, Eose, Notice, Closed, ...), `send_event` returns future or uses oneshot for response match by id.
- Keep optional "nostr" feature.
- Integration: new test/example in tests/ or examples/ that: 1. spins CosmosChain + nostr sidecar (same net), 2. gets ws_url from sidecar, 3. NostrClient::connect, send signed event, 4. daemon_builder_from_chain, deploy/use DaoCalendar via cw-orch, CreateEvent with nostr in extension.
- Mirrors: SidecarProcess (docker_sidecar.rs), DockerRelayer (for pattern), ict-rs-cw-orch bridge.

**Tech Stack:** Existing (bollard via runtime, tokio-tungstenite, serde_json, futures, ict_rs_cw_orch, cw_orch, dao-calendar types via reexport or direct).

**Verification:** 
- `cargo test --features nostr --test nostr-relay-test` spins real Docker Nostr + client + (mock calendar or real if contract avail) passes.
- Reusable in terp suites.
- Clean: real Docker, real WS bidi, no fragile code.

## Task 0 (Pre): Validate Docker Nostr Relay Image (non-TDD, setup)

**Objective:** Confirm mattn/nostr-relay (or chosen) runs clean as sidecar candidate, WS on 7777, minimal/no config.

**Files:** none (terminal validation)

**Step 1:** Run validation (already done in session: pull, run -p 17777:7777, TCP connect ok, logs clean, no extra setup).

**Verify:** docker ps shows it, port mapped, TCP connect succeeds. If fails for image, switch or clarify.

**Commit:** N/A (env validation).

## Task 1: Add NostrRelayConfig helper (TDD)

**Objective:** `nostr_relay_config() -> SidecarConfig` in new or existing docker_sidecar.rs. Usable in ChainConfig.sidecar_configs for tests.

**Files:**
- Modify: /Users/returniflost/abstract/ict-rs/ict-rs/src/cosmos/docker_sidecar.rs (add fn)
- Test: /Users/returniflost/abstract/ict-rs/ict-rs/tests/nostr-sidecar-test.rs (new, or extend)

**RED:** Write test that calls nostr_relay_config(), asserts fields (name="nostr-relay", image repo="mattn/nostr-relay", ports vec!["7777"], validator_process=false, pre_start=false, health=None).

Run: cargo test --features nostr ... expect fail (fn not exist).

**GREEN:** Implement minimal fn returning SidecarConfig { name: "nostr-relay".into(), image: DockerImage { repository: "mattn/nostr-relay".into(), version: "latest".into(), uid_gid: None }, home_dir: "/data".into(), ports: vec!["7777".into()], env: vec![], cmd: vec![], pre_start: false, validator_process: false, health_endpoint: None, ready_timeout_secs: 10, }.

**Verify pass:** test passes, cargo check --features nostr.

**REFACTOR:** None.

**Commit:** git add ... ; git commit -m "feat(ict-rs): add nostr_relay_config SidecarConfig helper mirroring hash-market"

## Task 2: Robust NostrClient (TDD)

**Objective:** Refactor client to handle bidi properly: spawn reader, use channels/oneshot for responses, parse full NIP-01 msgs, support send + recv without race.

**Files:**
- Modify: /Users/returniflost/abstract/ict-rs/ict-rs/src/nostr/client.rs
- Update test: tests/nostr-client-test.rs (add robust test cases)

**RED:** Update test to use new API e.g. `let (ok, rx) = client.send_event_and_wait(event).await? ; assert!(ok);` or similar, run expect compile fail or old fragile.

**GREEN:** 
- Add `use tokio::sync::{mpsc, oneshot};`
- Struct NostrClient { ws: ..., tx: mpsc::Sender<...>, ... }
- In connect: split stream, spawn reader task that loops .next(), parses to NostrMessage enum, routes via channels.
- send_event: send json, create oneshot, wait with timeout, match id.
- Handle errors, close, etc. Keep simple.

**Verify:** cargo test --features nostr (new tests for send/recv, sub if added) pass with real or mock.

**REFACTOR:** Extract msg types if needed.

**Commit:** ...

## Task 3: Integration Test with ict-rs-cw-orch + DAO Calendar (TDD)

**Objective:** End-to-end: chain + nostr sidecar + publish event + record via calendar using bridge.

**Files:**
- New: tests/nostr-calendar-integration-test.rs (#[ignore] if needs full setup)
- Possibly extend ict-rs-cw-orch if needed for calendar helper (but minimal, use direct in test).

**RED:** Test skeleton calling chain with nostr sidecar, client publish, then daemon create_event with nostr extension. Expect fail (no impl yet).

**GREEN:** Wire the pieces using existing start_sidecars, NostrClient, daemon_builder_from_chain, then use DaoCalendar interface or raw execute. Use extension: Some(serde_json::to_value(nostr_event).unwrap()) or define simple NostrMetadata.

**Verify pass:** (with --ignored if Docker heavy) the flow succeeds, event recorded (query calendar).

**REFACTOR:** Keep minimal.

**Commit.**

## Task 4: Docs + Example + Cleanup

**Objective:** Update README/examples, add usage in interchain or testing, finalize.

**Files:** README.md, examples/nostr-*.rs if new, docs, Cargo.toml if needed.

Then full test, commit, update note/kanban.

**End:** Working, unified, no stubs, reusable for DAO calendar Nostr flows.

**Assumptions (best judgement after clarify timeout):** 
- Image: mattn/nostr-relay:latest (validated TCP/WS port).
- Calendar record: CreateEvent { ..., extension: Some( NostrPayload { event: NostrEvent } ) } or similar; test will demonstrate.

If wrong, user will correct in review.

**Bite size:** 2-5 min per step. TDD enforced. Update frequently.