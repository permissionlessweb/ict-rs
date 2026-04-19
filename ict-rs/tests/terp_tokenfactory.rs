//! Tokenfactory integration test — port of Go `TestTerpTokenFactory`.
//!
//! Runs against a live Docker container by default (like Go interchaintest).
//! Can also run against a mock runtime for fast CI by setting `ICT_MOCK=1`.
//!
//! # Live mode (default)
//! ```sh
//! cargo test -p ict-rs --features "tokenfactory,testing,docker" --test terp_tokenfactory
//! ```
//!
//! # Mock mode (fast, no Docker)
//! ```sh
//! ICT_MOCK=1 cargo test -p ict-rs --features "tokenfactory,testing" --test terp_tokenfactory
//! ```

#![cfg(feature = "tokenfactory")]
#![cfg(feature = "testing")]

use ict_rs::chain::ChainConfig;
use ict_rs::modules::tokenfactory::{TokenfactoryMsgExt, TokenfactoryQueryExt};
use ict_rs::testing::{setup_chain, TestEnv};

/// Terp chain config for tokenfactory tests.
///
/// Uses zero gas for simpler test assertions. For production-like config,
/// use `TestEnv::terp_config()` instead.
fn terp_tf_config() -> ChainConfig {
    let mut cfg = TestEnv::terp_config();
    cfg.gas_prices = "0uterp".to_string();
    cfg.gas_adjustment = 2.0;
    cfg.trusting_period = "112h".to_string();
    cfg
}

/// Port of Go `TestTerpTokenFactory`: create-denom.
#[tokio::test]
async fn test_tokenfactory_create_denom() {
    let mut tc = setup_chain("tf_create_denom", terp_tf_config()).await.unwrap();

    let tx = tc
        .chain
        .tokenfactory_create_denom("default", "ictestdenom")
        .await
        .expect("create-denom should succeed");

    assert!(tx.height > 0, "tx should be included in a block");
    assert!(!tx.tx_hash.is_empty(), "tx hash should be non-empty");

    tc.cleanup().await.unwrap();
}

/// Port of Go `TestTerpTokenFactory`: mint.
#[tokio::test]
async fn test_tokenfactory_mint() {
    let mut tc = setup_chain("tf_mint", terp_tf_config()).await.unwrap();

    tc.chain
        .tokenfactory_create_denom("default", "ictmintdenom")
        .await
        .expect("create-denom should succeed");

    let tx = tc
        .chain
        .tokenfactory_mint(
            "default",
            "100factory/terp1user000000000000000000000000000000/ictmintdenom",
            "",
        )
        .await
        .expect("mint should succeed");

    assert!(tx.height > 0);

    tc.cleanup().await.unwrap();
}

/// Port of Go `TestTerpTokenFactory`: change-admin.
#[tokio::test]
async fn test_tokenfactory_change_admin() {
    let mut tc = setup_chain("tf_change_admin", terp_tf_config()).await.unwrap();

    tc.chain
        .tokenfactory_create_denom("default", "ictadmindenom")
        .await
        .expect("create-denom should succeed");

    let tx = tc
        .chain
        .tokenfactory_change_admin(
            "default",
            "factory/terp1user000000000000000000000000000000/ictadmindenom",
            "terp1newadmin00000000000000000000000000",
        )
        .await
        .expect("change-admin should succeed");

    assert!(tx.height > 0);

    tc.cleanup().await.unwrap();
}

/// Port of Go `GetTokenFactoryAdmin` pattern: query denom authority metadata.
#[tokio::test]
async fn test_tokenfactory_query_denom_authority_metadata() {
    let mut tc = setup_chain("tf_query_metadata", terp_tf_config()).await.unwrap();

    tc.chain
        .tokenfactory_create_denom("default", "ictquerydenom")
        .await
        .expect("create-denom should succeed");

    let result = tc
        .chain
        .tokenfactory_denom_authority_metadata(
            "factory/terp1user000000000000000000000000000000/ictquerydenom",
        )
        .await
        .expect("query should succeed");

    assert!(result.is_object(), "query result should be a JSON object");

    tc.cleanup().await.unwrap();
}

/// Query tokenfactory params.
#[tokio::test]
async fn test_tokenfactory_query_params() {
    let mut tc = setup_chain("tf_query_params", terp_tf_config()).await.unwrap();

    let result = tc
        .chain
        .tokenfactory_params()
        .await
        .expect("params query should succeed");

    assert!(result.is_object());

    tc.cleanup().await.unwrap();
}

/// End-to-end flow: create-denom → mint → query, like the full Go test.
#[tokio::test]
async fn test_tokenfactory_full_flow() {
    let mut tc = setup_chain("tf_full_flow", terp_tf_config()).await.unwrap();

    // 1. Create denom
    let create_tx = tc
        .chain
        .tokenfactory_create_denom("default", "icte2edenom")
        .await
        .expect("create-denom failed");
    assert!(create_tx.height > 0);

    // 2. Mint tokens
    let mint_tx = tc
        .chain
        .tokenfactory_mint(
            "default",
            "100factory/terp1user000000000000000000000000000000/icte2edenom",
            "",
        )
        .await
        .expect("mint failed");
    assert!(mint_tx.height > 0);

    // 3. Query authority metadata
    let metadata = tc
        .chain
        .tokenfactory_denom_authority_metadata(
            "factory/terp1user000000000000000000000000000000/icte2edenom",
        )
        .await
        .expect("query denom-authority-metadata failed");
    assert!(metadata.is_object());

    // 4. Query params
    let params = tc
        .chain
        .tokenfactory_params()
        .await
        .expect("query params failed");
    assert!(params.is_object());

    tc.cleanup().await.unwrap();
}
