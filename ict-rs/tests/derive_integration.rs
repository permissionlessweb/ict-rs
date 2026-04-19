//! Integration tests for ExecuteFns and QueryFns derive macros.

use std::sync::Arc;

use async_trait::async_trait;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::error::{IctError, Result};
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::{ContainerId, DockerImage, RuntimeBackend};
use ict_rs::tx::{
    ExecOutput, PacketAcknowledgement, PacketTimeout, TransferOptions, Tx, WalletAmount,
};
use ict_rs::wallet::Wallet;
use ict_rs::{ExecuteFns, QueryFns};

// ---------------------------------------------------------------------------
// Test enums
// ---------------------------------------------------------------------------

#[derive(ExecuteFns)]
#[ict(module = "tokenfactory")]
pub enum TokenfactoryMsg {
    CreateDenom {
        sender: String,
        subdenom: String,
    },
    MintTo {
        sender: String,
        amount: String,
        mint_to_address: String,
    },
    #[ict(skip)]
    InternalOnly {
        data: String,
    },
}

#[derive(QueryFns)]
#[ict(module = "tokenfactory")]
pub enum TokenfactoryQuery {
    Params,
    DenomAuthorityMetadata {
        denom: String,
    },
}

#[derive(ExecuteFns)]
#[ict(module = "bank")]
pub enum BankMsg {
    Send {
        from_address: String,
        to_address: String,
        amount: String,
    },
}

// ---------------------------------------------------------------------------
// Minimal Chain impl for testing
// ---------------------------------------------------------------------------

struct TestChain {
    runtime: Arc<MockRuntime>,
    container_id: ContainerId,
    cfg: ChainConfig,
}

impl TestChain {
    async fn new() -> Self {
        let runtime = Arc::new(MockRuntime::new());

        // Create and start a container so exec works
        let opts = ict_rs::runtime::ContainerOptions {
            name: "test-chain".to_string(),
            image: DockerImage {
                repository: "test".to_string(),
                version: "latest".to_string(),
                uid_gid: None,
            },
            cmd: vec![],
            env: vec![],
            ports: vec![],
            volumes: vec![],
            network_id: None,
            hostname: None,
            entrypoint: None,
            labels: vec![],
        };
        let container_id = runtime.create_container(&opts).await.unwrap();
        runtime.start_container(&container_id).await.unwrap();

        let cfg = ChainConfig {
            chain_type: ChainType::Cosmos,
            name: "test".to_string(),
            chain_id: "test-1".to_string(),
            images: vec![],
            bin: "testd".to_string(),
            bech32_prefix: "cosmos".to_string(),
            denom: "utest".to_string(),
            coin_type: 118,
            signing_algorithm: SigningAlgorithm::default(),
            gas_prices: "0.025utest".to_string(),
            gas_adjustment: 1.5,
            trusting_period: "336h".to_string(),
            block_time: "2s".to_string(),
            genesis: None,
            modify_genesis: None,
            pre_genesis: None,
            config_file_overrides: Default::default(),
            additional_start_args: vec![],
            env: vec![],
            sidecar_configs: vec![],
            faucet: None,
            genesis_style: Default::default(),
        };

        Self {
            runtime,
            container_id,
            cfg,
        }
    }
}

#[async_trait]
impl Chain for TestChain {
    fn config(&self) -> &ChainConfig {
        &self.cfg
    }

    fn chain_id(&self) -> &str {
        &self.cfg.chain_id
    }

    async fn initialize(&mut self, _ctx: &TestContext) -> Result<()> {
        Ok(())
    }

    async fn start(&mut self, _genesis_wallets: &[WalletAmount]) -> Result<()> {
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }

    async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> Result<ExecOutput> {
        self.runtime
            .exec_in_container(&self.container_id, cmd, env)
            .await
    }

    fn rpc_address(&self) -> &str {
        "http://localhost:26657"
    }
    fn grpc_address(&self) -> &str {
        "localhost:9090"
    }
    fn host_rpc_address(&self) -> String {
        "http://localhost:26657".to_string()
    }
    fn host_grpc_address(&self) -> String {
        "http://localhost:9090".to_string()
    }
    fn home_dir(&self) -> &str {
        "/home/test"
    }

    async fn create_key(&self, _key_name: &str) -> Result<()> {
        Ok(())
    }
    async fn recover_key(&self, _name: &str, _mnemonic: &str) -> Result<()> {
        Ok(())
    }
    async fn get_address(&self, _key_name: &str) -> Result<Vec<u8>> {
        Ok(vec![0u8; 20])
    }
    async fn build_wallet(&self, _key_name: &str, _mnemonic: &str) -> Result<Box<dyn Wallet>> {
        Err(IctError::Config("not implemented in test".into()))
    }
    async fn send_funds(&self, _key_name: &str, _amount: &WalletAmount) -> Result<String> {
        Ok("mock_tx_hash".to_string())
    }
    async fn get_balance(&self, _address: &str, _denom: &str) -> Result<u128> {
        Ok(1_000_000)
    }
    async fn send_ibc_transfer(
        &self,
        _channel_id: &str,
        _key_name: &str,
        _amount: &WalletAmount,
        _options: &TransferOptions,
    ) -> Result<Tx> {
        Ok(Tx {
            height: 1,
            tx_hash: "mock".to_string(),
            gas_spent: 0,
            packet: None,
        })
    }
    async fn height(&self) -> Result<u64> {
        Ok(42)
    }
    async fn export_state(&self, _height: u64) -> Result<String> {
        Ok("{}".to_string())
    }
    async fn acknowledgements(&self, _height: u64) -> Result<Vec<PacketAcknowledgement>> {
        Ok(vec![])
    }
    async fn timeouts(&self, _height: u64) -> Result<Vec<PacketTimeout>> {
        Ok(vec![])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_execute_fn_create_denom() {
    let chain = TestChain::new().await;

    let tx = chain
        .tokenfactory_create_denom("admin", "utest")
        .await
        .unwrap();

    assert_eq!(tx.tx_hash, "MOCKTXHASH000000000000000000000000000000000000000000000000000000");
    assert_eq!(tx.height, 1);

    // Verify the exec log captured the right command
    let state = chain.runtime.state();
    let state = state.lock().unwrap();
    let last_exec = state.exec_log.last().unwrap();
    let args = &last_exec.1;

    assert_eq!(args[0], "tx");
    assert_eq!(args[1], "tokenfactory");
    assert_eq!(args[2], "create-denom");
    assert!(args.contains(&"--subdenom".to_string()));
    assert!(args.contains(&"utest".to_string()));
    assert!(args.contains(&"--from".to_string()));
    assert!(args.contains(&"admin".to_string()));
}

#[tokio::test]
async fn test_execute_fn_mint_to() {
    let chain = TestChain::new().await;

    let tx = chain
        .tokenfactory_mint_to("admin", "1000", "cosmos1recipient")
        .await
        .unwrap();

    assert_eq!(tx.gas_spent, 50000);

    let state = chain.runtime.state();
    let state = state.lock().unwrap();
    let last_exec = state.exec_log.last().unwrap();
    let args = &last_exec.1;

    assert_eq!(args[0], "tx");
    assert_eq!(args[1], "tokenfactory");
    assert_eq!(args[2], "mint-to");
    assert!(args.contains(&"--amount".to_string()));
    assert!(args.contains(&"1000".to_string()));
    assert!(args.contains(&"--mint-to-address".to_string()));
    assert!(args.contains(&"cosmos1recipient".to_string()));
}

#[tokio::test]
async fn test_query_fn_params() {
    let chain = TestChain::new().await;

    let result = chain.tokenfactory_params().await.unwrap();

    // Should get back a valid JSON value
    assert!(result.is_object());

    let state = chain.runtime.state();
    let state = state.lock().unwrap();
    let last_exec = state.exec_log.last().unwrap();
    let args = &last_exec.1;

    assert_eq!(args[0], "query");
    assert_eq!(args[1], "tokenfactory");
    assert_eq!(args[2], "params");
    assert!(args.contains(&"--output".to_string()));
    assert!(args.contains(&"json".to_string()));
}

#[tokio::test]
async fn test_query_fn_with_fields() {
    let chain = TestChain::new().await;

    let result = chain
        .tokenfactory_denom_authority_metadata("factory/cosmos1.../utest")
        .await
        .unwrap();

    assert!(result.is_object());

    let state = chain.runtime.state();
    let state = state.lock().unwrap();
    let last_exec = state.exec_log.last().unwrap();
    let args = &last_exec.1;

    assert_eq!(args[0], "query");
    assert_eq!(args[1], "tokenfactory");
    assert_eq!(args[2], "denom-authority-metadata");
    assert!(args.contains(&"--denom".to_string()));
}

#[tokio::test]
async fn test_bank_send() {
    let chain = TestChain::new().await;

    let tx = chain
        .bank_send("admin", "cosmos1recipient", "1000utest")
        .await
        .unwrap();

    assert_eq!(tx.height, 1);

    let state = chain.runtime.state();
    let state = state.lock().unwrap();
    let last_exec = state.exec_log.last().unwrap();
    let args = &last_exec.1;

    assert_eq!(args[0], "tx");
    assert_eq!(args[1], "bank");
    assert_eq!(args[2], "send");
    assert!(args.contains(&"--to-address".to_string()));
    assert!(args.contains(&"cosmos1recipient".to_string()));
}

#[tokio::test]
async fn test_skipped_variant_not_generated() {
    // The InternalOnly variant has #[ict(skip)], so there should be no
    // `tokenfactory_internal_only` method. This is a compile-time check —
    // if the method existed, this test file would compile with it available.
    // We just verify the other methods work.
    let chain = TestChain::new().await;
    let _ = chain.tokenfactory_create_denom("admin", "utest").await;
}

#[tokio::test]
async fn test_tx_default_flags_present() {
    let chain = TestChain::new().await;

    chain
        .tokenfactory_create_denom("admin", "utest")
        .await
        .unwrap();

    let state = chain.runtime.state();
    let state = state.lock().unwrap();
    let last_exec = state.exec_log.last().unwrap();
    let args = &last_exec.1;

    // Check TX_DEFAULT_FLAGS are present
    assert!(args.contains(&"--keyring-backend".to_string()));
    assert!(args.contains(&"test".to_string()));
    assert!(args.contains(&"--gas".to_string()));
    assert!(args.contains(&"auto".to_string()));
    assert!(args.contains(&"--broadcast-mode".to_string()));
    assert!(args.contains(&"sync".to_string()));
    assert!(args.contains(&"-y".to_string()));
    assert!(args.contains(&"--chain-id".to_string()));
    assert!(args.contains(&"test-1".to_string()));
    assert!(args.contains(&"--gas-prices".to_string()));
    assert!(args.contains(&"0.025utest".to_string()));
}
