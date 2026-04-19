//! Comprehensive unit tests for ict-rs.
//!
//! These tests exercise the mock runtime, chain configuration, genesis helpers,
//! wallet/auth, IBC types, interchain builder, chain specs, reporter,
//! CosmosChain, and ChainNode — all without requiring Docker.

use std::collections::HashMap;
use std::sync::Arc;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::genesis::{get_genesis_module_value, set_genesis_module_value};
use ict_rs::ibc::{ChannelCounterparty, ChannelOptions, ChannelOrdering, ChannelOutput, ClientOptions};
use ict_rs::interchain::{Interchain, InterchainBuildOptions, InterchainLink};
use ict_rs::node::ChainNode;
use ict_rs::reporter::{ExecReport, TestReporter};
use ict_rs::runtime::mock::{MockContainerStatus, MockRuntime};
use ict_rs::runtime::{ContainerId, DockerImage, RuntimeBackend};
use ict_rs::spec::ChainSpec;
use ict_rs::tx::ExecOutput;
use ict_rs::wallet::{KeyWallet, Wallet};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a minimal Cosmos ChainConfig for testing.
fn test_chain_config(chain_id: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "testchain".to_string(),
        chain_id: chain_id.to_string(),
        images: vec![DockerImage {
            repository: "ghcr.io/test/chain".to_string(),
            version: "v1.0.0".to_string(),
            uid_gid: None,
        }],
        bin: "chaind".to_string(),
        bech32_prefix: "cosmos".to_string(),
        denom: "ustake".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.025ustake".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "336h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: HashMap::new(),
        additional_start_args: Vec::new(),
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

/// A well-known 12-word BIP39 test mnemonic.
const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

// ===========================================================================
// mock_runtime_tests
// ===========================================================================
mod mock_runtime_tests {
    use super::*;

    #[tokio::test]
    async fn test_create_and_start_container() {
        let rt = MockRuntime::new();
        let opts = ict_rs::runtime::ContainerOptions {
            image: DockerImage {
                repository: "test".to_string(),
                version: "latest".to_string(),
                uid_gid: None,
            },
            name: "my-container".to_string(),
            network_id: None,
            env: Vec::new(),
            cmd: Vec::new(),
            entrypoint: None,
            ports: Vec::new(),
            volumes: Vec::new(),
            labels: Vec::new(),
            hostname: None,
        };

        let id = rt.create_container(&opts).await.expect("create should succeed");

        {
            let state = rt.state();
            let guard = state.lock().unwrap();
            let c = guard.containers.get(&id.0).expect("container should exist");
            assert_eq!(c.status, MockContainerStatus::Created, "status should be Created after create");
            assert_eq!(c.name, "my-container");
        }

        rt.start_container(&id).await.expect("start should succeed");

        {
            let state = rt.state();
            let guard = state.lock().unwrap();
            let c = guard.containers.get(&id.0).unwrap();
            assert_eq!(c.status, MockContainerStatus::Running, "status should be Running after start");
        }
    }

    #[tokio::test]
    async fn test_pull_image_records() {
        let rt = MockRuntime::new();
        let img = DockerImage {
            repository: "ghcr.io/test/img".to_string(),
            version: "v2.0.0".to_string(),
            uid_gid: None,
        };

        rt.pull_image(&img).await.expect("pull should succeed");
        rt.pull_image(&img).await.expect("second pull should succeed");

        let state = rt.state();
        let guard = state.lock().unwrap();
        assert_eq!(guard.pulled_images.len(), 2, "should record each pull");
        assert_eq!(guard.pulled_images[0], "ghcr.io/test/img:v2.0.0");
    }

    #[tokio::test]
    async fn test_exec_returns_queued_response() {
        let rt = MockRuntime::new();
        let opts = ict_rs::runtime::ContainerOptions {
            image: DockerImage {
                repository: "x".to_string(),
                version: "1".to_string(),
                uid_gid: None,
            },
            name: "exec-test".to_string(),
            network_id: None,
            env: Vec::new(),
            cmd: Vec::new(),
            entrypoint: None,
            ports: Vec::new(),
            volumes: Vec::new(),
            labels: Vec::new(),
            hostname: None,
        };
        let id = rt.create_container(&opts).await.unwrap();
        rt.start_container(&id).await.unwrap();

        let queued = ExecOutput {
            stdout: b"hello world".to_vec(),
            stderr: Vec::new(),
            exit_code: 0,
        };
        rt.queue_exec_response(&id.0, queued);

        let output = rt.exec_in_container(&id, &["echo", "hello"], &[]).await.unwrap();
        assert_eq!(output.stdout_str(), "hello world", "should return queued response");
    }

    #[tokio::test]
    async fn test_exec_returns_default_when_empty() {
        let rt = MockRuntime::new();
        let opts = ict_rs::runtime::ContainerOptions {
            image: DockerImage {
                repository: "x".to_string(),
                version: "1".to_string(),
                uid_gid: None,
            },
            name: "default-exec".to_string(),
            network_id: None,
            env: Vec::new(),
            cmd: Vec::new(),
            entrypoint: None,
            ports: Vec::new(),
            volumes: Vec::new(),
            labels: Vec::new(),
            hostname: None,
        };
        let id = rt.create_container(&opts).await.unwrap();
        rt.start_container(&id).await.unwrap();

        let output = rt.exec_in_container(&id, &["ls"], &[]).await.unwrap();
        assert_eq!(output.exit_code, 0, "default exec should return exit code 0");
        assert!(output.stdout.is_empty(), "default exec should have empty stdout");
    }

    #[tokio::test]
    async fn test_create_and_remove_network() {
        let rt = MockRuntime::new();
        let net_id = rt.create_network("test-net").await.expect("create network should succeed");

        {
            let state = rt.state();
            let guard = state.lock().unwrap();
            assert!(guard.networks.contains_key(&net_id.0), "network should be tracked");
            assert_eq!(guard.networks[&net_id.0], "test-net");
        }

        rt.remove_network(&net_id).await.expect("remove network should succeed");

        {
            let state = rt.state();
            let guard = state.lock().unwrap();
            assert!(!guard.networks.contains_key(&net_id.0), "network should be removed");
        }
    }

    #[tokio::test]
    async fn test_stop_already_stopped() {
        let rt = MockRuntime::new();
        let opts = ict_rs::runtime::ContainerOptions {
            image: DockerImage {
                repository: "x".to_string(),
                version: "1".to_string(),
                uid_gid: None,
            },
            name: "stop-test".to_string(),
            network_id: None,
            env: Vec::new(),
            cmd: Vec::new(),
            entrypoint: None,
            ports: Vec::new(),
            volumes: Vec::new(),
            labels: Vec::new(),
            hostname: None,
        };
        let id = rt.create_container(&opts).await.unwrap();

        // Stop a container that was never started (Created -> Stopped).
        rt.stop_container(&id).await.expect("stopping a created container should succeed");
        {
            let state = rt.state();
            let guard = state.lock().unwrap();
            let c = guard.containers.get(&id.0).unwrap();
            assert_eq!(c.status, MockContainerStatus::Stopped);
        }

        // Stop it again — should still succeed.
        rt.stop_container(&id).await.expect("stopping an already stopped container should succeed");
    }

    #[tokio::test]
    async fn test_remove_nonexistent_fails() {
        let rt = MockRuntime::new();
        let bogus = ContainerId("does-not-exist".to_string());
        let result = rt.remove_container(&bogus).await;
        assert!(result.is_err(), "removing a nonexistent container should fail");
    }

    #[tokio::test]
    async fn test_container_logs() {
        let rt = MockRuntime::new();
        let opts = ict_rs::runtime::ContainerOptions {
            image: DockerImage {
                repository: "x".to_string(),
                version: "1".to_string(),
                uid_gid: None,
            },
            name: "logs-test".to_string(),
            network_id: None,
            env: Vec::new(),
            cmd: Vec::new(),
            entrypoint: None,
            ports: Vec::new(),
            volumes: Vec::new(),
            labels: Vec::new(),
            hostname: None,
        };
        let id = rt.create_container(&opts).await.unwrap();
        rt.set_container_logs(&id.0, "line1\nline2\n");

        let logs = rt.container_logs(&id).await.unwrap();
        assert_eq!(logs, "line1\nline2\n", "should return previously set logs");
    }

    #[tokio::test]
    async fn test_wait_for_container() {
        let rt = MockRuntime::new();
        let opts = ict_rs::runtime::ContainerOptions {
            image: DockerImage {
                repository: "x".to_string(),
                version: "1".to_string(),
                uid_gid: None,
            },
            name: "wait-test".to_string(),
            network_id: None,
            env: Vec::new(),
            cmd: Vec::new(),
            entrypoint: None,
            ports: Vec::new(),
            volumes: Vec::new(),
            labels: Vec::new(),
            hostname: None,
        };
        let id = rt.create_container(&opts).await.unwrap();
        let exit = rt.wait_for_container(&id).await.unwrap();
        assert_eq!(exit.code, 0, "wait should return exit code 0");
    }
}

// ===========================================================================
// chain_config_tests
// ===========================================================================
mod chain_config_tests {
    use super::*;

    #[test]
    fn test_chain_type_variants() {
        let cosmos = ChainType::Cosmos;
        let eth = ChainType::Ethereum;
        let pen = ChainType::Penumbra;
        let pol = ChainType::Polkadot;
        let thor = ChainType::Thorchain;
        let utxo = ChainType::Utxo;
        let nam = ChainType::Namada;

        // Verify Debug and PartialEq
        assert_eq!(cosmos, ChainType::Cosmos);
        assert_ne!(cosmos, eth);
        assert_ne!(eth, pen);
        assert_ne!(pol, thor);
        assert_ne!(utxo, nam);
        assert_eq!(format!("{:?}", cosmos), "Cosmos");
        assert_eq!(format!("{:?}", eth), "Ethereum");
    }

    #[test]
    fn test_signing_algorithm_default() {
        let alg: SigningAlgorithm = Default::default();
        assert!(matches!(alg, SigningAlgorithm::Secp256k1), "default signing algorithm should be Secp256k1");
    }

    #[test]
    fn test_chain_config_debug() {
        let cfg = test_chain_config("test-1");
        let dbg = format!("{:?}", cfg);
        assert!(dbg.contains("testchain"), "debug output should contain name");
        assert!(dbg.contains("test-1"), "debug output should contain chain_id");
        assert!(dbg.contains("ustake"), "debug output should contain denom");
        assert!(dbg.contains("chaind"), "debug output should contain bin");
    }
}

// ===========================================================================
// genesis_tests
// ===========================================================================
mod genesis_tests {
    use super::*;

    #[test]
    fn test_set_genesis_module_value() {
        let mut genesis = serde_json::json!({
            "app_state": {
                "staking": {
                    "params": {
                        "bond_denom": "uatom"
                    }
                }
            }
        });

        set_genesis_module_value(
            &mut genesis,
            &["app_state", "staking", "params", "bond_denom"],
            serde_json::json!("uterp"),
        )
        .expect("set should succeed");

        assert_eq!(
            genesis["app_state"]["staking"]["params"]["bond_denom"],
            "uterp",
            "bond_denom should be updated"
        );
    }

    #[test]
    fn test_set_nested_genesis_value() {
        let mut genesis = serde_json::json!({
            "app_state": {
                "gov": {
                    "params": {
                        "voting_period": "172800s"
                    }
                }
            }
        });

        set_genesis_module_value(
            &mut genesis,
            &["app_state", "gov", "params", "voting_period"],
            serde_json::json!("60s"),
        )
        .expect("set should succeed");

        assert_eq!(
            genesis["app_state"]["gov"]["params"]["voting_period"],
            "60s"
        );
    }

    #[test]
    fn test_get_genesis_module_value() {
        let genesis = serde_json::json!({
            "app_state": {
                "bank": {
                    "params": {
                        "default_send_enabled": true
                    }
                }
            }
        });

        let val = get_genesis_module_value(
            &genesis,
            &["app_state", "bank", "params", "default_send_enabled"],
        );
        assert_eq!(val, Some(&serde_json::json!(true)));
    }

    #[test]
    fn test_get_genesis_missing_key() {
        let genesis = serde_json::json!({"app_state": {}});
        let val = get_genesis_module_value(&genesis, &["app_state", "nope", "missing"]);
        assert!(val.is_none(), "missing path should return None");
    }

    #[test]
    fn test_set_genesis_missing_path_errors() {
        let mut genesis = serde_json::json!({"app_state": {}});
        let result = set_genesis_module_value(
            &mut genesis,
            &["app_state", "nonexistent_module", "params", "key"],
            serde_json::json!("value"),
        );
        assert!(result.is_err(), "setting through a nonexistent intermediate key should error");
    }
}

// ===========================================================================
// wallet_tests
// ===========================================================================
mod wallet_tests {
    use super::*;

    #[test]
    fn test_key_wallet_from_mnemonic() {
        let wallet = KeyWallet::from_mnemonic("test-key", TEST_MNEMONIC, "cosmos", 118)
            .expect("should create wallet from valid mnemonic");

        assert_eq!(wallet.key_name, "test-key");
        assert!(!wallet.address_bytes.is_empty(), "address bytes should not be empty");
        assert_eq!(wallet.address_bytes.len(), 20, "cosmos address bytes should be 20 bytes");
        assert!(
            wallet.bech32_address.starts_with("cosmos1"),
            "bech32 address should start with cosmos1, got: {}",
            wallet.bech32_address
        );
    }

    #[test]
    fn test_key_wallet_trait_methods() {
        let wallet = KeyWallet::from_mnemonic("mykey", TEST_MNEMONIC, "cosmos", 118).unwrap();

        assert_eq!(wallet.key_name(), "mykey");
        assert_eq!(wallet.address().len(), 20);
        assert!(wallet.formatted_address().starts_with("cosmos1"));
        assert_eq!(wallet.mnemonic(), TEST_MNEMONIC);
    }

    #[test]
    fn test_key_wallet_different_prefixes() {
        let cosmos = KeyWallet::from_mnemonic("k", TEST_MNEMONIC, "cosmos", 118).unwrap();
        let osmo = KeyWallet::from_mnemonic("k", TEST_MNEMONIC, "osmo", 118).unwrap();
        let terp = KeyWallet::from_mnemonic("k", TEST_MNEMONIC, "terp", 118).unwrap();

        assert!(cosmos.formatted_address().starts_with("cosmos1"));
        assert!(osmo.formatted_address().starts_with("osmo1"));
        assert!(terp.formatted_address().starts_with("terp1"));

        // Same mnemonic + coin_type => same raw address bytes.
        assert_eq!(cosmos.address(), osmo.address(), "same mnemonic should derive same raw address");
        assert_eq!(cosmos.address(), terp.address());
    }

    #[test]
    fn test_key_wallet_invalid_mnemonic() {
        let result = KeyWallet::from_mnemonic("k", "not a valid mnemonic phrase", "cosmos", 118);
        assert!(result.is_err(), "invalid mnemonic should produce an error");
    }
}

// ===========================================================================
// ibc_types_tests
// ===========================================================================
mod ibc_types_tests {
    use super::*;

    #[test]
    fn test_channel_ordering_display() {
        assert_eq!(format!("{}", ChannelOrdering::Unordered), "unordered");
        assert_eq!(format!("{}", ChannelOrdering::Ordered), "ordered");
    }

    #[test]
    fn test_channel_options_default() {
        let opts: ChannelOptions = Default::default();
        assert!(opts.src_port.is_empty(), "default src_port should be empty");
        assert!(opts.dst_port.is_empty(), "default dst_port should be empty");
        assert!(matches!(opts.ordering, ChannelOrdering::Unordered));
        assert!(opts.version.is_empty());
    }

    #[test]
    fn test_client_options_default() {
        let opts: ClientOptions = Default::default();
        assert!(opts.trusting_period.is_none());
        assert!(opts.max_clock_drift.is_none());
    }

    #[test]
    fn test_channel_output_serialize_roundtrip() {
        let output = ChannelOutput {
            state: "STATE_OPEN".to_string(),
            ordering: "ORDER_UNORDERED".to_string(),
            version: "ics20-1".to_string(),
            port_id: "transfer".to_string(),
            channel_id: "channel-0".to_string(),
            connection_hops: vec!["connection-0".to_string()],
            counterparty: ChannelCounterparty {
                port_id: "transfer".to_string(),
                channel_id: "channel-1".to_string(),
            },
        };

        let json = serde_json::to_string(&output).expect("serialize should succeed");
        let deserialized: ChannelOutput =
            serde_json::from_str(&json).expect("deserialize should succeed");

        assert_eq!(deserialized.state, "STATE_OPEN");
        assert_eq!(deserialized.channel_id, "channel-0");
        assert_eq!(deserialized.counterparty.channel_id, "channel-1");
        assert_eq!(deserialized.connection_hops.len(), 1);
    }
}

// ===========================================================================
// interchain_builder_tests
// ===========================================================================
mod interchain_builder_tests {
    use super::*;
    use async_trait::async_trait;
    use ict_rs::ibc::{ConnectionOutput, ClientOptions};
    use ict_rs::relayer::Relayer;

    fn mock_runtime_arc() -> Arc<dyn RuntimeBackend> {
        Arc::new(MockRuntime::new())
    }

    /// A minimal mock relayer for testing the Interchain builder.
    struct MockRelayer;

    #[async_trait]
    impl Relayer for MockRelayer {
        async fn add_key(&self, _chain_id: &str, _key_name: &str) -> ict_rs::error::Result<Box<dyn Wallet>> {
            Ok(Box::new(KeyWallet {
                key_name: "mock-key".to_string(),
                address_bytes: vec![0u8; 20],
                bech32_address: "cosmos1mock".to_string(),
                mnemonic_phrase: "mock".to_string(),
            }))
        }
        async fn restore_key(&self, _chain_id: &str, _key_name: &str, _mnemonic: &str) -> ict_rs::error::Result<()> {
            Ok(())
        }
        fn get_wallet(&self, _chain_id: &str) -> Option<&dyn Wallet> {
            None
        }
        async fn add_chain_configuration(
            &self, _config: &ict_rs::chain::ChainConfig, _key_name: &str, _rpc_addr: &str, _grpc_addr: &str,
        ) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn generate_path(&self, _src: &str, _dst: &str, _path: &str) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn link_path(&self, _path: &str, _opts: &ChannelOptions) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn create_clients(&self, _path: &str, _opts: &ClientOptions) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn create_connections(&self, _path: &str) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn create_channel(&self, _path: &str, _opts: &ChannelOptions) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn update_clients(&self, _path: &str) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn start(&self, _path_names: &[&str]) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn stop(&self) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn flush(&self, _path: &str, _channel_id: &str) -> ict_rs::error::Result<()> {
            Ok(())
        }
        async fn get_channels(&self, _chain_id: &str) -> ict_rs::error::Result<Vec<ChannelOutput>> {
            Ok(Vec::new())
        }
        async fn get_connections(&self, _chain_id: &str) -> ict_rs::error::Result<Vec<ConnectionOutput>> {
            Ok(Vec::new())
        }
        async fn exec(&self, _cmd: &[&str], _env: &[(&str, &str)]) -> ict_rs::error::Result<ExecOutput> {
            Ok(ExecOutput::default())
        }
    }

    #[test]
    fn test_builder_add_chain() {
        let rt = mock_runtime_arc();
        let chain = CosmosChain::new(test_chain_config("chain-a"), 1, 0, rt.clone());
        let ic = Interchain::new(rt).add_chain(Box::new(chain));

        assert!(ic.get_chain("chain-a").is_some(), "chain-a should be registered");
        assert!(ic.get_chain("chain-b").is_none(), "chain-b should not exist");
    }

    #[test]
    fn test_builder_add_relayer() {
        let rt = mock_runtime_arc();
        let ic = Interchain::new(rt).add_relayer("hermes", Box::new(MockRelayer));

        assert!(ic.get_relayer("hermes").is_some(), "hermes relayer should be registered");
        assert!(ic.get_relayer("rly").is_none(), "rly should not exist");
    }

    #[tokio::test]
    async fn test_builder_add_link() {
        let rt = mock_runtime_arc();
        let chain_a = CosmosChain::new(test_chain_config("chain-a"), 1, 0, rt.clone());
        let chain_b = CosmosChain::new(test_chain_config("chain-b"), 1, 0, rt.clone());

        let ic = Interchain::new(rt)
            .add_chain(Box::new(chain_a))
            .add_chain(Box::new(chain_b))
            .add_link(InterchainLink {
                chain1: "chain-a".to_string(),
                chain2: "chain-b".to_string(),
                relayer: "hermes".to_string(),
                path: "transfer".to_string(),
            });

        assert!(ic.get_chain("chain-a").is_some());
        assert!(ic.get_chain("chain-b").is_some());
    }

    #[tokio::test]
    async fn test_build_validates_chain_references() {
        let rt = mock_runtime_arc();
        // No chains added, but link references chain-a and chain-b.
        let mut ic = Interchain::new(rt).add_link(InterchainLink {
            chain1: "chain-a".to_string(),
            chain2: "chain-b".to_string(),
            relayer: "hermes".to_string(),
            path: "transfer".to_string(),
        });

        let result = ic
            .build(InterchainBuildOptions {
                test_name: "validate-test".to_string(),
                ..Default::default()
            })
            .await;

        assert!(result.is_err(), "build should fail when link references unknown chain");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("unknown chain"),
            "error should mention unknown chain, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_build_validates_relayer_references() {
        let rt = mock_runtime_arc();
        let chain_a = CosmosChain::new(test_chain_config("chain-a"), 1, 0, rt.clone());
        let chain_b = CosmosChain::new(test_chain_config("chain-b"), 1, 0, rt.clone());

        let mut ic = Interchain::new(rt)
            .add_chain(Box::new(chain_a))
            .add_chain(Box::new(chain_b))
            .add_link(InterchainLink {
                chain1: "chain-a".to_string(),
                chain2: "chain-b".to_string(),
                relayer: "nonexistent-relayer".to_string(),
                path: "transfer".to_string(),
            });

        let result = ic
            .build(InterchainBuildOptions {
                test_name: "validate-relayer".to_string(),
                ..Default::default()
            })
            .await;

        assert!(result.is_err(), "build should fail when link references unknown relayer");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("unknown relayer"),
            "error should mention unknown relayer, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_double_build_fails() {
        let rt = mock_runtime_arc();
        let chain_a = CosmosChain::new(test_chain_config("chain-a"), 1, 0, rt.clone());

        let mut ic = Interchain::new(rt).add_chain(Box::new(chain_a));

        // First build should succeed (no links = no relayer validation needed).
        ic.build(InterchainBuildOptions {
            test_name: "double-build".to_string(),
            skip_path_creation: true,
            ..Default::default()
        })
        .await
        .expect("first build should succeed");

        assert!(ic.is_built(), "should be marked as built");

        // Second build should fail.
        let result = ic
            .build(InterchainBuildOptions {
                test_name: "double-build-2".to_string(),
                ..Default::default()
            })
            .await;

        assert!(result.is_err(), "second build should fail");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("already built"),
            "error should mention already built, got: {err_msg}"
        );
    }
}

// ===========================================================================
// spec_tests
// ===========================================================================
mod spec_tests {
    use super::*;

    #[test]
    fn test_resolve_gaia() {
        let spec = ChainSpec {
            name: "gaia".to_string(),
            version: None,
            num_validators: None,
            num_full_nodes: None,
            chain_id: None,
            denom: None,
            bech32_prefix: None,
            gas_prices: None,
        };

        let cfg = spec.resolve().expect("gaia spec should resolve");
        assert_eq!(cfg.name, "gaia");
        assert_eq!(cfg.chain_id, "cosmoshub-test-1");
        assert_eq!(cfg.denom, "uatom");
        assert_eq!(cfg.bech32_prefix, "cosmos");
        assert_eq!(cfg.bin, "gaiad");
        assert_eq!(cfg.chain_type, ChainType::Cosmos);
    }

    #[test]
    fn test_resolve_osmosis() {
        let spec = ChainSpec {
            name: "osmosis".to_string(),
            version: None,
            num_validators: None,
            num_full_nodes: None,
            chain_id: None,
            denom: None,
            bech32_prefix: None,
            gas_prices: None,
        };

        let cfg = spec.resolve().expect("osmosis spec should resolve");
        assert_eq!(cfg.name, "osmosis");
        assert_eq!(cfg.denom, "uosmo");
        assert_eq!(cfg.bech32_prefix, "osmo");
        assert_eq!(cfg.bin, "osmosisd");
    }

    #[test]
    fn test_resolve_terp() {
        let spec = ChainSpec {
            name: "terp".to_string(),
            version: None,
            num_validators: None,
            num_full_nodes: None,
            chain_id: None,
            denom: None,
            bech32_prefix: None,
            gas_prices: None,
        };

        let cfg = spec.resolve().expect("terp spec should resolve");
        assert_eq!(cfg.name, "terp");
        assert_eq!(cfg.denom, "uterp");
        assert_eq!(cfg.bech32_prefix, "terp");
        assert_eq!(cfg.bin, "terpd");
        assert_eq!(cfg.chain_id, "terp-test-1");
    }

    #[test]
    fn test_resolve_juno() {
        let spec = ChainSpec {
            name: "juno".to_string(),
            version: None,
            num_validators: None,
            num_full_nodes: None,
            chain_id: None,
            denom: None,
            bech32_prefix: None,
            gas_prices: None,
        };

        let cfg = spec.resolve().expect("juno spec should resolve");
        assert_eq!(cfg.name, "juno");
        assert_eq!(cfg.denom, "ujuno");
        assert_eq!(cfg.bech32_prefix, "juno");
    }

    #[test]
    fn test_resolve_unknown_fails() {
        let spec = ChainSpec {
            name: "nonexistent-chain".to_string(),
            version: None,
            num_validators: None,
            num_full_nodes: None,
            chain_id: None,
            denom: None,
            bech32_prefix: None,
            gas_prices: None,
        };

        let result = spec.resolve();
        assert!(result.is_err(), "unknown chain name should fail");
    }

    #[test]
    fn test_resolve_with_version_override() {
        let spec = ChainSpec {
            name: "gaia".to_string(),
            version: Some("v99.0.0".to_string()),
            num_validators: None,
            num_full_nodes: None,
            chain_id: None,
            denom: None,
            bech32_prefix: None,
            gas_prices: None,
        };

        let cfg = spec.resolve().unwrap();
        assert_eq!(
            cfg.images[0].version, "v99.0.0",
            "version override should be applied"
        );
    }

    #[test]
    fn test_resolve_with_chain_id_override() {
        let spec = ChainSpec {
            name: "gaia".to_string(),
            version: None,
            num_validators: None,
            num_full_nodes: None,
            chain_id: Some("my-custom-chain-1".to_string()),
            denom: None,
            bech32_prefix: None,
            gas_prices: None,
        };

        let cfg = spec.resolve().unwrap();
        assert_eq!(cfg.chain_id, "my-custom-chain-1");
    }

    #[test]
    fn test_resolve_with_denom_override() {
        let spec = ChainSpec {
            name: "terp".to_string(),
            version: None,
            num_validators: None,
            num_full_nodes: None,
            chain_id: None,
            denom: Some("uthiolx".to_string()),
            bech32_prefix: None,
            gas_prices: None,
        };

        let cfg = spec.resolve().unwrap();
        assert_eq!(cfg.denom, "uthiolx");
    }

    #[test]
    fn test_build_cosmos_chain_from_spec() {
        let rt: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let spec = ChainSpec {
            name: "gaia".to_string(),
            version: None,
            num_validators: Some(2),
            num_full_nodes: Some(1),
            chain_id: None,
            denom: None,
            bech32_prefix: None,
            gas_prices: None,
        };

        let chain = spec
            .build_cosmos_chain(rt)
            .expect("should build CosmosChain from spec");

        assert_eq!(chain.config().chain_id, "cosmoshub-test-1");
        assert_eq!(chain.chain_id(), "cosmoshub-test-1");
    }
}

// ===========================================================================
// reporter_tests
// ===========================================================================
mod reporter_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn test_reporter_record_and_retrieve() {
        let mut reporter = TestReporter::new();
        assert!(reporter.reports().is_empty(), "new reporter should have no reports");

        let report = ExecReport {
            container_name: "ict-chain-a-val-0".to_string(),
            command: vec!["gaiad".to_string(), "status".to_string()],
            stdout: "{\"status\":\"ok\"}".to_string(),
            stderr: String::new(),
            exit_code: 0,
            started_at: Instant::now(),
            duration: Duration::from_millis(42),
        };

        reporter.record(report);
        assert_eq!(reporter.reports().len(), 1, "should have one report after recording");

        let r = &reporter.reports()[0];
        assert_eq!(r.container_name, "ict-chain-a-val-0");
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.command, vec!["gaiad", "status"]);
    }

    #[test]
    fn test_reporter_empty() {
        let reporter = TestReporter::new();
        assert!(reporter.reports().is_empty());
        assert_eq!(reporter.reports().len(), 0);
    }
}

// ===========================================================================
// cosmos_chain_tests
// ===========================================================================
mod cosmos_chain_tests {
    use super::*;

    fn setup() -> (MockRuntime, CosmosChain) {
        let rt = MockRuntime::new();
        let chain = CosmosChain::new(
            test_chain_config("cosmos-test-1"),
            1,
            0,
            Arc::new(rt.clone()),
        );
        (rt, chain)
    }

    #[test]
    fn test_cosmos_chain_new() {
        let (_rt, chain) = setup();
        assert_eq!(chain.chain_id(), "cosmos-test-1");
        assert_eq!(chain.config().name, "testchain");
        assert_eq!(chain.config().denom, "ustake");
    }

    #[tokio::test]
    async fn test_cosmos_chain_initialize_creates_containers() {
        let (rt, mut chain) = setup();
        let ctx = TestContext {
            test_name: "unit-test".to_string(),
            network_id: "ict-unit-test".to_string(),
        };

        chain.initialize(&ctx).await.expect("initialize should succeed with mock runtime");

        // Verify containers were created in the mock state.
        let state = rt.state();
        let guard = state.lock().unwrap();
        assert!(
            !guard.containers.is_empty(),
            "initialize should create at least one container"
        );
        assert!(
            !guard.networks.is_empty(),
            "initialize should create a network"
        );
        // With 1 validator and 0 full nodes, exactly 1 container.
        assert_eq!(guard.containers.len(), 1, "should have 1 validator container");
    }

    #[tokio::test]
    async fn test_cosmos_chain_primary_node() {
        let (_rt, mut chain) = setup();
        let ctx = TestContext {
            test_name: "primary-node-test".to_string(),
            network_id: "ict-pn-test".to_string(),
        };

        // Before initialize, primary_node should fail.
        assert!(chain.primary_node().is_err(), "primary_node should fail before initialize");

        chain.initialize(&ctx).await.unwrap();

        let node = chain.primary_node().expect("primary_node should succeed after initialize");
        assert!(node.is_validator, "primary node should be a validator");
        assert_eq!(node.index, 0, "primary node should have index 0");
    }

    #[tokio::test]
    async fn test_cosmos_chain_validators_count() {
        let rt = MockRuntime::new();
        let mut chain = CosmosChain::new(
            test_chain_config("multi-val-1"),
            3,
            2,
            Arc::new(rt),
        );

        let ctx = TestContext {
            test_name: "multi-val".to_string(),
            network_id: "ict-multi-val".to_string(),
        };

        chain.initialize(&ctx).await.unwrap();

        assert_eq!(chain.validators().len(), 3, "should have 3 validators");
        assert_eq!(chain.full_nodes().len(), 2, "should have 2 full nodes");
    }

    #[tokio::test]
    async fn test_cosmos_chain_config_access() {
        let (_rt, chain) = setup();
        let cfg = chain.config();
        assert_eq!(cfg.chain_type, ChainType::Cosmos);
        assert_eq!(cfg.gas_prices, "0.025ustake");
        assert_eq!(cfg.gas_adjustment, 1.5);
        assert_eq!(cfg.coin_type, 118);
    }

    #[tokio::test]
    async fn test_cosmos_chain_stop_after_initialize() {
        let (rt, mut chain) = setup();
        let ctx = TestContext {
            test_name: "stop-test".to_string(),
            network_id: "ict-stop-test".to_string(),
        };

        chain.initialize(&ctx).await.unwrap();
        chain.stop().await.expect("stop should succeed");

        // After stop, containers and network should be cleaned up.
        let state = rt.state();
        let guard = state.lock().unwrap();
        assert!(guard.containers.is_empty(), "containers should be removed after stop");
        assert!(guard.networks.is_empty(), "network should be removed after stop");
    }

    #[tokio::test]
    async fn test_cosmos_chain_double_initialize_is_noop() {
        let (rt, mut chain) = setup();
        let ctx = TestContext {
            test_name: "double-init".to_string(),
            network_id: "ict-double-init".to_string(),
        };

        chain.initialize(&ctx).await.unwrap();
        let container_count = {
            let state = rt.state();
            let guard = state.lock().unwrap();
            guard.containers.len()
        };

        // Second initialize should be a no-op.
        chain.initialize(&ctx).await.unwrap();
        let container_count_after = {
            let state = rt.state();
            let guard = state.lock().unwrap();
            guard.containers.len()
        };

        assert_eq!(
            container_count, container_count_after,
            "double initialize should not create more containers"
        );
    }
}

// ===========================================================================
// node_tests
// ===========================================================================
mod node_tests {
    use super::*;

    fn make_node(rt: Arc<dyn RuntimeBackend>) -> ChainNode {
        ChainNode::new(
            0,
            true,
            "test-chain-1",
            "chaind",
            DockerImage {
                repository: "ghcr.io/test/chain".to_string(),
                version: "v1.0.0".to_string(),
                uid_gid: None,
            },
            "unit-test",
            "mock-network-0",
            rt,
            None,
            Default::default(),
            "0.025utest",
            1.5,
        )
    }

    #[test]
    fn test_chain_node_new() {
        let rt: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let node = make_node(rt);

        assert_eq!(node.index, 0);
        assert!(node.is_validator);
        assert_eq!(node.chain_id, "test-chain-1");
        assert_eq!(node.chain_bin, "chaind");
        assert!(node.container_id.is_none(), "container_id should be None before creation");
    }

    #[test]
    fn test_chain_node_container_name() {
        let rt: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let node = make_node(rt);
        assert_eq!(
            node.container_name(),
            "ict-unit-test-test-chain-1-val-0",
            "container name should follow ict-{{test_name}}-{{hostname}} pattern"
        );
    }

    #[test]
    fn test_chain_node_rpc_address() {
        let rt: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let node = make_node(rt);
        assert_eq!(
            node.rpc_address(),
            "http://test-chain-1-val-0:26657",
            "RPC address should use hostname and default port"
        );
    }

    #[test]
    fn test_chain_node_grpc_address() {
        let rt: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let node = make_node(rt);
        assert_eq!(
            node.grpc_address(),
            "test-chain-1-val-0:9090",
            "gRPC address should use hostname and default port"
        );
    }

    #[tokio::test]
    async fn test_chain_node_create_start_stop() {
        let rt = MockRuntime::new();
        let rt_arc: Arc<dyn RuntimeBackend> = Arc::new(rt.clone());
        let mut node = make_node(rt_arc);

        // Create
        node.create_container()
            .await
            .expect("create_container should succeed");
        assert!(node.container_id.is_some(), "container_id should be set after creation");

        let cid = node.container_id.as_ref().unwrap().0.clone();
        {
            let state = rt.state();
            let guard = state.lock().unwrap();
            let c = guard.containers.get(&cid).unwrap();
            assert_eq!(c.status, MockContainerStatus::Created);
        }

        // Start
        node.start_container()
            .await
            .expect("start_container should succeed");
        {
            let state = rt.state();
            let guard = state.lock().unwrap();
            let c = guard.containers.get(&cid).unwrap();
            assert_eq!(c.status, MockContainerStatus::Running);
        }

        // Stop
        node.stop_container()
            .await
            .expect("stop_container should succeed");
        {
            let state = rt.state();
            let guard = state.lock().unwrap();
            let c = guard.containers.get(&cid).unwrap();
            assert_eq!(c.status, MockContainerStatus::Stopped);
        }

        // Remove
        node.remove_container()
            .await
            .expect("remove_container should succeed");
        assert!(node.container_id.is_none(), "container_id should be None after removal");
        {
            let state = rt.state();
            let guard = state.lock().unwrap();
            assert!(
                !guard.containers.contains_key(&cid),
                "container should be removed from state"
            );
        }
    }

    #[tokio::test]
    async fn test_chain_node_exec_cmd() {
        let rt = MockRuntime::new();
        let rt_arc: Arc<dyn RuntimeBackend> = Arc::new(rt.clone());
        let mut node = make_node(rt_arc);

        node.create_container().await.unwrap();
        node.start_container().await.unwrap();

        // Queue a response for the exec.
        let cid = node.container_id.as_ref().unwrap().0.clone();
        rt.queue_exec_response(
            &cid,
            ExecOutput {
                stdout: b"height: 42".to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
            },
        );

        let output = node.exec_cmd(&["status"]).await.expect("exec_cmd should succeed");
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout_str(), "height: 42");
    }

    #[tokio::test]
    async fn test_chain_node_hostname() {
        let rt: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let node = make_node(rt);
        assert_eq!(node.hostname, "test-chain-1-val-0");
    }

    #[tokio::test]
    async fn test_chain_node_home_dir() {
        let rt: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let node = make_node(rt);
        assert_eq!(node.home_dir, "/var/cosmos-chain/test-chain-1");
    }

    #[test]
    fn test_chain_node_p2p_address() {
        let rt: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let node = make_node(rt);
        assert_eq!(node.p2p_address(), "test-chain-1-val-0:26656");
    }

    #[test]
    fn test_chain_node_full_node_naming() {
        let rt: Arc<dyn RuntimeBackend> = Arc::new(MockRuntime::new());
        let node = ChainNode::new(
            2,
            false, // full node, not validator
            "mychain-1",
            "mychaind",
            DockerImage {
                repository: "img".to_string(),
                version: "v1".to_string(),
                uid_gid: None,
            },
            "my-test",
            "net-0",
            rt,
            None,
            Default::default(),
            "0.025utest",
            1.5,
        );

        assert_eq!(node.hostname, "mychain-1-fn-2");
        assert_eq!(node.container_name(), "ict-my-test-mychain-1-fn-2");
        assert!(!node.is_validator);
    }
}
