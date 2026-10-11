//! Circuit-deposit e2e — pay + query coverage (ict-rs / cw-orch path).
//!
//! Soundness of retention payment: a funded account can prepay yearly runway
//! and query `covered`. Fee-share *registration* is exercised as the intended
//! wiring for execute-fee → depositor (cash today; `circuit_runway` hook later,
//! see `x/feeshare/spec/09_circuit_runway.md`).
//!
//! ```sh
//! ICT_MOCK=1 cargo test -p ict-rs --features "testing,terp" --test terp_circuit_deposit
//! cargo test -p ict-rs --features "testing,terp,docker" --test terp_circuit_deposit
//! ```

#![cfg(feature = "testing")]
#![cfg(feature = "terp")]

use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::modules::feeshare::FeeshareMsgExt;
use ict_rs::testing::{setup_chain, TestEnv};

fn terp_cfg() -> ict_rs::chain::ChainConfig {
    let mut cfg = TestEnv::terp_config();
    cfg.gas_prices = "0uterp".to_string();
    cfg.gas_adjustment = 2.0;
    cfg
}

#[tokio::test]
async fn test_pay_and_query_circuit_deposit() {
    let mut tc = setup_chain("circuit_deposit_pay", terp_cfg())
        .await
        .unwrap();

    let tx = tc
        .chain
        .pay_circuit_deposit("default", 1)
        .await
        .expect("pay-circuit-deposit");
    assert!(tx.height > 0);
    assert!(!tx.tx_hash.is_empty());

    let q = tc
        .chain
        .query_circuit_deposit("terp1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq6m2qj3")
        .await
        .expect("query circuit-deposit");
    assert_eq!(q["covered"], true);

    tc.cleanup().await.unwrap();
}

/// Feeshare register stays the opt-in for execute-fee split; withdrawer is the
/// circuit depositor. Auto-credit of that split onto `paid_until` is spec'd,
/// not hooked yet.
#[tokio::test]
async fn test_feeshare_register_for_circuit_withdrawer() {
    let mut tc = setup_chain("circuit_deposit_feeshare", terp_cfg())
        .await
        .unwrap();

    let tx = tc
        .chain
        .feeshare_register_fee_share(
            "default",
            "terp1mockcontract1",
            "terp1mockdeployer",
            "terp1mockwithdrawer",
        )
        .await
        .expect("register-fee-share");
    assert!(tx.height > 0);

    tc.cleanup().await.unwrap();
}
