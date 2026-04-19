//! Cleanup behavior verification tests.
//!
//! These tests always run in mock mode to verify that TestChain properly
//! cleans up containers, volumes, and networks.
//!
//! Run with:
//!   cargo test -p ict-rs --features testing --test cleanup_tests

#![cfg(feature = "testing")]

use std::sync::Arc;

use ict_rs::chain::{ChainConfig, ChainType, SigningAlgorithm};
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::{DockerImage, RuntimeBackend};
use ict_rs::testing::{TestChain, TestChainConfig};

fn mock_chain_config() -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "test".to_string(),
        chain_id: "test-1".to_string(),
        images: vec![DockerImage {
            repository: "test/chain".to_string(),
            version: "latest".to_string(),
            uid_gid: None,
        }],
        bin: "testd".to_string(),
        bech32_prefix: "test".to_string(),
        denom: "utest".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.025utest".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "508h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: Default::default(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

async fn make_test_chain(name: &str, mock: Arc<MockRuntime>) -> TestChain {
    TestChain::setup_with_runtime(
        name,
        TestChainConfig {
            chain_config: mock_chain_config(),
            num_validators: 1,
            num_full_nodes: 0,
            genesis_wallets: Vec::new(),
        },
        mock as Arc<dyn RuntimeBackend>,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn test_cleanup_removes_containers_and_volumes() {
    let mock = Arc::new(MockRuntime::new());
    let state = mock.state();

    let mut tc = make_test_chain("cleanup_test", mock).await;
    // Ensure keep_containers is false (avoid env var race with parallel tests)
    tc.keep_containers = false;

    // Before cleanup: containers and networks should exist
    {
        let s = state.lock().unwrap();
        assert!(!s.containers.is_empty(), "should have containers before cleanup");
        assert!(!s.networks.is_empty(), "should have networks before cleanup");
    }

    tc.cleanup().await.unwrap();

    // After cleanup: containers removed, volumes removed, networks removed
    {
        let s = state.lock().unwrap();
        assert!(s.containers.is_empty(), "containers should be removed after cleanup");
        assert!(s.networks.is_empty(), "networks should be removed after cleanup");
        assert!(
            !s.volumes_removed.is_empty(),
            "volumes should have been removed"
        );
    }
}

#[tokio::test]
async fn test_keep_containers_skips_cleanup() {
    let mock = Arc::new(MockRuntime::new());
    let state = mock.state();

    let mut tc = make_test_chain("keep_test", mock).await;
    // Set keep_containers directly (not via env var to avoid parallel test races)
    tc.keep_containers = true;

    tc.cleanup().await.unwrap();

    // Containers should still exist because keep_containers = true
    {
        let s = state.lock().unwrap();
        assert!(
            !s.containers.is_empty(),
            "containers should remain when keep_containers=true"
        );
    }

    // Now force actual cleanup for test isolation
    tc.keep_containers = false;
    tc.cleaned_up = false;
    tc.cleanup().await.unwrap();
}

#[tokio::test]
async fn test_cleanup_is_idempotent() {
    let mock = Arc::new(MockRuntime::new());

    let mut tc = make_test_chain("idempotent_test", mock).await;
    tc.keep_containers = false;

    // Call cleanup twice — should not panic
    tc.cleanup().await.unwrap();
    tc.cleanup().await.unwrap();
}
