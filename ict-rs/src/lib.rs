#![doc = include_str!("../README.md")]

pub mod auth;
pub mod chain;
pub mod cli;
pub mod cosmos;
pub mod error;
pub mod faucet;
pub mod image_env;
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
pub use cosmos::hasher_apps;
pub use cosmos::hasher_ibc;
pub use cosmos::ibc;
pub use cosmos::ibc_v2;
pub use cosmos::ibc_wasm;
pub use cosmos::ibc_wasm_path;
pub use cosmos::interchain;
pub use cosmos::modules;
pub use cosmos::node;
pub use cosmos::tx;
pub use cosmos::tx_builder;

#[cfg(feature = "testing")]
pub mod testing;

/// Re-export derive macros.
pub use ict_rs_derive::{ExecuteFns, QueryFns};

/// Convenience re-exports for common usage (examples, tests, one-shot binaries).
pub mod prelude {
    pub use crate::auth::{generate_mnemonic, Authenticator};
    pub use crate::chain::{
        cosmos::CosmosChain, Chain, ChainConfig, ChainType, FaucetConfig, GenesisStyle,
        SidecarConfig, SigningAlgorithm, TestContext,
    };
    #[cfg(feature = "terp")]
    pub use crate::chain::terp::terp_chain_config;
    #[cfg(feature = "ethereum")]
    pub use crate::chain::ethereum::{AnvilChain, ANVIL_DEFAULT_ACCOUNTS};
    #[cfg(feature = "zakura")]
    pub use crate::chain::zakura::{
        owner_binding_from_dest_display, spawn_zakura_local, zakura_sidecar_config,
        zakurad_is_linux_elf, ZakuraNode, ZakuraNodeConfig, DEST_BINDING_DOMAIN,
        ENV_ZAKURAD_BIN, ENV_ZAKURA_CROSSLINK_LAB, ENV_ZAKURA_RPC, REGTEST_MINER_DEST,
        REGTEST_MINER_OWNER_BINDING_HEX, ZAKURA_RPC_PORT,
    };
    #[cfg(feature = "zakura")]
    pub use crate::chain::zcash_devtool::{
        write_default_regtest_heights, ZcashDevtoolWallet, ENV_ZCASH_DEVTOOL_ACTIVATION_HEIGHTS,
        ENV_ZCASH_DEVTOOL_BIN, ENV_ZCASH_DEVTOOL_NETWORK, ENV_ZCASH_DEVTOOL_SERVER,
        ENV_ZCASH_DEVTOOL_WALLET,
    };
    #[cfg(feature = "dregg-node")]
    pub use crate::chain::dregg::{
        spawn_dregg_local, DreggNode, DreggNodeConfig, DREGG_QUERY_PORT, ENV_DREGG_NODE_BIN,
        ENV_DREGG_NODE_URL,
    };
    #[cfg(feature = "bitcoin")]
    pub use crate::chain::bitcoin::{
        spawn_bitcoind_regtest, BitcoinNode, BitcoinNodeConfig, BTC_RPC_HOST_PORT,
    };
    pub use crate::cosmwasm::CosmWasmExt;
    pub use crate::error::{IctError, Result as IctResult};
    pub use crate::image_env::{terp_docker_image, terp_image_repo, terp_image_version};
    pub use crate::faucet::FaucetExt;
    pub use crate::governance::{status, GovernanceExt};
    pub use crate::ibc_v2::{
        create_ibc_v2_tendermint_path, ibc_v2_add_counterparty_tx_args,
        ibc_v2_create_client_tx_args, IbcV2Ext, IBC_V2_MERKLE_PREFIX,
    };
    pub use crate::hasher_ibc::{
        collect_ibc_msgs, default_out_dir, encode_merkle_proof, first_of, iavl_root_from_proof,
        markdown_table, parse_abci_store_proof, parse_variant, refuse_mock, rows_from_ibc_msgs,
        to_hex, write_report, write_three_report, HasherActionRow, HasherIbcEnv, HasherIbcReport,
        HasherThreeChainReport,
        HasherVariant, IbcMsgKind, StoreProve, TransferSample,
    };
    pub use crate::cosmos::hasher_apps;
    pub use crate::ibc_wasm::{
        parse_ibc_wasm_checksums_json, parse_ibc_wasm_store_txhash, store_code_tx_base_args,
        IbcWasmExt, IbcWasmStoreProposal, IbcWasmStoreResult,
    };
    pub use crate::ibc_wasm_path;
    pub use crate::ibc::{
        ibc_denom, ibc_denom_multi_hop, ChannelOptions, ChannelOutput, ClientOptions,
        ConnectionOutput,
    };
    pub use crate::interchain::{
        wait_for_blocks, Interchain, InterchainBuildOptions, InterchainLink,
    };
    pub use crate::relayer::{
        build_relayer, hermes_create_client_cmd, hermes_create_wasm_client_cmd, hermes_require_ok,
        hermes_update_client_cmd, DockerRelayer, Relayer, RelayerType,
    };
    pub use crate::runtime::mock::MockRuntime;
    pub use crate::runtime::{
        ContainerOptions, DockerConfig, DockerImage, IctRuntime, PortBinding, RuntimeBackend,
    };
    #[cfg(feature = "docker")]
    pub use crate::runtime::docker::DockerBackend;
    pub use crate::sidecar::SidecarProcess;
    pub use crate::spec::{builtin_chain_config, ChainSpec};
    pub use crate::tx::{ExecOutput, TransferOptions, Tx, TxOptions, WalletAmount};
    pub use crate::tx_builder::{TxBuilder, TxMiddlewareBody, TxMiddlewareResp, TxResponse};
    pub use crate::wallet::{KeyWallet, Wallet};

    pub use crate::{ExecuteFns, QueryFns};
}
