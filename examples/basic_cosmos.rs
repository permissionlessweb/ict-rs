//! Basic example: spin up a single Cosmos chain and interact with it.
//!
//! This example uses the mock runtime so it runs without Docker.
//! Replace `MockRuntime` with `DockerBackend` for real container usage.
//!
//! ```sh
//! cargo run --example basic_cosmos
//! ```

use std::sync::Arc;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, TestContext};
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::RuntimeBackend;
use ict_rs::spec::builtin_chain_config;
use ict_rs::tx::WalletAmount;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Create a runtime backend (mock for this example)
    let runtime: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());

    // 2. Load a built-in chain config and create a chain
    let config = builtin_chain_config("terp")?;
    let mut chain = CosmosChain::new(config, 1, 0, runtime);

    println!("Chain ID: {}", chain.chain_id());
    println!("Binary: {}", chain.config().bin);
    println!("Denom: {}", chain.config().denom);

    // 3. Initialize the chain (creates containers, inits node configs)
    let ctx = TestContext {
        test_name: "basic-cosmos-example".to_string(),
        network_id: "example-net".to_string(),
    };
    chain.initialize(&ctx).await?;
    println!("Chain initialized with {} validator(s)", chain.validators().len());

    // 4. Start the chain with genesis-funded wallets
    let genesis_wallets = vec![WalletAmount {
        address: "terp1faucet000000000000000000000000".to_string(),
        denom: "uterp".to_string(),
        amount: 10_000_000_000,
    }];
    chain.start(&genesis_wallets).await?;
    println!("Chain started!");

    // 5. Query chain state
    let height = chain.height().await?;
    println!("Current height: {height}");

    // 6. Stop and cleanup
    chain.stop().await?;
    println!("Chain stopped. Done!");

    Ok(())
}
