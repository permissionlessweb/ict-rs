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
