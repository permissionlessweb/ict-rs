//! Local IBC v2 client path: `tx ibc client create` + `add-counterparty`.
//!
//! This is **not** Hermes v1 `create client`. Hermes remains for packets.
//!
//! ```sh
//! ICT_MOCK=1 cargo run -p ict-rs --example ibc_v2 --features testing,terp,docker
//! ```

use std::sync::Arc;

use ict_rs::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mock = std::env::var("ICT_MOCK").ok().as_deref() != Some("0");
    let runtime: Arc<dyn RuntimeBackend> = if mock {
        Arc::new(MockRuntime::new())
    } else {
        IctRuntime::Docker(DockerConfig::default())
            .into_backend()
            .await?
    };

    let mut chain = CosmosChain::new(builtin_chain_config("terp")?, 1, 0, runtime);
    let ctx = TestContext {
        test_name: "ibc-v2-local".to_string(),
        network_id: "ict-ibc-v2-local".to_string(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;

    let create = ibc_v2_create_client_tx_args(
        "/tmp/ibc_v2_client_state.json",
        "/tmp/ibc_v2_cons_state.json",
    );
    let cp = ibc_v2_add_counterparty_tx_args("07-tendermint-0", "08-wasm-0", IBC_V2_MERKLE_PREFIX);
    println!("ibc_v2 create argv: {create:?}");
    println!("ibc_v2 add-counterparty argv: {cp:?}");

    let client_id = chain.create_ibc_v2_tendermint_path("validator").await?;
    if !client_id.starts_with("07-tendermint-") {
        return Err(format!("expected 07-tendermint client, got {client_id}").into());
    }
    println!("ibc_v2 tendermint client {client_id} + counterparty 08-wasm-0");
    chain.stop().await?;
    println!(
        "ibc_v2 create+add-counterparty PASSED (ICT_MOCK={}, no live broadcast)",
        mock
    );
    Ok(())
}
