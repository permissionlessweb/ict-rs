# Dummy Stwo ICT handshake (pair A — retuned)

Implementer landed at `feat/lean-consensus` `@71545a3`  
(`/Users/returniflost/abstract/terp-core/.worktrees/lean-consensus/crates/lean-stwo-dummy`).

**Do not edit `v6.0.0-dev` app.** ict-rs here is examples-only (no `Cargo.toml`).

```sh
cargo run --example lean_stwo_dummy --features docker
LEAN_STWO_DUMMY=1 cargo run --example lean_stwo_dummy --features docker
```

When ict-rs can take a path dep:

```toml
lean-stwo-dummy = { path = "/Users/returniflost/abstract/terp-core/.worktrees/lean-consensus/crates/lean-stwo-dummy" }
```

```rust
use lean_stwo_dummy::{dummy_m31_hash, DummyStwo, M31, Verifier, STWO_DUMMY_VERIFY_GAS};
// charge STWO_DUMMY_VERIFY_GAS (150_000) before rust
let a = M31::new(11).unwrap();
let b = M31::new(22).unwrap();
let c = dummy_m31_hash(a, b);
let proof = DummyStwo::prove(a, b);
assert_eq!(DummyStwo.verify_dummy(&proof, a.0, b.0, c.0).unwrap(), true);
```

Wire: `b"DSTW" || prover_id:u8 || curve_id:u8 || a_le:u32 || b_le:u32 || c_le:u32` (`DUMMY_PROOF_LEN = 18`).

GPU: CPU-only. `NVIDIA_VISIBLE_DEVICES=void`, `CUDA_VISIBLE_DEVICES=`, `STWO_GPU=0`. Crate `gpu` feature is empty.

## Bind vs still gated

| Case | ICT fn / crate test | Binding |
|------|---------------------|---------|
| 1. Valid dummy | `dummy_stwo_valid_ok` + `valid_dummy_proof_accepted` | **Crate-bound** via `verify_dummy` / `DummyStwo::prove`. Docker inject still gated (no FinalizeBlock LNPR). |
| 2. One-byte flip | `dummy_stwo_bitflip_fail` + `one_byte_flip_rejected` | **Crate-bound** (`proof[14] ^= 1`). Docker = ProcessProposal / FinalizeBlock when host exists. |
| 3. Two nodes | `two_nodes_same_verify` + `two_nodes_same_binary` | **Crate-bound** (two `verify_dummy` same 0/1). Docker 2-val image still gated. |
| 4. &gt; 2 MiB | `dummy_stwo_oversize_proof_rejected` | **Crate-bound** `ProofTooLarge`, no hang. Docker inject optional. |
| 5. Missing inject | `missing_required_inject` | **Docker-gated** — crate has no inclusion / ABCI. |
| 6. No GPU | `gpu_not_required*` | **Documented env** + empty crate `gpu` feature. Live chain still gated. |

Also crate-bound: `dummy_stwo_wrong_prover_id_fail_closed` (`prover_id` 0 / 99).

Pins: `prover_id=2`, `curve_id=5`, cap **2 MiB**, `STWO_DUMMY_VERIFY_GAS=150000` (placeholder; not a bench).

## Remaining TODOs

1. Add `lean-stwo-dummy` path dep to real ict-rs `Cargo.toml` and drop the inline shim.
2. `tx leanval inject-dummy` still fictional — no `x/leanval` FinalizeBlock in implementer crate.
3. `stwo` git rev **not** pinned (too heavy); DummyStwo is not production S-two.
4. Gas 150k is a placeholder until a real stwo bench.
5. Inclusion-rule genesis path still a guess.
