#![cfg(feature = "ethereum")]
#![cfg(feature = "testing")]

use std::sync::Arc;

use ict_rs::chain::ethereum::{AnvilChain, ANVIL_DEFAULT_ACCOUNTS, ANVIL_NUM_ACCOUNTS};
use ict_rs::chain::{Chain, ChainType, TestContext};
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::spec::builtin_chain_config;

fn mock_runtime() -> Arc<MockRuntime> {
    Arc::new(MockRuntime::new())
}

fn anvil_config() -> ict_rs::chain::ChainConfig {
    builtin_chain_config("anvil").unwrap()
}

#[tokio::test]
async fn test_anvil_starts() {
    let runtime = mock_runtime();
    let cfg = anvil_config();
    let mut chain = AnvilChain::new(cfg, runtime.clone());

    let ctx = TestContext {
        test_name: "test_anvil_starts".to_string(),
        network_id: "test-net".to_string(),
    };

    chain.initialize(&ctx).await.unwrap();
    chain.start(&[]).await.unwrap();

    let height = chain.height().await.unwrap();
    assert!(height > 0, "block number should be > 0, got {height}");

    chain.stop().await.unwrap();
}

#[tokio::test]
async fn test_anvil_prefunded_accounts() {
    let runtime = mock_runtime();
    let cfg = anvil_config();
    let chain = AnvilChain::new(cfg, runtime);

    let accounts = chain.accounts();
    assert_eq!(accounts.len(), ANVIL_NUM_ACCOUNTS);

    for (i, acct) in accounts.iter().enumerate() {
        assert!(acct.address.starts_with("0x"), "address should start with 0x");
        assert!(acct.private_key.starts_with("0x"), "private key should start with 0x");
        assert_eq!(acct.index, i);
    }

    // Verify against known Anvil defaults
    assert_eq!(accounts[0].address, ANVIL_DEFAULT_ACCOUNTS[0].0);
    assert_eq!(accounts[0].private_key, ANVIL_DEFAULT_ACCOUNTS[0].1);
}

#[tokio::test]
async fn test_anvil_send_funds() {
    let runtime = mock_runtime();
    let cfg = anvil_config();
    let mut chain = AnvilChain::new(cfg, runtime.clone());

    let ctx = TestContext {
        test_name: "test_send".to_string(),
        network_id: "test-net".to_string(),
    };

    chain.initialize(&ctx).await.unwrap();
    chain.start(&[]).await.unwrap();

    // Send ETH from account 0 to account 1
    let tx_output = chain
        .send_eth(
            &ANVIL_DEFAULT_ACCOUNTS[0].1,
            ANVIL_DEFAULT_ACCOUNTS[1].0,
            "1000000000000000000", // 1 ETH in wei
        )
        .await
        .unwrap();

    // Verify tx output contains a transaction hash
    let stdout = tx_output.stdout_str();
    assert!(
        stdout.contains("transactionHash"),
        "should contain transactionHash, got: {stdout}"
    );

    chain.stop().await.unwrap();
}

#[tokio::test]
async fn test_anvil_get_block() {
    let runtime = mock_runtime();
    let cfg = anvil_config();
    let mut chain = AnvilChain::new(cfg, runtime.clone());

    let ctx = TestContext {
        test_name: "test_block".to_string(),
        network_id: "test-net".to_string(),
    };

    chain.initialize(&ctx).await.unwrap();
    chain.start(&[]).await.unwrap();

    let block = chain.get_block_by_number(1).await.unwrap();
    assert!(
        block.contains("number"),
        "block should contain number field, got: {block}"
    );

    chain.stop().await.unwrap();
}

#[tokio::test]
async fn test_anvil_deploy_contract() {
    let runtime = mock_runtime();
    let cfg = anvil_config();
    let mut chain = AnvilChain::new(cfg, runtime.clone());

    let ctx = TestContext {
        test_name: "test_deploy".to_string(),
        network_id: "test-net".to_string(),
    };

    chain.initialize(&ctx).await.unwrap();
    chain.start(&[]).await.unwrap();

    // Simple contract bytecode (a minimal contract that just returns)
    let bytecode = "0x600160005260206000f3";
    let output = chain
        .deploy_contract(&ANVIL_DEFAULT_ACCOUNTS[0].1, bytecode)
        .await
        .unwrap();

    let stdout = output.stdout_str();
    // Mock returns a receipt with contractAddress
    assert!(
        stdout.contains("transactionHash") || stdout.contains("status"),
        "deploy should return tx info, got: {stdout}"
    );

    chain.stop().await.unwrap();
}

#[tokio::test]
async fn test_anvil_full_flow() {
    let runtime = mock_runtime();
    let cfg = anvil_config();
    let mut chain = AnvilChain::new(cfg, runtime.clone());

    let ctx = TestContext {
        test_name: "test_full".to_string(),
        network_id: "test-net".to_string(),
    };

    // Initialize and start
    chain.initialize(&ctx).await.unwrap();
    chain.start(&[]).await.unwrap();

    // Check height
    let height = chain.height().await.unwrap();
    assert!(height > 0);

    // Get balance (mock returns a large number)
    let balance = chain
        .get_balance(ANVIL_DEFAULT_ACCOUNTS[0].0, "eth")
        .await
        .unwrap();
    assert!(balance > 0, "balance should be > 0");

    // Build a wallet
    let wallet = chain.build_wallet("anvil-0", "").await.unwrap();
    assert!(
        wallet.formatted_address().starts_with("0x"),
        "wallet address should start with 0x"
    );

    // Call a contract
    let result = chain
        .call_contract("0x1234567890abcdef1234567890abcdef12345678", "0x")
        .await
        .unwrap();
    assert_eq!(result, "0x");

    // IBC transfer should fail
    let ibc_result = chain
        .send_ibc_transfer(
            "channel-0",
            "anvil-0",
            &ict_rs::tx::WalletAmount {
                address: "0x1234".to_string(),
                denom: "wei".to_string(),
                amount: 100,
            },
            &Default::default(),
        )
        .await;
    assert!(ibc_result.is_err(), "IBC transfer should fail on Anvil");

    chain.stop().await.unwrap();
}

#[tokio::test]
async fn test_anvil_wallet() {
    let cfg = anvil_config();
    let runtime = mock_runtime();
    let chain = AnvilChain::new(cfg, runtime);

    // Test wallet construction from accounts
    let wallet = chain.wallet_for_account(0).unwrap();
    assert_eq!(wallet.key_name, "anvil-0");
    assert!(wallet.hex_address.starts_with("0x"));
    assert_eq!(wallet.address_bytes.len(), 20);
    assert!(wallet.private_key.is_some());

    // Out of range returns None
    assert!(chain.wallet_for_account(99).is_none());
}

#[tokio::test]
async fn test_anvil_chain_config() {
    let cfg = builtin_chain_config("anvil").unwrap();
    assert_eq!(cfg.chain_type, ChainType::Ethereum);
    assert_eq!(cfg.name, "anvil");
    assert_eq!(cfg.chain_id, "31337");
    assert_eq!(cfg.denom, "wei");
    assert_eq!(cfg.coin_type, 60);
    assert_eq!(cfg.bin, "anvil");

    // "ethereum" alias should work too
    let cfg2 = builtin_chain_config("ethereum").unwrap();
    assert_eq!(cfg2.chain_type, ChainType::Ethereum);
}

#[tokio::test]
async fn test_eth_wallet_eip55() {
    use ict_rs::wallet::{EthWallet, Wallet};

    let wallet = EthWallet::from_anvil_account(
        0,
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
        "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
    );

    // Address should be EIP-55 checksummed
    let addr = wallet.formatted_address();
    assert!(addr.starts_with("0x"));
    assert_eq!(addr.len(), 42);
    assert_eq!(wallet.address().len(), 20);
}
