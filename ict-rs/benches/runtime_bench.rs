use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, TestContext};
use ict_rs::interchain::Interchain;
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::RuntimeBackend;
use ict_rs::spec::builtin_chain_config;

fn mock_runtime() -> Arc<dyn RuntimeBackend> {
    Arc::new(MockRuntime::new())
}

fn bench_chain_spec_resolution(c: &mut Criterion) {
    let mut group = c.benchmark_group("chain_spec_resolution");
    for name in ["gaia", "osmosis", "terp", "juno"] {
        group.bench_with_input(format!("resolve_{name}"), &name, |b, &name| {
            b.iter(|| {
                let cfg = builtin_chain_config(black_box(name)).unwrap();
                black_box(cfg);
            });
        });
    }
    group.finish();
}

fn bench_cosmos_chain_new(c: &mut Criterion) {
    c.bench_function("cosmos_chain_new", |b| {
        b.iter(|| {
            let rt = mock_runtime();
            let cfg = builtin_chain_config("gaia").unwrap();
            let chain = CosmosChain::new(cfg, 1, 0, rt);
            black_box(chain);
        });
    });
}

fn bench_cosmos_chain_initialize(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("cosmos_chain_initialize", |b| {
        b.iter(|| {
            let runtime = mock_runtime();
            let cfg = builtin_chain_config("gaia").unwrap();
            let mut chain = CosmosChain::new(cfg, 1, 0, runtime);
            let ctx = TestContext {
                test_name: "bench".to_string(),
                network_id: "bench-net".to_string(),
            };
            rt.block_on(chain.initialize(&ctx)).unwrap();
            black_box(&chain);
        });
    });
}

fn bench_cosmos_chain_full_lifecycle(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("cosmos_chain_full_lifecycle_1val", |b| {
        b.iter(|| {
            let runtime = mock_runtime();
            let cfg = builtin_chain_config("gaia").unwrap();
            let mut chain = CosmosChain::new(cfg, 1, 0, runtime);
            let ctx = TestContext {
                test_name: "bench".to_string(),
                network_id: "bench-net".to_string(),
            };
            rt.block_on(async {
                chain.initialize(&ctx).await.unwrap();
                chain.start(&[]).await.unwrap();
                chain.stop().await.unwrap();
            });
            black_box(&chain);
        });
    });
}

fn bench_cosmos_chain_multi_validator(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("cosmos_chain_multi_validator");
    for num_vals in [1usize, 2, 4] {
        group.bench_with_input(
            format!("{num_vals}_validators"),
            &num_vals,
            |b, &nv| {
                b.iter(|| {
                    let runtime = mock_runtime();
                    let cfg = builtin_chain_config("gaia").unwrap();
                    let mut chain = CosmosChain::new(cfg, nv, 0, runtime);
                    let ctx = TestContext {
                        test_name: "bench".to_string(),
                        network_id: "bench-net".to_string(),
                    };
                    rt.block_on(async {
                        chain.initialize(&ctx).await.unwrap();
                        chain.start(&[]).await.unwrap();
                    });
                    black_box(&chain);
                });
            },
        );
    }
    group.finish();
}

fn bench_genesis_modification(c: &mut Criterion) {
    use ict_rs::genesis::{get_genesis_module_value, set_genesis_module_value};

    c.bench_function("genesis_set_value", |b| {
        b.iter(|| {
            let mut genesis = serde_json::json!({
                "app_state": {
                    "staking": {
                        "params": {
                            "bond_denom": "stake",
                            "max_validators": 100
                        }
                    }
                }
            });
            set_genesis_module_value(
                &mut genesis,
                &["app_state", "staking", "params", "bond_denom"],
                serde_json::json!("uatom"),
            )
            .unwrap();
            black_box(&genesis);
        });
    });

    c.bench_function("genesis_get_value", |b| {
        let genesis = serde_json::json!({
            "app_state": {
                "staking": {
                    "params": {
                        "bond_denom": "uatom",
                        "max_validators": 100
                    }
                }
            }
        });
        b.iter(|| {
            let val = get_genesis_module_value(
                black_box(&genesis),
                &["app_state", "staking", "params", "bond_denom"],
            );
            black_box(val);
        });
    });
}

fn bench_interchain_builder(c: &mut Criterion) {
    c.bench_function("interchain_builder_setup", |b| {
        b.iter(|| {
            let runtime = mock_runtime();
            let ic = Interchain::new(runtime);
            black_box(ic);
        });
    });
}

criterion_group!(
    benches,
    bench_chain_spec_resolution,
    bench_cosmos_chain_new,
    bench_cosmos_chain_initialize,
    bench_cosmos_chain_full_lifecycle,
    bench_cosmos_chain_multi_validator,
    bench_genesis_modification,
    bench_interchain_builder,
);
criterion_main!(benches);
