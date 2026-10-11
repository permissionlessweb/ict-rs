//! Fresh local Penumbra validator that signs consensus with cosmos/kms.
//!
//! Does not join a public network. Requires `PD_BIN`, `COMETBFT_BIN` (v0.37),
//! and `KMS_BIN` (the `local/kms-cometbft-0.37` build) on PATH via the env.
//!
//! ```text
//! cargo run -p ict-rs --example penumbra_kms_local
//! ```

use std::process::Command;

fn main() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../scripts/penumbra_kms_local.sh");
    let status = Command::new("bash")
        .arg(script)
        .status()
        .expect("spawn penumbra_kms_local.sh");
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}
