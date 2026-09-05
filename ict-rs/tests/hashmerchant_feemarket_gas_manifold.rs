//! HashMerchant × CosmWasm gas manifold vs the Reece / skip-mev feemarket 0-day.
//!
//! The 0-day is: CosmWasm makes `GasConsumed()` differ across validators
//! (compile vs cache, ReplyCosts / event-attribute gas, submessage metering),
//! then `x/feemarket` writes that number into consensus KV (`Window[i] += gas`)
//! so AppHash splits and the chain halts.
//!
//! Terp-core does **not** ship skip-mev feemarket. HashMerchant sudo runs in
//! PreBlocker on an infinite gas meter and does not persist `GasConsumed()`.
//! This file confirms that claim:
//!
//! 1. Always-on: `go.mod` has no feemarket promoter.
//! 2. Docker 2-val e2e (`ICT_HM_GAS=1`): CosmWasm store (compile) + execute
//!    (event attributes / cache hit) + `x/hashmerchant` register-contract,
//!    then both validators report the **same** `latest_app_hash` and height
//!    still advances (no AppHash halt).
//!
//! ```sh
//! cargo test -p ict-rs --test hashmerchant_feemarket_gas_manifold --features testing
//!
//! ICT_HM_GAS=1 cargo test -p ict-rs --features "testing,docker,terp" \
//!   --test hashmerchant_feemarket_gas_manifold -- --ignored --nocapture --test-threads=1
//! ```

#![cfg(feature = "testing")]

use std::path::PathBuf;

fn repo_go_mod() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../go.mod")
}

/// Skip-mev `x/feemarket` is the promoter that turns a CosmWasm gas delta into
/// a consensus-critical KV write. Terp-core must not depend on it.
#[test]
fn go_mod_has_no_skip_mev_feemarket() {
    let path = repo_go_mod();
    let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for needle in [
        "skip-mev/feemarket",
        "github.com/skip-mev/feemarket",
        "x/feemarket",
    ] {
        assert!(
            !body.contains(needle),
            "{} must not appear in terp-core go.mod (Reece 0-day promoter); found in {}",
            needle,
            path.display()
        );
    }
}

#[cfg(all(feature = "docker", feature = "hashmerchant"))]
mod docker_e2e {
    use super::*;
    use std::time::Duration;

    use ict_rs::cosmwasm::CosmWasmExt;
    use ict_rs::interchain::wait_for_blocks;

    use ict_rs::prelude::*;
    use ict_rs::testing::env::TestEnv;
    use ict_rs::testing::{TestChain, TestChainConfig};

    fn require_ict() {
        assert_eq!(
            std::env::var("ICT_HM_GAS").ok().as_deref(),
            Some("1"),
            "ICT_HM_GAS=1 required (2-validator Docker e2e)"
        );
        assert!(
            !TestEnv::is_mock(),
            "ICT_MOCK cannot prove AppHash lockstep; unset ICT_MOCK"
        );
    }

    fn wasm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/interchaintest/contracts/ibchooks_counter.wasm")
    }

    fn app_hash(status: &serde_json::Value) -> Option<String> {
        status
            .pointer("/sync_info/latest_app_hash")
            .or_else(|| status.pointer("/SyncInfo/latest_app_hash"))
            .or_else(|| status.pointer("/result/sync_info/latest_app_hash"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    }

    fn height_of(status: &serde_json::Value) -> Option<u64> {
        let v = status
            .pointer("/sync_info/latest_block_height")
            .or_else(|| status.pointer("/SyncInfo/latest_block_height"))
            .or_else(|| status.pointer("/result/sync_info/latest_block_height"))?;
        v.as_u64().or_else(|| v.as_str()?.parse().ok())
    }

    async fn snapshots(
        chain: &CosmosChain,
    ) -> Result<(Vec<String>, u64), String> {
        let vals = chain.validators();
        if vals.len() < 2 {
            return Err(format!("need ≥2 validators, got {}", vals.len()));
        }
        let mut hashes = Vec::new();
        let mut heights = Vec::new();
        for (i, node) in vals.iter().enumerate() {
            let st = node
                .query_status_json()
                .await
                .map_err(|e| format!("val-{i} status: {e}"))?;
            let h = app_hash(&st).ok_or_else(|| format!("val-{i} missing latest_app_hash: {st}"))?;
            let ht = height_of(&st).unwrap_or(0);
            hashes.push(h);
            heights.push(ht);
        }
        let min_h = *heights.iter().min().unwrap_or(&0);
        Ok((hashes, min_h))
    }

    fn assert_lockstep(label: &str, hashes: &[String]) -> Result<(), String> {
        let first = hashes.first().ok_or_else(|| "no app hashes".to_string())?;
        if hashes.iter().any(|h| h != first) {
            return Err(format!(
                "{label}: AppHash split (feemarket-class halt) {hashes:?}"
            ));
        }
        println!("PASS {label} app_hash={first}");
        Ok(())
    }

    fn terp_cfg() -> ChainConfig {
        let mut cfg = TestEnv::terp_config();
        cfg.chain_id = "ict-hm-gas".into();
        cfg.gas_prices = "0uterp".into();
        cfg.gas_adjustment = 2.0;
        cfg.modify_genesis = Some(Box::new(|_cfg, bytes| {
            let mut g: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| ict_rs::error::IctError::Config(e.to_string()))?;
            g["app_state"]["hashmerchant"]["registered_chains"] = serde_json::json!([{
                "chain_uid": "gas-lab",
                "name": "feemarket-gas-manifold",
                "rpc_endpoints": [],
                "hash_algos": ["sha256"],
                "enabled": true
            }]);
            g["app_state"]["hashmerchant"]["params"]["quorum_fraction"] =
                serde_json::json!("0.667000000000000000");
            serde_json::to_vec(&g).map_err(|e| ict_rs::error::IctError::Config(e.to_string()))
        }));
        cfg
    }

    async fn run() -> Result<(), String> {
        require_ict();
        let wasm = wasm_path();
        if !wasm.exists() {
            return Err(format!("missing {}", wasm.display()));
        }

        let mut tc = TestChain::setup(
            "hm_gas_manifold",
            TestChainConfig {
                chain_config: terp_cfg(),
                num_validators: 2,
                num_full_nodes: 0,
                genesis_wallets: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("setup: {e}"))?;

        let result = run_on(&tc.chain, &wasm).await;
        let _ = tc.cleanup().await;
        result
    }

    async fn run_on(chain: &CosmosChain, wasm: &std::path::Path) -> Result<(), String> {
        wait_for_blocks(chain, 2).await.map_err(|e| e.to_string())?;

        // Promoter absent: feemarket query must fail. HashMerchant is present.
        let fm = chain
            .chain_exec(&["query", "feemarket", "params", "--output", "json"])
            .await
            .map_err(|e| e.to_string())?;
        let fm_out = format!("{} {}", fm.stdout_str().trim(), fm.stderr_str().trim());
        if fm.exit_code == 0 && fm.stdout_str().trim().contains("base_gas_price") {
            return Err(format!("feemarket module present (0-day promoter): {fm_out}"));
        }
        println!("PASS no-feemarket-module ({fm_out})");

        let hm_out = chain
            .chain_exec(&["query", "hashmerchant", "params", "--output", "json"])
            .await
            .map_err(|e| format!("hashmerchant params exec: {e}"))?;
        let hm_owned = hm_out.stdout_str();
        let hm_raw = hm_owned.trim();
        if hm_out.exit_code != 0 || hm_raw.is_empty() {
            return Err(format!(
                "hashmerchant params failed exit={} stdout={:?} stderr={}",
                hm_out.exit_code,
                hm_raw,
                hm_out.stderr_str().trim()
            ));
        }
        let hm: serde_json::Value = serde_json::from_str(hm_raw)
            .map_err(|e| format!("hashmerchant params JSON: {e}: {hm_raw}"))?;
        if !hm.is_object() {
            return Err(format!("hashmerchant params not object: {hm}"));
        }
        println!("PASS hashmerchant-params");

        let (h0, _) = snapshots(chain).await?;
        assert_lockstep("genesis-lockstep", &h0)?;

        chain.create_key("default").await.map_err(|e| e.to_string())?;
        let user = chain
            .primary_node()
            .map_err(|e| e.to_string())?
            .get_key_address("default")
            .await
            .map_err(|e| e.to_string())?;
        chain
            .send_funds(
                "validator",
                &ict_rs::tx::WalletAmount {
                    address: user,
                    denom: "uterp".into(),
                    amount: 50_000_000_000,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;

        let node = chain.primary_node().map_err(|e| e.to_string())?;
        node.copy_file_from_host(wasm, "/tmp/ibchooks_counter.wasm")
            .await
            .map_err(|e| e.to_string())?;

        // Compile path: first wasm store. Both vals process the same tx.
        let code_id = chain
            .store_code("default", "/tmp/ibchooks_counter.wasm")
            .await
            .map_err(|e| format!("wasm store (compile): {e}"))?;
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        let (h_store, _) = snapshots(chain).await?;
        assert_lockstep("after-wasm-store-compile", &h_store)?;

        let addr = chain
            .instantiate_contract("default", &code_id, r#"{"count":0}"#, "hm-gas-counter", None)
            .await
            .map_err(|e| format!("instantiate: {e}"))?;
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        let (h_init, _) = snapshots(chain).await?;
        assert_lockstep("after-instantiate", &h_init)?;

        // Event-attribute / cache-hit path (ReplyCosts analog): several executes.
        for i in 0..3 {
            chain
                .execute_contract("default", &addr, r#"{"increment":{}}"#, None)
                .await
                .map_err(|e| format!("execute {i}: {e}"))?;
            wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        }
        let (h_exec, ht_exec) = snapshots(chain).await?;
        assert_lockstep("after-event-executes", &h_exec)?;

        // HashMerchant DeliverTx write (register), not GasConsumed().
        let opts = chain.default_tx_opts().from("default");
        let reg = chain
            .chain_exec_tx_with(
                &[
                    "tx",
                    "hashmerchant",
                    "register-contract",
                    &addr,
                    "gas-lab",
                    "",
                    "1000000uterp",
                ],
                opts,
            )
            .await
            .map_err(|e| format!("register-contract exec: {e}"))?;
        if reg.exit_code != 0 {
            return Err(format!(
                "register-contract exit={} stdout={} stderr={}",
                reg.exit_code,
                reg.stdout_str().trim(),
                reg.stderr_str().trim()
            ));
        }
        wait_for_blocks(chain, 2).await.map_err(|e| e.to_string())?;
        let (h_reg, ht_reg) = snapshots(chain).await?;
        assert_lockstep("after-hashmerchant-register", &h_reg)?;
        if ht_reg < ht_exec {
            return Err(format!(
                "height went backwards {ht_exec} → {ht_reg} — AppHash halt"
            ));
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
        let (h_later, ht_later) = snapshots(chain).await?;
        assert_lockstep("still-producing", &h_later)?;
        if ht_later < ht_reg {
            return Err("chain stalled after CosmWasm+hashmerchant — AppHash halt".into());
        }
        println!("PASS still-advancing {ht_reg} → {ht_later}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "ignored-without-ICT"]
    async fn hashmerchant_feemarket_gas_manifold_no_apphash_split() {
        match run().await {
            Ok(()) => println!("PASS hashmerchant_feemarket_gas_manifold (0-day not present)"),
            Err(e) => {
                println!("FAIL hashmerchant_feemarket_gas_manifold {e}");
                panic!("{e}");
            }
        }
    }
}
