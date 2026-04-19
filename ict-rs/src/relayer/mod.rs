pub mod docker_relayer;
pub mod hermes;
pub mod rly;

use std::sync::Arc;

use async_trait::async_trait;

use crate::chain::ChainConfig;
use crate::error::Result;
use crate::ibc::{ChannelOptions, ChannelOutput, ClientOptions, ConnectionOutput};
use crate::runtime::RuntimeBackend;
use crate::tx::ExecOutput;
use crate::wallet::Wallet;

pub use docker_relayer::DockerRelayer;
pub use hermes::HermesRelayer;
pub use rly::CosmosRlyCommander;

/// Supported relayer implementations.
#[derive(Debug, Clone)]
pub enum RelayerType {
    Hermes,
    CosmosRly,
    Hyperspace,
}

/// The IBC relayer abstraction.
///
/// Mirrors Go ICT's `ibc.Relayer` interface. Every relayer implementation
/// (Hermes, CosmosRly, Hyperspace) implements this trait.
#[async_trait]
pub trait Relayer: Send + Sync {
    // -- Key management --

    /// Add a new key for a chain and return the wallet.
    async fn add_key(&self, chain_id: &str, key_name: &str) -> Result<Box<dyn Wallet>>;

    /// Restore a key from a mnemonic.
    async fn restore_key(
        &self,
        chain_id: &str,
        key_name: &str,
        mnemonic: &str,
    ) -> Result<()>;

    /// Get the wallet for a chain, if one exists.
    fn get_wallet(&self, chain_id: &str) -> Option<&dyn Wallet>;

    // -- Chain configuration --

    /// Register a chain with the relayer.
    async fn add_chain_configuration(
        &self,
        config: &ChainConfig,
        key_name: &str,
        rpc_addr: &str,
        grpc_addr: &str,
    ) -> Result<()>;

    // -- Path management --

    /// Generate a path between two chains.
    async fn generate_path(
        &self,
        src_chain_id: &str,
        dst_chain_id: &str,
        path_name: &str,
    ) -> Result<()>;

    /// Create IBC clients, connections, and channels for a path.
    async fn link_path(&self, path_name: &str, opts: &ChannelOptions) -> Result<()>;

    /// Create IBC clients for a path.
    async fn create_clients(&self, path_name: &str, opts: &ClientOptions) -> Result<()>;

    /// Create IBC connections for a path.
    async fn create_connections(&self, path_name: &str) -> Result<()>;

    /// Create an IBC channel for a path.
    async fn create_channel(&self, path_name: &str, opts: &ChannelOptions) -> Result<()>;

    /// Update IBC clients on a path.
    async fn update_clients(&self, path_name: &str) -> Result<()>;

    // -- Lifecycle --

    /// Start relaying on the specified paths.
    async fn start(&self, path_names: &[&str]) -> Result<()>;

    /// Stop the relayer.
    async fn stop(&self) -> Result<()>;

    /// Flush pending packets on a path/channel.
    async fn flush(&self, path_name: &str, channel_id: &str) -> Result<()>;

    // -- Queries --

    /// List IBC channels for a chain.
    async fn get_channels(&self, chain_id: &str) -> Result<Vec<ChannelOutput>>;

    /// List IBC connections for a chain.
    async fn get_connections(&self, chain_id: &str) -> Result<Vec<ConnectionOutput>>;

    // -- Execution --

    /// Execute a raw command on the relayer container.
    async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> Result<ExecOutput>;
}

/// Factory function to build a relayer by type.
pub async fn build_relayer(
    relayer_type: RelayerType,
    runtime: Arc<dyn RuntimeBackend>,
    test_name: &str,
    network_id: &str,
) -> Result<Box<dyn Relayer>> {
    match relayer_type {
        RelayerType::CosmosRly => {
            let commander = Box::new(CosmosRlyCommander::new());
            let relayer =
                DockerRelayer::new(commander, runtime, test_name, network_id).await?;
            Ok(Box::new(relayer))
        }
        RelayerType::Hermes => {
            let relayer = HermesRelayer::new(runtime, test_name, network_id).await?;
            Ok(Box::new(relayer))
        }
        RelayerType::Hyperspace => {
            Err(crate::error::IctError::Config(
                "Hyperspace relayer not yet implemented".to_string(),
            ))
        }
    }
}
