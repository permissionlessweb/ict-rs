#![cfg(feature = "testing")]

use std::sync::Arc;

use ict_rs::chain::{ChainConfig, ChainType, SigningAlgorithm};
use ict_rs::ibc::{ChannelOptions, ClientOptions};
use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, TestContext};
use ict_rs::ibc_v2::{
    ibc_v2_add_counterparty_tx_args, ibc_v2_create_client_tx_args, IbcV2Ext, IBC_V2_MERKLE_PREFIX,
};
use ict_rs::relayer::docker_relayer::RelayerCommander;
use ict_rs::relayer::rly::CosmosRlyCommander;
use ict_rs::relayer::{build_relayer, DockerRelayer, HermesRelayer, Relayer, RelayerType};
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::{DockerImage, RuntimeBackend};

fn mock_runtime() -> Arc<MockRuntime> {
    Arc::new(MockRuntime::new())
}

fn test_chain_config(chain_id: &str) -> ChainConfig {
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: chain_id.to_string(),
        chain_id: chain_id.to_string(),
        images: vec![DockerImage {
            repository: "ghcr.io/strangelove-ventures/heighliner/gaia".to_string(),
            version: "v19.0.0".to_string(),
            uid_gid: None,
        }],
        bin: "gaiad".to_string(),
        bech32_prefix: "cosmos".to_string(),
        denom: "uatom".to_string(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.025uatom".to_string(),
        gas_adjustment: 1.5,
        trusting_period: "336h".to_string(),
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
    }
}

// -- CosmosRly Commander Tests --

#[test]
fn test_cosmos_rly_commander_metadata() {
    let cmd = CosmosRlyCommander::new();
    assert_eq!(cmd.name(), "cosmos-relayer");
    assert_eq!(cmd.docker_user(), "100:1000");
    assert_eq!(cmd.home_dir(), "/home/relayer/.relayer");
    assert!(cmd.default_image().repository.contains("cosmos/relayer"));
}

#[test]
fn test_cosmos_rly_init_cmd() {
    let cmd = CosmosRlyCommander::new();
    let init = cmd.init_cmd("/home/relayer/.relayer");
    assert!(init.is_some());
    let init = init.unwrap();
    assert_eq!(init[0], "rly");
    assert_eq!(init[1], "config");
    assert_eq!(init[2], "init");
}

#[test]
fn test_cosmos_rly_add_key_cmd() {
    let cmd = CosmosRlyCommander::new();
    let key_cmd = cmd.add_key_cmd("chain-1", "relayer-key", 118, "secp256k1", "/home");
    assert!(key_cmd.contains(&"rly".to_string()));
    assert!(key_cmd.contains(&"keys".to_string()));
    assert!(key_cmd.contains(&"add".to_string()));
    assert!(key_cmd.contains(&"chain-1".to_string()));
    assert!(key_cmd.contains(&"relayer-key".to_string()));
}

#[test]
fn test_cosmos_rly_generate_path_cmd() {
    let cmd = CosmosRlyCommander::new();
    let path_cmd = cmd.generate_path_cmd("chain-a", "chain-b", "transfer", "/home");
    assert!(path_cmd.contains(&"paths".to_string()));
    assert!(path_cmd.contains(&"new".to_string()));
    assert!(path_cmd.contains(&"chain-a".to_string()));
    assert!(path_cmd.contains(&"chain-b".to_string()));
    assert!(path_cmd.contains(&"transfer".to_string()));
}

#[test]
fn test_cosmos_rly_link_path_cmd() {
    let cmd = CosmosRlyCommander::new();
    let ch_opts = ChannelOptions {
        src_port: "transfer".to_string(),
        dst_port: "transfer".to_string(),
        ..Default::default()
    };
    let cl_opts = Default::default();
    let link_cmd = cmd.link_path_cmd("my-path", "/home", &ch_opts, &cl_opts);
    assert!(link_cmd.contains(&"tx".to_string()));
    assert!(link_cmd.contains(&"link".to_string()));
    assert!(link_cmd.contains(&"my-path".to_string()));
    assert!(link_cmd.contains(&"--src-port".to_string()));
    assert!(link_cmd.contains(&"transfer".to_string()));
}

#[test]
fn test_cosmos_rly_start_cmd() {
    let cmd = CosmosRlyCommander::new();
    let start = cmd.start_cmd("/home", &["path1", "path2"]);
    assert!(start.contains(&"start".to_string()));
    assert!(start.contains(&"--debug".to_string()));
    assert!(start.contains(&"path1".to_string()));
    assert!(start.contains(&"path2".to_string()));
}

#[test]
fn test_cosmos_rly_config_content() {
    let cmd = CosmosRlyCommander::new();
    let cfg = test_chain_config("chain-1");
    let content = cmd
        .config_content(&cfg, "my-key", "http://chain:26657", "chain:9090")
        .unwrap();

    let json: serde_json::Value = serde_json::from_slice(&content).unwrap();
    assert_eq!(json["type"], "cosmos");
    assert_eq!(json["value"]["chain-id"], "chain-1");
    assert_eq!(json["value"]["key"], "my-key");
    assert_eq!(json["value"]["rpc-addr"], "http://chain:26657");
    assert_eq!(json["value"]["account-prefix"], "cosmos");
}

#[test]
fn test_cosmos_rly_parse_add_key() {
    let cmd = CosmosRlyCommander::new();
    let wallet = cmd
        .parse_add_key_output(r#"{"address":"cosmos1abc123"}"#, "")
        .unwrap();
    assert_eq!(wallet.formatted_address(), "cosmos1abc123");
}

#[test]
fn test_cosmos_rly_parse_channels() {
    let cmd = CosmosRlyCommander::new();

    // Empty
    let channels = cmd.parse_channels_output("").unwrap();
    assert!(channels.is_empty());

    // Valid JSON array
    let json = r#"[{"state":"STATE_OPEN","ordering":"ORDER_UNORDERED","version":"ics20-1","port_id":"transfer","channel_id":"channel-0","connection_hops":["connection-0"],"counterparty":{"port_id":"transfer","channel_id":"channel-0"}}]"#;
    let channels = cmd.parse_channels_output(json).unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].channel_id, "channel-0");
}

// -- DockerRelayer Lifecycle Tests --

#[tokio::test]
async fn test_cosmos_rly_lifecycle() {
    let runtime = mock_runtime();
    let commander = Box::new(CosmosRlyCommander::new());
    let relayer = DockerRelayer::new(commander, runtime.clone(), "test-rly", "test-net")
        .await
        .unwrap();

    // Add a chain config
    let cfg = test_chain_config("chain-1");
    relayer
        .add_chain_configuration(&cfg, "relayer-key", "http://chain:26657", "chain:9090")
        .await
        .unwrap();

    // Add a key
    let wallet = relayer.add_key("chain-1", "relayer-key").await.unwrap();
    assert!(!wallet.formatted_address().is_empty());

    // Generate path
    relayer
        .generate_path("chain-1", "chain-2", "transfer")
        .await
        .unwrap();

    // Link path
    let ch_opts = ChannelOptions {
        src_port: "transfer".to_string(),
        dst_port: "transfer".to_string(),
        ..Default::default()
    };
    relayer.link_path("transfer", &ch_opts).await.unwrap();

    // Start
    relayer.start(&["transfer"]).await.unwrap();

    // Query channels
    let channels = relayer.get_channels("chain-1").await.unwrap();
    assert!(!channels.is_empty(), "should have mock channels");

    // Stop
    relayer.stop().await.unwrap();
}

// -- Hermes Lifecycle Tests --

#[tokio::test]
async fn test_hermes_lifecycle() {
    let runtime = mock_runtime();
    let relayer = HermesRelayer::new(runtime.clone(), "test-hermes", "test-net")
        .await
        .unwrap();

    // Add chain configs
    let cfg1 = test_chain_config("chain-a");
    relayer
        .add_chain_configuration(&cfg1, "key-a", "http://chain-a:26657", "chain-a:9090")
        .await
        .unwrap();

    let cfg2 = test_chain_config("chain-b");
    relayer
        .add_chain_configuration(&cfg2, "key-b", "http://chain-b:26657", "chain-b:9090")
        .await
        .unwrap();

    // Add keys
    let wallet_a = relayer.add_key("chain-a", "key-a").await.unwrap();
    assert!(!wallet_a.formatted_address().is_empty());

    // Generate path
    relayer
        .generate_path("chain-a", "chain-b", "transfer")
        .await
        .unwrap();

    // Link path (create clients + connections + channel)
    let ch_opts = ChannelOptions {
        src_port: "transfer".to_string(),
        dst_port: "transfer".to_string(),
        ..Default::default()
    };
    relayer.link_path("transfer", &ch_opts).await.unwrap();

    // Start
    relayer.start(&["transfer"]).await.unwrap();

    // Stop
    relayer.stop().await.unwrap();
}

// -- Factory Tests --

#[tokio::test]
async fn test_relayer_factory_cosmos_rly() {
    let runtime = mock_runtime();
    let relayer = build_relayer(RelayerType::CosmosRly, runtime, "test", "net")
        .await
        .unwrap();

    // Should be able to use the relayer
    let cfg = test_chain_config("chain-1");
    relayer
        .add_chain_configuration(&cfg, "key", "http://rpc:26657", "grpc:9090")
        .await
        .unwrap();
}

#[tokio::test]
async fn test_relayer_factory_hermes() {
    let runtime = mock_runtime();
    let relayer = build_relayer(RelayerType::Hermes, runtime, "test", "net")
        .await
        .unwrap();

    let cfg = test_chain_config("chain-1");
    relayer
        .add_chain_configuration(&cfg, "key", "http://rpc:26657", "grpc:9090")
        .await
        .unwrap();
}

#[tokio::test]
async fn test_relayer_factory_hyperspace_fails() {
    let runtime = mock_runtime();
    let result = build_relayer(RelayerType::Hyperspace, runtime, "test", "net").await;
    assert!(result.is_err(), "Hyperspace should not be implemented yet");
}

// -- Interchain Integration Test --

#[tokio::test]
async fn test_interchain_two_cosmos_chains() {
    use ict_rs::chain::cosmos::CosmosChain;
    use ict_rs::interchain::{Interchain, InterchainBuildOptions, InterchainLink};

    let runtime = mock_runtime();

    // Create two Cosmos chains
    let cfg1 = test_chain_config("chain-a");
    let chain1 = CosmosChain::new(cfg1, 1, 0, runtime.clone());

    let cfg2 = test_chain_config("chain-b");
    let chain2 = CosmosChain::new(cfg2, 1, 0, runtime.clone());

    // Create a relayer
    let relayer = build_relayer(
        RelayerType::CosmosRly,
        runtime.clone(),
        "interchain-test",
        "test-net",
    )
    .await
    .unwrap();

    // Build the interchain environment
    let mut ic = Interchain::new(runtime.clone())
        .add_chain(Box::new(chain1))
        .add_chain(Box::new(chain2))
        .add_relayer("rly", relayer)
        .add_link(InterchainLink {
            chain1: "chain-a".to_string(),
            chain2: "chain-b".to_string(),
            relayer: "rly".to_string(),
            path: "transfer".to_string(),
        });

    let opts = InterchainBuildOptions {
        test_name: "interchain-test".to_string(),
        skip_path_creation: false,
        ..Default::default()
    };

    ic.build(opts).await.unwrap();

    // Verify chains are accessible
    assert!(ic.get_chain("chain-a").is_some());
    assert!(ic.get_chain("chain-b").is_some());
    assert!(ic.is_built());

    // Close
    ic.close().await.unwrap();
}

// -- Shipped Hermes / 08-wasm / IBCv2 helpers (not a second mock relayer) --

#[test]
fn test_hermes_update_client_cmd_is_shipped_cli() {
    let cmd = ict_rs::relayer::hermes_update_client_cmd("240u-1", "08-wasm-0");
    assert_eq!(cmd[0], "hermes");
    assert!(cmd.contains(&"update".to_string()));
    assert!(cmd.contains(&"client".to_string()));
    assert!(cmd.contains(&"--host-chain".to_string()));
    assert!(cmd.contains(&"240u-1".to_string()));
    assert!(cmd.contains(&"08-wasm-0".to_string()));
}

#[test]
fn test_hermes_create_client_cmd_is_shipped_cli() {
    let cmd = ict_rs::relayer::hermes_create_client_cmd("240u-1", "juno-local");
    assert_eq!(cmd[0], "hermes");
    assert!(cmd.contains(&"create".to_string()));
    assert!(cmd.contains(&"--reference-chain".to_string()));
    assert!(cmd.contains(&"juno-local".to_string()));
    assert!(!cmd.contains(&"--wasm-checksum".to_string()));
}

#[test]
fn test_hermes_create_wasm_client_cmd_is_informal_flag() {
    let cmd = ict_rs::relayer::hermes_create_wasm_client_cmd(
        "terp-c-1",
        "terp-a-1",
        "cdaa4c2866d7d250d92b34e80070fa23107b851a4a0e8d84ce9590aec38dc7ae",
    );
    assert_eq!(cmd[0], "hermes");
    assert!(cmd.contains(&"--wasm-checksum".to_string()));
    assert!(cmd.contains(&"terp-c-1".to_string()));
    assert!(cmd.contains(&"terp-a-1".to_string()));
    assert!(cmd.contains(
        &"cdaa4c2866d7d250d92b34e80070fa23107b851a4a0e8d84ce9590aec38dc7ae".to_string()
    ));
}

#[test]
fn test_client_options_wasm_checksum_is_directional() {
    let opts = ClientOptions {
        wasm_checksum: Some("abcd".into()),
        wasm_host_chain_id: Some("terp-c-1".into()),
        ..Default::default()
    };
    assert_eq!(opts.wasm_checksum_for("terp-c-1"), Some("abcd"));
    assert_eq!(opts.wasm_checksum_for("terp-a-1"), None);
}

#[test]
fn test_client_options_wasm_checksum_none_host_means_both() {
    let opts = ClientOptions {
        wasm_checksum: Some("abcd".into()),
        wasm_host_chain_id: None,
        ..Default::default()
    };
    assert_eq!(opts.wasm_checksum_for("terp-c-1"), Some("abcd"));
    assert_eq!(opts.wasm_checksum_for("terp-a-1"), Some("abcd"));
}

#[test]
fn test_newest_local_channel_id_ignores_counterparty() {
    let v = serde_json::json!({
        "channels": [{
            "channel_id": "channel-0",
            "counterparty": { "channel_id": "channel-1", "port_id": "transfer" },
            "state": "STATE_OPEN"
        }]
    });
    assert_eq!(
        ict_rs::ibc_wasm_path::newest_local_channel_id(&v).as_deref(),
        Some("channel-0")
    );
    let two = serde_json::json!({
        "channels": [
            {
                "channel_id": "channel-0",
                "counterparty": { "channel_id": "channel-0" }
            },
            {
                "channel_id": "channel-1",
                "counterparty": { "channel_id": "channel-0" }
            }
        ]
    });
    assert_eq!(
        ict_rs::ibc_wasm_path::newest_local_channel_id(&two).as_deref(),
        Some("channel-1")
    );
}

#[test]
fn test_ibc_wasm_store_code_tx_base_args() {
    use ict_rs::ibc_wasm::{store_code_tx_base_args, IbcWasmStoreProposal};
    let args = store_code_tx_base_args("/tmp/cw-ics08-wasm-crosslink.wasm");
    assert_eq!(
        args,
        vec![
            "tx".to_string(),
            "ibc-wasm".to_string(),
            "store-code".to_string(),
            "/tmp/cw-ics08-wasm-crosslink.wasm".to_string(),
        ]
    );
    let prop = IbcWasmStoreProposal {
        wasm_path: "/lc.wasm".into(),
        ..Default::default()
    };
    assert_eq!(prop.store_code_tx_base_args()[2], "store-code");
}

#[test]
fn test_cosmos_rly_update_wasm_client_cmd() {
    let cmd = CosmosRlyCommander::new().update_wasm_client_cmd(
        "240u-1",
        "08-wasm-0",
        "/home/relayer/.relayer",
    );
    assert_eq!(cmd[0], "rly");
    assert!(cmd.contains(&"update-clients".to_string()));
    assert!(cmd.contains(&"08-wasm-0".to_string()));
    assert!(cmd.contains(&"--client".to_string()));
}

#[tokio::test]
async fn test_hermes_create_clients_wasm_checksum_on_host_b() {
    let runtime = mock_runtime();
    let relayer = HermesRelayer::new(runtime.clone(), "test-wasm-create", "test-net")
        .await
        .unwrap();
    let cfg_a = test_chain_config("terp-a-1");
    let cfg_b = test_chain_config("terp-c-1");
    relayer
        .add_chain_configuration(&cfg_a, "key-a", "http://a:26657", "a:9090")
        .await
        .unwrap();
    relayer
        .add_chain_configuration(&cfg_b, "key-b", "http://c:26657", "c:9090")
        .await
        .unwrap();
    relayer
        .generate_path("terp-a-1", "terp-c-1", "path-ac")
        .await
        .unwrap();
    let opts = ClientOptions {
        wasm_checksum: Some("cdaa4c2866d7d250d92b34e80070fa23107b851a4a0e8d84ce9590aec38dc7ae".into()),
        wasm_host_chain_id: Some("terp-c-1".into()),
        ..Default::default()
    };
    relayer.create_clients("path-ac", &opts).await.unwrap();
    relayer.stop().await.unwrap();
}

#[tokio::test]
async fn test_hermes_update_clients_and_wasm_client_on_mock() {
    let runtime = mock_runtime();
    let relayer = HermesRelayer::new(runtime.clone(), "test-wasm-lc", "test-net")
        .await
        .unwrap();

    let cfg_a = test_chain_config("chain-a");
    let cfg_b = test_chain_config("chain-b");
    relayer
        .add_chain_configuration(&cfg_a, "key-a", "http://a:26657", "a:9090")
        .await
        .unwrap();
    relayer
        .add_chain_configuration(&cfg_b, "key-b", "http://b:26657", "b:9090")
        .await
        .unwrap();
    relayer
        .generate_path("chain-a", "chain-b", "ibc-path")
        .await
        .unwrap();
    let ch_opts = ChannelOptions {
        src_port: "transfer".to_string(),
        dst_port: "transfer".to_string(),
        ..Default::default()
    };
    relayer.link_path("ibc-path", &ch_opts).await.unwrap();
    relayer.update_clients("ibc-path").await.unwrap();
    relayer
        .update_wasm_client("chain-a", "08-wasm-0")
        .await
        .unwrap();
    relayer.stop().await.unwrap();
}

#[tokio::test]
async fn test_rly_update_wasm_client_on_mock() {
    let runtime = mock_runtime();
    let commander = Box::new(CosmosRlyCommander::new());
    let relayer = DockerRelayer::new(commander, runtime, "test-rly-wasm", "test-net")
        .await
        .unwrap();
    relayer
        .update_wasm_client("chain-1", "08-wasm-0")
        .await
        .unwrap();
}

#[test]
fn test_ibc_v2_create_and_counterparty_argv() {
    let create = ibc_v2_create_client_tx_args(
        "/tmp/ibc_v2_client_state.json",
        "/tmp/ibc_v2_cons_state.json",
    );
    assert_eq!(
        create,
        vec![
            "tx".to_string(),
            "ibc".to_string(),
            "client".to_string(),
            "create".to_string(),
            "/tmp/ibc_v2_client_state.json".to_string(),
            "/tmp/ibc_v2_cons_state.json".to_string(),
        ]
    );
    let cp = ibc_v2_add_counterparty_tx_args("07-tendermint-0", "08-wasm-0", IBC_V2_MERKLE_PREFIX);
    assert_eq!(cp[3], "add-counterparty");
    assert_eq!(cp[4], "07-tendermint-0");
    assert_eq!(cp[5], "08-wasm-0");
    assert_eq!(cp[6], "aWJj");
}

#[tokio::test]
async fn test_ibc_v2_tendermint_path_on_mock() {
    let runtime = mock_runtime();
    let mut chain = CosmosChain::new(test_chain_config("terp-test-1"), 1, 0, runtime);
    let ctx = TestContext {
        test_name: "ibc-v2-mock".to_string(),
        network_id: "ict-ibc-v2-mock".to_string(),
    };
    chain.initialize(&ctx).await.unwrap();
    chain.start(&[]).await.unwrap();
    let id = chain
        .create_ibc_v2_tendermint_path("validator")
        .await
        .expect("IBCv2 create+add-counterparty on mock");
    assert!(
        id.starts_with("07-tendermint-"),
        "shipped helper must return 07-tendermint client, got {id}"
    );
    chain.stop().await.unwrap();
}

#[test]
fn test_parse_ibc_wasm_store_and_checksums_json() {
    use ict_rs::ibc_wasm::{parse_ibc_wasm_checksums_json, parse_ibc_wasm_store_txhash};
    let hash = parse_ibc_wasm_store_txhash(
        r#"{"height":"1","txhash":"MOCKIBCWASMSTORE","code":0}"#,
    )
    .unwrap();
    assert_eq!(hash, "MOCKIBCWASMSTORE");
    let sums = parse_ibc_wasm_checksums_json(
        r#"{"checksums":["aa","bb"]}"#,
    )
    .unwrap();
    assert_eq!(sums, vec!["aa", "bb"]);
    let nested = parse_ibc_wasm_checksums_json(
        r#"{"checksums":[{"checksum":"cc"}]}"#,
    )
    .unwrap();
    assert_eq!(nested, vec!["cc"]);
}

/// Hermes `update_wasm_client` is the same `update client` CLI as packets.
/// It is **not** Mercury/IBCv2 Crosslink header relay and not a Docker pass.
#[test]
fn test_update_wasm_client_cli_is_not_crosslink_proof() {
    let cmd = ict_rs::relayer::hermes_update_client_cmd("240u-1", "08-wasm-0");
    assert_eq!(cmd[0], "hermes");
    assert!(cmd.contains(&"update".to_string()));
    assert!(cmd.contains(&"08-wasm-0".to_string()));
    // Gap: no TFL/Crosslink header bytes on this argv. CX7 lives in terp-rs.
}

#[tokio::test]
async fn test_ibc_wasm_ext_submit_and_query_checksums_on_mock() {
    use async_trait::async_trait;
    use ict_rs::chain::{Chain, TestContext};
    use ict_rs::error::{IctError, Result};
    use ict_rs::ibc_wasm::{IbcWasmExt, IbcWasmStoreProposal};
    use ict_rs::runtime::{ContainerId, ContainerOptions, DockerImage};
    use ict_rs::tx::{
        ExecOutput, PacketAcknowledgement, PacketTimeout, TransferOptions, Tx, WalletAmount,
    };
    use ict_rs::wallet::Wallet;

    struct ExecMockChain {
        runtime: Arc<MockRuntime>,
        container_id: ContainerId,
        cfg: ChainConfig,
    }

    impl ExecMockChain {
        async fn new() -> Self {
            let runtime = mock_runtime();
            let opts = ContainerOptions {
                name: "ibc-wasm-ext-mock".to_string(),
                image: DockerImage {
                    repository: "test".to_string(),
                    version: "latest".to_string(),
                    uid_gid: None,
                },
                cmd: vec![],
                env: vec![],
                ports: vec![],
                volumes: vec![],
                network_id: None,
                hostname: None,
                entrypoint: None,
                labels: vec![],
            };
            let container_id = runtime.create_container(&opts).await.unwrap();
            runtime.start_container(&container_id).await.unwrap();
            Self {
                runtime,
                container_id,
                cfg: test_chain_config("240u-1"),
            }
        }
    }

    #[async_trait]
    impl Chain for ExecMockChain {
        fn config(&self) -> &ChainConfig {
            &self.cfg
        }
        fn chain_id(&self) -> &str {
            &self.cfg.chain_id
        }
        async fn initialize(&mut self, _ctx: &TestContext) -> Result<()> {
            Ok(())
        }
        async fn start(&mut self, _genesis_wallets: &[WalletAmount]) -> Result<()> {
            Ok(())
        }
        async fn stop(&mut self) -> Result<()> {
            Ok(())
        }
        async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> Result<ExecOutput> {
            self.runtime
                .exec_in_container(&self.container_id, cmd, env)
                .await
        }
        fn rpc_address(&self) -> &str {
            "http://localhost:26657"
        }
        fn grpc_address(&self) -> &str {
            "localhost:9090"
        }
        fn host_rpc_address(&self) -> String {
            "http://localhost:26657".to_string()
        }
        fn host_grpc_address(&self) -> String {
            "http://localhost:9090".to_string()
        }
        fn home_dir(&self) -> &str {
            "/home/test"
        }
        async fn create_key(&self, _key_name: &str) -> Result<()> {
            Ok(())
        }
        async fn recover_key(&self, _name: &str, _mnemonic: &str) -> Result<()> {
            Ok(())
        }
        async fn get_address(&self, _key_name: &str) -> Result<Vec<u8>> {
            Ok(vec![0u8; 20])
        }
        async fn build_wallet(&self, _key_name: &str, _mnemonic: &str) -> Result<Box<dyn Wallet>> {
            Err(IctError::Config("not implemented in test".into()))
        }
        async fn send_funds(&self, _key_name: &str, _amount: &WalletAmount) -> Result<String> {
            Ok("mock".into())
        }
        async fn get_balance(&self, _address: &str, _denom: &str) -> Result<u128> {
            Ok(0)
        }
        async fn send_ibc_transfer(
            &self,
            _channel_id: &str,
            _key_name: &str,
            _amount: &WalletAmount,
            _options: &TransferOptions,
        ) -> Result<Tx> {
            Err(IctError::Config("no live transfer".into()))
        }
        async fn height(&self) -> Result<u64> {
            Ok(1)
        }
        async fn export_state(&self, _height: u64) -> Result<String> {
            Ok("{}".into())
        }
        async fn acknowledgements(&self, _height: u64) -> Result<Vec<PacketAcknowledgement>> {
            Ok(vec![])
        }
        async fn timeouts(&self, _height: u64) -> Result<Vec<PacketTimeout>> {
            Ok(vec![])
        }
    }

    let chain = ExecMockChain::new().await;
    let prop = IbcWasmStoreProposal {
        wasm_path: "/tmp/cw-ics08-wasm-crosslink.wasm".into(),
        ..Default::default()
    };
    let (proposal_id, txhash) = chain
        .submit_ibc_wasm_store_code("validator", &prop)
        .await
        .expect("mock store-code");
    assert!(!txhash.is_empty());
    assert!(proposal_id >= 1);
    let checksums = chain.query_ibc_wasm_checksums().await.unwrap();
    assert!(
        checksums.iter().any(|c| c.len() == 64),
        "mock checksums JSON should parse, got {checksums:?}"
    );
}

#[tokio::test]
async fn test_relayer_update_clients_packet_path_not_crosslink() {
    // Relayer::update_clients refreshes 07-tendermint clients on a path.
    // 08-wasm Crosslink headers are Mercury/IBCv2 (CX7), not this call.
    let runtime = mock_runtime();
    let relayer = HermesRelayer::new(runtime.clone(), "test-update-clients", "test-net")
        .await
        .unwrap();
    let cfg_a = test_chain_config("chain-a");
    let cfg_b = test_chain_config("chain-b");
    relayer
        .add_chain_configuration(&cfg_a, "key-a", "http://a:26657", "a:9090")
        .await
        .unwrap();
    relayer
        .add_chain_configuration(&cfg_b, "key-b", "http://b:26657", "b:9090")
        .await
        .unwrap();
    relayer
        .generate_path("chain-a", "chain-b", "ibc-path")
        .await
        .unwrap();
    relayer
        .create_clients("ibc-path", &Default::default())
        .await
        .unwrap();
    relayer.update_clients("ibc-path").await.unwrap();
    relayer.stop().await.unwrap();
}
