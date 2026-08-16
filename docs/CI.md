# ict-rs in CI

One compile of the example binaries, then jobs only *run* them against a prebuilt chain image.

## Shared binary

```sh
just ci-build
# target/release/ict-ci
# target/release/examples/{ibc_transfer,polytone}

ICT_CI_BIN_DIR=target/release/examples ./target/release/ict-ci run polytone
```

`ict-ci list` and `just ci-build` are the same two suites. Do not `cargo run --example` in a matrix.

## Image

One tag: `terpnetwork/terp-core:local`. Override with `TERP_IMAGE_REPO` / `TERP_IMAGE_VERSION`.

## Wasm (polytone)

Set `TERP_CORE` to the terp-core repo root (CI: `$GITHUB_WORKSPACE`). Do not rely on `CARGO_MANIFEST_DIR` after downloading a prebuilt binary.

## Path-deps

terp-core does not store `crates/*` gitlinks. CI clones by URL via `scripts/ci/ict-rs-submodules.sh`.
