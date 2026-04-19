#![cfg(feature = "testing")]

use std::sync::Arc;

use ict_rs::chain::{ChainConfig, ChainType, SigningAlgorithm};
use ict_rs::ibc::ChannelOptions;
use ict_rs::relayer::docker_relayer::RelayerCommander;
use ict_rs::relayer::rly::CosmosRlyCommander;
use ict_rs::relayer::{build_relayer, DockerRelayer, HermesRelayer, Relayer, RelayerType};
use ict_rs::runtime::mock::MockRuntime;
use ict_rs::runtime::DockerImage;

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
