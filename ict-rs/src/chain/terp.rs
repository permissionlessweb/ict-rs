//! Terp Network chain support for ict-rs.
//!
//! Provides abstract traits for ZK circuit + CosmWasm contract development
//! on Terp Network, centred around the `zk-wasmvm` custom CosmWasm VM
//! extension that stores and executes on-chain verifying keys.
//!
//! # Feature gate
//!
//! This entire module is gated behind the `terp` feature flag.  Enable it
//! in your `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! ict-rs = { version = "*", features = ["terp"] }
//! ```
//!
//! # Design
//!
//! Three layered traits model the lifecycle of a ZK-powered dApp on Terp:
//!
//! 1. [`Circuit`] — describes a halo2 circuit: params, key-gen,
//!    prove, verify, and serialisation.
//! 2. [`Contract`] — binds a CosmWasm contract (as embedded bytes)
//!    to a specific [`Circuit`].
//! 3. [`TerpVmSuite`] — the full development interface: key management,
//!    proof operations, and on-chain deployment via `terpd tx wasm store-circuit`.
//!
//! None of the circuit/proof methods have default implementations — those
//! belong in the concrete structs that implement the traits.  Only lifecycle
//! helpers with trivially obvious defaults are provided here.

use std::path::Path;

use async_trait::async_trait;

// Re-export the Circuit<Chain> upload-tracker struct defined in cw-orch-core.
// Renamed to `UploadableCircuit` to avoid shadowing the `Circuit` trait below.
pub use cw_orch_core::contract::circuits::{
    Circuit as UploadableCircuit, CircuitInstance, CircuitPath, CircuitSpec, CircuitsDir,
    CwOrchCircuitUpload,
};

use crate::error::IctError;

// ─── Error type ──────────────────────────────────────────────────────────────

/// Errors that can occur inside the ZK + CosmWasm development suite.
#[derive(Debug, thiserror::Error)]
pub enum ZkSuiteError {
    /// Failure inside circuit parameter generation, key-gen, proving, or
    /// serialisation.
    #[error("circuit error: {0}")]
    Circuit(String),

    /// I/O failure while reading or writing proving/verifying key files.
    #[error("key I/O error: {0}")]
    KeyIo(#[from] std::io::Error),

    /// Proof verification rejected the supplied proof or public inputs.
    #[error("proof verification failed: {0}")]
    Verification(String),

    /// Failure during on-chain deployment (store code, instantiate, register VK).
    #[error("deploy error: {0}")]
    Deploy(String),

    /// The verifying-key bytes do not begin or end with the expected magic
    /// header/footer that the on-chain `zk-wasmvm` host expects.
    #[error("vk validation failed: {0}")]
    VkValidation(String),

    /// Propagated error from the underlying ict-rs framework.
    #[error("ict-rs error: {0}")]
    Ict(#[from] IctError),
}

/// Convenience `Result` alias used throughout the ZK suite traits.
pub type Result<T> = std::result::Result<T, ZkSuiteError>;

// ─── Circuit ─────────────────────────────────────────────────────────

/// Abstract description of a halo2 circuit for use with Terp's `zk-wasmvm`.
///
/// Implementors supply all circuit-specific types and operations.  The trait
/// is deliberately free of any halo2 concrete type references so that callers
/// do not need halo2 as a direct dependency merely to write test harnesses.
///
/// # Associated types
///
/// | Type | Meaning |
/// |------|---------|
/// | `Params` | Structured Reference String (SRS) |
/// | `ProvingKey` | Proving key produced by `keygen` |
/// | `VerifyingKey` | Verifying key produced by `keygen` |
/// | `Proof` | Serialisable proof blob |
/// | `PublicInputs` | Public witness / instance values |
pub trait Circuit: Send + Sync {
    /// Circuit-specific SRS / parameter type.
    type Params: Send + Sync;
    /// Proving key type.
    type ProvingKey: Send + Sync;
    /// Verifying key type.
    type VerifyingKey: Send + Sync;
    /// Public instance values consumed by `prove` and `verify`.
    type PublicInputs: Send + Sync;
    /// Proof type (typically an opaque byte blob but kept generic for testing).
    type Proof: Send + Sync;

    /// Human-readable circuit name used in error messages and file names.
    fn circuit_name() -> &'static str;

    /// Generate a proving key and verifying key from the given params.
    fn keygen(
        params: &Self::Params,
    ) -> std::result::Result<(Self::ProvingKey, Self::VerifyingKey), ZkSuiteError>;

    /// Create a proof for the given public inputs using the proving key.
    fn prove(
        params: &Self::Params,
        pk: &Self::ProvingKey,
        inputs: &Self::PublicInputs,
    ) -> std::result::Result<Self::Proof, ZkSuiteError>;

    /// Verify a proof against the verifying key and public inputs.
    ///
    /// Returns `Ok(())` on success; `Err(ZkSuiteError::Verification(_))` on
    /// failure.
    fn verify(
        params: &Self::Params,
        vk: &Self::VerifyingKey,
        proof: &Self::Proof,
        inputs: &Self::PublicInputs,
    ) -> std::result::Result<(), ZkSuiteError>;

    /// Serialise a proof to raw bytes for on-chain submission.
    fn proof_to_bytes(proof: &Self::Proof) -> Vec<u8>;

    /// Serialise a verifying key to raw bytes for on-chain registration.
    fn vk_to_bytes(vk: &Self::VerifyingKey) -> Vec<u8>;

    /// Deserialise a verifying key from raw bytes (e.g. read back from disk).
    fn vk_from_bytes(bytes: &[u8]) -> std::result::Result<Self::VerifyingKey, ZkSuiteError>;
}

// ─── Contract ────────────────────────────────────────────────────────────────

/// Binds a CosmWasm verifier contract (as embedded bytes) to a [`Circuit`].
pub trait Contract: Send + Sync {
    /// The ZK circuit this contract verifies.
    type Circuit: Circuit;

    /// Human-readable contract name used in labels and error messages.
    fn contract_name() -> &'static str;

    /// Raw WASM bytes of the compiled contract (via `include_bytes!`).
    fn wasm_byte_code() -> &'static [u8];

    /// Validate that the verifying-key bytes start with the expected magic
    /// header for the `zk-wasmvm` host.  Return `Err` to reject.
    fn validate_vk_header(vk_bytes: &[u8]) -> std::result::Result<(), ZkSuiteError>;

    /// Validate that the verifying-key bytes end with the expected magic
    /// footer for the `zk-wasmvm` host.  Return `Err` to reject.
    fn validate_vk_footer(vk_bytes: &[u8]) -> std::result::Result<(), ZkSuiteError>;
}

// ─── DeployedContract ────────────────────────────────────────────────────────

/// Result of a successful on-chain deployment via [`TerpVmSuite::deploy`].
#[derive(Debug, Clone)]
pub struct DeployedContract {
    /// On-chain code ID of the stored verifier WASM.
    pub code_id: u64,
    /// Bech32 address of the instantiated contract.
    pub contract_addr: String,
    /// Whether the verifying key was successfully registered after instantiation.
    pub vk_registered: bool,
}

// ─── TerpVmSuite ─────────────────────────────────────────────────────────────

/// Full development interface for a ZK circuit + CosmWasm verifier pair on
/// Terp Network.
///
/// Combines key management, proof operations, and on-chain deployment.
/// Each circuit gets its own concrete impl (e.g. `NoRickSuite`).
#[async_trait]
pub trait TerpVmSuite: Send + Sync {
    /// The ZK circuit this suite manages.
    type Circuit: Circuit;
    /// The CosmWasm verifier contract bound to [`Self::Circuit`].
    type Contract: Contract<Circuit = Self::Circuit>;
    /// The ict-rs chain type used for Docker-based deployment.
    type Chain: Send + Sync;

    /// Directory where proving/verifying key files are persisted.
    fn keys_dir(&self) -> &Path;

    /// Load the proving and verifying keys from [`Self::keys_dir`].
    fn load_circuit_keys(
        &self,
    ) -> std::result::Result<
        (
            <Self::Circuit as Circuit>::ProvingKey,
            <Self::Circuit as Circuit>::VerifyingKey,
        ),
        ZkSuiteError,
    >;

    /// Persist proving and verifying keys to [`Self::keys_dir`].
    fn save_circuit_keys(
        &self,
        pk: &<Self::Circuit as Circuit>::ProvingKey,
        vk: &<Self::Circuit as Circuit>::VerifyingKey,
    ) -> std::result::Result<(), ZkSuiteError>;

    /// Generate fresh circuit keys for depth `k` and save them to disk.
    fn build_and_save_keys(&self, k: u32) -> std::result::Result<(), ZkSuiteError>;

    /// Create a proof for `inputs` using the stored proving key.
    fn prove(
        &self,
        inputs: &<Self::Circuit as Circuit>::PublicInputs,
    ) -> std::result::Result<<Self::Circuit as Circuit>::Proof, ZkSuiteError>;

    /// Verify `proof` against `inputs` using the stored verifying key.
    fn verify(
        &self,
        proof: &<Self::Circuit as Circuit>::Proof,
        inputs: &<Self::Circuit as Circuit>::PublicInputs,
    ) -> std::result::Result<(), ZkSuiteError>;

    /// Deploy the verifier contract to a running Terp Network node via
    /// `terpd tx wasm store` + `terpd tx wasm instantiate` + VK registration.
    async fn deploy(
        &self,
        chain: &Self::Chain,
        deployer_key: &str,
    ) -> std::result::Result<DeployedContract, ZkSuiteError>;
}

// ─── TerpChainConfig helper ───────────────────────────────────────────────────

/// Build a [`crate::chain::ChainConfig`] suitable for a local Terp Network
/// node running the `zk-wasmvm` module.
///
/// Defaults:
/// - Image: `terpnetwork/terp-core:local-zk`
/// - Binary: `terpd`
/// - Denom: `uterp`
/// - Chain ID: `terp-local-1`
/// - Vote extensions enabled at height 2 (required for hashmerchant VE flow)
/// - Fast block time: `200ms`
///
/// Override individual fields after construction as needed.
pub fn terp_chain_config() -> crate::chain::ChainConfig {
    use crate::chain::{ChainConfig, ChainType, GenesisStyle, SigningAlgorithm};
    use crate::runtime::DockerImage;
    use serde_json::json;
    use std::collections::HashMap;

    let mut config_overrides: HashMap<String, serde_json::Value> = HashMap::new();

    // Enable REST API and gRPC in app.toml.
    config_overrides.insert(
        "config/app.toml".into(),
        json!({
            "api": {
                "enable": true,
                "address": "tcp://0.0.0.0:1317"
            },
            "grpc": {
                "enable": true,
                "address": "0.0.0.0:9090"
            },
            // hashmerchant sidecar URL — override if running a custom sidecar
            "hashmerchant": {
                "sidecar-url": "http://localhost:8080"
            }
        }),
    );

    // Fast block times and vote extension enable height in config.toml / genesis.
    config_overrides.insert(
        "config/config.toml".into(),
        json!({
            "consensus": {
                "timeout_propose":        "200ms",
                "timeout_propose_delta":  "200ms",
                "timeout_prevote":        "200ms",
                "timeout_prevote_delta":  "200ms",
                "timeout_precommit":      "200ms",
                "timeout_precommit_delta":"200ms",
                "timeout_commit":         "200ms"
            }
        }),
    );

    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".to_string(),
        chain_id: "terp-local-1".to_string(),
        images: vec![DockerImage {
            repository: "terpnetwork/terp-core".to_string(),
            version: "local-zk".to_string(),
            uid_gid: None,
        }],
        bin: "terpd".to_string(),
        bech32_prefix: "terp".to_string(),
        denom: "uterp".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.025uterp".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "336h".to_string(),
        block_time: "200ms".to_string(),
        genesis: None,
        // Enable vote extensions at height 2 so the hashmerchant VE flow works.
        modify_genesis: Some(Box::new(modify_terp_genesis)),
        pre_genesis: None,
        config_file_overrides: config_overrides,
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: GenesisStyle::Modern,
    }
}

/// Apply Terp-specific genesis mutations for local testing.
///
/// Sets fast governance, the correct staking/mint denom, and enables vote
/// extensions at height 2 (required for the hashmerchant module).
pub fn modify_terp_genesis(
    _cfg: &crate::chain::ChainConfig,
    raw: Vec<u8>,
) -> crate::error::Result<Vec<u8>> {
    use serde_json::json;

    let mut genesis: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| IctError::Config(format!("parse terp genesis: {e}")))?;

    // ── Staking ───────────────────────────────────────────────────────────
    if let Some(params) = genesis.pointer_mut("/app_state/staking/params") {
        params["bond_denom"] = json!("uterp");
        params["unbonding_time"] = json!("120s");
    }

    // ── Mint ──────────────────────────────────────────────────────────────
    if let Some(params) = genesis.pointer_mut("/app_state/mint/params") {
        params["mint_denom"] = json!("uterp");
    }

    // ── Governance (fast voting for tests) ────────────────────────────────
    if let Some(params) = genesis.pointer_mut("/app_state/gov/params") {
        params["min_deposit"] = json!([{"denom": "uterp", "amount": "10000000"}]);
        params["voting_period"] = json!("90s");
        params["max_deposit_period"] = json!("90s");
    }
    // v1beta1 fallback
    if let Some(dp) = genesis.pointer_mut("/app_state/gov/deposit_params") {
        dp["min_deposit"] = json!([{"denom": "uterp", "amount": "10000000"}]);
        dp["max_deposit_period"] = json!("90s");
    }
    if let Some(vp) = genesis.pointer_mut("/app_state/gov/voting_params") {
        vp["voting_period"] = json!("90s");
    }

    // ── Crisis ────────────────────────────────────────────────────────────
    if let Some(fee) = genesis.pointer_mut("/app_state/crisis/constant_fee") {
        fee["denom"] = json!("uterp");
    }

    // ── Vote extensions (hashmerchant / zk-wasmvm) ────────────────────────
    // Enable at height 2 so the chain produces at least one normal block first.
    if let Some(params) = genesis.pointer_mut("/consensus/params/abci") {
        params["vote_extensions_enable_height"] = json!("2");
    }
    // Cosmos SDK 0.50 path
    if let Some(params) = genesis.pointer_mut("/app_state/consensus/params/abci") {
        params["vote_extensions_enable_height"] = json!("2");
    }

    // ── hashmerchant genesis state ────────────────────────────────────────
    // Ensure the module is initialised with sensible defaults.
    if let Some(hm) = genesis.pointer_mut("/app_state/hashmerchant") {
        if hm.get("params").is_none() {
            hm["params"] = json!({
                "quorum_fraction": "0.67"
            });
        }
        if hm.get("registered_chains").is_none() {
            hm["registered_chains"] = json!([]);
        }
    }

    serde_json::to_vec_pretty(&genesis)
        .map_err(|e| IctError::Config(format!("serialize terp genesis: {e}")))
}
