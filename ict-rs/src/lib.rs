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
        cosmos::CosmosChain, Chain, ChainConfig, ChainType, FaucetConfig, SidecarConfig,
        SigningAlgorithm,
    };
    pub use crate::cosmwasm::CosmWasmExt;
    pub use crate::error::{IctError, Result};
    pub use crate::faucet::FaucetExt;
    pub use crate::governance::GovernanceExt;
    pub use crate::ibc::{ibc_denom, ibc_denom_multi_hop, ChannelOptions, ClientOptions};
    pub use crate::interchain::{
        wait_for_blocks, Interchain, InterchainBuildOptions, InterchainLink,
    };
    pub use crate::relayer::{build_relayer, DockerRelayer, Relayer, RelayerType};
    pub use crate::runtime::{DockerConfig, DockerImage, IctRuntime, RuntimeBackend};
    pub use crate::sidecar::SidecarProcess;
    pub use crate::spec::ChainSpec;
    pub use crate::tx::{ExecOutput, TransferOptions, Tx, TxOptions, WalletAmount};
    pub use crate::tx_builder::{TxBuilder, TxMiddlewareBody, TxMiddlewareResp, TxResponse};
    pub use crate::wallet::Wallet;

    pub use crate::{ExecuteFns, QueryFns};
}
