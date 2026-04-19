//! Faucet extension trait for chains with an in-container faucet.
//!
//! Provides `faucet_fund()` and `faucet_status()` as convenience methods
//! on any type implementing [`Chain`] that has a [`FaucetConfig`] set.

use async_trait::async_trait;

use crate::chain::Chain;
use crate::error::{IctError, Result};

/// Extension trait for in-container faucet operations on any chain.
///
/// Blanket-implemented for all `T: Chain`, so any chain type with a faucet
/// configured automatically gains faucet functionality.
#[async_trait]
pub trait FaucetExt: Chain {
    /// Request tokens from the faucet for the given address.
    /// Returns the raw JSON response body on success.
    async fn faucet_fund(&self, address: &str) -> Result<String> {
        let port = self
            .config()
            .faucet
            .as_ref()
            .ok_or_else(|| IctError::Config("no faucet configured".into()))?
            .port;
        let cmd = format!(
            "curl -sf 'http://localhost:{port}/faucet?address={address}'"
        );
        let output = self.exec(&["sh", "-c", &cmd], &[]).await?;
        if output.exit_code != 0 {
            return Err(IctError::ExecFailed {
                exit_code: output.exit_code,
                stderr: output.stderr_str(),
            });
        }
        Ok(output.stdout_str().trim().to_string())
    }

    /// Query faucet status (address, amount, denoms).
    async fn faucet_status(&self) -> Result<serde_json::Value> {
        let port = self
            .config()
            .faucet
            .as_ref()
            .ok_or_else(|| IctError::Config("no faucet configured".into()))?
            .port;
        let cmd = format!("curl -sf 'http://localhost:{port}/status'");
        let output = self.exec(&["sh", "-c", &cmd], &[]).await?;
        if output.exit_code != 0 {
            return Err(IctError::ExecFailed {
                exit_code: output.exit_code,
                stderr: output.stderr_str(),
            });
        }
        let json: serde_json::Value =
            serde_json::from_str(output.stdout_str().trim()).map_err(|e| {
                IctError::Config(format!("invalid faucet status JSON: {e}"))
            })?;
        Ok(json)
    }
}

/// Blanket implementation for all chain types.
impl<T: Chain + ?Sized> FaucetExt for T {}
