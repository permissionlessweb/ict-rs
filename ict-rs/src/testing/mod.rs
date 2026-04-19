//! Test harness for ict-rs integration tests.
//!
//! Provides [`TestChain`] — a wrapper around [`CosmosChain`] that handles
//! setup, teardown, and Docker resource cleanup automatically.
//!
//! # Environment Variables
//!
//! | Variable | Effect |
//! |----------|--------|
//! | `ICT_MOCK=1` | Use mock runtime (no Docker needed) |
//! | `ICT_KEEP_CONTAINERS=1` | Skip cleanup, keep containers alive |
//! | `ICT_SHOW_LOGS=1` | Dump container logs on test failure |
//! | `ICT_SHOW_LOGS=always` | Always dump container logs |
//! | `ICT_IMAGE_REPO` | Override Docker image repository |
//! | `ICT_IMAGE_VERSION` | Override Docker image version |

pub mod env;

use std::ops::Deref;
use std::sync::Arc;

use tracing::info;

use crate::chain::cosmos::CosmosChain;
use crate::chain::{Chain, ChainConfig, TestContext};
use crate::error::Result;
use crate::runtime::RuntimeBackend;
use crate::tx::WalletAmount;

pub use env::{LogMode, TestEnv};

/// Configuration for setting up a test chain.
pub struct TestChainConfig {
    pub chain_config: ChainConfig,
    pub num_validators: usize,
    pub num_full_nodes: usize,
    pub genesis_wallets: Vec<WalletAmount>,
}

/// A test-managed chain with automatic cleanup.
///
/// Wraps [`CosmosChain`] and provides:
/// - Automatic Docker resource cleanup via [`Drop`]
/// - Environment-based configuration ([`TestEnv`])
/// - Log dumping on failure
/// - `ICT_KEEP_CONTAINERS` support for debugging
///
/// Implements [`Deref<Target=CosmosChain>`] so you can call chain methods directly.
pub struct TestChain {
    pub chain: CosmosChain,
    pub runtime: Arc<dyn RuntimeBackend>,
    pub test_name: String,
    pub keep_containers: bool,
    pub show_logs: LogMode,
    pub cleaned_up: bool,
}

impl TestChain {
    /// Set up a test chain with the given config.
    ///
    /// Auto-selects mock or Docker runtime based on `ICT_MOCK` env var.
    pub async fn setup(test_name: &str, config: TestChainConfig) -> Result<Self> {
        let runtime: Arc<dyn RuntimeBackend> = if TestEnv::is_mock() {
            Arc::new(crate::runtime::mock::MockRuntime::new())
        } else {
            #[cfg(feature = "docker")]
            {
                let backend = crate::runtime::docker::DockerBackend::new(
                    crate::runtime::DockerConfig::default(),
                )
                .await?;
                Arc::new(backend)
            }
            #[cfg(not(feature = "docker"))]
            {
                return Err(crate::error::IctError::Runtime(anyhow::anyhow!(
                    "Docker feature not enabled and ICT_MOCK not set"
                )));
            }
        };

        Self::setup_with_runtime(test_name, config, runtime).await
    }

    /// Set up a test chain with a specific runtime (useful for mock injection in tests).
    pub async fn setup_with_runtime(
        test_name: &str,
        config: TestChainConfig,
        runtime: Arc<dyn RuntimeBackend>,
    ) -> Result<Self> {
        let mut chain = CosmosChain::new(
            config.chain_config,
            config.num_validators,
            config.num_full_nodes,
            runtime.clone(),
        );

        // Generate a unique network name using PID + atomic counter.
        // The old millis & 0xFFFF approach caused collisions in parallel tests.
        use std::sync::atomic::{AtomicU32, Ordering};
        static NETWORK_COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique_id = format!(
            "{}-{}",
            std::process::id(),
            NETWORK_COUNTER.fetch_add(1, Ordering::Relaxed),
        );
        let network_id = format!("ict-{test_name}-{unique_id}");

        let ctx = TestContext {
            test_name: test_name.to_string(),
            network_id,
        };

        chain.initialize(&ctx).await?;
        chain.start(&config.genesis_wallets).await?;

        Ok(Self {
            chain,
            runtime,
            test_name: test_name.to_string(),
            keep_containers: TestEnv::keep_containers(),
            show_logs: TestEnv::log_mode(),
            cleaned_up: false,
        })
    }

    /// Whether this test chain is using the mock runtime.
    pub fn is_mock(&self) -> bool {
        TestEnv::is_mock()
    }

    /// Explicitly clean up all Docker resources (containers, volumes, networks).
    ///
    /// Idempotent — safe to call multiple times.
    /// Skips cleanup if `ICT_KEEP_CONTAINERS=1` is set.
    pub async fn cleanup(&mut self) -> Result<()> {
        if self.cleaned_up {
            return Ok(());
        }

        if self.keep_containers {
            info!(
                test = %self.test_name,
                "ICT_KEEP_CONTAINERS=1: skipping cleanup"
            );
            self.cleaned_up = true;
            return Ok(());
        }

        self.do_cleanup().await?;
        self.cleaned_up = true;
        Ok(())
    }

    /// Clean up with test result context — dumps logs on failure if configured.
    pub async fn cleanup_with_result(&mut self, passed: bool) -> Result<()> {
        if !passed
            && (self.show_logs == LogMode::OnFailure || self.show_logs == LogMode::Always)
        {
            self.dump_logs().await;
        } else if self.show_logs == LogMode::Always {
            self.dump_logs().await;
        }

        self.cleanup().await
    }

    /// Dump container logs to stderr for debugging.
    pub async fn dump_logs(&self) {
        for id in self.chain.container_ids() {
            match self.runtime.container_logs(&id).await {
                Ok(logs) => {
                    eprintln!("=== Container {} logs ===", id.0);
                    eprintln!("{logs}");
                    eprintln!("=== End container {} ===", id.0);
                }
                Err(e) => {
                    eprintln!("Failed to get logs for container {}: {e}", id.0);
                }
            }
        }
    }

    /// Internal cleanup implementation.
    async fn do_cleanup(&mut self) -> Result<()> {
        info!(test = %self.test_name, "Cleaning up test resources");
        self.chain.stop().await
    }
}

impl Drop for TestChain {
    fn drop(&mut self) {
        if self.cleaned_up || self.keep_containers {
            return;
        }

        // Try to clean up using the current tokio runtime.
        // This mirrors Go's t.Cleanup() — runs even on panic.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let mut chain = std::mem::replace(
                &mut self.chain,
                // Create a dummy chain that won't do anything on stop.
                // We need to move `self.chain` out so we can call stop() in a
                // separate thread without holding &mut self.
                CosmosChain::new(
                    ChainConfig {
                        chain_type: crate::chain::ChainType::Cosmos,
                        name: String::new(),
                        chain_id: String::new(),
                        images: Vec::new(),
                        bin: String::new(),
                        bech32_prefix: String::new(),
                        denom: String::new(),
                        coin_type: 0,
                        signing_algorithm: Default::default(),
                        gas_prices: String::new(),
                        gas_adjustment: 0.0,
                        trusting_period: String::new(),
                        block_time: "2s".to_string(),
                        genesis: None,
                        modify_genesis: None,
                        pre_genesis: None,
                        config_file_overrides: Default::default(),
                        additional_start_args: Vec::new(),
                        env: Vec::new(),
                        sidecar_configs: Vec::new(),
                        faucet: None,
                        genesis_style: Default::default(),
                    },
                    0,
                    0,
                    self.runtime.clone(),
                ),
            );

            // Run async cleanup in a blocking thread to avoid
            // "Cannot block the current thread from within a runtime" panic.
            std::thread::spawn(move || {
                handle.block_on(async {
                    if let Err(e) = chain.stop().await {
                        eprintln!("[ict-rs] Drop cleanup failed: {e}");
                    }
                });
            })
            .join()
            .ok();
        } else {
            eprintln!(
                "[ict-rs] WARNING: TestChain dropped without cleanup and no tokio runtime available."
            );
            eprintln!(
                "[ict-rs] Leaked containers may still be running. Clean up manually with:"
            );
            eprintln!(
                "[ict-rs]   docker ps -a --filter label=ict.test={} -q | xargs docker rm -f",
                self.test_name
            );
        }
    }
}

impl Deref for TestChain {
    type Target = CosmosChain;

    fn deref(&self) -> &Self::Target {
        &self.chain
    }
}

/// Convenience: set up a 1-validator chain with no extra wallets.
pub async fn setup_chain(test_name: &str, chain_config: ChainConfig) -> Result<TestChain> {
    TestChain::setup(
        test_name,
        TestChainConfig {
            chain_config,
            num_validators: 1,
            num_full_nodes: 0,
            genesis_wallets: Vec::new(),
        },
    )
    .await
}

// -- Ethereum/Anvil test support --

#[cfg(feature = "ethereum")]
pub use anvil::*;

#[cfg(feature = "ethereum")]
mod anvil {
    use std::ops::Deref;
    use std::sync::Arc;

    use crate::chain::ethereum::AnvilChain;
    use crate::chain::{Chain, TestContext};
    use crate::error::Result;
    use crate::runtime::RuntimeBackend;
    use crate::spec::builtin_chain_config;
    use crate::testing::env::{LogMode, TestEnv};

    /// A test-managed Anvil chain with automatic cleanup.
    pub struct TestEthChain {
        pub chain: AnvilChain,
        pub runtime: Arc<dyn RuntimeBackend>,
        pub test_name: String,
        pub keep_containers: bool,
        pub show_logs: LogMode,
        pub cleaned_up: bool,
    }

    impl TestEthChain {
        /// Set up an Anvil test chain.
        pub async fn setup(test_name: &str) -> Result<Self> {
            let runtime: Arc<dyn RuntimeBackend> = if TestEnv::is_mock() {
                Arc::new(crate::runtime::mock::MockRuntime::new())
            } else {
                #[cfg(feature = "docker")]
                {
                    let backend = crate::runtime::docker::DockerBackend::new(
                        crate::runtime::DockerConfig::default(),
                    )
                    .await?;
                    Arc::new(backend)
                }
                #[cfg(not(feature = "docker"))]
                {
                    return Err(crate::error::IctError::Runtime(anyhow::anyhow!(
                        "Docker feature not enabled and ICT_MOCK not set"
                    )));
                }
            };

            Self::setup_with_runtime(test_name, runtime).await
        }

        /// Set up an Anvil chain with a specific runtime.
        pub async fn setup_with_runtime(
            test_name: &str,
            runtime: Arc<dyn RuntimeBackend>,
        ) -> Result<Self> {
            let cfg = TestEnv::anvil_config();
            let mut chain = AnvilChain::new(cfg, runtime.clone());

            use std::sync::atomic::{AtomicU32, Ordering};
            static ETH_NETWORK_COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique_id = format!(
                "{}-{}",
                std::process::id(),
                ETH_NETWORK_COUNTER.fetch_add(1, Ordering::Relaxed),
            );
            let network_id = format!("ict-{test_name}-{unique_id}");

            let ctx = TestContext {
                test_name: test_name.to_string(),
                network_id,
            };

            chain.initialize(&ctx).await?;
            chain.start(&[]).await?;

            Ok(Self {
                chain,
                runtime,
                test_name: test_name.to_string(),
                keep_containers: TestEnv::keep_containers(),
                show_logs: TestEnv::log_mode(),
                cleaned_up: false,
            })
        }

        pub async fn cleanup(&mut self) -> Result<()> {
            if self.cleaned_up || self.keep_containers {
                return Ok(());
            }
            self.chain.stop().await?;
            self.cleaned_up = true;
            Ok(())
        }
    }

    impl Deref for TestEthChain {
        type Target = AnvilChain;

        fn deref(&self) -> &Self::Target {
            &self.chain
        }
    }

    impl Drop for TestEthChain {
        fn drop(&mut self) {
            if self.cleaned_up || self.keep_containers {
                return;
            }
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let mut chain = std::mem::replace(
                    &mut self.chain,
                    AnvilChain::new(
                        builtin_chain_config("anvil").unwrap(),
                        self.runtime.clone(),
                    ),
                );
                std::thread::spawn(move || {
                    handle.block_on(async {
                        if let Err(e) = chain.stop().await {
                            eprintln!("[ict-rs] TestEthChain drop cleanup failed: {e}");
                        }
                    });
                })
                .join()
                .ok();
            }
        }
    }

    /// Convenience function to set up a test Anvil chain.
    pub async fn setup_anvil(test_name: &str) -> Result<TestEthChain> {
        TestEthChain::setup(test_name).await
    }
}

/// Convenience: set up a 1-validator chain with genesis-funded wallets.
pub async fn setup_chain_with_wallets(
    test_name: &str,
    chain_config: ChainConfig,
    wallets: Vec<WalletAmount>,
) -> Result<TestChain> {
    TestChain::setup(
        test_name,
        TestChainConfig {
            chain_config,
            num_validators: 1,
            num_full_nodes: 0,
            genesis_wallets: wallets,
        },
    )
    .await
}

// -- Example Relayer for mock-mode examples --

use crate::ibc::{ChannelCounterparty, ChannelOptions, ChannelOutput, ClientOptions, ConnectionOutput};
use crate::relayer::Relayer;
use crate::tx::ExecOutput;
use crate::wallet::{KeyWallet, Wallet};
use async_trait::async_trait;
use std::sync::Mutex;

/// A mock relayer for use in examples and integration tests.
///
/// Tracks configured chains and IBC links, and returns realistic mock
/// `ChannelOutput` values from `get_channels()` so examples can reference
/// `channel-0`, `channel-1`, etc.
#[cfg(feature = "testing")]
pub struct ExampleRelayer {
    configured_chains: Mutex<Vec<String>>,
    /// Tracks linked paths: `(path_name, src_chain, dst_chain, channel_index)`.
    links: Mutex<Vec<(String, String, String, usize)>>,
    next_channel: Mutex<usize>,
}

#[cfg(feature = "testing")]
impl ExampleRelayer {
    /// Create a new `ExampleRelayer`.
    pub fn new() -> Box<Self> {
        Box::new(Self {
            configured_chains: Mutex::new(Vec::new()),
            links: Mutex::new(Vec::new()),
            next_channel: Mutex::new(0),
        })
    }
}

#[cfg(feature = "testing")]
impl Default for ExampleRelayer {
    fn default() -> Self {
        Self {
            configured_chains: Mutex::new(Vec::new()),
            links: Mutex::new(Vec::new()),
            next_channel: Mutex::new(0),
        }
    }
}

#[cfg(feature = "testing")]
#[async_trait]
impl Relayer for ExampleRelayer {
    async fn add_key(
        &self,
        chain_id: &str,
        key_name: &str,
    ) -> crate::error::Result<Box<dyn Wallet>> {
        Ok(Box::new(KeyWallet {
            key_name: key_name.to_string(),
            address_bytes: vec![0u8; 20],
            bech32_address: format!("cosmos1relayer{chain_id}"),
            mnemonic_phrase: String::new(),
        }))
    }

    async fn restore_key(
        &self,
        _chain_id: &str,
        _key_name: &str,
        _mnemonic: &str,
    ) -> crate::error::Result<()> {
        Ok(())
    }

    fn get_wallet(&self, _chain_id: &str) -> Option<&dyn Wallet> {
        None
    }

    async fn add_chain_configuration(
        &self,
        config: &crate::chain::ChainConfig,
        _key_name: &str,
        _rpc_addr: &str,
        _grpc_addr: &str,
    ) -> crate::error::Result<()> {
        self.configured_chains
            .lock()
            .unwrap()
            .push(config.chain_id.clone());
        println!("  Relayer: configured chain {}", config.chain_id);
        Ok(())
    }

    async fn generate_path(
        &self,
        src: &str,
        dst: &str,
        path_name: &str,
    ) -> crate::error::Result<()> {
        println!("  Relayer: generated path '{path_name}' ({src} <-> {dst})");
        Ok(())
    }

    async fn link_path(
        &self,
        path_name: &str,
        _opts: &ChannelOptions,
    ) -> crate::error::Result<()> {
        let idx = {
            let mut next = self.next_channel.lock().unwrap();
            let idx = *next;
            *next += 1;
            idx
        };
        self.links.lock().unwrap().push((
            path_name.to_string(),
            String::new(), // filled by generate_path context
            String::new(),
            idx,
        ));
        println!("  Relayer: linked path '{path_name}' → channel-{idx} (clients + connections + channel)");
        Ok(())
    }

    async fn create_clients(
        &self,
        _path_name: &str,
        _opts: &ClientOptions,
    ) -> crate::error::Result<()> {
        Ok(())
    }

    async fn create_connections(&self, _path_name: &str) -> crate::error::Result<()> {
        Ok(())
    }

    async fn create_channel(
        &self,
        _path_name: &str,
        _opts: &ChannelOptions,
    ) -> crate::error::Result<()> {
        Ok(())
    }

    async fn update_clients(&self, _path_name: &str) -> crate::error::Result<()> {
        Ok(())
    }

    async fn start(&self, path_names: &[&str]) -> crate::error::Result<()> {
        println!(
            "  Relayer: started on paths: {}",
            path_names.join(", ")
        );
        Ok(())
    }

    async fn stop(&self) -> crate::error::Result<()> {
        println!("  Relayer: stopped");
        Ok(())
    }

    async fn flush(&self, _path_name: &str, _channel_id: &str) -> crate::error::Result<()> {
        Ok(())
    }

    async fn get_channels(&self, _chain_id: &str) -> crate::error::Result<Vec<ChannelOutput>> {
        let links = self.links.lock().unwrap();
        let channels: Vec<ChannelOutput> = links
            .iter()
            .map(|(_, _, _, idx)| ChannelOutput {
                state: "STATE_OPEN".to_string(),
                ordering: "ORDER_UNORDERED".to_string(),
                version: "ics20-1".to_string(),
                port_id: "transfer".to_string(),
                channel_id: format!("channel-{idx}"),
                connection_hops: vec!["connection-0".to_string()],
                counterparty: ChannelCounterparty {
                    port_id: "transfer".to_string(),
                    channel_id: format!("channel-{idx}"),
                },
            })
            .collect();
        Ok(channels)
    }

    async fn get_connections(
        &self,
        _chain_id: &str,
    ) -> crate::error::Result<Vec<ConnectionOutput>> {
        Ok(Vec::new())
    }

    async fn exec(
        &self,
        _cmd: &[&str],
        _env: &[(&str, &str)],
    ) -> crate::error::Result<ExecOutput> {
        Ok(ExecOutput::default())
    }
}
