//! IBC Hooks with CosmWasm memo example.
//!
//! Mirrors `module_ibchooks_test.go` — two Terp chains, deploy a counter
//! contract on chain B, IBC transfer from A with wasm memo that triggers
//! the contract's increment.
//!
//! ```sh
//! cargo run --example ibc_hooks
//! ```

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use ict_rs::auth::generate_mnemonic;
use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::Chain;
use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::ibc::{ChannelOptions, ChannelOutput, ClientOptions, ConnectionOutput};
use ict_rs::interchain::{Interchain, InterchainBuildOptions, InterchainLink};
use ict_rs::relayer::Relayer;
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::RuntimeBackend;
use ict_rs::spec::builtin_chain_config;
use ict_rs::tx::{ExecOutput, TransferOptions, WalletAmount};
use ict_rs::wallet::{KeyWallet, Wallet};

// Inline mock relayer
struct ExampleRelayer {
    configured_chains: Arc<Mutex<Vec<String>>>,
}

impl ExampleRelayer {
    fn new() -> Box<Self> {
        Box::new(Self {
            configured_chains: Arc::new(Mutex::new(Vec::new())),
        })
    }
}

#[async_trait]
impl Relayer for ExampleRelayer {
    async fn add_key(
        &self,
        chain_id: &str,
        key_name: &str,
    ) -> ict_rs::error::Result<Box<dyn Wallet>> {
        Ok(Box::new(KeyWallet {
            key_name: key_name.to_string(),
            address_bytes: vec![0u8; 20],
            bech32_address: format!("cosmos1relayer{chain_id}"),
            mnemonic_phrase: String::new(),
        }))
    }

    async fn restore_key(
        &self,
        _chain_id: &str,
        _key_name: &str,
        _mnemonic: &str,
    ) -> ict_rs::error::Result<()> {
        Ok(())
    }

    fn get_wallet(&self, _chain_id: &str) -> Option<&dyn Wallet> {
        None
    }

    async fn add_chain_configuration(
        &self,
        config: &ict_rs::chain::ChainConfig,
        _key_name: &str,
        _rpc_addr: &str,
        _grpc_addr: &str,
    ) -> ict_rs::error::Result<()> {
        self.configured_chains
            .lock()
            .unwrap()
            .push(config.chain_id.clone());
        Ok(())
    }

    async fn generate_path(
        &self,
        _src: &str,
        _dst: &str,
        _path_name: &str,
    ) -> ict_rs::error::Result<()> {
        Ok(())
    }

    async fn link_path(
        &self,
        _path_name: &str,
        _opts: &ChannelOptions,
    ) -> ict_rs::error::Result<()> {
        Ok(())
    }

    async fn create_clients(
        &self,
        _path_name: &str,
        _opts: &ClientOptions,
    ) -> ict_rs::error::Result<()> {
        Ok(())
    }

    async fn create_connections(&self, _path_name: &str) -> ict_rs::error::Result<()> {
        Ok(())
    }

    async fn create_channel(
        &self,
        _path_name: &str,
        _opts: &ChannelOptions,
    ) -> ict_rs::error::Result<()> {
        Ok(())
    }

    async fn update_clients(&self, _path_name: &str) -> ict_rs::error::Result<()> {
        Ok(())
    }

    async fn start(&self, _path_names: &[&str]) -> ict_rs::error::Result<()> {
        Ok(())
    }

    async fn stop(&self) -> ict_rs::error::Result<()> {
        Ok(())
    }

    async fn flush(&self, _path_name: &str, _channel_id: &str) -> ict_rs::error::Result<()> {
        Ok(())
    }

    async fn get_channels(&self, _chain_id: &str) -> ict_rs::error::Result<Vec<ChannelOutput>> {
        Ok(Vec::new())
    }

    async fn get_connections(
        &self,
        _chain_id: &str,
    ) -> ict_rs::error::Result<Vec<ConnectionOutput>> {
        Ok(Vec::new())
    }

    async fn exec(
        &self,
        _cmd: &[&str],
        _env: &[(&str, &str)],
    ) -> ict_rs::error::Result<ExecOutput> {
        Ok(ExecOutput::default())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== IBC Hooks Test ===\n");

    let runtime: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());

    // 1. Create two Terp chains
    let mut config_a = builtin_chain_config("terp")?;
    config_a.chain_id = "terp-test-1".to_string();
    let mut config_b = builtin_chain_config("terp")?;
    config_b.chain_id = "counterparty-2".to_string();

    let chain_a = CosmosChain::new(config_a, 1, 0, runtime.clone());
    let chain_b = CosmosChain::new(config_b, 1, 0, runtime.clone());

    println!("Chains: {} and {}", chain_a.chain_id(), chain_b.chain_id());

    // 2. Build interchain
    let relayer = ExampleRelayer::new();
    let mut ic = Interchain::new(runtime)
        .add_chain(Box::new(chain_a))
        .add_chain(Box::new(chain_b))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink {
            chain1: "terp-test-1".to_string(),
            chain2: "counterparty-2".to_string(),
            relayer: "hermes".to_string(),
            path: "transfer".to_string(),
        });

    ic.build(InterchainBuildOptions {
        test_name: "ibc-hooks".to_string(),
        ..Default::default()
    })
    .await?;
    println!("Interchain environment ready!\n");

    // 3. Get references and create + fund users
    let chain_a_ref = ic.get_chain("terp-test-1").unwrap();
    let chain_b_ref = ic.get_chain("counterparty-2").unwrap();

    let user_a = KeyWallet::from_mnemonic("user-a-0", &generate_mnemonic(), "terp", 118)?;
    let user_b = KeyWallet::from_mnemonic("user-b-0", &generate_mnemonic(), "terp", 118)?;
    for (chain, user) in [(chain_a_ref, &user_a), (chain_b_ref, &user_b)] {
        let fund = WalletAmount {
            address: user.bech32_address.clone(),
            denom: "uterp".to_string(),
            amount: 10_000_000_000,
        };
        chain.send_funds("validator-0", &fund).await?;
    }

    // 4. Deploy ibchooks_counter.wasm on chain B
    println!("--- Deploy Counter Contract on Chain B ---");
    let code_id = chain_b_ref
        .store_code(&user_b.key_name, "ibchooks_counter.wasm")
        .await?;
    println!("  Stored code: {}", code_id);

    let init_msg = r#"{"count":0}"#;
    let contract_addr = chain_b_ref
        .instantiate_contract(&user_b.key_name, &code_id, init_msg, "ibchooks-counter", None)
        .await?;
    println!("  Contract address: {}", contract_addr);

    // 5. IBC transfer A -> B with wasm memo (first -- creates IBC-hooks account)
    println!("\n--- IBC Transfer with Wasm Memo (1st) ---");
    let wasm_memo = serde_json::json!({
        "wasm": {
            "contract": contract_addr,
            "msg": {"increment": {}}
        }
    });
    let transfer_amount = WalletAmount {
        address: user_b.bech32_address.clone(),
        denom: "uterp".to_string(),
        amount: 1000,
    };
    let opts = TransferOptions {
        memo: Some(wasm_memo.to_string()),
        ..Default::default()
    };
    let tx1 = chain_a_ref
        .send_ibc_transfer("channel-0", &user_a.key_name, &transfer_amount, &opts)
        .await?;
    println!("  Transfer 1 tx: {}", tx1.tx_hash);

    // 6. Second transfer (increments counter)
    println!("\n--- IBC Transfer with Wasm Memo (2nd) ---");
    let tx2 = chain_a_ref
        .send_ibc_transfer("channel-0", &user_a.key_name, &transfer_amount, &opts)
        .await?;
    println!("  Transfer 2 tx: {}", tx2.tx_hash);

    // 7. Query contract counter
    println!("\n--- Query Counter ---");
    let query_msg = r#"{"get_count":{}}"#;
    let result = chain_b_ref.query_contract(&contract_addr, query_msg).await?;
    let count = result["data"]["count"].as_u64().unwrap_or(0);
    println!("  Counter value: {}", count);
    assert!(count >= 1, "counter should be at least 1");

    // 8. Shutdown
    println!("\n--- Shutdown ---");
    ic.close().await?;
    println!("IBC Hooks test passed!");

    Ok(())
}
