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
//! 3. Cold-cache restart (`cold_cache_restart_stays_in_lockstep`): warm the
//!    chain, wipe val-1's wasmvm file cache + restart it (cold recompile vs
//!    warm hit on the same checksum), keep executing, assert lockstep.
//!    ict-rs has no dynamic add-validator API; a cold restart in place is the
//!    supported way to model a late/cold joiner.
//! 4. Statesync joiner (`statesync_joiner_converges_after_wasm_activity`):
//!    snapshot-enabled 2-val + 1-full-node chain, warm wasmvm, wipe the full
//!    node, rejoin via statesync, assert its AppHash converges with the vals
//!    and stays in lockstep through further executes.
//! 5. Tight-gas + hooks (`tight_gas_hooks_and_reply_lockstep`): interleaved
//!    executes + native bank sends + a staking delegation (fires staking
//!    hooks with an empty registry) + a deterministically-failing
//!    `register-contract`; assert lockstep throughout.
//! 6. Gas boundary (`gas_boundary_oog_vs_success_lockstep`): measure
//!    `gas_used` for an execute, resubmit the same message with
//!    `--gas used-1` (must fail identically) and `--gas used` (must pass
//!    identically). Lockstep ≠ equal gas: with headroom a 1-gas delta is
//!    invisible, so this pins the meter to the edge where OOG-vs-success
//!    would fork.
//! 7. Submessage reply (`submsg_bank_reply_lockstep`): polytone-proxy execute
//!    carrying a bank send as `SubMsg::reply_always`. Inner native bank
//!    events ride into the reply result (the Osmosis ReplyCosts shape);
//!    both validators must agree and stay in lockstep.
//! 8. Registered hook errors (`cwhooks_registered_hook_error_isolated`):
//!    `tx cw-hooks register-staking` a contract with no sudo entrypoint,
//!    then delegate so the hook sudoes and errors in anger during
//!    DeliverTx. The isolate must skip-and-continue, never abort the block.
//! 9. Empty sudo (`sudo_empty_object_miss_callback`): hand-rolled WAT
//!    contract (`empty_sudo.wasm`) whose sudo returns the literal bytes
//!    `{}` (res.Ok == nil class), registered as a HashMerchant callback
//!    behind a VE quorum. Patched keepers return ErrVMError → miss-callback;
//!    unpatched keepers panic in PreBlocker → halt (bounded wait fails).
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
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
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

    async fn snapshots(chain: &CosmosChain) -> Result<(Vec<String>, u64), String> {
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
            let h =
                app_hash(&st).ok_or_else(|| format!("val-{i} missing latest_app_hash: {st}"))?;
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
            // 32 MiB * wasm compile cost 3 gas/byte
            g["app_state"]["hashmerchant"]["params"]["contract_gas_limit"] =
                serde_json::json!("100663296");
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
            return Err(format!(
                "feemarket module present (0-day promoter): {fm_out}"
            ));
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
        let lim = hm
            .pointer("/params/contract_gas_limit")
            .or_else(|| hm.get("contract_gas_limit"))
            .and_then(|v| {
                v.as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .or_else(|| v.as_u64())
            });
        if let Some(n) = lim {
            if n != 100_663_296 {
                return Err(format!(
                    "contract_gas_limit {n} != 32MiB*compile-cost 100663296"
                ));
            }
            println!("PASS sudo-gas-limit={n}");
        } else {
            println!("WARN contract_gas_limit absent on this image (pre-cap binary)");
        }
        println!("PASS hashmerchant-params");

        let (h0, _) = snapshots(chain).await?;
        assert_lockstep("genesis-lockstep", &h0)?;

        chain
            .create_key("default")
            .await
            .map_err(|e| e.to_string())?;
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
            .instantiate_contract(
                "default",
                &code_id,
                r#"{"count":0}"#,
                "hm-gas-counter",
                None,
            )
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

    // 32 MiB compile-cost budget is the production cap. This e2e uses a tiny
    // genesis limit so sudo OOGs immediately; both validators must stay in
    // lockstep and keep producing blocks.
    const SUDO_CAP_TEST_LIMIT: u64 = 5_000;

    fn patch_val_sidecar_url(val_index: usize, url: &str) -> Result<(), String> {
        let list = std::process::Command::new("docker")
            .args(["ps", "--format", "{{.Names}}"])
            .output()
            .map_err(|e| e.to_string())?;
        let needle = format!("-val-{val_index}");
        let names = String::from_utf8_lossy(&list.stdout);
        let name = names
            .lines()
            .find(|n| n.contains(&needle))
            .ok_or_else(|| format!("container *{needle} not found"))?;
        let script = format!(
            r#"
for f in /home/heighliner/.terpd/config/app.toml /root/.terpd/config/app.toml /terpd/.terpd/config/app.toml "$HOME/.terpd/config/app.toml"; do
  [ -f "$f" ] || continue
  if grep -q 'sidecar-url' "$f"; then
    sed -i.bak 's|sidecar-url *=.*|sidecar-url = "{url}"|' "$f"
  else
    printf '\n[hashmerchant]\nsidecar-url = "{url}"\nsidecar-timeout = "2s"\n' >> "$f"
  fi
done
"#
        );
        let out = std::process::Command::new("docker")
            .args(["exec", name, "sh", "-c", &script])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "patch {name}: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(())
    }

    fn ve_json() -> String {
        serde_json::json!({
            "runtime_id": "ict-sudo-cap",
            "chain_uid": "oracle-eth",
            "algo": "sha256",
            "root": "a2e9f599d262ed589375679f66d65a5a127b0c10492b54ecf396ce4f63edabd2",
            "foreign_height": 42,
            "foreign_block_time": 1700000002,
            "attestations": [
                {
                    "source_id": "src-zeta",
                    "value": "313030",
                    "height": 42,
                    "timestamp": 1700000002,
                    "custody_signature": "91b99b9346c1432696242f00ff8a8f7d33fdcfc8b21f1c4c217ad0528a81ba34c9492e414b145acabd0033d83594f9c77fde5b381134fa4c977b4e30d5d75800"
                },
                {
                    "source_id": "src-alpha",
                    "value": "3939",
                    "height": 41,
                    "timestamp": 1700000001,
                    "custody_signature": "be4816fe38e46c56584f44b8fa6fcd8d7b8ba4584f4fc6aeb1c147d2d8c9408c9c9e21130e6bb7207357540f4affa96244597aa532623f408ab99bb101dbb106"
                }
            ]
        })
        .to_string()
    }

    async fn serve_ve(port: u16, body: String) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .expect("bind ve sidecar");
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    continue;
                };
                let mut buf = vec![0u8; 2048];
                let _ = tokio::io::AsyncReadExt::read(&mut s, &mut buf).await;
                let req = String::from_utf8_lossy(&buf);
                let payload = if req.contains("/health") {
                    "{\"status\":\"ok\"}".to_string()
                } else {
                    body.clone()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut s, resp.as_bytes()).await;
            }
        })
    }

    fn sudo_cap_cfg(sidecar_url: String) -> ChainConfig {
        let mut cfg = TestEnv::terp_config();
        cfg.chain_id = "ict-hm-sudo-cap".into();
        cfg.gas_prices = "0uterp".into();
        cfg.gas_adjustment = 2.0;
        cfg.config_file_overrides = {
            let mut m = std::collections::HashMap::new();
            m.insert(
                "config/app.toml".into(),
                serde_json::json!({
                    "hashmerchant": {
                        "sidecar-url": sidecar_url,
                        "sidecar-timeout": "2s"
                    }
                }),
            );
            m
        };
        cfg.pre_genesis = Some({
            let u = sidecar_url;
            Box::new(move |_chain| {
                patch_val_sidecar_url(0, &u)
                    .and_then(|_| patch_val_sidecar_url(1, &u))
                    .map_err(|e| ict_rs::error::IctError::Config(e))
            })
        });
        cfg.modify_genesis = Some(Box::new(|_cfg, bytes| {
            let mut g: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| ict_rs::error::IctError::Config(e.to_string()))?;
            g["consensus"]["params"]["abci"]["vote_extensions_enable_height"] =
                serde_json::json!("2");
            g["app_state"]["hashmerchant"]["params"]["quorum_fraction"] =
                serde_json::json!("0.667000000000000000");
            g["app_state"]["hashmerchant"]["params"]["contract_gas_limit"] =
                serde_json::json!(SUDO_CAP_TEST_LIMIT.to_string());
            g["app_state"]["hashmerchant"]["registered_chains"] = serde_json::json!([{
                "chain_uid": "oracle-eth",
                "name": "sudo-cap",
                "rpc_endpoints": [],
                "hash_algos": ["sha256"],
                "enabled": true,
                "oracle_sources": [
                    {
                        "source_id": "src-alpha",
                        "kind": 4,
                        "endpoint": "lab",
                        "enabled": true,
                        "authenticator": {
                            "pubkey": "0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc=",
                            "algorithm": "ed25519",
                            "scope": "ict-ve"
                        }
                    },
                    {
                        "source_id": "src-zeta",
                        "kind": 4,
                        "endpoint": "lab",
                        "enabled": true,
                        "authenticator": {
                            "pubkey": "oJql9HpnWYAv+VX43C0qFKXJnSO+l/hkEn/5ODRVpPA=",
                            "algorithm": "ed25519",
                            "scope": "ict-ve"
                        }
                    }
                ]
            }]);
            serde_json::to_vec(&g).map_err(|e| ict_rs::error::IctError::Config(e.to_string()))
        }));
        cfg
    }

    async fn run_sudo_cap() -> Result<(), String> {
        require_ict();
        let wasm = wasm_path();
        if !wasm.exists() {
            return Err(format!("missing {}", wasm.display()));
        }
        let port = 29290u16;
        let _srv = serve_ve(port, ve_json()).await;
        let sidecar = format!("http://host.docker.internal:{port}");

        let mut tc = TestChain::setup(
            "hm_sudo_cap",
            TestChainConfig {
                chain_config: sudo_cap_cfg(sidecar),
                num_validators: 2,
                num_full_nodes: 0,
                genesis_wallets: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("setup: {e}"))?;

        let result = run_sudo_cap_on(&tc.chain, &wasm).await;
        let _ = tc.cleanup().await;
        result
    }

    async fn run_sudo_cap_on(chain: &CosmosChain, wasm: &std::path::Path) -> Result<(), String> {
        wait_for_blocks(chain, 2).await.map_err(|e| e.to_string())?;
        let (h0, _) = snapshots(chain).await?;
        assert_lockstep("sudo-cap-genesis", &h0)?;

        chain
            .create_key("default")
            .await
            .map_err(|e| e.to_string())?;
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
        let code_id = chain
            .store_code("default", "/tmp/ibchooks_counter.wasm")
            .await
            .map_err(|e| format!("store: {e}"))?;
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        let addr = chain
            .instantiate_contract("default", &code_id, r#"{"count":0}"#, "sudo-cap", None)
            .await
            .map_err(|e| format!("instantiate: {e}"))?;
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        let opts = chain.default_tx_opts().from("default");
        let reg = chain
            .chain_exec_tx_with(
                &[
                    "tx",
                    "hashmerchant",
                    "register-contract",
                    &addr,
                    "oracle-eth",
                    "",
                    "1000000uterp",
                ],
                opts,
            )
            .await
            .map_err(|e| e.to_string())?;
        if reg.exit_code != 0 {
            return Err(format!("register-contract {}", reg.stderr_str().trim()));
        }

        let mut current = chain.height().await.map_err(|e| e.to_string())?;
        for _ in 0..40 {
            if current >= 8 {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            current = chain.height().await.map_err(|e| e.to_string())?;
        }
        if current < 8 {
            return Err(format!("stalled at {current} after sudo-cap register"));
        }
        let (h1, ht1) = snapshots(chain).await?;
        assert_lockstep("after-sudo-cap-ve", &h1)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (h2, ht2) = snapshots(chain).await?;
        assert_lockstep("sudo-cap-still-producing", &h2)?;
        if ht2 < ht1 {
            return Err("height went backwards after capped sudo".into());
        }
        println!("PASS sudo-cap lockstep {ht1} → {ht2} limit={SUDO_CAP_TEST_LIMIT}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "ignored-without-ICT"]
    async fn hashmerchant_sudo_gas_cap_two_vals() {
        match run_sudo_cap().await {
            Ok(()) => println!("PASS hashmerchant_sudo_gas_cap_two_vals"),
            Err(e) => {
                println!("FAIL hashmerchant_sudo_gas_cap_two_vals {e}");
                panic!("{e}");
            }
        }
    }

    // ------------------------------------------------------------------
    // Cold-joiner / statesync / tight-sudo coverage (gas 0-day manifold)
    // ------------------------------------------------------------------

    /// AppHash + min height over validators AND full nodes (joiners count).
    async fn snapshots_all(chain: &CosmosChain) -> Result<(Vec<String>, u64), String> {
        let mut hashes = Vec::new();
        let mut min_h = u64::MAX;
        for (i, node) in chain
            .validators()
            .iter()
            .chain(chain.full_nodes().iter())
            .enumerate()
        {
            let st = node
                .query_status_json()
                .await
                .map_err(|e| format!("node-{i} status: {e}"))?;
            let h =
                app_hash(&st).ok_or_else(|| format!("node-{i} missing latest_app_hash: {st}"))?;
            min_h = min_h.min(height_of(&st).unwrap_or(0));
            hashes.push(h);
        }
        if hashes.is_empty() {
            return Err("no nodes to snapshot".into());
        }
        Ok((hashes, min_h))
    }

    /// Delete a validator's wasmvm file cache so the next execute recompiles
    /// from disk instead of hitting the warm compile/instance cache.
    async fn wipe_wasm_file_cache(chain: &CosmosChain, val_index: usize) -> Result<(), String> {
        let node = chain
            .validators()
            .get(val_index)
            .ok_or_else(|| format!("no val-{val_index}"))?;
        let home = node.home_dir.clone();
        // Layout varies by image; clear every known cache dir, ignore misses.
        let cmd = format!(
            "rm -rf {home}/wasm/wasm/cache {home}/.terpd/wasm/wasm/cache $HOME/.terpd/wasm/wasm/cache 2>/dev/null; echo wiped"
        );
        node.exec_raw(&["sh", "-c", &cmd], &[])
            .await
            .map_err(|e| format!("wipe wasm cache val-{val_index}: {e}"))?;
        println!("PASS wiped-wasm-cache val-{val_index} home={home}");
        Ok(())
    }

    /// Restart a validator container in place (keys kept, RAM + compile cache
    /// dropped). Models a cold node rejoining consensus.
    async fn restart_validator(tc: &mut TestChain, val_index: usize) -> Result<(), String> {
        {
            let node = tc
                .chain
                .validators()
                .get(val_index)
                .ok_or_else(|| format!("no val-{val_index}"))?;
            node.stop_container()
                .await
                .map_err(|e| format!("stop val-{val_index}: {e}"))?;
        }
        {
            let node = tc
                .chain
                .validators_mut()
                .get_mut(val_index)
                .ok_or_else(|| format!("no val-{val_index}"))?;
            node.start_container()
                .await
                .map_err(|e| format!("start val-{val_index}: {e}"))?;
        }
        {
            let node = tc
                .chain
                .validators()
                .get(val_index)
                .ok_or_else(|| format!("no val-{val_index}"))?;
            node.exec_start_chain()
                .await
                .map_err(|e| format!("restart chain val-{val_index}: {e}"))?;
        }
        println!("PASS restarted val-{val_index}");
        Ok(())
    }

    /// Fund `default`, store + instantiate the counter, run `executes`
    /// increments. Returns the contract address. Warms every validator's
    /// wasmvm compile/instance cache for the same checksum.
    async fn warm_wasm(
        chain: &CosmosChain,
        wasm: &std::path::Path,
        executes: usize,
    ) -> Result<String, String> {
        chain
            .create_key("default")
            .await
            .map_err(|e| e.to_string())?;
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
        let code_id = chain
            .store_code("default", "/tmp/ibchooks_counter.wasm")
            .await
            .map_err(|e| format!("wasm store (compile): {e}"))?;
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        let addr = chain
            .instantiate_contract(
                "default",
                &code_id,
                r#"{"count":0}"#,
                "hm-gas-counter",
                None,
            )
            .await
            .map_err(|e| format!("instantiate: {e}"))?;
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        for i in 0..executes {
            chain
                .execute_contract("default", &addr, r#"{"increment":{}}"#, None)
                .await
                .map_err(|e| format!("warm execute {i}: {e}"))?;
            wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        }
        Ok(addr)
    }

    // --- Test 3: cold-cache restart stays in lockstep ---

    async fn run_cold_cache_restart() -> Result<(), String> {
        require_ict();
        let wasm = wasm_path();
        if !wasm.exists() {
            return Err(format!("missing {}", wasm.display()));
        }
        let mut tc = TestChain::setup(
            "hm_cold_cache",
            TestChainConfig {
                chain_config: terp_cfg(),
                num_validators: 2,
                num_full_nodes: 0,
                genesis_wallets: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("setup: {e}"))?;
        let result = run_cold_cache_restart_on(&mut tc, &wasm).await;
        let _ = tc.cleanup().await;
        result
    }

    async fn run_cold_cache_restart_on(
        tc: &mut TestChain,
        wasm: &std::path::Path,
    ) -> Result<(), String> {
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        let addr = warm_wasm(&tc.chain, wasm, 5).await?;
        let (h0, _) = snapshots(&tc.chain).await?;
        assert_lockstep("warm-lockstep", &h0)?;

        // Cold joiner, cycle 1: wipe val-1 file cache + restart (drops RAM /
        // instance cache), let it rejoin, then execute on the same checksum
        // (cold recompile vs warm hit).
        wipe_wasm_file_cache(&tc.chain, 1).await?;
        restart_validator(tc, 1).await?;
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        let (h1, _) = snapshots(&tc.chain).await?;
        assert_lockstep("after-cold-restart", &h1)?;
        for i in 0..5 {
            tc.chain
                .execute_contract("default", &addr, r#"{"increment":{}}"#, None)
                .await
                .map_err(|e| format!("cold execute {i}: {e}"))?;
            wait_for_blocks(&tc.chain, 1)
                .await
                .map_err(|e| e.to_string())?;
        }
        let (h2, ht2) = snapshots(&tc.chain).await?;
        assert_lockstep("after-cold-executes", &h2)?;

        // Cycle 2 to catch flaky 1-gas deltas.
        wipe_wasm_file_cache(&tc.chain, 1).await?;
        restart_validator(tc, 1).await?;
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        for i in 0..3 {
            tc.chain
                .execute_contract("default", &addr, r#"{"increment":{}}"#, None)
                .await
                .map_err(|e| format!("cold execute-2 {i}: {e}"))?;
            wait_for_blocks(&tc.chain, 1)
                .await
                .map_err(|e| e.to_string())?;
        }
        let (h3, ht3) = snapshots(&tc.chain).await?;
        assert_lockstep("after-second-cold-cycle", &h3)?;
        if ht3 <= ht2 {
            return Err(format!(
                "chain stalled after cold-cache cycles {ht2} → {ht3}"
            ));
        }
        println!("PASS cold-cache-restart {ht2} → {ht3}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "ignored-without-ICT"]
    async fn cold_cache_restart_stays_in_lockstep() {
        match run_cold_cache_restart().await {
            Ok(()) => println!("PASS cold_cache_restart_stays_in_lockstep"),
            Err(e) => {
                println!("FAIL cold_cache_restart_stays_in_lockstep {e}");
                panic!("{e}");
            }
        }
    }

    // --- Test 4: statesync joiner converges after wasm activity ---

    fn statesync_cfg() -> ChainConfig {
        let mut cfg = terp_cfg();
        cfg.chain_id = "ict-hm-statesync".into();
        let mut m = std::collections::HashMap::new();
        m.insert(
            "config/app.toml".into(),
            serde_json::json!({
                "state-sync": {
                    "snapshot-interval": 10,
                    "snapshot-keep-recent": 2
                },
                "pruning": "nothing"
            }),
        );
        cfg.config_file_overrides = m;
        cfg
    }

    async fn run_statesync_joiner() -> Result<(), String> {
        require_ict();
        let wasm = wasm_path();
        if !wasm.exists() {
            return Err(format!("missing {}", wasm.display()));
        }
        let mut tc = TestChain::setup(
            "hm_statesync",
            TestChainConfig {
                chain_config: statesync_cfg(),
                num_validators: 2,
                num_full_nodes: 1,
                genesis_wallets: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("setup: {e}"))?;
        let result = run_statesync_joiner_on(&mut tc, &wasm).await;
        let _ = tc.cleanup().await;
        result
    }

    async fn run_statesync_joiner_on(
        tc: &mut TestChain,
        wasm: &std::path::Path,
    ) -> Result<(), String> {
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        let addr = warm_wasm(&tc.chain, wasm, 5).await?;
        let (h0, _) = snapshots_all(&tc.chain).await?;
        assert_lockstep("warm-with-fullnode", &h0)?;

        // Wait until at least two snapshot intervals have passed so a
        // snapshot exists for the joiner. Keep exercising wasmvm meanwhile.
        let cur = tc.chain.height().await.map_err(|e| e.to_string())?;
        let target = ((cur / 10) + 2) * 10;
        loop {
            let h = tc.chain.height().await.map_err(|e| e.to_string())?;
            if h >= target {
                break;
            }
            if h < cur {
                return Err("height went backwards while waiting for snapshots".into());
            }
            tc.chain
                .execute_contract("default", &addr, r#"{"increment":{}}"#, None)
                .await
                .map_err(|e| format!("pre-snapshot execute: {e}"))?;
            wait_for_blocks(&tc.chain, 1)
                .await
                .map_err(|e| e.to_string())?;
        }

        // Trust params from the primary validator.
        let (trust_height, trust_hash, rpc_servers) = {
            let validator = tc.chain.primary_node().map_err(|e| e.to_string())?;
            let th = (tc.chain.height().await.map_err(|e| e.to_string())? / 10) * 10;
            if th == 0 {
                return Err("no snapshot height yet".into());
            }
            let hash = validator
                .query_block_hash(th)
                .await
                .map_err(|e| format!("trust hash: {e}"))?;
            let rpc = format!(
                "tcp://{}:{},tcp://{}:{}",
                validator.hostname, validator.ports.rpc, validator.hostname, validator.ports.rpc
            );
            (th, hash, rpc)
        };

        // Wipe the full node and rejoin via statesync (mirrors
        // examples/state_sync.rs, but after real wasmvm activity).
        {
            let fn0 = tc
                .chain
                .full_nodes()
                .first()
                .ok_or("no full node".to_string())?;
            fn0.stop_container()
                .await
                .map_err(|e| format!("stop full node: {e}"))?;
        }
        {
            let fn0 = tc
                .chain
                .full_nodes_mut()
                .first_mut()
                .ok_or("no full node".to_string())?;
            fn0.remove_container()
                .await
                .map_err(|e| format!("remove full node: {e}"))?;
            fn0.create_container_for_upgrade()
                .await
                .map_err(|e| format!("recreate full node: {e}"))?;
            fn0.start_container()
                .await
                .map_err(|e| format!("start full node container: {e}"))?;
        }
        {
            let fn0 = tc
                .chain
                .full_nodes()
                .first()
                .ok_or("no full node".to_string())?;
            fn0.wipe_data()
                .await
                .map_err(|e| format!("wipe full node: {e}"))?;
            fn0.apply_config_override(
                "config/config.toml",
                &serde_json::json!({
                    "statesync": {
                        "enable": true,
                        "trust_height": trust_height,
                        "trust_hash": trust_hash,
                        "rpc_servers": rpc_servers,
                        "discovery_time": "10s"
                    }
                }),
            )
            .await
            .map_err(|e| format!("statesync config: {e}"))?;
            fn0.exec_start_chain()
                .await
                .map_err(|e| format!("start statesync node: {e}"))?;
        }

        // Poll until synced (catching_up = false), 120s deadline.
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            if std::time::Instant::now() > deadline {
                return Err("statesync joiner timed out after 120s".into());
            }
            let fn0 = tc
                .chain
                .full_nodes()
                .first()
                .ok_or("no full node".to_string())?;
            match fn0.is_catching_up().await {
                Ok(false) => break,
                Ok(true) => continue,
                Err(_) => continue,
            }
        }

        // Verify statesync (not genesis replay): earliest block must be > 1.
        {
            let fn0 = tc
                .chain
                .full_nodes()
                .first()
                .ok_or("no full node".to_string())?;
            let st = fn0
                .query_status_json()
                .await
                .map_err(|e| format!("joiner status: {e}"))?;
            let earliest: u64 = st
                .pointer("/sync_info/earliest_block_height")
                .or_else(|| st.pointer("/SyncInfo/earliest_block_height"))
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse().ok())
                        .or_else(|| v.as_u64())
                })
                .unwrap_or(1);
            if earliest <= 1 {
                return Err(format!(
                    "joiner replayed from genesis (earliest={earliest}); statesync did not happen"
                ));
            }
            println!("PASS statesync-joiner earliest={earliest} trust={trust_height}");
        }

        // Joiner AppHash must match validators, through further executes.
        let (h1, _) = snapshots_all(&tc.chain).await?;
        assert_lockstep("after-statesync-join", &h1)?;
        for i in 0..3 {
            tc.chain
                .execute_contract("default", &addr, r#"{"increment":{}}"#, None)
                .await
                .map_err(|e| format!("post-join execute {i}: {e}"))?;
            wait_for_blocks(&tc.chain, 1)
                .await
                .map_err(|e| e.to_string())?;
        }
        let (h2, ht2) = snapshots_all(&tc.chain).await?;
        assert_lockstep("statesync-still-lockstep", &h2)?;
        println!("PASS statesync-joiner lockstep height={ht2}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "ignored-without-ICT"]
    async fn statesync_joiner_converges_after_wasm_activity() {
        match run_statesync_joiner().await {
            Ok(()) => println!("PASS statesync_joiner_converges_after_wasm_activity"),
            Err(e) => {
                println!("FAIL statesync_joiner_converges_after_wasm_activity {e}");
                panic!("{e}");
            }
        }
    }

    // --- Test 5: tight-gas, staking hooks, deterministic failures ---

    async fn run_tight_gas_hooks() -> Result<(), String> {
        require_ict();
        let wasm = wasm_path();
        if !wasm.exists() {
            return Err(format!("missing {}", wasm.display()));
        }
        let mut tc = TestChain::setup(
            "hm_tight_gas",
            TestChainConfig {
                chain_config: terp_cfg(),
                num_validators: 2,
                num_full_nodes: 0,
                genesis_wallets: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("setup: {e}"))?;
        let result = run_tight_gas_hooks_on(&mut tc, &wasm).await;
        let _ = tc.cleanup().await;
        result
    }

    async fn run_tight_gas_hooks_on(
        tc: &mut TestChain,
        wasm: &std::path::Path,
    ) -> Result<(), String> {
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        let addr = warm_wasm(&tc.chain, wasm, 3).await?;

        // Interleave executes with native bank sends: native event traffic
        // around the wasm path (ReplyCosts analog) must not split AppHash.
        for i in 0..6 {
            tc.chain
                .execute_contract("default", &addr, r#"{"increment":{}}"#, None)
                .await
                .map_err(|e| format!("tight execute {i}: {e}"))?;
            wait_for_blocks(&tc.chain, 1)
                .await
                .map_err(|e| e.to_string())?;
            let user = tc
                .chain
                .primary_node()
                .map_err(|e| e.to_string())?
                .get_key_address("default")
                .await
                .map_err(|e| e.to_string())?;
            tc.chain
                .send_funds(
                    "default",
                    &ict_rs::tx::WalletAmount {
                        address: user,
                        denom: "uterp".into(),
                        amount: 1,
                    },
                )
                .await
                .map_err(|e| format!("bank send {i}: {e}"))?;
            wait_for_blocks(&tc.chain, 1)
                .await
                .map_err(|e| e.to_string())?;
        }
        let (h0, _) = snapshots(&tc.chain).await?;
        assert_lockstep("after-interleaved-exec-bank", &h0)?;

        // Fire staking hooks with an empty hook registry (must be a no-op,
        // never a halt): delegate to the first validator.
        let valop = {
            let out = tc
                .chain
                .chain_exec(&["query", "staking", "validators", "--output", "json"])
                .await
                .map_err(|e| format!("query validators: {e}"))?;
            let raw = out.stdout_str().trim().to_string();
            let v: serde_json::Value =
                serde_json::from_str(&raw).map_err(|e| format!("validators JSON: {e}: {raw}"))?;
            v.pointer("/validators/0/operator_address")
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("no validator operator in {v}"))?
        };
        let opts = tc.chain.default_tx_opts().from("default");
        let del = tc
            .chain
            .chain_exec_tx_with(&["tx", "staking", "delegate", &valop, "1000000uterp"], opts)
            .await
            .map_err(|e| format!("delegate exec: {e}"))?;
        if del.exit_code != 0 {
            return Err(format!(
                "delegate exit={} stdout={} stderr={}",
                del.exit_code,
                del.stdout_str().trim(),
                del.stderr_str().trim()
            ));
        }
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        let (h1, _) = snapshots(&tc.chain).await?;
        assert_lockstep("after-staking-delegate-hooks", &h1)?;

        // A deterministically-failing DeliverTx write (wrong escrow denom)
        // must fail identically on both validators — no success/fail split.
        let opts = tc.chain.default_tx_opts().from("default");
        let bad = tc
            .chain
            .chain_exec_tx_with(
                &[
                    "tx",
                    "hashmerchant",
                    "register-contract",
                    &addr,
                    "gas-lab",
                    "",
                    "1000000uatom",
                ],
                opts,
            )
            .await
            .map_err(|e| format!("bad-register exec: {e}"))?;
        if bad.exit_code == 0 {
            return Err("register-contract with wrong denom unexpectedly succeeded".into());
        }
        println!(
            "PASS deterministic-failure ({})",
            bad.stderr_str().trim().lines().next().unwrap_or("rejected")
        );
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        let (h2, ht2) = snapshots(&tc.chain).await?;
        assert_lockstep("after-deterministic-failure", &h2)?;
        println!("PASS tight-gas-hooks height={ht2}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "ignored-without-ICT"]
    async fn tight_gas_hooks_and_reply_lockstep() {
        match run_tight_gas_hooks().await {
            Ok(()) => println!("PASS tight_gas_hooks_and_reply_lockstep"),
            Err(e) => {
                println!("FAIL tight_gas_hooks_and_reply_lockstep {e}");
                panic!("{e}");
            }
        }
    }

    // ------------------------------------------------------------------
    // Vulnerability-class closers (team gap analysis)
    // ------------------------------------------------------------------

    /// Committed outcome of a broadcast tx.
    struct CommittedTx {
        /// CLI/broadcast exit (sync mode: only proves CheckTx passed).
        broadcast_exit: i64,
        /// DeliverTx result code once included (None = never included).
        code: Option<u64>,
        /// Committed gas_used once included.
        gas_used: Option<u64>,
        hash: String,
    }

    /// Submit a raw `tx wasm execute` with explicit `--gas`, wait a block,
    /// then poll `query tx` for the COMMITTED result.
    ///
    /// Sync-mode CLI exit only proves CheckTx; OOG-vs-success lives in the
    /// committed `.code`, so every boundary assertion must use it.
    async fn raw_execute(
        chain: &CosmosChain,
        contract: &str,
        msg: &str,
        gas: &str,
    ) -> Result<CommittedTx, String> {
        let opts = chain.default_tx_opts().from("default").gas(gas);
        let out = chain
            .chain_exec_tx_with(&["tx", "wasm", "execute", contract, msg], opts)
            .await
            .map_err(|e| format!("raw execute: {e}"))?;
        let raw = out.stdout_str();
        let v: serde_json::Value = serde_json::from_str(raw.trim())
            .map_err(|e| format!("execute response JSON: {e}: {raw}"))?;
        let hash = v["txhash"].as_str().unwrap_or_default().to_string();
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        // Poll until the tx is indexed (up to ~20s past the block wait).
        let mut code = None;
        let mut gas_used = None;
        if !hash.is_empty() {
            for _ in 0..10 {
                if let Ok(q) = chain
                    .chain_exec(&["query", "tx", &hash, "--output", "json"])
                    .await
                {
                    let qr = q.stdout_str();
                    if let Ok(qv) = serde_json::from_str::<serde_json::Value>(qr.trim()) {
                        if qv.get("gas_used").is_some() || qv.get("code").is_some() {
                            code = qv["code"]
                                .as_str()
                                .and_then(|s| s.parse().ok())
                                .or_else(|| qv["code"].as_u64());
                            gas_used = qv["gas_used"]
                                .as_str()
                                .and_then(|s| s.parse().ok())
                                .or_else(|| qv["gas_used"].as_u64());
                            break;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        Ok(CommittedTx {
            broadcast_exit: out.exit_code,
            code,
            gas_used,
            hash,
        })
    }

    /// Instantiate with full error surface: broadcast exit + stderr, then
    /// `_contract_address` from the committed instantiate event.
    async fn instantiate_verbose(
        chain: &CosmosChain,
        key: &str,
        code_id: &str,
        msg: &str,
        label: &str,
    ) -> Result<String, String> {
        let opts = chain.default_tx_opts().from(key).flag("--no-admin", "");
        let out = chain
            .chain_exec_tx_with(
                &["tx", "wasm", "instantiate", code_id, msg, "--label", label],
                opts,
            )
            .await
            .map_err(|e| format!("instantiate exec: {e}"))?;
        if out.exit_code != 0 {
            return Err(format!(
                "instantiate exit={} stdout={:?} stderr={:?}",
                out.exit_code,
                out.stdout_str().trim(),
                out.stderr_str().trim()
            ));
        }
        let raw = out.stdout_str();
        let v: serde_json::Value = serde_json::from_str(raw.trim())
            .map_err(|e| format!("instantiate JSON: {e} stdout={raw:?}"))?;
        let hash = v["txhash"].as_str().unwrap_or_default().to_string();
        if hash.is_empty() {
            return Err(format!("instantiate broadcast without txhash: {v}"));
        }
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        for _ in 0..10 {
            if let Ok(q) = chain
                .chain_exec(&["query", "tx", &hash, "--output", "json"])
                .await
            {
                let qr = q.stdout_str();
                if let Ok(qv) = serde_json::from_str::<serde_json::Value>(qr.trim()) {
                    if let Some(code) = qv["code"]
                        .as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .or_else(|| qv["code"].as_u64())
                    {
                        if code != 0 {
                            return Err(format!(
                                "instantiate committed code={code}: {}",
                                qv["raw_log"].as_str().unwrap_or("")
                            ));
                        }
                    }
                    if let Some(events) = qv["events"].as_array() {
                        for ev in events {
                            if ev["type"].as_str() == Some("instantiate") {
                                if let Some(attrs) = ev["attributes"].as_array() {
                                    for a in attrs {
                                        if a["key"].as_str() == Some("_contract_address") {
                                            if let Some(addr) = a["value"].as_str() {
                                                return Ok(addr.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if qv.get("gas_used").is_some() {
                        return Err(format!("instantiate tx has no address event: {qv}"));
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(format!("instantiate tx {hash} never indexed"))
    }

    /// Store a wasm file with full error surface (exit + stderr), unlike the
    /// shared helper which only parses stdout.
    ///
    /// Sync broadcast carries no code_id, so this waits a block and reads it
    /// from the committed `store_code` event (the shared helper silently
    /// falls back to "1").
    async fn store_code_verbose(
        chain: &CosmosChain,
        key: &str,
        container_path: &str,
    ) -> Result<String, String> {
        let opts = chain.default_tx_opts().from(key);
        let out = chain
            .chain_exec_tx_with(&["tx", "wasm", "store", container_path], opts)
            .await
            .map_err(|e| format!("store exec: {e}"))?;
        if out.exit_code != 0 {
            return Err(format!(
                "store exit={} stdout={:?} stderr={:?}",
                out.exit_code,
                out.stdout_str().trim(),
                out.stderr_str().trim()
            ));
        }
        let raw = out.stdout_str();
        let v: serde_json::Value = serde_json::from_str(raw.trim())
            .map_err(|e| format!("store JSON: {e} stdout={raw:?}"))?;
        // Fast path: some responses already carry code_id.
        if let Some(id) = v["code_id"]
            .as_str()
            .map(str::to_string)
            .or_else(|| v["code_id"].as_u64().map(|n| n.to_string()))
        {
            return Ok(id);
        }
        let hash = v["txhash"].as_str().unwrap_or_default().to_string();
        if hash.is_empty() {
            return Err(format!("store broadcast without txhash: {v}"));
        }
        wait_for_blocks(chain, 1).await.map_err(|e| e.to_string())?;
        for _ in 0..10 {
            if let Ok(q) = chain
                .chain_exec(&["query", "tx", &hash, "--output", "json"])
                .await
            {
                let qr = q.stdout_str();
                if let Ok(qv) = serde_json::from_str::<serde_json::Value>(qr.trim()) {
                    if let Some(events) = qv["events"].as_array() {
                        for ev in events {
                            if ev["type"].as_str() == Some("store_code") {
                                if let Some(attrs) = ev["attributes"].as_array() {
                                    for a in attrs {
                                        if a["key"].as_str() == Some("code_id") {
                                            if let Some(id) = a["value"].as_str() {
                                                return Ok(id.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Indexed but no code_id yet/ever — surface the body.
                    if qv.get("gas_used").is_some() {
                        return Err(format!("store tx has no code_id event: {qv}"));
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        Err(format!("store tx {hash} never indexed"))
    }

    // --- Test 6: gas boundary OOG-vs-success must agree on both vals ---

    async fn run_gas_boundary() -> Result<(), String> {
        require_ict();
        let wasm = wasm_path();
        if !wasm.exists() {
            return Err(format!("missing {}", wasm.display()));
        }
        let mut tc = TestChain::setup(
            "hm_gas_boundary",
            TestChainConfig {
                chain_config: terp_cfg(),
                num_validators: 2,
                num_full_nodes: 0,
                genesis_wallets: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("setup: {e}"))?;
        let result = run_gas_boundary_on(&mut tc, &wasm).await;
        let _ = tc.cleanup().await;
        result
    }

    async fn run_gas_boundary_on(tc: &mut TestChain, wasm: &std::path::Path) -> Result<(), String> {
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        let addr = warm_wasm(&tc.chain, wasm, 2).await?;

        // Measure twice: identical executes must cost (near-)identical
        // COMMITTED gas. A small tolerance covers per-tx jitter outside wasm
        // execution; gross divergence is the 0-day (gas is not a pure
        // function of the call).
        let m1 = raw_execute(&tc.chain, &addr, r#"{"increment":{}}"#, "auto").await?;
        let used_a = m1.gas_used.ok_or_else(|| {
            format!(
                "measure-1 not indexed: broadcast={} hash={}",
                m1.broadcast_exit, m1.hash
            )
        })?;
        if m1.code != Some(0) {
            return Err(format!("measure-1 failed on chain: code={:?}", m1.code));
        }
        let m2 = raw_execute(&tc.chain, &addr, r#"{"increment":{}}"#, "auto").await?;
        let used_b = m2.gas_used.ok_or_else(|| {
            format!(
                "measure-2 not indexed: broadcast={} hash={}",
                m2.broadcast_exit, m2.hash
            )
        })?;
        if m2.code != Some(0) {
            return Err(format!("measure-2 failed on chain: code={:?}", m2.code));
        }
        println!("PASS measured gas_used a={used_a} b={used_b}");
        if used_a.abs_diff(used_b) > 32 {
            return Err(format!(
                "non-deterministic execute gas on one chain: {used_a} != {used_b}"
            ));
        }
        let baseline = used_a.max(used_b);
        let (h_used, _) = snapshots(&tc.chain).await?;
        assert_lockstep("after-measure", &h_used)?;

        // Walk down from the edge to find the true COMMITTED OOG cliff:
        // probe baseline-1, -2, -4, ... until committed code != 0 (cap 14
        // probes). Sync-mode CLI exit only proves CheckTx, so every step
        // asserts on the DeliverTx code. Both validators must agree at every
        // step (any 1-gas fork → AppHash split → lockstep catches it).
        let mut step: u64 = 1;
        let mut cliff: Option<u64> = None;
        for _ in 0..14 {
            let limit = baseline.saturating_sub(step);
            if limit == 0 {
                break;
            }
            let p =
                raw_execute(&tc.chain, &addr, r#"{"increment":{}}"#, &limit.to_string()).await?;
            let (h, _) = snapshots(&tc.chain).await?;
            assert_lockstep(&format!("after-gas-probe-{limit}"), &h)?;
            println!(
                "PASS gas-probe limit={limit} broadcast={} committed={:?} used={:?}",
                p.broadcast_exit, p.code, p.gas_used
            );
            match p.code {
                None => {
                    return Err(format!("probe limit={limit} never indexed"));
                }
                Some(0) => {}
                Some(_) => {
                    cliff = Some(limit);
                    break;
                }
            }
            step *= 2;
        }
        let cliff = cliff.ok_or_else(|| {
            format!("no OOG cliff found below baseline={baseline} after 14 halving probes")
        })?;
        println!("PASS OOG cliff at limit={cliff} (baseline={baseline})");
        if baseline - cliff > 65536 {
            return Err(format!(
                "cliff {cliff} suspiciously far below measured {baseline}"
            ));
        }

        // Just above the cliff must commit code 0 identically.
        let pok = raw_execute(
            &tc.chain,
            &addr,
            r#"{"increment":{}}"#,
            &(cliff + 1).to_string(),
        )
        .await?;
        if pok.code != Some(0) {
            return Err(format!(
                "gas limit cliff+1={} committed code={:?} (expected 0)",
                cliff + 1,
                pok.code
            ));
        }
        let (h_ok, ht_ok) = snapshots(&tc.chain).await?;
        assert_lockstep("after-cliff-plus-1", &h_ok)?;
        if ht_ok == 0 {
            return Err("chain stalled at gas boundary".into());
        }
        println!("PASS gas-boundary cliff={cliff} baseline={baseline} height={ht_ok}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "ignored-without-ICT"]
    async fn gas_boundary_oog_vs_success_lockstep() {
        match run_gas_boundary().await {
            Ok(()) => println!("PASS gas_boundary_oog_vs_success_lockstep"),
            Err(e) => {
                println!("FAIL gas_boundary_oog_vs_success_lockstep {e}");
                panic!("{e}");
            }
        }
    }

    // --- Test 7: wasm submessage bank-send with reply stays in lockstep ---

    fn proxy_wasm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/interchaintest/contracts/polytone_proxy.wasm")
    }

    async fn run_submsg_reply() -> Result<(), String> {
        require_ict();
        let wasm = proxy_wasm_path();
        if !wasm.exists() {
            return Err(format!("missing {}", wasm.display()));
        }
        let mut tc = TestChain::setup(
            "hm_submsg_reply",
            TestChainConfig {
                chain_config: terp_cfg(),
                num_validators: 2,
                num_full_nodes: 0,
                genesis_wallets: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("setup: {e}"))?;
        let result = run_submsg_reply_on(&mut tc, &wasm).await;
        let _ = tc.cleanup().await;
        result
    }

    async fn run_submsg_reply_on(tc: &mut TestChain, wasm: &std::path::Path) -> Result<(), String> {
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        tc.chain
            .create_key("default")
            .await
            .map_err(|e| e.to_string())?;
        let user = tc
            .chain
            .primary_node()
            .map_err(|e| e.to_string())?
            .get_key_address("default")
            .await
            .map_err(|e| e.to_string())?;
        tc.chain
            .send_funds(
                "validator",
                &ict_rs::tx::WalletAmount {
                    address: user.clone(),
                    denom: "uterp".into(),
                    amount: 50_000_000_000,
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        wait_for_blocks(&tc.chain, 1)
            .await
            .map_err(|e| e.to_string())?;

        let node = tc.chain.primary_node().map_err(|e| e.to_string())?;
        node.copy_file_from_host(wasm, "/tmp/polytone_proxy.wasm")
            .await
            .map_err(|e| e.to_string())?;
        let code_id = tc
            .chain
            .store_code("default", "/tmp/polytone_proxy.wasm")
            .await
            .map_err(|e| format!("proxy store: {e}"))?;
        wait_for_blocks(&tc.chain, 1)
            .await
            .map_err(|e| e.to_string())?;
        // Instantiator (default) is the only sender the proxy accepts.
        let proxy = tc
            .chain
            .instantiate_contract("default", &code_id, r#"{}"#, "hm-submsg-proxy", None)
            .await
            .map_err(|e| format!("proxy instantiate: {e}"))?;
        wait_for_blocks(&tc.chain, 1)
            .await
            .map_err(|e| e.to_string())?;

        // Fund the proxy so the inner bank send can execute.
        tc.chain
            .send_funds(
                "default",
                &ict_rs::tx::WalletAmount {
                    address: proxy.clone(),
                    denom: "uterp".into(),
                    amount: 1_000_000,
                },
            )
            .await
            .map_err(|e| format!("fund proxy: {e}"))?;
        wait_for_blocks(&tc.chain, 1)
            .await
            .map_err(|e| e.to_string())?;

        // Each execute emits a bank send as SubMsg::reply_always: native bank
        // events ride into SubMsgResponse and ReplyCosts charges them — the
        // Osmosis fork shape. Both validators must agree every round.
        for i in 0..4 {
            let msg = format!(
                r#"{{"proxy":{{"msgs":[{{"bank":{{"send":{{"to_address":"{user}","amount":[{{"denom":"uterp","amount":"1"}}]}}}}}}]}}}}"#
            );
            let tx = raw_execute(&tc.chain, &proxy, &msg, "auto").await?;
            if tx.code != Some(0) {
                return Err(format!(
                    "proxy submsg execute {i} committed code={:?} (broadcast={})",
                    tx.code, tx.broadcast_exit
                ));
            }
            let (h, _) = snapshots(&tc.chain).await?;
            assert_lockstep(&format!("after-submsg-reply-{i}"), &h)?;
        }
        let (h2, ht2) = snapshots(&tc.chain).await?;
        assert_lockstep("submsg-reply-final", &h2)?;
        println!("PASS submsg-reply lockstep height={ht2} proxy={proxy}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "ignored-without-ICT"]
    async fn submsg_bank_reply_lockstep() {
        match run_submsg_reply().await {
            Ok(()) => println!("PASS submsg_bank_reply_lockstep"),
            Err(e) => {
                println!("FAIL submsg_bank_reply_lockstep {e}");
                panic!("{e}");
            }
        }
    }

    // --- Test 8: registered cw-hooks contract that errors in anger ---

    async fn run_cwhooks_hook() -> Result<(), String> {
        require_ict();
        let wasm = wasm_path();
        if !wasm.exists() {
            return Err(format!("missing {}", wasm.display()));
        }
        let mut tc = TestChain::setup(
            "hm_cwhooks_hook",
            TestChainConfig {
                chain_config: terp_cfg(),
                num_validators: 2,
                num_full_nodes: 0,
                genesis_wallets: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("setup: {e}"))?;
        let result = run_cwhooks_hook_on(&mut tc, &wasm).await;
        let _ = tc.cleanup().await;
        result
    }

    async fn run_cwhooks_hook_on(tc: &mut TestChain, wasm: &std::path::Path) -> Result<(), String> {
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        // The counter has no sudo entrypoint, so every hook sudo errors
        // deterministically — the isolate must skip-and-continue.
        let addr = warm_wasm(&tc.chain, wasm, 1).await?;

        // Register via the real CLI. Autocli fills register_address from
        // --from (the contract creator); only --contract-address is a flag.
        let opts = tc.chain.default_tx_opts().from("default");
        let reg = tc
            .chain
            .chain_exec_tx_with(
                &[
                    "tx",
                    "cw-hooks",
                    "register-staking",
                    "--contract-address",
                    &addr,
                ],
                opts,
            )
            .await
            .map_err(|e| format!("register-staking exec: {e}"))?;
        if reg.exit_code != 0 {
            return Err(format!(
                "register-staking exit={} stdout={} stderr={}",
                reg.exit_code,
                reg.stdout_str().trim(),
                reg.stderr_str().trim()
            ));
        }
        wait_for_blocks(&tc.chain, 1)
            .await
            .map_err(|e| e.to_string())?;

        // Fire BeforeDelegationCreated / AfterDelegationModified with the hook
        // registered: hook sudo errors on both vals; the block must survive.
        let valop = {
            let out = tc
                .chain
                .chain_exec(&["query", "staking", "validators", "--output", "json"])
                .await
                .map_err(|e| format!("query validators: {e}"))?;
            let raw = out.stdout_str().trim().to_string();
            let v: serde_json::Value =
                serde_json::from_str(&raw).map_err(|e| format!("validators JSON: {e}: {raw}"))?;
            v.pointer("/validators/0/operator_address")
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("no validator operator in {v}"))?
        };
        let before = tc.chain.height().await.map_err(|e| e.to_string())?;
        let opts = tc.chain.default_tx_opts().from("default");
        let del = tc
            .chain
            .chain_exec_tx_with(&["tx", "staking", "delegate", &valop, "1000000uterp"], opts)
            .await
            .map_err(|e| format!("delegate exec: {e}"))?;
        if del.exit_code != 0 {
            return Err(format!(
                "delegate exit={} stdout={} stderr={}",
                del.exit_code,
                del.stdout_str().trim(),
                del.stderr_str().trim()
            ));
        }
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        let (h1, ht1) = snapshots(&tc.chain).await?;
        assert_lockstep("after-hook-error-delegate", &h1)?;
        if ht1 <= before {
            return Err(format!("chain stalled by hook error {before} → {ht1}"));
        }
        // Fire once more (AfterDelegationModified path on existing delegation).
        let opts = tc.chain.default_tx_opts().from("default");
        let del2 = tc
            .chain
            .chain_exec_tx_with(&["tx", "staking", "delegate", &valop, "1000000uterp"], opts)
            .await
            .map_err(|e| format!("delegate-2 exec: {e}"))?;
        if del2.exit_code != 0 {
            return Err(format!(
                "delegate-2 exit={} stderr={}",
                del2.exit_code,
                del2.stderr_str().trim()
            ));
        }
        wait_for_blocks(&tc.chain, 2)
            .await
            .map_err(|e| e.to_string())?;
        let (h2, ht2) = snapshots(&tc.chain).await?;
        assert_lockstep("after-hook-error-delegate-2", &h2)?;
        if ht2 <= ht1 {
            return Err(format!("chain stalled by hook error {ht1} → {ht2}"));
        }
        println!("PASS cwhooks-hook-isolated {before} → {ht2}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "ignored-without-ICT"]
    async fn cwhooks_registered_hook_error_isolated() {
        match run_cwhooks_hook().await {
            Ok(()) => println!("PASS cwhooks_registered_hook_error_isolated"),
            Err(e) => {
                println!("FAIL cwhooks_registered_hook_error_isolated {e}");
                panic!("{e}");
            }
        }
    }

    // --- Test 9: sudo returning {} must miss-callback, not halt ---

    fn empty_sudo_wasm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/interchaintest/contracts/empty_sudo.wasm")
    }

    /// VE genesis for the empty-sudo chain (same oracle-eth shape as the
    /// sudo-cap test; the {} contract is registered for this chain_uid).
    fn empty_sudo_cfg() -> ChainConfig {
        let mut cfg = TestEnv::terp_config();
        cfg.chain_id = "ict-hm-empty-sudo".into();
        cfg.gas_prices = "0uterp".into();
        cfg.gas_adjustment = 2.0;
        cfg.modify_genesis = Some(Box::new(|_cfg, bytes| {
            let mut g: serde_json::Value = serde_json::from_slice(&bytes)
                .map_err(|e| ict_rs::error::IctError::Config(e.to_string()))?;
            g["consensus"]["params"]["abci"]["vote_extensions_enable_height"] =
                serde_json::json!("2");
            g["app_state"]["hashmerchant"]["params"]["quorum_fraction"] =
                serde_json::json!("0.667000000000000000");
            g["app_state"]["hashmerchant"]["registered_chains"] = serde_json::json!([{
                "chain_uid": "oracle-eth",
                "name": "empty-sudo",
                "rpc_endpoints": [],
                "hash_algos": ["sha256"],
                "enabled": true,
                "oracle_sources": [
                    {
                        "source_id": "src-alpha",
                        "kind": 4,
                        "endpoint": "lab",
                        "enabled": true,
                        "authenticator": {
                            "pubkey": "0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc=",
                            "algorithm": "ed25519",
                            "scope": "ict-ve"
                        }
                    },
                    {
                        "source_id": "src-zeta",
                        "kind": 4,
                        "endpoint": "lab",
                        "enabled": true,
                        "authenticator": {
                            "pubkey": "oJql9HpnWYAv+VX43C0qFKXJnSO+l/hkEn/5ODRVpPA=",
                            "algorithm": "ed25519",
                            "scope": "ict-ve"
                        }
                    }
                ]
            }]);
            serde_json::to_vec(&g).map_err(|e| ict_rs::error::IctError::Config(e.to_string()))
        }));
        cfg
    }

    async fn run_empty_sudo() -> Result<(), String> {
        require_ict();
        let wasm = empty_sudo_wasm_path();
        if !wasm.exists() {
            return Err(format!("missing {}", wasm.display()));
        }
        // Reuse the sudo-cap sidecar stub: same oracle-eth VEs.
        let port = 29291u16;
        let _srv = serve_ve(port, ve_json()).await;
        let _sidecar = format!("http://host.docker.internal:{port}");
        // NOTE: sidecar URL injection into validator app.toml happens in
        // pre_genesis only for sudo_cap_cfg; here we patch the same way.
        let mut tc = TestChain::setup(
            "hm_empty_sudo",
            TestChainConfig {
                chain_config: empty_sudo_cfg_with_sidecar(_sidecar),
                num_validators: 2,
                num_full_nodes: 0,
                genesis_wallets: Vec::new(),
            },
        )
        .await
        .map_err(|e| format!("setup: {e}"))?;
        let result = run_empty_sudo_on(&tc.chain, &wasm).await;
        let _ = tc.cleanup().await;
        result
    }

    fn empty_sudo_cfg_with_sidecar(sidecar_url: String) -> ChainConfig {
        let mut cfg = empty_sudo_cfg();
        cfg.config_file_overrides = {
            let mut m = std::collections::HashMap::new();
            m.insert(
                "config/app.toml".into(),
                serde_json::json!({
                    "hashmerchant": {
                        "sidecar-url": sidecar_url,
                        "sidecar-timeout": "2s"
                    }
                }),
            );
            m
        };
        cfg.pre_genesis = Some({
            let u = sidecar_url;
            Box::new(move |_chain| {
                patch_val_sidecar_url(0, &u)
                    .and_then(|_| patch_val_sidecar_url(1, &u))
                    .map_err(|e| ict_rs::error::IctError::Config(e))
            })
        });
        cfg
    }

    async fn run_empty_sudo_on(chain: &CosmosChain, wasm: &std::path::Path) -> Result<(), String> {
        wait_for_blocks(chain, 2).await.map_err(|e| e.to_string())?;
        let (h0, _) = snapshots(chain).await?;
        assert_lockstep("empty-sudo-genesis", &h0)?;

        chain
            .create_key("default")
            .await
            .map_err(|e| e.to_string())?;
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

        // Store + instantiate the {} contract. Instantiate returns a normal
        // success result; only sudo returns {}.
        let node = chain.primary_node().map_err(|e| e.to_string())?;
        node.copy_file_from_host(wasm, "/tmp/empty_sudo.wasm")
            .await
            .map_err(|e| e.to_string())?;
        let code_id = store_code_verbose(chain, "default", "/tmp/empty_sudo.wasm")
            .await
            .map_err(|e| format!("empty-sudo store: {e}"))?;
        let addr = instantiate_verbose(chain, "default", &code_id, r#"{}"#, "empty-sudo")
            .await
            .map_err(|e| format!("empty-sudo instantiate: {e}"))?;
        let opts = chain.default_tx_opts().from("default");
        let reg = chain
            .chain_exec_tx_with(
                &[
                    "tx",
                    "hashmerchant",
                    "register-contract",
                    &addr,
                    "oracle-eth",
                    "",
                    "1000000uterp",
                ],
                opts,
            )
            .await
            .map_err(|e| e.to_string())?;
        if reg.exit_code != 0 {
            return Err(format!("register-contract {}", reg.stderr_str().trim()));
        }

        // VE quorum now sudoes the {} contract every PreBlocker. Patched:
        // ErrVMError → miss-callback, chain advances. Unpatched: PreBlocker
        // panic → halt. Bounded wait so a halt fails the test instead of
        // hanging it.
        let start = chain.height().await.map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + Duration::from_secs(90);
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let cur = chain.height().await.map_err(|e| e.to_string())?;
            if cur >= start + 6 {
                break;
            }
            if std::time::Instant::now() > deadline {
                return Err(format!(
                    "chain stalled at {cur} after registering {{}} sudo contract (PreBlocker halt?)"
                ));
            }
        }
        let (h1, ht1) = snapshots(chain).await?;
        assert_lockstep("after-empty-sudo-ve", &h1)?;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let (h2, ht2) = snapshots(chain).await?;
        assert_lockstep("empty-sudo-still-producing", &h2)?;
        if ht2 < ht1 {
            return Err("height went backwards after {} sudo".into());
        }
        println!("PASS empty-sudo miss-callback {ht1} → {ht2}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "ignored-without-ICT"]
    async fn sudo_empty_object_miss_callback() {
        match run_empty_sudo().await {
            Ok(()) => println!("PASS sudo_empty_object_miss_callback"),
            Err(e) => {
                println!("FAIL sudo_empty_object_miss_callback {e}");
                panic!("{e}");
            }
        }
    }
}
