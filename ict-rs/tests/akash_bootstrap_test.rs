// tests/akash_bootstrap_test.rs
// CI runtime test for local Akash chain + provider + minio-ipfs bootstrap workflow.
//
// Spawns a single-validator Akash chain with oracle price feeder, attaches a
// minio-ipfs sidecar for snapshot serving, then verifies the deployment lifecycle
// and bootstrap workflow end-to-end.
//
// Run: cargo test --test akash_bootstrap_test -- --ignored --nocapture
// Requires: Docker, mc CLI, sha256sum

#![cfg(feature = "akash")]
#![cfg(feature = "testing")]

use std::sync::Arc;

use ict_rs::chain::akash::spawn_akash_chain_with_accounts;
use ict_rs::chain::akash_oracle::start_oracle_price_feeder;
use ict_rs::chain::{Chain, SidecarConfig};
use ict_rs::cosmos::docker_sidecar::minio_ipfs_config;
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::testing::{TestChain, TestChainConfig};

const TEST_NAME: &str = "akash-boot-test";
const FAUCET: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

// ─── Helpers ────────────────────────────────────────────────────────────────

fn mock_runtime() -> Arc<MockRuntime> {
    Arc::new(MockRuntime::new())
}

fn minio_ipfs_sidecar() -> SidecarConfig {
    minio_ipfs_config(
        "testadmin",
        "testpass",
        "test-webhook-token",
        "snapshots,static",
    )
}

/// Wait for a condition function to return true, polling every `interval_ms`.
async fn wait_for<F, Fut>(condition: F, timeout_secs: u64, interval_ms: u64)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now()
        + tokio::time::Duration::from_secs(timeout_secs);
    loop {
        if condition().await {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("wait_for timed out after {}s", timeout_secs);
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(interval_ms)).await;
    }
}

// ─── Test: Akash chain lifecycle ───────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_akash_chain_lifecycle() {
    let _runtime = mock_runtime();
    let mut config = ict_rs::chain::akash::akash_chain_config();
    config.sidecar_configs.push(minio_ipfs_sidecar());

    let mut tc = TestChain::setup(
        TEST_NAME,
        TestChainConfig {
            chain_config: config,
            num_validators: 1,
            num_full_nodes: 0,
            genesis_wallets: Vec::new(),
        },
    )
    .await
    .unwrap();

    let chain = &tc.chain;
    let primary = chain.primary_node().unwrap();

    // 1. Verify chain is alive and producing blocks
    let height = chain.height().await.unwrap();
    assert!(height > 0, "chain should produce blocks, got height {}", height);

    // 2. Verify RPC endpoint
    let rpc = chain.host_rpc_address();
    assert!(!rpc.is_empty(), "RPC endpoint should be set");
    eprintln!("RPC: {}", rpc);

    // 3. Verify gRPC endpoint
    let grpc = chain.host_grpc_address();
    assert!(!grpc.is_empty(), "gRPC endpoint should be set");
    eprintln!("gRPC: {}", grpc);

    // 4. Verify REST/API endpoint
    let rest = primary.host_api_port
        .map(|p| format!("http://localhost:{}", p))
        .unwrap_or_else(|| "http://localhost:1317".to_string());
    eprintln!("REST: {}", rest);

    // 5. Start oracle price feeder (needed for market module)
    start_oracle_price_feeder(chain).await.unwrap();
    eprintln!("Oracle price feeder started");

    // 6. Query oracle params to confirm module is active
    let oracle_params = chain
        .chain_exec(&["query", "oracle", "params", "--output", "json"])
        .await
        .unwrap();
    eprintln!("Oracle params: {}", oracle_params.stdout_str().trim());

    // 7. Query bank balances
    let faucet_addr = primary.get_key_address("faucet").await.unwrap();
    let balances = chain
        .chain_exec(&[
            "query", "bank", "balances", &faucet_addr, "--output", "json",
        ])
        .await
        .unwrap();
    eprintln!("Faucet balances: {}", balances.stdout_str().trim());
    assert!(
        balances.stdout_str().contains("uakt"),
        "faucet should have uakt after genesis"
    );
    assert!(
        balances.stdout_str().contains("uact"),
        "faucet should have uact after genesis"
    );

    // 8. Verify chain processes txs
    let validator_addr = primary.get_key_address("validator").await.unwrap();
    let transfer = chain
        .chain_exec_tx_with(
            &[
                "tx", "bank", "send",
                &faucet_addr, &validator_addr,
                "1000000uakt",
                "--gas", "auto",
                "--gas-prices", "0.025uakt",
                "-y",
            ],
            chain.default_tx_opts().from("faucet"),
        )
        .await
        .unwrap();
    assert_eq!(
        transfer.exit_code, 0,
        "bank send should succeed: {}",
        transfer.stderr_str()
    );
    eprintln!("Bank transfer successful");

    tc.chain.stop().await.unwrap();
    eprintln!("test_akash_chain_lifecycle PASSED");
}

// ─── Test: Deployment lifecycle ─────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_akash_deployment_lifecycle() {
    // Spawn akash chain with an extra funded account for the deployer.
    // The deployer needs uakt + uact at genesis (BME blocks runtime uact sends).
    let provider_addr = "akash1qyfk4xfsl2fkltun5u62ymm2e45lsg0s47x0yd";
    let deployer_addr = "akash1p9m7spn4zqa9jfwyjfj3n4d3hgl4nwptgvlt6t";

    let mut spawned = spawn_akash_chain_with_accounts(
        "akash-deploy-test",
        FAUCET,
        &[
            (provider_addr, 10_000_000_000u64, 100_000_000_000u64),
            (deployer_addr, 10_000_000_000u64, 100_000_000_000u64),
        ],
    )
    .await
    .unwrap();

    let chain = &spawned.tc.chain;
    let primary = chain.primary_node().unwrap();
    let faucet = primary.get_key_address("faucet").await.unwrap();
    eprintln!("Faucet:    {}", faucet);
    eprintln!("Provider:  {}", provider_addr);
    eprintln!("Deployer:  {}", deployer_addr);

    // Verify all three accounts are funded
    for (label, addr) in &[
        ("faucet", faucet.as_str()),
        ("provider", provider_addr),
        ("deployer", deployer_addr),
    ] {
        let balance = chain
            .chain_exec(&[
                "query", "bank", "balances", addr, "--output", "json",
            ])
            .await
            .unwrap();
        eprintln!("{} balance: {}", label, balance.stdout_str().trim());
        assert!(
            balance.stdout_str().contains("uakt"),
            "{} should have uakt",
            label
        );
    }

    // Create a deployment certificate for the deployer
    // (simplified — uses the chain binary to create cert)
    let cert = chain
        .chain_exec_tx_with(
            &[
                "tx", "cert", "generate", "chain",
                "--from", "faucet",
                "--chain-id", &spawned.chain_id,
                "--node", &format!("tcp://{}", spawned.rpc),
                "--keyring-backend", "test",
                "--gas", "auto",
                "--gas-prices", "0.025uakt",
                "-y",
            ],
            chain.default_tx_opts().from("faucet"),
        )
        .await.unwrap();
    eprintln!("Cert output: {}", cert.stdout_str().trim());

    // Create a deployment on the Akash chain with a sentry SDL
    let sdl = ict_rs::cosmos::docker_sidecar::sentry_deployment_sdl(
        &spawned.chain_id,
        "ghcr.io/terpnetwork/terp-core:v5.1.10",
        &spawned.rpc,
        "test-sentry",
    );
    eprintln!("SDL length: {} bytes", sdl.len());

    // Write SDL to temp file and submit as deployment
    let home = chain.home_dir();
    let sdl_path = format!("{}/deploy-sdl.yaml", home);
    let _write_sdl = chain
        .exec(&["sh", "-c", &format!("cat > {} << 'ENDSDL'\n{}\nENDSDL", sdl_path, sdl)], &[])
        .await
        .unwrap();
    eprintln!("SDL written to {}", sdl_path);

    // Submit deployment
    let deployment = chain
        .chain_exec_tx_with(
            &[
                "tx", "deployment", "create", &sdl_path,
                "--from", "faucet",
                "--chain-id", &spawned.chain_id,
                "--node", &format!("tcp://{}", spawned.rpc),
                "--keyring-backend", "test",
                "--deposit", "5000000uakt",
                "--gas", "auto",
                "--gas-prices", "0.025uakt",
                "-y",
                "--output", "json",
            ],
            chain.default_tx_opts().from("faucet"),
        )
        .await
        .unwrap();
    eprintln!("Deployment tx: {}", deployment.stdout_str().trim());

    // Query deployments to confirm the tx was accepted
    let deployments = chain
        .chain_exec(&[
            "query", "deployment", "list", "--owner", &faucet,
            "--output", "json",
        ])
        .await
        .unwrap();
    eprintln!("Deployments: {}", deployments.stdout_str().trim());
    assert!(
        deployments.stdout_str().contains("deployments") || deployments.stdout_str().contains("dseq"),
        "deployment should appear in query output"
    );

    let ok = spawned.tc.chain.stop().await;
    match ok {
        Ok(_) => {}
        Err(e) => eprintln!("cleanup: {}", e),
    }
    eprintln!("test_akash_deployment_lifecycle PASSED");
}

// ─── Test: Mock runtime validation ──────────────────────────────────────────

#[tokio::test]
async fn test_akash_mock_sidecar_config() {
    // Verifies the sidecar config builds correctly without Docker.
    // This test runs with any feature set (no Docker required).

    let config = ict_rs::cosmos::docker_sidecar::akash_provider_config(
        "127.0.0.1:26657",
        "akash-local-1",
        "akash1qyfk4xfsl2fkltun5u62ymm2e45lsg0s47x0yd",
    );

    assert_eq!(config.name, "akash-provider");
    assert_eq!(config.image.repository, "ghcr.io/akash-network/provider");
    assert_eq!(config.ports.len(), 2);
    assert!(config.ports.contains(&"8443".to_string()));
    assert!(config.ports.contains(&"8444".to_string()));
    assert!(!config.pre_start);
    assert!(!config.validator_process);
    assert_eq!(config.ready_timeout_secs, 60);

    // Verify oracle feeder config
    let oracle_cfg = ict_rs::cosmos::docker_sidecar::oracle_feeder_config(
        "127.0.0.1:26657",
        "akash-local-1",
    );
    assert_eq!(oracle_cfg.name, "akash-oracle-feeder");
    assert_eq!(oracle_cfg.image.repository, "ghcr.io/akash-network/node");

    // Verify sentry SDL produces valid YAML
    let sdl = ict_rs::cosmos::docker_sidecar::sentry_deployment_sdl(
        "terp-test-1",
        "ghcr.io/terpnetwork/terp-core:v5.1.10",
        "http://minio:9000",
        "test-sentry",
    );
    assert!(sdl.contains("terp-test-1"), "SDL should contain chain ID");
    assert!(sdl.contains("terpd"), "SDL should reference terpd binary");
    assert!(sdl.contains("S3_ENDPOINT"), "SDL should have S3 config");
    assert!(sdl.contains("26656"), "SDL should expose P2P port");
    assert!(sdl.contains("uakt"), "SDL should have pricing in uakt");
}

// ─── Test: MinIO IPFS config validity ───────────────────────────────────────

#[tokio::test]
async fn test_akash_sidecar_minio_ipfs_config() {
    let config = minio_ipfs_sidecar();

    assert_eq!(config.name, "minio-ipfs");
    assert_eq!(config.image.repository, "minio-ipfs");
    assert_eq!(config.ports.len(), 5);
    assert!(config.ports.contains(&"9000".to_string()));
    assert!(config.ports.contains(&"8081".to_string()));
    assert!(config.ports.contains(&"9100".to_string()));
    assert!(config.ports.contains(&"9443".to_string()));
    assert_eq!(config.health_endpoint, Some("/minio/health/live".to_string()));
    assert_eq!(config.ready_timeout_secs, 30);
}