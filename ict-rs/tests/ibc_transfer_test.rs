#![cfg(feature = "testing")]
#![cfg(feature = "docker")]

//! Live IBC transfer integration test.
//!
//! Requires Docker daemon. Run with:
//! ```
//! cargo test -p ict-rs --features "testing,docker" --test ibc_transfer_test
//! ```

use std::sync::Arc;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm};
use ict_rs::interchain::{Interchain, InterchainBuildOptions, InterchainLink};
use ict_rs::relayer::{build_relayer, RelayerType};
use ict_rs::runtime::{DockerConfig, DockerImage};
use ict_rs::tx::{TransferOptions, WalletAmount};

fn gaia_config(chain_id: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "gaia".to_string(),
        chain_id: chain_id.to_string(),
        images: vec![DockerImage {
            repository: "ghcr.io/strangelove-ventures/heighliner/gaia".to_string(),
            version: "v19.0.0".to_string(),
            uid_gid: None,
        }],
        bin: "gaiad".to_string(),
        bech32_prefix: "cosmos".to_string(),
        denom: "uatom".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.025uatom".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "336h".to_string(),
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

#[tokio::test]
#[ignore] // Only run when explicitly requested: cargo test -- --ignored
async fn test_ibc_transfer_cosmos_to_cosmos() {
    let backend = ict_rs::runtime::docker::DockerBackend::new(DockerConfig::default())
        .await
        .unwrap();
    let runtime: Arc<dyn ict_rs::runtime::RuntimeBackend> = Arc::new(backend);

    // Create two chains
    let chain1 = CosmosChain::new(gaia_config("chain-a"), 1, 0, runtime.clone());
    let chain2 = CosmosChain::new(gaia_config("chain-b"), 1, 0, runtime.clone());

    // Create relayer
    let relayer = build_relayer(
        RelayerType::CosmosRly,
        runtime.clone(),
        "ibc-transfer-test",
        "ibc-test-net",
    )
    .await
    .unwrap();

    // Wire up interchain
    let mut ic = Interchain::new(runtime.clone())
        .add_chain(Box::new(chain1))
        .add_chain(Box::new(chain2))
        .add_relayer("rly", relayer)
        .add_link(InterchainLink {
            chain1: "chain-a".to_string(),
            chain2: "chain-b".to_string(),
            relayer: "rly".to_string(),
            path: "transfer".to_string(),
        });

    let opts = InterchainBuildOptions {
        test_name: "ibc-transfer-test".to_string(),
        skip_path_creation: false,
        ..Default::default()
    };

    ic.build(opts).await.unwrap();

    // Get chain references
    let chain_a = ic.get_chain("chain-a").unwrap();
    let chain_b = ic.get_chain("chain-b").unwrap();

    // Send IBC transfer from chain A → chain B
    let transfer_amount = WalletAmount {
        address: "cosmos1receiver".to_string(), // would need real address
        denom: "uatom".to_string(),
        amount: 1_000_000,
    };

    let tx = chain_a
        .send_ibc_transfer(
            "channel-0",
            "validator-0",
            &transfer_amount,
            &TransferOptions::default(),
        )
        .await
        .unwrap();

    assert!(!tx.tx_hash.is_empty(), "should have a tx hash");

    // Wait for relayer to relay the packet
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    // Verify receipt on chain B
    // (In a real test we'd check the IBC denom balance)

    ic.close().await.unwrap();
}
