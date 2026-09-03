# Pair B handshake — `x/leanval` BondedSet + LNPR wrap HMVE

Retuned to implementer **feat/lean-consensus @ e640367**  
(`/Users/returniflost/abstract/terp-core/.worktrees/lean-consensus`, `x/leanval`).  
Notes: `/tmp/grok-lean-b-impl.md`. **Not** `v6.0.0-dev`.

Go unit tests (`go test ./x/leanval/...`) already cover the same cases. This example is the ict-rs / Docker surface.

## Exact names (use these, not earlier guesses)

| Implementer | Meaning |
|-------------|---------|
| `types.PeriodFromHeight(h)` | `period = height / 600` (`BlocksPerPeriod = 600`) |
| `k.QueryBondedSet(period)` | `[]keeper.SubjectPower` — `Subject []byte`, `Weight int64`, `HasProof bool` |
| `k.BondedSet(period)` | same store; missing accepted proof ⇒ `Weight == 0` |
| `k.AcceptProof` / `ApplyLNPR` / `ProcessInjectedLNPR` | write EB after valid inject |
| `k.ValidatorUpdates(period)` | `[]abci.ValidatorUpdate` from BondedSet only — never `LastValidatorPowers` |
| `types.EncodeLNPR` / `DecodeLNPR` | proposal inject blob (not a user `Msg`) |
| `keeper.WrapPrepareProposal` / `WrapProcessProposal` | wrap hashmerchant handlers |
| `k.RequireLNPR` | default **true**; omitted LNPR ⇒ ProcessProposal **REJECT** |
| `types.PrefixHMVE` | `[]byte("HMVE")` = `0x48 0x4D 0x56 0x45` |
| `types.PrefixLNPR` | `[]byte("LNPR")` = `0x4C 0x4E 0x50 0x52` |

## Period

```
P = height / 600          // 599 → 0, 600 → 1
```

LNPR blob `period` must equal `PeriodFromHeight(proposalHeight)` or ProcessProposal REJECT.

## LNPR wire (`EncodeLNPR`)

```
LNPR | version=1 | period u64be | nsubj u16be | repeated {
  addrLen u8 | addr | weight i64be | proofLen u32be | proof
}
```

Do **not** send as `tx leanval …`. Prepare inject only.

## Prefix order (compose, do not replace HMVE)

| txs[0] starts with HMVE? | LNPR slot |
|--------------------------|-----------|
| yes                      | **txs[1]** |
| no                       | **txs[0]** |

```go
app.SetPrepareProposal(lean.WrapPrepareProposal(hm.PrepareProposalHandler))
app.SetProcessProposal(lean.WrapProcessProposal(hm.ProcessProposalHandler))
```

REJECT if: `RequireLNPR` and LNPR omitted; LNPR at wrong index; `period != height/600`; `VerifyDummy` fails (bitflip / closed / >2MiB).

## Query surface (no gRPC yet)

Keeper: `k.QueryBondedSet(period)`.

When CLI/gRPC is added, this harness expects names that match:

| Planned `terpd` | Maps to |
|-----------------|---------|
| `query leanval bonded-set [period]` | `QueryBondedSet` JSON `{ subjects: [{ subject, weight, has_proof }] }` |
| `query staking last-validator-power [valoper]` | **must not** confer Comet power after cutover |

Until CLI exists, `lean_val.rs` decodes **block txs** with `DecodeLNPR` and computes `period` locally from `chain.height()`. Missing query prints `QUERY_MISSING`.

## Cases (same as `keeper/power_test.go` + `keeper/abci_test.go`)

1. **power0** — subject in set without `AcceptProof` ⇒ `Weight==0`, `HasProof==false`. Staking LastValidatorPowers does not halt/vote.
2. **bonded** — after `ProcessInjectedLNPR` / valid dummy, `QueryBondedSet(P)` includes subject with `Weight>0`.
3. **reject** — `RequireLNPR` (default true) + omitted LNPR ⇒ ProcessProposal REJECT (chain cannot finalize a no-LNPR proposal).
4. **hmve** — if `txs[0]` is HMVE, LNPR is `txs[1]` (`WrapPrepareProposal` injects after inner HMVE).
5. **split** — two subjects; only the one with accepted proof has `Weight>0`.

## Run

```sh
cargo run --example lean_val --features docker
ICT_LEAN_CASES=power0,bonded,reject,hmve,split cargo run --example lean_val --features docker
```

This `ict-rs` checkout is examples-only; compile against the tree that builds `cosmos_upgrade`.
