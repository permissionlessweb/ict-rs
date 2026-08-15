# ict-rs in CI

Goal: **one compile** of the ict-rs example binaries, then many jobs only *run* them against a prebuilt chain image.

## Shared binary

```sh
just ci-build
# produces:
#   target/release/ict-ci
#   target/release/examples/{ibc_transfer,polytone,cosmos_upgrade}

ICT_CI_BIN_DIR=target/release/examples ./target/release/ict-ci run polytone
```

`ict-ci` does not rebuild. It execs the named example and forwards `TERP_IMAGE_*` / `ICT_UPGRADE_*`.

## Cache keys

| Layer | Key | Shared by |
|-------|-----|-----------|
| Cargo registry + `target/` | `ict-rs/Cargo.lock` + rustc | `ci-build` job only |
| Example + `ict-ci` binaries | artifact `ict-rs-bins` | all e2e matrix jobs |
| Chain image | artifact `terp-docker-image` | all e2e matrix jobs |

Do **not** `cargo run --example` in the matrix. That recompiles per suite.

## Submodules (from terp-core)

Path deps: `cosmos-rust`, `tendermint-rs`, `cw-orchestrator`, `terp-rs`, `ibc-proto-rs`.

```sh
git submodule update --init --depth 1 \
  crates/ict-rs crates/cosmos-rust crates/tendermint-rs \
  crates/cw-orchestrator crates/terp-rs crates/ibc-proto-rs
```

## Image tags

Examples default to `terpnetwork/terp-core:local-zk`. CI should tag the built image as both `local` and `local-zk`.
