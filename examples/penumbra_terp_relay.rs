//! Terp ↔ local Penumbra ICS-20 round-trip via the Rotko Hermes fork.
//!
//! Host `pd` (kms-signed, loopback) + Docker Terp + host `ibc-hermes`.
//! Does not join penumbra-1 / rotko. Requires the bins below on PATH via env.
//!
//! ```text
//! PD_BIN=… COMETBFT_BIN=… KMS_BIN=… PCLI_BIN=… HERMES_BIN=… \
//!   cargo run -p ict-rs --example penumbra_terp_relay
//! ```

use std::process::Command;

fn main() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../scripts/penumbra_terp_relay.sh"
    );
    let status = Command::new("bash")
        .arg(script)
        .status()
        .expect("spawn penumbra_terp_relay.sh");
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}
