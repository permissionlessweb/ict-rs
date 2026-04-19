//! Packet Forward Middleware (PFM) 4-chain multi-hop example.
//!
//! Mirrors `module_pfm_test.go` — 4 Terp chains (A through D), 3 IBC links,
//! forward tokens A → B → C → D using nested PFM memo.
//!
//! ```sh
//! cargo run --example pfm
//! ```

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use ict_rs::auth::generate_mnemonic;
use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::Chain;
use ict_rs::ibc::{ibc_denom_multi_hop, ChannelOptions, ChannelOutput, ClientOptions, ConnectionOutput};
use ict_rs::interchain::{Interchain, InterchainBuildOptions, InterchainLink};
use ict_rs::relayer::Relayer;
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::RuntimeBackend;
use ict_rs::spec::builtin_chain_config;
use ict_rs::tx::{ExecOutput, TransferOptions, WalletAmount};
use ict_rs::wallet::{KeyWallet, Wallet};

// ---------------------------------------------------------------------------
// Inline mock relayer (same pattern as ibc_transfer.rs / ibc_hooks.rs)
// ---------------------------------------------------------------------------
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
        println!("  Relayer: configured chain {}", config.chain_id);
        Ok(())
    }

    async fn generate_path(
        &self,
        src: &str,
        dst: &str,
        path_name: &str,
    ) -> ict_rs::error::Result<()> {
        println!("  Relayer: generated path '{path_name}' ({src} <-> {dst})");
        Ok(())
    }

    async fn link_path(
        &self,
        path_name: &str,
        _opts: &ChannelOptions,
    ) -> ict_rs::error::Result<()> {
        println!("  Relayer: linked path '{path_name}'");
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

    async fn start(&self, path_names: &[&str]) -> ict_rs::error::Result<()> {
        println!(
            "  Relayer: started on paths: {}",
            path_names.join(", ")
        );
        Ok(())
    }

    async fn stop(&self) -> ict_rs::error::Result<()> {
        println!("  Relayer: stopped");
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

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== PFM Multi-Hop Transfer Test ===\n");

    let runtime: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());

    // 1. Create 4 chains with distinct chain IDs
    let mut config_a = builtin_chain_config("terp")?;
    config_a.chain_id = "chain-a".to_string();
    let mut config_b = builtin_chain_config("terp")?;
    config_b.chain_id = "chain-b".to_string();
    let mut config_c = builtin_chain_config("terp")?;
    config_c.chain_id = "chain-c".to_string();
    let mut config_d = builtin_chain_config("terp")?;
    config_d.chain_id = "chain-d".to_string();

    let chain_a = CosmosChain::new(config_a, 1, 0, runtime.clone());
    let chain_b = CosmosChain::new(config_b, 1, 0, runtime.clone());
    let chain_c = CosmosChain::new(config_c, 1, 0, runtime.clone());
    let chain_d = CosmosChain::new(config_d, 1, 0, runtime.clone());

    println!(
        "Chains: {}, {}, {}, {}",
        chain_a.chain_id(),
        chain_b.chain_id(),
        chain_c.chain_id(),
        chain_d.chain_id()
    );

    // 2. Build interchain with 3 links (A<->B, B<->C, C<->D)
    let relayer = ExampleRelayer::new();
    let mut ic = Interchain::new(runtime)
        .add_chain(Box::new(chain_a))
        .add_chain(Box::new(chain_b))
        .add_chain(Box::new(chain_c))
        .add_chain(Box::new(chain_d))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink {
            chain1: "chain-a".to_string(),
            chain2: "chain-b".to_string(),
            relayer: "hermes".to_string(),
            path: "ab-transfer".to_string(),
        })
        .add_link(InterchainLink {
            chain1: "chain-b".to_string(),
            chain2: "chain-c".to_string(),
            relayer: "hermes".to_string(),
            path: "bc-transfer".to_string(),
        })
        .add_link(InterchainLink {
            chain1: "chain-c".to_string(),
            chain2: "chain-d".to_string(),
            relayer: "hermes".to_string(),
            path: "cd-transfer".to_string(),
        });

    println!("\nBuilding interchain environment (4 chains, 3 paths)...");
    ic.build(InterchainBuildOptions {
        test_name: "pfm-test".to_string(),
        ..Default::default()
    })
    .await?;
    println!("Interchain environment ready!\n");

    // 3. Create and fund test users on all chains
    let ref_a = ic.get_chain("chain-a").unwrap();
    let ref_b = ic.get_chain("chain-b").unwrap();
    let ref_c = ic.get_chain("chain-c").unwrap();
    let ref_d = ic.get_chain("chain-d").unwrap();

    let user_a = KeyWallet::from_mnemonic("user-a-0", &generate_mnemonic(), "terp", 118)?;
    let user_b = KeyWallet::from_mnemonic("user-b-0", &generate_mnemonic(), "terp", 118)?;
    let user_c = KeyWallet::from_mnemonic("user-c-0", &generate_mnemonic(), "terp", 118)?;
    let user_d = KeyWallet::from_mnemonic("user-d-0", &generate_mnemonic(), "terp", 118)?;

    for (chain_ref, user) in [(ref_a, &user_a), (ref_b, &user_b), (ref_c, &user_c), (ref_d, &user_d)] {
        let fund = WalletAmount {
            address: user.bech32_address.clone(),
            denom: "uterp".to_string(),
            amount: 10_000_000_000,
        };
        chain_ref.send_funds("validator-0", &fund).await?;
    }

    println!("Users funded on all 4 chains");
    println!("  A: {}", user_a.bech32_address);
    println!("  B: {}", user_b.bech32_address);
    println!("  C: {}", user_c.bech32_address);
    println!("  D: {}", user_d.bech32_address);

    // 4. Compute expected multi-hop IBC denom on chain D
    //    Token goes: A --channel-0--> B --channel-1--> C --channel-2--> D
    let expected_denom_on_d = ibc_denom_multi_hop(
        &[
            ("transfer", "channel-0"),
            ("transfer", "channel-1"),
            ("transfer", "channel-2"),
        ],
        "uterp",
    );
    println!("\n--- Multi-Hop IBC Denom ---");
    println!("  Expected on D: {}", expected_denom_on_d);

    // 5. Build nested PFM memo for A -> B -> C -> D
    let pfm_memo = serde_json::json!({
        "forward": {
            "receiver": user_c.bech32_address,
            "port": "transfer",
            "channel": "channel-1",
            "next": {
                "forward": {
                    "receiver": user_d.bech32_address,
                    "port": "transfer",
                    "channel": "channel-2"
                }
            }
        }
    });
    println!("\n--- PFM Memo ---");
    println!("{}", serde_json::to_string_pretty(&pfm_memo)?);

    // 6. Send IBC transfer A -> B with PFM memo
    //    The immediate receiver on chain B is user_b; PFM on B forwards to C, then D.
    println!("\n--- Sending Multi-Hop Transfer A -> D ---");
    let transfer_amount = WalletAmount {
        address: user_b.bech32_address.clone(),
        denom: "uterp".to_string(),
        amount: 5000,
    };
    let opts = TransferOptions {
        memo: Some(pfm_memo.to_string()),
        ..Default::default()
    };
    let tx = ref_a
        .send_ibc_transfer("channel-0", &user_a.key_name, &transfer_amount, &opts)
        .await?;
    println!("  Transfer tx: {} (height: {})", tx.tx_hash, tx.height);

    // 7. Check balances
    let bal_a = ref_a
        .get_balance(&user_a.bech32_address, "uterp")
        .await?;
    let bal_d = ref_d
        .get_balance(&user_d.bech32_address, &expected_denom_on_d)
        .await?;
    println!("\n--- Final Balances ---");
    println!("  Chain A user: {} uterp", bal_a);
    println!("  Chain D user: {} {}", bal_d, expected_denom_on_d);

    // 8. Shutdown
    println!("\n--- Shutdown ---");
    ic.close().await?;
    println!("PFM multi-hop transfer test passed!");

    Ok(())
}
