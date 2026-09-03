# Pair C — sudo ante + valset cutover (ict-rs)

Implementers own chain behavior. Testers ship only `examples/lean_waist.rs` and
this handshake. **No `v6.0.0-dev` edits from testers.**

## Where the implementation lives

Pair C landed in a **different tree**, not in `.worktrees/lean-consensus` yet:

| | |
| --- | --- |
| Tree | `/Users/returniflost/terp-core-lean-consensus` |
| Branch | `feat/lean-consensus` @ `c51dbee` |
| Note | **Not merged** into this repo’s `.worktrees/lean-consensus`. ICT images built from the unmerged worktree (or after merge) are what these tests run against. Document expected merge; do not assume `v6.0.0-dev` has ante/flag. |

Reference: `/tmp/grok-lean-c-impl.md`. Unit tests: `go test ./x/leanval/...`.

## Flag (`leanval_owns_valset`)

| | |
| --- | --- |
| Name | **`leanval_owns_valset`** (`types.FlagOwnsValset`) |
| Default | **OFF** (`Keeper.OwnsValset() == false`) |
| Off | Stock staking `EndBlock` (`ApplyAndReturnValidatorSetUpdates`). `x/leanval` EndBlock emits **no** updates. |
| On | Staking wrap **skips** staking EndBlock. `x/leanval` EndBlock emits pending `[]abci.ValidatorUpdate`. |

Not an `app.toml` knob yet. In-process tests use `SetOwnsValset`. ICT sets
`app_state.leanval.params.leanval_owns_valset` in genesis (image must honor it
once the impl is in the Docker binary).

## Sudo waist (assertion 1)

- Public msg: `wasm.MsgSudoContract` (`/cosmwasm.wasm.v1.MsgSudoContract`)
- Ante: `x/leanval/ante.RejectLeanSudoDecorator` (after `SetUpContext`)
- Reject when `contract` == Lean verifier:

  `types.TestLeanVerifierAcc()` = **20 zero bytes**. With prefix `terp`:

  **`terp1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq5ffndw`**

  (override with `ICT_LEAN_VERIFIER` if the image pins another address)

- **ABCI / SDK error (required):**
  - codespace: **`leanval`**
  - code: **`2`**
  - type: `types.ErrSudoLeanVerifier`
  - message contains `MsgSudoContract targeting Lean verifier is forbidden`

`code != 0` alone is **not** enough; testers require **code == 2**.

Other `MsgSudoContract` targets still pass this decorator. Unrelated
`wasm execute` is assertion 2 (not sudo).

## Two-run polarity (assertions 3 and 4)

**One process, one flag value.** Default image/genesis is **off** → run **4**.
Flip genesis/`ICT_LEAN_OWNS_VALSET=true` → second process → run **3**.

```sh
# Run A — default OFF — classic staking still moves Comet VP
ICT_LEAN_IMAGE=local cargo run --example lean_waist --features docker

# Run B — owns valset ON — bond-only must not change Comet VP
ICT_LEAN_OWNS_VALSET=true ICT_LEAN_IMAGE=local \
  cargo run --example lean_waist --features docker
```

Do not require a second image. Same binary, opposite `leanval_owns_valset`.

## Env

| Variable | Default | Meaning |
| --- | --- | --- |
| `ICT_LEAN_IMAGE_REPO` | `terpnetwork/terp-core` | Docker repo |
| `ICT_LEAN_IMAGE` | `local` | Image tag (build from **feat/lean-consensus**, not v6.0.0-dev) |
| `ICT_LEAN_VERIFIER` | 20-zero terp acc above | Lean verifier bech32 |
| `ICT_LEAN_DUMMY_WASM` | *(empty)* | Non-Lean contract for execute-still-works |
| `ICT_LEAN_OWNS_VALSET` | unset → **off** | `true`/`false`; writes genesis `leanval_owns_valset` |

## Assertions

### 1. `MsgSudoContract` → Lean verifier rejected (**code 2**)

`tx wasm sudo <verifier> '{"verify_and_apply":{}}'` from a funded user.

Pass: CheckTx or DeliverTx **code == 2**, codespace **`leanval`** when present.

### 2. Unrelated wasm execute still works

`wasm execute` on a non-Lean contract → **code == 0**. Skip only if no dummy
and no instantiated non-Lean contract.

### 3. Run B (`leanval_owns_valset=true`) — bond-only does not change Comet VP

Delegate extra tokens; wait ≥2 blocks; Comet `voting_power` **unchanged**.
Staking shares may still change. No Lean proof in this harness → no VP bump.

### 4. Run A (flag **default off**) — classic staking valset (regression)

Delegate; after ≥2 blocks Comet VP **increases**.

### 5. Evidence / surround still registered (both runs)

`module_versions` lists `evidence`, or `query evidence`, or `slashing signing-infos`.

## Out of scope

- No dummy always-true verifier.
- No surround exploit crafting.
- No edits under `v6.0.0-dev`.
- No assumption that `.worktrees/lean-consensus` already contains `c51dbee`.
