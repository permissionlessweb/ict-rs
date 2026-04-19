//! Programmatic transaction construction, signing, and broadcasting.
//!
//! `TxBuilder` constructs transactions using `cosmrs` types and the
//! [`Authenticator`](crate::auth::Authenticator) trait, then broadcasts via
//! Tendermint RPC — no Docker exec or CLI involved.
//!
//! ## Middleware
//!
//! Two open trait systems allow users to inject custom logic:
//!
//! - [`TxMiddlewareBody`] — transform the `TxBody` before signing (e.g. inject
//!   SDK 0.50+ `extension_options`, add memo, etc.)
//! - [`TxMiddlewareResp`] — transform the response after broadcast (e.g.
//!   logging, retry, metrics)

use async_trait::async_trait;
use prost::Message;
use tracing::debug;

use crate::auth::Authenticator;
use crate::chain::ChainConfig;
use crate::error::{IctError, Result};

/// Response from a successfully broadcast transaction.
#[derive(Debug, Clone)]
pub struct TxResponse {
    /// Block height (0 for async/sync broadcast modes until confirmed).
    pub height: u64,
    /// Transaction hash.
    pub tx_hash: String,
    /// Gas used (if available).
    pub gas_used: u64,
    /// Raw broadcast response code. 0 = success.
    pub code: u32,
    /// Raw log from the broadcast response.
    pub raw_log: String,
}

// ---------------------------------------------------------------------------
// Middleware traits
// ---------------------------------------------------------------------------

/// Transform the tx body before signing.
///
/// Use this to inject `extension_options`, modify the memo, or add
/// non-critical extension options.
#[async_trait]
pub trait TxMiddlewareBody: Send + Sync {
    async fn map_body(&self, body: cosmrs::tx::Body) -> Result<cosmrs::tx::Body>;
}

/// Transform the tx response after broadcast.
///
/// Use this for logging, retry logic, metrics collection, etc.
#[async_trait]
pub trait TxMiddlewareResp: Send + Sync {
    async fn map_response(&self, resp: TxResponse) -> Result<TxResponse>;
}

// ---------------------------------------------------------------------------
// TxBuilder
// ---------------------------------------------------------------------------

/// Programmatic transaction builder.
///
/// Constructs, signs, and broadcasts Cosmos SDK transactions using
/// `cosmrs` types and the [`Authenticator`] trait.
///
/// # Example
///
/// ```ignore
/// let resp = TxBuilder::new(&chain_config, &auth, "http://localhost:26657")
///     .message(msg_send_any)
///     .memo("test transfer")
///     .middleware_body(Box::new(LoggingMiddleware))
///     .broadcast()
///     .await?;
/// ```
pub struct TxBuilder<'a> {
    config: &'a ChainConfig,
    signer: &'a dyn Authenticator,
    rpc_url: String,
    messages: Vec<cosmrs::Any>,
    memo: Option<String>,
    gas_limit: Option<u64>,
    gas_adjustment: f64,
    timeout_height: u64,
    extension_options: Vec<cosmrs::Any>,
    non_critical_extension_options: Vec<cosmrs::Any>,
    middleware_body: Vec<Box<dyn TxMiddlewareBody>>,
    middleware_resp: Vec<Box<dyn TxMiddlewareResp>>,
}

impl<'a> TxBuilder<'a> {
    /// Create a new `TxBuilder`.
    pub fn new(
        config: &'a ChainConfig,
        signer: &'a dyn Authenticator,
        rpc_url: &str,
    ) -> Self {
        Self {
            config,
            signer,
            rpc_url: rpc_url.to_string(),
            messages: Vec::new(),
            memo: None,
            gas_limit: None,
            gas_adjustment: config.gas_adjustment,
            timeout_height: 0,
            extension_options: Vec::new(),
            non_critical_extension_options: Vec::new(),
            middleware_body: Vec::new(),
            middleware_resp: Vec::new(),
        }
    }

    /// Add a message to the transaction.
    pub fn message(mut self, msg: cosmrs::Any) -> Self {
        self.messages.push(msg);
        self
    }

    /// Set the memo field.
    pub fn memo(mut self, memo: impl Into<String>) -> Self {
        self.memo = Some(memo.into());
        self
    }

    /// Override the gas limit (skips simulation).
    pub fn gas_limit(mut self, gas: u64) -> Self {
        self.gas_limit = Some(gas);
        self
    }

    /// Override gas adjustment (used when simulating).
    pub fn gas_adjustment(mut self, adj: f64) -> Self {
        self.gas_adjustment = adj;
        self
    }

    /// Set the timeout height.
    pub fn timeout_height(mut self, height: u64) -> Self {
        self.timeout_height = height;
        self
    }

    /// Add an `extension_option` to the tx body.
    pub fn extension_option(mut self, opt: cosmrs::Any) -> Self {
        self.extension_options.push(opt);
        self
    }

    /// Add a non-critical extension option.
    pub fn non_critical_extension_option(mut self, opt: cosmrs::Any) -> Self {
        self.non_critical_extension_options.push(opt);
        self
    }

    /// Add a body middleware.
    pub fn middleware_body(mut self, mw: Box<dyn TxMiddlewareBody>) -> Self {
        self.middleware_body.push(mw);
        self
    }

    /// Add a response middleware.
    pub fn middleware_resp(mut self, mw: Box<dyn TxMiddlewareResp>) -> Self {
        self.middleware_resp.push(mw);
        self
    }

    /// Build, sign, and broadcast the transaction.
    ///
    /// Steps:
    /// 1. Construct `cosmrs::tx::Body`
    /// 2. Run body middleware chain
    /// 3. Query account number + sequence via RPC
    /// 4. Simulate gas if not explicitly set
    /// 5. Build `AuthInfo` (fee + signer_info)
    /// 6. Build `SignDoc`, call `signer.sign()`
    /// 7. Construct `TxRaw`, broadcast via `broadcast_tx_sync`
    /// 8. Run response middleware chain
    pub async fn broadcast(self) -> Result<TxResponse> {
        // 1. Build body
        let mut body = cosmrs::tx::Body::new(
            self.messages,
            self.memo.unwrap_or_default(),
            cosmrs::tendermint::block::Height::try_from(self.timeout_height)
                .unwrap_or_default(),
        );
        body.extension_options = self.extension_options;
        body.non_critical_extension_options = self.non_critical_extension_options;

        // 2. Body middleware
        let mut body = body;
        for mw in &self.middleware_body {
            body = mw.map_body(body).await?;
        }

        // 3. Query account info via RPC /abci_query
        let pub_key_bytes = self.signer.public_key().await?;
        let address = self.signer.address(&self.config.bech32_prefix).await?;

        let (account_number, sequence) =
            query_account_info(&self.rpc_url, &address).await?;

        // 4. Determine gas
        let gas_limit = match self.gas_limit {
            Some(g) => g,
            None => {
                // Simulate to estimate gas
                let simulated = simulate_tx(
                    &self.rpc_url,
                    &body,
                    &pub_key_bytes,
                    account_number,
                    sequence,
                    self.config.coin_type,
                )
                .await
                .unwrap_or(200_000);
                (simulated as f64 * self.gas_adjustment) as u64
            }
        };

        // 5. Build AuthInfo
        let fee = build_fee(gas_limit, &self.config.gas_prices)?;

        let signer_info = build_signer_info(&pub_key_bytes, sequence, self.config.coin_type)?;

        let auth_info = cosmrs::tx::AuthInfo {
            signer_infos: vec![signer_info],
            fee,
        };

        // 6. Build SignDoc and sign
        let chain_id: cosmrs::tendermint::chain::Id = self
            .config
            .chain_id
            .parse()
            .map_err(|e| IctError::Config(format!("invalid chain_id: {e}")))?;

        let sign_doc = cosmrs::tx::SignDoc::new(
            &body,
            &auth_info,
            &chain_id,
            account_number,
        )
        .map_err(|e| IctError::Config(format!("failed to build SignDoc: {e}")))?;

        let sign_doc_bytes = sign_doc.into_bytes()
            .map_err(|e| IctError::Config(format!("failed to serialize SignDoc: {e}")))?;

        let signature = self.signer.sign(&sign_doc_bytes).await?;

        // 7. Build TxRaw and broadcast
        let body_bytes = body.into_bytes()
            .map_err(|e| IctError::Config(format!("failed to encode body: {e}")))?;
        let auth_info_bytes = auth_info.into_bytes()
            .map_err(|e| IctError::Config(format!("failed to encode auth_info: {e}")))?;

        let tx_raw: cosmrs::tx::Raw = cosmrs::proto::cosmos::tx::v1beta1::TxRaw {
            body_bytes,
            auth_info_bytes,
            signatures: vec![signature],
        }
        .into();

        let tx_bytes = tx_raw.to_bytes()
            .map_err(|e| IctError::Config(format!("failed to serialize TxRaw: {e}")))?;

        let mut resp = broadcast_tx_sync(&self.rpc_url, &tx_bytes).await?;

        // 8. Response middleware
        for mw in &self.middleware_resp {
            resp = mw.map_response(resp).await?;
        }

        Ok(resp)
    }
}

// ---------------------------------------------------------------------------
// Built-in middleware implementations
// ---------------------------------------------------------------------------

/// Logs the tx body (message count, memo) and response at debug level.
pub struct LoggingMiddleware;

#[async_trait]
impl TxMiddlewareBody for LoggingMiddleware {
    async fn map_body(&self, body: cosmrs::tx::Body) -> Result<cosmrs::tx::Body> {
        debug!(
            messages = body.messages.len(),
            memo = %body.memo,
            extensions = body.extension_options.len(),
            "TxBuilder: body before signing"
        );
        Ok(body)
    }
}

#[async_trait]
impl TxMiddlewareResp for LoggingMiddleware {
    async fn map_response(&self, resp: TxResponse) -> Result<TxResponse> {
        debug!(
            tx_hash = %resp.tx_hash,
            code = resp.code,
            gas_used = resp.gas_used,
            "TxBuilder: broadcast response"
        );
        Ok(resp)
    }
}

/// Injects a fixed set of `extension_options` into every transaction body.
pub struct ExtensionInjector {
    pub extensions: Vec<cosmrs::Any>,
}

#[async_trait]
impl TxMiddlewareBody for ExtensionInjector {
    async fn map_body(&self, mut body: cosmrs::tx::Body) -> Result<cosmrs::tx::Body> {
        body.extension_options.extend(self.extensions.clone());
        Ok(body)
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Query account number and sequence from the chain via ABCI query.
async fn query_account_info(rpc_url: &str, address: &str) -> Result<(u64, u64)> {
    let client = tendermint_rpc::HttpClient::new(rpc_url)
        .map_err(|e| IctError::Config(format!("failed to create RPC client: {e}")))?;

    use tendermint_rpc::Client;
    let path = format!("/cosmos/auth/v1beta1/accounts/{address}");
    let resp = client
        .abci_query(Some(path), vec![], None, false)
        .await
        .map_err(|e| IctError::Chain {
            chain_id: String::new(),
            source: anyhow::anyhow!("ABCI query failed: {e}"),
        })?;

    // The response value is protobuf-encoded QueryAccountResponse.
    // Parse with serde_json after converting the response.
    // ABCI query for REST-style paths returns JSON in the value field.
    let value = resp.value;
    if value.is_empty() {
        return Err(IctError::Config(format!(
            "account not found: {address}"
        )));
    }

    // Try to parse as JSON (some nodes return JSON for REST paths)
    let json_str = String::from_utf8_lossy(&value);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json_str) {
        let account = &v["account"];
        let account_number = account["account_number"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| account["account_number"].as_u64())
            .unwrap_or(0);
        let sequence = account["sequence"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| account["sequence"].as_u64())
            .unwrap_or(0);
        return Ok((account_number, sequence));
    }

    // Fallback: try protobuf decode of BaseAccount
    #[derive(prost::Message)]
    struct AnyProto {
        #[prost(string, tag = "1")]
        type_url: String,
        #[prost(bytes = "vec", tag = "2")]
        value: Vec<u8>,
    }

    #[derive(prost::Message)]
    struct QueryAccountResponse {
        #[prost(message, optional, tag = "1")]
        account: Option<AnyProto>,
    }

    #[derive(prost::Message)]
    struct BaseAccount {
        #[prost(string, tag = "1")]
        address: String,
        #[prost(message, optional, tag = "2")]
        pub_key: Option<AnyProto>,
        #[prost(uint64, tag = "3")]
        account_number: u64,
        #[prost(uint64, tag = "4")]
        sequence: u64,
    }

    let resp_msg = QueryAccountResponse::decode(value.as_slice())
        .map_err(|e| IctError::Config(format!("failed to decode account response: {e}")))?;

    let account_any = resp_msg
        .account
        .ok_or_else(|| IctError::Config("no account in response".into()))?;

    let base = BaseAccount::decode(account_any.value.as_slice())
        .map_err(|e| IctError::Config(format!("failed to decode BaseAccount: {e}")))?;

    Ok((base.account_number, base.sequence))
}

/// Simulate a transaction to estimate gas.
async fn simulate_tx(
    rpc_url: &str,
    body: &cosmrs::tx::Body,
    pub_key_bytes: &[u8],
    _account_number: u64,
    sequence: u64,
    coin_type: u32,
) -> Result<u64> {
    // Build a dummy tx with empty signature for simulation
    let signer_info = build_signer_info(pub_key_bytes, sequence, coin_type)?;
    let auth_info = cosmrs::tx::AuthInfo {
        signer_infos: vec![signer_info],
        fee: cosmrs::tx::Fee {
            amount: vec![],
            gas_limit: 0,
            payer: None,
            granter: None,
        },
    };

    let body_bytes = body.clone().into_bytes()
        .map_err(|e| IctError::Config(format!("sim: failed to encode body: {e}")))?;
    let auth_info_bytes = auth_info.into_bytes()
        .map_err(|e| IctError::Config(format!("sim: failed to encode auth_info: {e}")))?;

    // Empty signature for simulation
    let tx_raw: cosmrs::tx::Raw = cosmrs::proto::cosmos::tx::v1beta1::TxRaw {
        body_bytes,
        auth_info_bytes,
        signatures: vec![vec![]],
    }
    .into();
    let tx_bytes = tx_raw.to_bytes()
        .map_err(|e| IctError::Config(format!("sim: failed to serialize: {e}")))?;

    // Simulate via ABCI query to /cosmos/tx/v1beta1/simulate
    #[derive(serde::Serialize)]
    struct SimulateRequest {
        tx_bytes: String,
    }

    let client = tendermint_rpc::HttpClient::new(rpc_url)
        .map_err(|e| IctError::Config(format!("sim: failed to create RPC client: {e}")))?;

    use tendermint_rpc::Client;
    let b64_tx = base64_encode(&tx_bytes);

    // Use ABCI query with the simulate path
    let query_data = serde_json::to_vec(&SimulateRequest {
        tx_bytes: b64_tx,
    })
    .unwrap_or_default();

    let resp = client
        .abci_query(
            Some("/cosmos.tx.v1beta1.Service/Simulate".to_string()),
            query_data,
            None,
            false,
        )
        .await
        .map_err(|e| IctError::Chain {
            chain_id: String::new(),
            source: anyhow::anyhow!("simulation failed: {e}"),
        })?;

    // Parse simulation response
    #[derive(prost::Message)]
    struct GasInfo {
        #[prost(uint64, tag = "1")]
        gas_wanted: u64,
        #[prost(uint64, tag = "2")]
        gas_used: u64,
    }

    #[derive(prost::Message)]
    struct SimulateResponse {
        #[prost(message, optional, tag = "1")]
        gas_info: Option<GasInfo>,
    }

    if let Ok(sim_resp) = SimulateResponse::decode(resp.value.as_slice()) {
        if let Some(gas_info) = sim_resp.gas_info {
            return Ok(gas_info.gas_used);
        }
    }

    // Fallback
    Ok(200_000)
}

/// Build a Fee from gas_limit and gas_prices string (e.g. "0.025uakt").
fn build_fee(gas_limit: u64, gas_prices: &str) -> Result<cosmrs::tx::Fee> {
    // Parse "0.025uakt" → amount=ceil(gas * price), denom="uakt"
    let (price_str, denom) = split_gas_prices(gas_prices)?;
    let price: f64 = price_str
        .parse()
        .map_err(|e| IctError::Config(format!("invalid gas price '{price_str}': {e}")))?;

    let fee_amount = (gas_limit as f64 * price).ceil() as u128;

    let coin = cosmrs::Coin {
        denom: denom
            .parse()
            .map_err(|e| IctError::Config(format!("invalid denom '{denom}': {e}")))?,
        amount: fee_amount,
    };

    Ok(cosmrs::tx::Fee {
        amount: vec![coin],
        gas_limit,
        payer: None,
        granter: None,
    })
}

/// Split "0.025uakt" → ("0.025", "uakt").
fn split_gas_prices(s: &str) -> Result<(&str, &str)> {
    let idx = s
        .find(|c: char| c.is_alphabetic())
        .ok_or_else(|| IctError::Config(format!("invalid gas_prices format: '{s}'")))?;
    Ok((&s[..idx], &s[idx..]))
}

/// Build signer info with a secp256k1 public key.
fn build_signer_info(
    pub_key_bytes: &[u8],
    sequence: u64,
    _coin_type: u32,
) -> Result<cosmrs::tx::SignerInfo> {
    // Build a cosmrs::crypto::PublicKey from raw secp256k1 bytes (33 compressed)
    let tm_pk = tendermint::PublicKey::from_raw_secp256k1(pub_key_bytes)
        .ok_or_else(|| IctError::Config("invalid secp256k1 public key bytes".into()))?;
    let pub_key = cosmrs::crypto::PublicKey::from(tm_pk.clone());

    Ok(cosmrs::tx::SignerInfo::single_direct(Some(pub_key), sequence))
}

/// Broadcast a signed transaction via broadcast_tx_sync.
async fn broadcast_tx_sync(rpc_url: &str, tx_bytes: &[u8]) -> Result<TxResponse> {
    let client = tendermint_rpc::HttpClient::new(rpc_url)
        .map_err(|e| IctError::Config(format!("broadcast: failed to create RPC client: {e}")))?;

    use tendermint_rpc::Client;
    let resp = client
        .broadcast_tx_sync(tx_bytes.to_vec())
        .await
        .map_err(|e| IctError::Chain {
            chain_id: String::new(),
            source: anyhow::anyhow!("broadcast failed: {e}"),
        })?;

    Ok(TxResponse {
        height: 0, // sync broadcast doesn't return height
        tx_hash: resp.hash.to_string(),
        gas_used: 0,
        code: resp.code.value(),
        raw_log: resp.log.to_string(),
    })
}

/// Simple base64 encoder (standard alphabet with padding).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[(n >> 18 & 0x3F) as usize] as char);
        result.push(CHARS[(n >> 12 & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[(n >> 6 & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_gas_prices() {
        let (price, denom) = split_gas_prices("0.025uakt").unwrap();
        assert_eq!(price, "0.025");
        assert_eq!(denom, "uakt");
    }

    #[test]
    fn test_split_gas_prices_integer() {
        let (price, denom) = split_gas_prices("1uterp").unwrap();
        assert_eq!(price, "1");
        assert_eq!(denom, "uterp");
    }

    #[test]
    fn test_build_fee() {
        let fee = build_fee(200_000, "0.025uakt").unwrap();
        assert_eq!(fee.gas_limit, 200_000);
        assert_eq!(fee.amount.len(), 1);
        // 200_000 * 0.025 = 5000
        assert_eq!(fee.amount[0].amount, 5000);
    }

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"ab"), "YWI=");
    }
}
