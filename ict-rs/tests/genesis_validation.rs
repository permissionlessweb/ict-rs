//! Genesis validation integration tests.
//!
//! These tests verify that the genesis pipeline produces correct genesis.json files.
//! They run against both mock and live Docker containers (controlled by ICT_MOCK env var).
//!
//! Run with:
//!   ICT_MOCK=1 cargo test -p ict-rs --features testing --test genesis_validation
//!   cargo test -p ict-rs --features "testing,docker" --test genesis_validation

#![cfg(feature = "testing")]

use anyhow::Context;
use ict_rs::chain::Chain;
use ict_rs::testing::{setup_chain, TestChain, TestChainConfig, TestEnv};

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("ict_rs=debug")
        .try_init();
}

#[tokio::test]
async fn test_genesis_accounts_exist() -> anyhow::Result<()> {
    init_tracing();

    let config = TestEnv::terp_config();
    let mut tc = setup_chain("genesis_accounts", config)
        .await
        .context("setup_chain failed for genesis_accounts")?;

    let result = async {
        let genesis = tc
            .chain
            .read_genesis()
            .await
            .context("read_genesis failed")?;
        let accounts = genesis["app_state"]["auth"]["accounts"]
            .as_array()
            .context("accounts should be an array")?;

        anyhow::ensure!(
            !accounts.is_empty(),
            "genesis should have at least one account"
        );
        Ok(())
    }
    .await;

    tc.cleanup().await.context("cleanup failed")?;
    result
}

#[tokio::test]
async fn test_genesis_gentx_entries() -> anyhow::Result<()> {
    init_tracing();

    let config = TestEnv::terp_config();
    let mut tc = setup_chain("genesis_gentx", config)
        .await
        .context("setup_chain failed for genesis_gentx")?;

    let result = async {
        let genesis = tc
            .chain
            .read_genesis()
            .await
            .context("read_genesis failed")?;
        let gen_txs = genesis["app_state"]["genutil"]["gen_txs"]
            .as_array()
            .context("gen_txs should be an array")?;

        anyhow::ensure!(
            !gen_txs.is_empty(),
            "genesis should have at least one gentx entry"
        );
        Ok(())
    }
    .await;

    tc.cleanup().await.context("cleanup failed")?;
    result
}

#[tokio::test]
async fn test_genesis_bond_denom_replaced() -> anyhow::Result<()> {
    init_tracing();

    let config = TestEnv::terp_config();
    let expected_denom = config.denom.clone();
    let mut tc = setup_chain("genesis_denom", config)
        .await
        .context("setup_chain failed for genesis_denom")?;

    let result = async {
        let genesis = tc
            .chain
            .read_genesis()
            .await
            .context("read_genesis failed")?;
        let bond_denom = genesis["app_state"]["staking"]["params"]["bond_denom"]
            .as_str()
            .context("bond_denom should be a string")?;

        anyhow::ensure!(
            bond_denom == expected_denom,
            "bond_denom '{}' should match configured denom '{}'",
            bond_denom,
            expected_denom
        );
        Ok(())
    }
    .await;

    tc.cleanup().await.context("cleanup failed")?;
    result
}

#[tokio::test]
async fn test_validate_genesis_passes() -> anyhow::Result<()> {
    init_tracing();

    let config = TestEnv::terp_config();
    let mut tc = setup_chain("genesis_validate", config)
        .await
        .context("setup_chain failed for genesis_validate")?;

    let result = async {
        let genesis = tc
            .chain
            .validate_genesis()
            .await
            .context("validate_genesis failed")?;

        anyhow::ensure!(genesis["app_state"].is_object(), "app_state should exist");
        Ok(())
    }
    .await;

    tc.cleanup().await.context("cleanup failed")?;
    result
}

#[tokio::test]
async fn test_chain_produces_blocks() -> anyhow::Result<()> {
    init_tracing();

    let config = TestEnv::terp_config();
    let mut tc = setup_chain("genesis_blocks", config)
        .await
        .context("setup_chain failed for genesis_blocks")?;

    let result = async {
        let height = tc.chain.height().await.context("height query failed")?;
        anyhow::ensure!(height > 0, "chain should have produced at least one block");
        Ok(())
    }
    .await;

    tc.cleanup().await.context("cleanup failed")?;
    result
}

#[tokio::test]
async fn test_genesis_hash_consistent() -> anyhow::Result<()> {
    init_tracing();

    let config = TestEnv::terp_config();
    let mut tc = TestChain::setup(
        "genesis_hash",
        TestChainConfig {
            chain_config: config,
            num_validators: 2,
            num_full_nodes: 0,
            genesis_wallets: Vec::new(),
        },
    )
    .await
    .context("TestChain::setup failed for genesis_hash")?;

    let result = async {
        let validators = tc.chain.validators();
        if validators.len() >= 2 {
            let hash0 = validators[0]
                .genesis_hash()
                .await
                .context("genesis_hash failed for validator 0")?;
            let hash1 = validators[1]
                .genesis_hash()
                .await
                .context("genesis_hash failed for validator 1")?;
            anyhow::ensure!(
                hash0 == hash1,
                "all validators should have the same genesis hash: {} vs {}",
                hash0,
                hash1
            );
        }
        Ok(())
    }
    .await;

    tc.cleanup().await.context("cleanup failed")?;
    result
}
