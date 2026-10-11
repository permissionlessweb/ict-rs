//! Local 08-wasm light-client store-code argv + mock IbcWasmExt submit/query.
//!
//! Does **not** claim Crosslink headers from a live zakurad. Keep
//! `ZAKURA_CROSSLINK_LAB=1` (groot-wan) out of this example.
//!
//! ICT_MOCK skip ≠ Docker pass. No live broadcast.
//!
//! ```sh
//! ICT_MOCK=1 cargo run -p ict-rs --example ibc_wasm_lc --features testing,terp,docker
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use ict_rs::chain::{Chain, ChainConfig, ChainType, SigningAlgorithm, TestContext};
use ict_rs::error::{IctError, Result as IctResult};
use ict_rs::prelude::*;
use ict_rs::runtime::{ContainerId, DockerImage};
use ict_rs::tx::{PacketAcknowledgement, PacketTimeout, TransferOptions, Tx, WalletAmount};
use ict_rs::wallet::Wallet;

struct ExecMockChain {
    runtime: Arc<MockRuntime>,
    container_id: ContainerId,
    cfg: ChainConfig,
}

impl ExecMockChain {
    async fn new() -> IctResult<Self> {
        let runtime = Arc::new(MockRuntime::new());
        let opts = ContainerOptions {
            name: "ibc-wasm-lc-mock".to_string(),
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
        let container_id = runtime.create_container(&opts).await?;
        runtime.start_container(&container_id).await?;
        let cfg = ChainConfig {
            chain_type: ChainType::Cosmos,
            name: "terp-local".to_string(),
            chain_id: "240u-1".to_string(),
            images: vec![],
            bin: "terpd".to_string(),
            bech32_prefix: "terp".to_string(),
            denom: "uterp".to_string(),
            coin_type: 118,
            signing_algorithm: SigningAlgorithm::default(),
            gas_prices: "0.025uterp".to_string(),
            gas_adjustment: 1.5,
            trusting_period: "112h".to_string(),
            block_time: "2s".to_string(),
            genesis: None,
            modify_genesis: None,
            pre_genesis: None,
            config_file_overrides: Default::default(),
            additional_start_args: vec![],
            env: vec![],
            sidecar_configs: vec![],
            faucet: None,
            genesis_style: Default::default(),
        };
        Ok(Self {
            runtime,
            container_id,
            cfg,
        })
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
    async fn initialize(&mut self, _ctx: &TestContext) -> IctResult<()> {
        Ok(())
    }
    async fn start(&mut self, _genesis_wallets: &[WalletAmount]) -> IctResult<()> {
        Ok(())
    }
    async fn stop(&mut self) -> IctResult<()> {
        Ok(())
    }
    async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> IctResult<ExecOutput> {
        self.runtime
            .exec_in_container(&self.container_id, cmd, env)
            .await
    }
    fn rpc_address(&self) -> &str {
        "http://127.0.0.1:26657"
    }
    fn grpc_address(&self) -> &str {
        "127.0.0.1:9090"
    }
    fn host_rpc_address(&self) -> String {
        "http://127.0.0.1:26657".to_string()
    }
    fn host_grpc_address(&self) -> String {
        "http://127.0.0.1:9090".to_string()
    }
    fn home_dir(&self) -> &str {
        "/home/terp"
    }
    async fn create_key(&self, _key_name: &str) -> IctResult<()> {
        Ok(())
    }
    async fn recover_key(&self, _name: &str, _mnemonic: &str) -> IctResult<()> {
        Ok(())
    }
    async fn get_address(&self, _key_name: &str) -> IctResult<Vec<u8>> {
        Ok(vec![0u8; 20])
    }
    async fn build_wallet(&self, _key_name: &str, _mnemonic: &str) -> IctResult<Box<dyn Wallet>> {
        Err(IctError::Config("not implemented in example mock".into()))
    }
    async fn send_funds(&self, _key_name: &str, _amount: &WalletAmount) -> IctResult<String> {
        Ok("mock".into())
    }
    async fn get_balance(&self, _address: &str, _denom: &str) -> IctResult<u128> {
        Ok(0)
    }
    async fn send_ibc_transfer(
        &self,
        _channel_id: &str,
        _key_name: &str,
        _amount: &WalletAmount,
        _options: &TransferOptions,
    ) -> IctResult<Tx> {
        Err(IctError::Config("no live IBC transfer in this example".into()))
    }
    async fn height(&self) -> IctResult<u64> {
        Ok(1)
    }
    async fn export_state(&self, _height: u64) -> IctResult<String> {
        Ok("{}".into())
    }
    async fn acknowledgements(&self, _height: u64) -> IctResult<Vec<PacketAcknowledgement>> {
        Ok(vec![])
    }
    async fn timeouts(&self, _height: u64) -> IctResult<Vec<PacketTimeout>> {
        Ok(vec![])
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ZAKURA_CROSSLINK_LAB").ok().as_deref() == Some("1") {
        eprintln!(
            "ZAKURA_CROSSLINK_LAB=1 is groot-wan only; this example does not fetch live TFL headers."
        );
    }
    let mock = std::env::var("ICT_MOCK").ok().as_deref() != Some("0");

    let prop = IbcWasmStoreProposal {
        wasm_path: "/wasm/cw-ics08-wasm-crosslink.wasm".into(),
        title: "Store cw-ics08-wasm-crosslink".into(),
        summary: "Local 08-wasm LC bytecode".into(),
        deposit: "10000000uterp".into(),
        authority: None,
        metadata: None,
    };
    let argv = prop.store_code_tx_base_args();
    assert_eq!(argv[1], "ibc-wasm");
    assert_eq!(argv[2], "store-code");
    println!("08-wasm store-code argv: {argv:?}");

    let sample_tx = r#"{"height":"1","txhash":"DEADBEEF","code":0}"#;
    println!(
        "parse store-code txhash: {}",
        parse_ibc_wasm_store_txhash(sample_tx)?
    );
    let checksums = parse_ibc_wasm_checksums_json(
        r#"{"checksums":["33d506ee481a06a14668bc5f79cc0a1363cf1f3bcc214e82deaf2a69b11f7d33"]}"#,
    )?;
    println!("parse checksums: {checksums:?}");

    if mock {
        let chain = ExecMockChain::new().await?;
        let (proposal_id, txhash) = chain
            .submit_ibc_wasm_store_code("validator", &prop)
            .await?;
        let listed = chain.query_ibc_wasm_checksums().await?;
        println!("IbcWasmExt submit_ibc_wasm_store_code proposal_id={proposal_id} txhash={txhash}");
        println!("IbcWasmExt query_ibc_wasm_checksums: {listed:?}");
        println!(
            "next-step: after checksums, create 08-wasm client (not hardcoded 08-wasm-0). \
             Hermes update_wasm_client shares `hermes update client`. \
             Crosslink headers require groot-wan ZAKURA_CROSSLINK_LAB (CX7), not this example."
        );
    } else {
        panic!(
            "ICT_MOCK=0: Docker gov_store_ibc_wasm_lc needs a running terpd with ibc-wasm. \
             next-step: ICT_MOCK=1 (this example) or groot-wan lab. No live broadcast."
        );
    }

    println!(
        "ibc_wasm_lc local path PASSED (ICT_MOCK={}, no live broadcast). mock skip ≠ Docker pass.",
        mock
    );
    Ok(())
}
