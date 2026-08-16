# ict-rs justfile — unified test/build entrypoint
# Usage: just test          (all mock tests, all features)
#        just test-docker    (Docker-backed tests, requires Docker daemon)
#        just check          (cargo check all features)
#        just clippy         (lint all features)

set dotenv-load := false

pkg := "ict-rs"
all_features := "docker,ethereum,testing,kuasar,terp"
mock_features := "testing,ethereum,kuasar,terp"

# Run all tests (mock mode, all features, all test targets)
test:
    ICT_MOCK=1 cargo test -p {{pkg}} --features {{mock_features}}

# Run only unit + lib tests (no integration test files)
test-unit:
    ICT_MOCK=1 cargo test -p {{pkg}} --features {{mock_features}} --lib

# Run a specific integration test file (e.g. just test-file integration_tests)
test-file name:
    ICT_MOCK=1 cargo test -p {{pkg}} --features {{mock_features}} --test {{name}}

# Run Docker-backed tests (requires running Docker daemon)
test-docker:
    cargo test -p {{pkg}} --features "docker,testing,terp" --test genesis_validation
    cargo test -p {{pkg}} --features "docker,testing,terp" --test cleanup_tests

# cargo check with all features
check:
    cargo check -p {{pkg}} --features {{all_features}}

# clippy with all features
clippy:
    cargo clippy -p {{pkg}} --features {{all_features}} -- -D warnings

# Run an example (e.g. just example basic_cosmos)
example name:
    cargo run -p {{pkg}} --features {{all_features}} --example {{name}}

# Run benchmarks
bench:
    cargo bench -p {{pkg}} --features {{mock_features}}

# --- Contract wasm builds (via cosmwasm/optimizer) ---

terp_core_dir := env("TERP_CORE_DIR", home_directory() / "abstract/terp-core")
loyalty_verifier_dir := terp_core_dir / "tests/tsh/hashmerchant/contracts/loyalty-verifier"

# Build loyalty-verifier contract wasm via optimizer and copy to interchaintest/contracts/
wasm:
    cd "{{loyalty_verifier_dir}}" && just wasm

# --- CI: one compile, many suite runs (no second cargo) ---

ci_features := "docker,terp,testing"

# Same suite set as ict-ci::SUITES. Do not add examples here unless CI runs them.
ci-build:
    cargo build -p ict-rs --release --features {{ci_features}} --bin ict-ci \
      --example ibc_transfer --example polytone

# Run a prebuilt suite. Usage: just ci-run ibc_transfer
ci-run suite:
    ICT_CI_BIN_DIR="{{justfile_directory()}}/target/release/examples" \
      "{{justfile_directory()}}/target/release/ict-ci" run {{suite}}

ci-list:
    ICT_CI_BIN_DIR="{{justfile_directory()}}/target/release/examples" \
      "{{justfile_directory()}}/target/release/ict-ci" list


# Pack host-arch tarball from the last ci-build (darwin/linux as uname).
ci-pack:
    scripts/ci/pack-bins.sh

# Cross-arch via Docker (default linux/amd64 for GHA / s3.terp.network).
ci-build-docker:
    scripts/ci/build-bins-docker.sh


# Commit-keyed prebuilts (same model as terp-core).
ci-resolve-prebuilt:
    scripts/ci/resolve-prebuilt.sh

ci-fetch-prebuilt dest="/tmp/ict-prebuilt":
    scripts/ci/fetch-prebuilt.sh {{dest}}

ci-publish-prebuilt tarball="dist/ict-ci-linux-x86_64.tar.gz":
    TARBALL={{tarball}} scripts/ci/publish-prebuilt-commit.sh

# Fetch pinned terp-core image for this ict-rs E2E (does not compile terpd).
ci-fetch-terp dest="/tmp/terp-prebuilt":
    scripts/ci/fetch-terp-prebuilt.sh {{dest}}

# Rebuild linux/amd64 ict-ci via Dockerfile.ci (same path as GHA rebuild job).
ci-rebuild-docker:
    docker build --platform linux/amd64 -f Dockerfile.ci -t ict-rs-ci:rebuild --target export ..

