//! Multi-chain IBC transfer example.
//!
//! Demonstrates spinning up two chains with a relayer and performing an IBC transfer.
//! Uses mock runtime — replace with DockerBackend for real usage.
//!
//! ```sh
//! cargo run --example ibc_transfer
//! ```

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::Chain;
use ict_rs::ibc::{ChannelOptions, ChannelOutput, ClientOptions, ConnectionOutput};
use ict_rs::interchain::{Interchain, InterchainBuildOptions, InterchainLink};
use ict_rs::relayer::Relayer;
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::RuntimeBackend;
use ict_rs::spec::builtin_chain_config;
use ict_rs::tx::ExecOutput;
use ict_rs::wallet::{KeyWallet, Wallet};

// A simple mock relayer for the example
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
        println!("  Relayer: linked path '{path_name}' (clients + connections + channel)");
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());

    // 1. Create two chains
    let gaia_config = builtin_chain_config("gaia")?;
    let osmo_config = builtin_chain_config("osmosis")?;

    let gaia = CosmosChain::new(gaia_config, 1, 0, runtime.clone());
    let osmosis = CosmosChain::new(osmo_config, 1, 0, runtime.clone());

    println!("Created chains: {} and {}", gaia.chain_id(), osmosis.chain_id());

    // 2. Create a relayer
    let relayer = ExampleRelayer::new();

    // 3. Build the interchain environment
    let mut ic = Interchain::new(runtime)
        .add_chain(Box::new(gaia))
        .add_chain(Box::new(osmosis))
        .add_relayer("hermes", relayer)
        .add_link(InterchainLink {
            chain1: "cosmoshub-test-1".to_string(),
            chain2: "osmosis-test-1".to_string(),
            relayer: "hermes".to_string(),
            path: "transfer".to_string(),
        });

    println!("\nBuilding interchain environment...");
    let opts = InterchainBuildOptions {
        test_name: "ibc-transfer-example".to_string(),
        skip_path_creation: false,
        ..Default::default()
    };
    ic.build(opts).await?;
    println!("Interchain environment ready!");

    // 4. Query chain heights
    if let Some(gaia) = ic.get_chain("cosmoshub-test-1") {
        let h = gaia.height().await?;
        println!("\nGaia height: {h}");
    }

    if let Some(osmo) = ic.get_chain("osmosis-test-1") {
        let h = osmo.height().await?;
        println!("Osmosis height: {h}");
    }

    // 5. Shutdown
    println!("\nShutting down...");
    ic.close().await?;
    println!("Done!");

    Ok(())
}
