# Justfile Recipes

Located at workspace root `justfile`.

## Test Recipes

```bash
# Run all tests (mock mode, all features, all test targets)
just test
# => ICT_MOCK=1 cargo test -p ict-rs --features testing,ethereum,kuasar,terp

# Run only unit + lib tests (no integration test files)
just test-unit

# Run a specific integration test file
just test-file integration_tests
just test-file terp_tokenfactory
just test-file genesis_validation

# Run Docker-backed tests (requires running Docker daemon)
just test-docker
# => cargo test -p ict-rs --features docker,testing,terp --test genesis_validation
# => cargo test -p ict-rs --features docker,testing,terp --test cleanup_tests
```

## Build & Lint

```bash
# cargo check with all features
just check
# => cargo check -p ict-rs --features docker,ethereum,testing,kuasar,terp

# clippy with all features
just clippy
# => cargo clippy -p ict-rs --features docker,ethereum,testing,kuasar,terp -- -D warnings
```

## Examples

```bash
# Run any example
just example basic_cosmos
just example ibc_transfer
just example hashmerchant
# => cargo run -p ict-rs --features <all_features> --example <name>
```

## Benchmarks

```bash
just bench
# => cargo bench -p ict-rs --features testing,ethereum,kuasar,terp
```

## Feature Sets

The justfile defines two feature sets:
- `all_features`: `docker,ethereum,testing,kuasar,terp` — for check/clippy/examples
- `mock_features`: `testing,ethereum,kuasar,terp` — for mock-mode tests (no docker)
