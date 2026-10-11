# Zakura × Crosslink implementation loop

SSOT for `/workflow zakura-crosslink`. One card per run. Proof is a fail-closed **ict-rs** Docker test **on groot-wan**, not a README sentence and not laptop `cargo test`.

## Source of truth: groot-wan (non-negotiable)

**Every agent, every phase, every craft:** `ssh groot-wan`. Hostname must be `groot.local`. The Grok session laptop (`returniflost`) is a different tree. A laptop `crates/zakura` clone is leftover and is **not** the fork.

| MUST | MUST NOT |
|------|----------|
| `ssh groot-wan` before claiming disk/git/Docker/tests | Treat laptop `read_file` / `cargo test` as proof |
| Edit only under `/Users/returniflost/abstract/terp-core/crates` **on groot** | Clone, rsync, or pull `crates/zakura` onto the laptop |
| Quote `ssh` stdout for `fork_present` / `lab_ok` / `criteria_met` | Mark `done-lab` because a laptop test is green |
| Set `PATH` to include Docker.app `Resources/bin` | Conclude “no docker” from a default PATH |

| What | Where |
|------|--------|
| Host | `ssh groot-wan` (required, not optional) |
| Tree | `/Users/returniflost/abstract/terp-core/crates` |
| PATH | `/usr/local/bin:/Applications/Docker.app/Contents/Resources/bin:$HOME/.cargo/bin:$PATH` |
| Docker | Docker Desktop on groot. Missing `docker` on a default PATH is **not** “no docker”. |
| `zakurad` | `~/abstract/terp-core/crates/zakura/target/zakura-regtest-e2e-linux/zakurad` or `ZAKURAD_BIN` |
| Clone / craft | On groot only. Do **not** clone, rsync, or pull `crates/zakura` onto the Grok session laptop. |
| Proof | Quoted `ssh groot-wan` command output. Laptop `read_file` is a hint only. |

`fork_present`, `lab_ok`, and `criteria_met` are false unless the agent quotes groot output. Do not mark `done-lab` because a laptop test is green.

```bash
ssh groot-wan 'export PATH="/usr/local/bin:/Applications/Docker.app/Contents/Resources/bin:$HOME/.cargo/bin:$PATH"
test -f ~/abstract/terp-core/crates/zakura/Cargo.toml && echo FORK_YES || echo FORK_NO
curl -sS -X POST -H "content-type: application/json" \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getblockchaininfo\",\"params\":[]}" \
  http://127.0.0.1:18232 || true
docker ps --filter name=zakura --format "{{.Names}} {{.Status}}"'
```

## Goal

On **`ssh groot-wan` only**, fork [zakura-core/zakura](https://github.com/zakura-core/zakura) (Zebra-derived Zcash node) at `crates/zakura` and port the [zebra-crosslink](https://github.com/ShieldedLabs/zebra-crosslink) design: hybrid PoW + BFT finality gadget (TFL / Crosslink 2), so a live `zakurad` can report **crosslink-finalized** heights. Verify that with ict-rs (`feature = "zakura"`) spawning Linux `zakurad` in `debian:trixie-slim`.

This is a **lab prototype**, not mainnet Crosslink and not the CosmWasm `cw-ics08-wasm-crosslink` light client.

## Upstreams (open source)

| Tree | URL | Role |
|------|-----|------|
| Zakura | https://github.com/zakura-core/zakura | Fork base. Crates are `zakura-*` / `zakurad` (Zebra rename). Pin a release tag (prefer `v1.2.0` or later) in `crates/zakura/ORIGIN.md`. |
| zebra-crosslink | https://github.com/ShieldedLabs/zebra-crosslink | Design + prototype. Book: https://shieldedlabs.github.io/zebra-crosslink/ |
| Crosslink 2 | https://electric-coin-company.github.io/tfl-book/design/crosslink.html | Abstract construction zebra-crosslink adapts |
| ict-rs Zakura spawn | `crates/ict-rs/ict-rs/src/chain/zakura.rs` | Docker bind-mount of host Linux `zakurad`. RPC **18232**. Feature `zakura`. |

Local reference trees (if present): `crates/zakura`, `crates/zebra-crosslink`, `crates/zcl`. Do not invent them.

## Modes

| Mode | What it does |
|------|----------------|
| `observe` | Read SSOT + disk + lab. Critique honesty. **No product edits.** |
| `plan` | Critique + ranked file-level actions only. |
| `craft` | **One card.** Tester writes failing Pass tests first. Implementer makes them green. Skeptic fail-closes theater. |

Default card = first row whose Status is `doing`, else `CX0`. Never implement two cards in one run.

## Status

Update only when the Pass is a real test (or CX0: real fork + ORIGIN pin).

| Card | Status | Pass |
|------|--------|------|
| CX0 | `doing` | Fork + ORIGIN.md (v1.2.0) exist on groot and GitHub `permissionlessweb/zakura` (`cx0-stock-fork`). Remaining: compile `zakurad` (not cargo metadata); `ZAKURA_CROSSLINK_LAB=1` spawn must not skip. |
| CX1 | `todo` | Types live under the fork (`zakura-crosslink` and/or `zakura-state::crosslink`): `BftBlock`, fat pointer, `TFLBlockFinality`, roster/validator. Round-trip `ZcashSerialize`/`ZcashDeserialize`. A test fails if bytes diverge from a pinned zebra-crosslink fixture (or a checked-in vector). |
| CX2 | `todo` | TFL service wired into `zakurad` behind feature `crosslink`. Activation height configurable. Feature **off** = stock PoW path (existing PIR/regtest corridor still works). |
| CX3 | `todo` | Service/RPC: `IsTFLActivated`, `FinalBlockHeightHash`, `BlockFinalityStatus`, `FatPointerToBFTChainTip`. Missing gadget ⇒ error, not a fake finalized tip. |
| CX4 | `todo` | ict-rs Docker e2e: spawn fork `zakurad` with `crosslink`. After enough PoW + BFT, `FinalBlockHeightHash` is some height `F` and tip `T` with `F <= T`, and a block at `F` reports finalized. `#[ignore]` / skip-print is not a pass. |
| CX5 | `todo` | Tamper: invalid fat pointer or bad signatures rejected. ict-rs or in-crate test fail-closed (assert, not log). |
| CX6 | `todo` | Honesty: docs cannot claim mainnet Crosslink, IBC Crosslink LC, or Orchard-note finality from this fork unless those tests exist. Byte-compat vs `cw-ics08-wasm-crosslink` is documented as residual unless a vector test exists. |
| CX7 | `todo` | terp-rs `zakura_crosslink_live_lc` on **groot**: ict-rs spawns Zakura node(s); `CrosslinkHeader` comes from node TFL RPC (`get_tfl_fat_pointer_to_bft_chain_tip` / `get_tfl_final_block_height_and_hash`), not `SigningKey::from([i;32])`. That header updates `cw-ics08-wasm-crosslink`. `ZAKURA_CROSSLINK_LAB=1` fail-closes. Synthetic ed25519 cases stay for state-machine only. |
| CX8 | `doing` | **Tenderlink required.** `zakura-crosslink` depends on `tenderlink` (`git.sr.ht/~azmr/stuff` rev `0e88509`, same as zebra-crosslink). `[crosslink] listen_address` + `malachite_peers` starts `tenderlink::entry_point`. Wire BFT blocks use full PoW `Header` serialize. Single-node auto-sign only when no peers. |
| CX9 | `todo` | Join current Shielded Labs Crosslink prototype testnet from groot: testnet toml (P2P + tenderlink peers), `zakurad --features crosslink`, quoted `getpeerinfo` / `get_tfl_is_activated` / `get_tfl_fat_pointer_to_bft_chain_tip`. Workflow: `zakura-crosslink-testnet`. |

## Crate map (Zakura ↔ Zebra ↔ Crosslink)

Zakura is already a Zebra fork with renamed crates. Port Crosslink onto **those names**, do not reintroduce `zebra-*` inside the Zakura tree.

| Zakura crate | Zebra / zebra-crosslink analogue | Crosslink cut |
|--------------|----------------------------------|---------------|
| `zakura-chain` | `zebra-chain` | Headers in `BftBlock`; staking-action tx variant if present upstream |
| `zakura-state` | `zebra-state` | `crosslink` module: finalize-block request, fat pointer, finality status |
| `zakura-consensus` | `zebra-consensus` | Activation / contextual checks; feature-gated |
| `zakura-rpc` | `zebra-rpc` | Finality + roster RPCs |
| `zakura-network` | `zebra-network` | Optional BFT/tenderlink listen; keep legacy P2P default |
| `zakurad` | `zebrad` | Wire TFL service like zebra-crosslink `zebra-crosslink` crate |
| **new** `zakura-crosslink` | `zebra-crosslink` crate | TFL main loop, propose/validate/decide BFT block, roster updates |

zebra-crosslink internals to port (read before writing):

- `zebra-crosslink/src/lib.rs` — TFL service, activation height, propose/validate/decide
- `zebra-crosslink/src/service.rs` — request/response API
- `zebra-crosslink/src/chain.rs` — chain helpers
- `zebra-state` `crosslink` module — `BftBlock`, `FatPointerToBftBlock*`, `TFLBlockFinality`, `CrosslinkFinalizeBlock`
- Book: Design in a Nutshell, Five Component Model, CL2 construction

Keep **PoW** as the liveness/censorship path. Finalizers only add **height-eventual finality**. Do not replace Equihash.

## ict-rs e2e contract

Existing spawn (`feature = "zakura"`):

- Host Linux `zakurad` via `ZAKURAD_BIN` bind-mounted into `debian:trixie-slim`
- Config: `corridor_node_toml()` — `network = "Regtest"`, RPC `0.0.0.0:18232`, ephemeral state
- Ready: JSON-RPC `getblockchaininfo` contains `"result"`
- macOS Mach-O cannot run in Debian — on groot, use the Linux `zakurad` cache (or build there). Never bind-mount a laptop Mach-O into the Debian test image.

Crosslink extension (CX4+):

1. Feature `zakura` stays. Add optional config: `crosslink: bool`, listen/public addrs, activation height (regtest: low, e.g. 5–20).
2. Generated TOML grows a `[crosslink]` (or Zakura-native) section **only** when enabled. Stock corridor TOML must stay valid for PIR (`private-inference-rent`).
3. New test file: `crates/ict-rs/ict-rs/tests/zakura_crosslink_e2e.rs`
   - `zakura_crosslink_rpc_ready` — spawn + `getblockchaininfo`
   - `zakura_crosslink_reports_finality` — after mine/wait, finality RPC/service is not `NotImplemented` and `F <= T`
   - `zakura_crosslink_rejects_bad_fat_pointer` — CX5
4. Gate live Docker with `ZAKURAD_BIN` + Docker **on groot**. Missing bin ⇒ **hard fail** when `ZAKURA_CROSSLINK_LAB=1`; otherwise `return` with a clear skip is allowed only on CX0/CX1 unit tests, never on CX4/CX5 when that env is set.
5. Lab evidence is always `ssh groot-wan` (not the session laptop):

```bash
ssh groot-wan 'export PATH="/usr/local/bin:/Applications/Docker.app/Contents/Resources/bin:$HOME/.cargo/bin:$PATH"
export ZAKURAD_BIN=$HOME/abstract/terp-core/crates/zakura/target/zakura-regtest-e2e-linux/zakurad
export ZAKURA_CROSSLINK_LAB=1
cd $HOME/abstract/terp-core/crates/ict-rs/ict-rs
cargo test --features zakura --test zakura_crosslink_e2e -- --nocapture'
```

Do not mark a card `done-lab` from a laptop `cargo test`.

## Design invariants (script + agents)

1. **Skip is not verification.** `ZAKURA_CROSSLINK_LAB=1` fail-closes CX4/CX5.
2. **Feature-off is stock Zakura.** PIR corridor (`18232`, miner dest, dest-binding) must keep working.
3. **Byte-compat is a test**, not a comment. Port `ZcashSerialize` types; do not invent a second encoding and call it zebra-crosslink.
4. **TFL is a gadget.** Do not claim IBC, 08-wasm, ZAP1 inclusion, or Orchard note opening from this work.
5. **Prototype honesty.** zebra-crosslink is an early prototype (in-house BFT / tenderlink, activation placeholder). Our fork inherits that; do not call it ZIP-complete or mainnet-safe.
6. **One card per craft.** Update Status only when Pass is encoded as a test (CX0: ORIGIN + spawn).
7. **No exploits.** Tests reject bad signatures; do not ship attack payloads.

## Non-goals (this loop)

- Mainnet activation / ZIP ratification
- Casual-user staking UX / mobile wallets
- IBC or CCIP
- Replacing PIR ZAP1 / `cw-ics08-wasm-zap1`
- Compiling Zakura inside the Debian test image (always bind-mount a host Linux binary)

## Craft sequence

1. On **groot-wan**, write or adjust the Pass test so it **fails** on the groot tree.
2. On **groot-wan**, implement the smallest cut that greens that test.
3. On **groot-wan**, run ict-rs (Docker e2e). Quote the ssh output.
4. Skeptic: `ssh groot-wan` and re-run. `criteria_met=true` only with test name + groot command output. Laptop review is not verification.

## ORIGIN.md template (CX0)

```markdown
# Origin

- upstream: https://github.com/zakura-core/zakura
- tag / commit: <fill>
- date forked: <ISO date>
- license: MIT AND Apache-2.0 (see upstream)
- crosslink source: https://github.com/ShieldedLabs/zebra-crosslink (design + prototype port)
- status: stock Zakura until CX1+
```
