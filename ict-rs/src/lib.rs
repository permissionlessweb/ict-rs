#![doc = include_str!("../README.md")]

pub mod auth;
pub mod chain;
pub mod cli;
pub mod cosmos;
pub mod error;
pub mod faucet;
pub mod relayer;
pub mod reporter;
pub mod runtime;
pub mod sidecar;
pub mod spec;
pub mod wallet;

// Re-export Zakura local spawn surface when the feature is enabled.
#[cfg(feature = "zakura")]
pub use chain::zakura;

#[cfg(feature = "nostr")]
pub mod nostr;

#[cfg(feature = "docker")]
pub mod quickspawn;

// Re-export cosmos submodules at the crate root so existing `crate::node`,
// `crate::tx`, `crate::ibc`, etc. paths continue to work.
pub use cosmos::cosmwasm;
pub use cosmos::genesis;
pub use cosmos::governance;
pub use cosmos::ibc;
pub use cosmos::ibc_wasm;
pub use cosmos::interchain;
pub use cosmos::modules;
pub use cosmos::node;
pub use cosmos::tx;
pub use cosmos::tx_builder;

#[cfg(feature = "testing")]
pub mod testing;

/// Re-export derive macros.
pub use ict_rs_derive::{ExecuteFns, QueryFns};

/// Convenience re-exports for common usage.
pub mod prelude {
    pub use crate::auth::Authenticator;
    pub use crate::chain::{
        cosmos::CosmosChain, terp::terp_chain_config, Chain, ChainConfig, ChainType, FaucetConfig,
        SidecarConfig, SigningAlgorithm, TestContext,
    };
    #[cfg(feature = "zakura")]
    pub use crate::chain::zakura::{
        owner_binding_from_dest_display, spawn_zakura_local, zakura_sidecar_config,
        ZakuraNode, ZakuraNodeConfig, DEST_BINDING_DOMAIN, ENV_ZAKURAD_BIN, ENV_ZAKURA_RPC,
        REGTEST_MINER_DEST, REGTEST_MINER_OWNER_BINDING_HEX, ZAKURA_RPC_PORT,
    };
    pub use crate::cosmwasm::CosmWasmExt;
    pub use crate::error::{IctError, Result};
    pub use crate::faucet::FaucetExt;
    pub use crate::governance::GovernanceExt;
    pub use crate::ibc_wasm::{IbcWasmExt, IbcWasmStoreProposal, IbcWasmStoreResult};
    pub use crate::ibc::{ibc_denom, ibc_denom_multi_hop, ChannelOptions, ClientOptions};
    pub use crate::interchain::{
        wait_for_blocks, Interchain, InterchainBuildOptions, InterchainLink,
    };
    pub use crate::relayer::{build_relayer, DockerRelayer, Relayer, RelayerType};
    pub use crate::runtime::{
        docker::DockerBackend, DockerConfig, DockerImage, IctRuntime, RuntimeBackend,
    };
    pub use crate::sidecar::SidecarProcess;
    pub use crate::spec::ChainSpec;
    pub use crate::tx::{ExecOutput, TransferOptions, Tx, TxOptions, WalletAmount};
    pub use crate::tx_builder::{TxBuilder, TxMiddlewareBody, TxMiddlewareResp, TxResponse};
    pub use crate::wallet::Wallet;

    pub use crate::{ExecuteFns, QueryFns};
}
