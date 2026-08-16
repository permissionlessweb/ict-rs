//! Store + execute a rustc ≥ 1.87 CosmWasm guest against a node that advertises
//! `bulk_memory`, and prove a huge `memory.copy` dies on **gas** (DoS meter).
//!
//! Stack (groot-wan worktrees):
//! - `crates/_worktrees/cosmwasm-bulk-memory` (metered VM)
//! - `crates/_worktrees/wasmvm-bulk-memory` (built libwasmvm)
//! - `crates/_worktrees/wasmd-bulk-memory` (`build/wasmd`, BuiltInCapabilities + bulk_memory)
//! - terp-core worktree `feat/bulk-memory-ict` go.mod replace → those forks
//!
//! ```sh
//! # 1) compile guest (rustc 1.87+)
//! bash crates/ict-rs/scripts/build-bulk-meter-guest.sh
//!
//! # 2) image that contains the wasmd/terpd linked to that libwasmvm
//! cargo run -p ict-rs --example bulk_memory_gas --features docker
//! ```
//!
//! Env:
//! - `ICT_BULK_IMAGE_REPO` (default `terpnetwork/terp-core`)
//! - `ICT_BULK_IMAGE` (default `local`)
//! - `ICT_BULK_BIN` (default `terpd`; use `wasmd` for the wasmd worktree binary)
//! - `ICT_BULK_DENOM` (default `uterp`)
//! - `ICT_BULK_PREFIX` (default `terp`)
//! - `ICT_BULK_WASM` path to `bulk_meter_guest.wasm`
//! - `ICT_BULK_COPY_N` huge length (default `2147483647`)

use ict_rs::cli::parse_query_response;
use ict_rs::prelude::*;
use std::path::PathBuf;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn guest_wasm() -> PathBuf {
    if let Ok(p) = std::env::var("ICT_BULK_WASM") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../contracts/bulk-meter-guest/artifacts/bulk_meter_guest.wasm")
}

fn chain_config(version: &str) -> ChainConfig {
    let denom = env_or("ICT_BULK_DENOM", "uterp");
    ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "bulk-meter".to_string(),
        chain_id: "bulk-meter-1".to_string(),
        images: vec![DockerImage {
            repository: env_or("ICT_BULK_IMAGE_REPO", "terpnetwork/terp-core"),
            version: version.to_string(),
            uid_gid: None,
        }],
        bin: env_or("ICT_BULK_BIN", "terpd"),
        bech32_prefix: env_or("ICT_BULK_PREFIX", "terp"),
        denom: denom.clone(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: format!("0{denom}"),
        gas_adjustment: 1.3,
        trusting_period: "112h".to_string(),
        block_time: "2s".to_string(),
        genesis: None,
        modify_genesis: None,
        pre_genesis: None,
        config_file_overrides: std::collections::HashMap::new(),
        additional_start_args: vec!["--wasm.skip_wasmvm_version_check".to_string()],
        env: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
    }
}

fn tx_code(raw: &str) -> Option<i64> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    v.get("code")
        .or_else(|| v.pointer("/tx_response/code"))
        .and_then(|c| c.as_i64())
}

fn tx_raw_log(raw: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return raw.to_string();
    };
    v.get("raw_log")
        .or_else(|| v.pointer("/tx_response/raw_log"))
        .and_then(|c| c.as_str())
        .unwrap_or(raw)
        .to_string()
}

struct TxOut {
    stdout: String,
    stderr: String,
    code: Option<i64>,
}

async fn tx(chain: &CosmosChain, args: &[&str]) -> Result<TxOut, Box<dyn std::error::Error>> {
    tx_opts(chain, args, |o| o).await
}

async fn tx_opts(
    chain: &CosmosChain,
    args: &[&str],
    tweak: impl FnOnce(TxOptions) -> TxOptions,
) -> Result<TxOut, Box<dyn std::error::Error>> {
    let node = chain.primary_node()?;
    let opts = tweak(node.default_tx_opts().from("validator"));
    let mut full = vec!["tx"];
    full.extend_from_slice(args);
    let out = node.exec_tx_with(&full, opts).await?;
    let stdout = out.stdout_str();
    let stderr = out.stderr_str();
    let blob = if tx_code(&stdout).is_some() {
        stdout.clone()
    } else {
        stderr.clone()
    };
    // `broadcast-mode sync` is CheckTx only (height=0, gas_used=0). Wait for
    // DeliverTx via `query tx` so code/raw_log reflect the wasm execute.
    let hash = serde_json::from_str::<serde_json::Value>(&blob)
        .ok()
        .and_then(|v| {
            v.get("txhash")
                .and_then(|h| h.as_str())
                .map(|s| s.to_string())
        });
    if let Some(hash) = hash {
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            if let Ok(v) = query_json(chain, &["tx", &hash]).await {
                let stdout = v.to_string();
                return Ok(TxOut {
                    code: tx_code(&stdout),
                    stdout,
                    stderr,
                });
            }
        }
    }
    Ok(TxOut {
        code: tx_code(&blob),
        stdout,
        stderr,
    })
}

async fn query_json(
    chain: &CosmosChain,
    args: &[&str],
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let node = chain.primary_node()?;
    let mut cmd = vec!["query"];
    cmd.extend_from_slice(args);
    cmd.extend_from_slice(&["--output", "json"]);
    let out = node.exec_cmd(&cmd).await?;
    if out.exit_code != 0 {
        return Err(format!("query {:?} failed: {}", args, out.stderr_str()).into());
    }
    Ok(parse_query_response(&out)?)
}

fn assert_gas_trap(log: &str) -> Result<(), String> {
    let l = log.to_lowercase();
    let hit = l.contains("out of gas")
        || l.contains("gas limit")
        || l.contains("unreachable")
        || l.contains("exhausted")
        || l.contains("error calling the vm");
    if hit {
        Ok(())
    } else {
        Err(format!("expected gas/vm trap, raw_log={log}"))
    }
}

async fn run_test(chain: &mut CosmosChain) -> Result<(), Box<dyn std::error::Error>> {
    let wasm = guest_wasm();
    if !wasm.is_file() {
        return Err(format!(
            "guest wasm missing at {} — run crates/ict-rs/scripts/build-bulk-meter-guest.sh",
            wasm.display()
        )
        .into());
    }
    let wasm_bytes = std::fs::read(&wasm)?;
    println!(
        "guest wasm {} ({} bytes, sha256 not printed — store checksum is of these bytes)",
        wasm.display(),
        wasm_bytes.len()
    );

    let ctx = TestContext {
        test_name: "bulk-memory-gas".to_string(),
        network_id: String::new(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    println!("Chain started.");


    let in_ctr = "/tmp/bulk_meter_guest.wasm";
    chain
        .primary_node()?
        .copy_file_from_host(&wasm, in_ctr)
        .await?;
    println!("  [0] copied guest into container {in_ctr}");

    // Node must accept rustc ≥ 1.87 wasm (bulk_memory capability).
    let store = tx(
        chain,
        &[
            "wasm",
            "store",
            in_ctr,
            "--from",
            "validator",
            "--gas",
            "auto",
            "--gas-adjustment",
            "1.4",
        ],
    )
    .await?;
    if store.code != Some(0) {
        return Err(format!(
            "wasm store failed (need BuiltInCapabilities bulk_memory): code={:?} log={}",
            store.code,
            tx_raw_log(&store.stdout) + &store.stderr
        )
        .into());
    }
    println!("  [1] wasm store accepted rustc≥1.87 guest");

    let codes = query_json(chain, &["wasm", "list-code"]).await?;
    let code_id = codes
        .pointer("/code_infos/0/code_id")
        .or_else(|| codes.pointer("/code_infos/0/id"))
        .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| v.as_u64().map(|n| n.to_string())))
        .ok_or("no code_id after store")?;
    println!("  [1] code_id={code_id}");

    let inst = tx(
        chain,
        &[
            "wasm",
            "instantiate",
            &code_id,
            "{}",
            "--label",
            "bulk-meter",
            "--no-admin",
            "--from",
            "validator",
            "--gas",
            "auto",
        ],
    )
    .await?;
    if inst.code != Some(0) {
        return Err(format!("instantiate failed: {:?}", inst.code).into());
    }
    let contracts = query_json(chain, &["wasm", "list-contract-by-code", &code_id]).await?;
    let addr = contracts
        .pointer("/contracts/0")
        .and_then(|v| v.as_str())
        .ok_or("no contract address")?
        .to_string();
    println!("  [2] instantiated {addr}");

    let small = tx(
        chain,
        &[
            "wasm",
            "execute",
            &addr,
            r#"{"copy":{"n":16}}"#,
            "--from",
            "validator",
            "--gas",
            "auto",
        ],
    )
    .await?;
    if small.code != Some(0) {
        return Err(format!(
            "small copy must succeed: code={:?} log={}",
            small.code,
            tx_raw_log(&small.stdout)
        )
        .into());
    }
    println!("  [3] copy n=16 succeeded (happy path)");

    let huge_n = env_or("ICT_BULK_COPY_N", "2147483647");
    // Hard cap: default TxOptions.gas is "auto" and would append after any
    // `--gas` in `args`, funding the copy. Meter must see a tight limit.
    let huge = tx_opts(
        chain,
        &[
            "wasm",
            "execute",
            &addr,
            &format!(r#"{{"copy":{{"n":{huge_n}}}}}"#),
            "--from",
            "validator",
        ],
        |o| o.gas("250000").gas_adjustment(1.0),
    )
    .await?;
    if huge.code == Some(0) {
        return Err("huge copy succeeded — meter did not stop i32::MAX memory.copy".into());
    }
    let log = tx_raw_log(&huge.stdout) + &huge.stderr;
    assert_gas_trap(&log)?;
    println!("  [4] copy n={huge_n} trapped on gas/vm (DoS meter) code={:?}", huge.code);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let tag = env_or("ICT_BULK_IMAGE", "local");
    println!(
        "=== bulk_memory_gas ({}:{} bin={}) ===\n",
        env_or("ICT_BULK_IMAGE_REPO", "terpnetwork/terp-core"),
        tag,
        env_or("ICT_BULK_BIN", "terpd")
    );

    let runtime = IctRuntime::Docker(DockerConfig::default())
        .into_backend()
        .await?;
    let mut chain = CosmosChain::new(chain_config(&tag), 1, 0, runtime);
    let result = run_test(&mut chain).await;
    let _ = chain.stop().await;
    match result {
        Ok(()) => {
            println!("bulk_memory_gas PASSED");
            Ok(())
        }
        Err(e) => {
            eprintln!("bulk_memory_gas FAILED: {e}");
            Err(e)
        }
    }
}
