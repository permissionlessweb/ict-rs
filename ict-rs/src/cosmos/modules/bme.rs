//! BME (Burn-Mint Equilibrium) module extension traits for Akash.
//!
//! Provides CLI-wrapped transaction and query methods for the BME module.
//!
//! **Important:** The BME module requires a minimum mint of 10 ACT (10_000_000 uact).
//! Callers must ensure the amount meets this floor.

/// Transaction methods for the BME module.
#[crate::cli::async_trait]
pub trait BmeMsgExt: crate::chain::Chain {
    /// Burn AKT to mint ACT.
    ///
    /// Wraps: `akash tx bme mint-act <amount> --from <key_name>`
    ///
    /// **Note:** The BME module requires a minimum mint of 10 ACT (10_000_000 uact).
    async fn bme_mint_act(
        &self,
        key_name: &str,
        amount: &str,
    ) -> crate::error::Result<crate::tx::Tx> {
        let opts = self.default_tx_opts().from(key_name);
        let output = self
            .chain_exec_tx_with(&["tx", "bme", "mint-act", amount], opts)
            .await?;
        crate::cli::parse_tx_response(&output)
    }

    /// Burn ACT back to AKT.
    ///
    /// Wraps: `akash tx bme burn-act <amount> --from <key_name>`
    async fn bme_burn_act(
        &self,
        key_name: &str,
        amount: &str,
    ) -> crate::error::Result<crate::tx::Tx> {
        let opts = self.default_tx_opts().from(key_name);
        let output = self
            .chain_exec_tx_with(&["tx", "bme", "burn-act", amount], opts)
            .await?;
        crate::cli::parse_tx_response(&output)
    }

    /// Burn one denom to mint another (generic burn-mint).
    ///
    /// Wraps: `akash tx bme burn-mint <amount> --to-denom <to_denom> --from <key_name>`
    async fn bme_burn_mint(
        &self,
        key_name: &str,
        amount: &str,
        to_denom: &str,
    ) -> crate::error::Result<crate::tx::Tx> {
        let opts = self.default_tx_opts().from(key_name);
        let output = self
            .chain_exec_tx_with(
                &["tx", "bme", "burn-mint", amount, "--to-denom", to_denom],
                opts,
            )
            .await?;
        crate::cli::parse_tx_response(&output)
    }
}

impl<T: crate::chain::Chain + ?Sized> BmeMsgExt for T {}

/// Query methods for the BME module.
#[crate::cli::async_trait]
pub trait BmeQueryExt: crate::chain::Chain {
    /// Query BME circuit breaker status.
    ///
    /// Wraps: `akash query bme status`
    async fn bme_status(&self) -> crate::error::Result<serde_json::Value> {
        let mut args: Vec<String> = vec![
            "query".to_string(),
            "bme".to_string(),
            "status".to_string(),
        ];
        for flag in crate::cli::QUERY_DEFAULT_FLAGS {
            args.push(flag.to_string());
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.exec(&arg_refs, &[]).await?;
        crate::cli::parse_query_response(&output)
    }

    /// Query BME module parameters.
    ///
    /// Wraps: `akash query bme params`
    async fn bme_params(&self) -> crate::error::Result<serde_json::Value> {
        let mut args: Vec<String> = vec![
            "query".to_string(),
            "bme".to_string(),
            "params".to_string(),
        ];
        for flag in crate::cli::QUERY_DEFAULT_FLAGS {
            args.push(flag.to_string());
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.exec(&arg_refs, &[]).await?;
        crate::cli::parse_query_response(&output)
    }

    /// Query BME vault state.
    ///
    /// Wraps: `akash query bme vault-state`
    async fn bme_vault_state(&self) -> crate::error::Result<serde_json::Value> {
        let mut args: Vec<String> = vec![
            "query".to_string(),
            "bme".to_string(),
            "vault-state".to_string(),
        ];
        for flag in crate::cli::QUERY_DEFAULT_FLAGS {
            args.push(flag.to_string());
        }
        let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.exec(&arg_refs, &[]).await?;
        crate::cli::parse_query_response(&output)
    }
}

impl<T: crate::chain::Chain + ?Sized> BmeQueryExt for T {}
