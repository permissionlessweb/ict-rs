//! Oracle price seeding for local Akash BME testing.
//!
//! On mainnet, oracle prices are submitted by authorized feeders. In local
//! test environments, we run a background price feeder so the BME module
//! can calculate collateral ratios and allow AKT→ACT minting.
//!
//! Requires the validator to be registered as an oracle source in genesis
//! (see `modify_akash_genesis` in `akash.rs`).

use crate::chain::Chain;
use crate::error::Result;

/// Start a continuous background oracle price feeder inside the container.
///
/// 1. Launches a background shell loop that feeds every 5 seconds. The first
///    feed happens immediately (no initial sleep) so prices start accumulating
///    right away.
/// 2. Submits synchronous feeds with short pauses to seed the TWAP window.
///    Some feeds may hit "sequence mismatch" when racing with the background
///    loop — that's fine, enough will land.
/// 3. Queries oracle state for diagnostics.
pub async fn start_oracle_price_feeder(
    chain: &(impl Chain + ?Sized),
) -> Result<()> {
    let bin = &chain.config().bin;
    let home = chain.home_dir();
    let chain_id = chain.chain_id();
    let gas_prices = &chain.config().gas_prices;

    // ── 1. Start continuous background feeder ────────────────────────────
    // First feed fires immediately; subsequent feeds every 5 seconds.
    // Output goes to a log file for debugging (not /dev/null).
    let feeder_log = format!("{}/oracle-feeder.log", home);
    let script = format!(
        "nohup sh -c 'while true; do \
            {bin} tx oracle feed akt usd 1.000000000000000000 \
                \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\" \
                --from validator \
                --keyring-backend test \
                --chain-id {chain_id} \
                --gas-prices {gas_prices} \
                --gas auto --gas-adjustment 1.5 \
                --broadcast-mode sync \
                --output json -y \
                --home {home} >> {feeder_log} 2>&1; \
            sleep 5; \
        done' > /dev/null 2>&1 &"
    );
    chain.exec(&["sh", "-c", &script], &[]).await?;
    tracing::info!("background oracle price feeder started (5s interval)");

    // ── 2. Synchronous feeds to seed TWAP ────────────────────────────────
    // Feed 3 times with 2 s gaps. Occasional "sequence mismatch" from
    // racing with the background loop is expected and harmless — we only
    // need enough feeds to populate the TWAP window.
    for i in 0..3u32 {
        let date_output = chain.exec(&["date", "-u", "+%Y-%m-%dT%H:%M:%SZ"], &[]).await?;
        let ts = date_output.stdout_str().trim().to_string();
        let opts = chain.default_tx_opts().from("validator");
        let output = chain
            .chain_exec_tx_with(
                &["tx", "oracle", "feed", "akt", "usd", "1.000000000000000000", &ts],
                opts,
            )
            .await?;
        if output.exit_code != 0 {
            let stderr = output.stderr_str();
            // Sequence mismatch from racing with background feeder — not fatal
            if stderr.contains("sequence mismatch") {
                tracing::warn!(feed = i, "oracle feed sequence mismatch (expected, retrying)");
            } else {
                tracing::error!(stderr = %stderr, feed = i, "oracle feed failed");
                return Err(crate::error::IctError::ExecFailed {
                    exit_code: output.exit_code,
                    stderr: format!("oracle feed {} failed: {}", i, stderr),
                });
            }
        } else {
            tracing::info!(feed = i, timestamp = %ts, "oracle price feed submitted");
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    // ── 3. Diagnostics ───────────────────────────────────────────────────
    let output = chain
        .chain_exec(&["query", "oracle", "params", "--output", "json"])
        .await?;
    tracing::info!(oracle_params = %output.stdout_str().trim(), "oracle params");

    let output = chain
        .chain_exec(&["query", "oracle", "prices", "--output", "json"])
        .await?;
    tracing::info!(oracle_prices = %output.stdout_str().trim(), "oracle raw prices");

    let output = chain
        .chain_exec(&["query", "oracle", "aggregated-price", "akt", "--output", "json"])
        .await?;
    tracing::info!(aggregated_price = %output.stdout_str().trim(), "oracle aggregated price");

    Ok(())
}
