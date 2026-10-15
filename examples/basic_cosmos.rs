//! Single Terp chain on the image from `TERP_IMAGE_REPO`:`TERP_IMAGE_VERSION`.
//!
//! ```sh
//! cargo run --example basic_cosmos --features docker,terp,testing
//! ```

use ict_rs::chain::Chain;
use ict_rs::testing::{setup_chain, TestEnv};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("basic_cosmos uses the terp-core image. Unset ICT_MOCK and set TERP_IMAGE_REPO and TERP_IMAGE_VERSION.".into());
    }
    let mut tc = setup_chain("basic-cosmos", TestEnv::terp_config()).await?;
    println!("Chain ID: {}", tc.chain.chain_id());
    println!("Binary: {}", tc.chain.config().bin);
    println!("Denom: {}", tc.chain.config().denom);
    println!(
        "Image: {}",
        tc.chain
            .config()
            .images
            .first()
            .map(|i| format!("{}:{}", i.repository, i.version))
            .unwrap_or_default()
    );
    let height = tc.chain.height().await?;
    println!("Current height: {height}");
    if height == 0 {
        tc.cleanup().await?;
        return Err("chain height is 0".into());
    }
    tc.cleanup().await?;
    println!("basic_cosmos passed");
    Ok(())
}
