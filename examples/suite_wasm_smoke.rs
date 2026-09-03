//! Spawn Terp (custom zk-wasmvm) via ict-rs and `wasm store` every curated product artifact.
//!
//! This is the pre-testnet gate: if store_code fails, the blob is not compatible
//! with this chain's wasmvm (or is missing). Instantiation is out of scope.
//!
//! ```sh
//! export PATH=/usr/local/bin:/Applications/Docker.app/Contents/Resources/bin:$PATH
//! cargo run -p ict-rs --example suite_wasm_smoke --features docker,testing,terp
//! # keep the chain: ICT_KEEP_CONTAINERS=1
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ict_rs::prelude::*;
use ict_rs::testing::env::TestEnv;

struct Item {
    family: &'static str,
    path: PathBuf,
}

fn crates_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn skip_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("-xion")
        || n.contains("-juno")
        || n.contains("-osmosis")
        || n.contains("-neutron")
        || n.contains("-archway")
        || n.contains("-kujira")
        || n.contains("thorchain")
        || n.contains("_bg.wasm")
        || n.contains("aarch64")
}

fn collect_dir(family: &'static str, dir: &Path, out: &mut Vec<Item>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        eprintln!("MISSING dir {family}: {}", dir.display());
        return;
    };
    let mut found = 0;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("wasm") {
            continue;
        }
        let name = p.file_name().unwrap().to_string_lossy();
        if skip_name(&name) {
            continue;
        }
        out.push(Item {
            family,
            path: p,
        });
        found += 1;
    }
    if found == 0 {
        eprintln!("EMPTY {family}: {}", dir.display());
    }
}

fn inventory(root: &Path) -> Vec<Item> {
    let mut items = Vec::new();
    let dirs: &[(&str, &str)] = &[
        ("dao", "dao-contracts/artifacts"),
        ("nft", "cw-nfts/artifacts"),
        ("cw-plus", "cw-plus/artifacts"),
        ("polytone", "polytone/artifacts"),
        ("abstract-fw", "abstract/framework/artifacts"),
        ("headstash", "headstash/artifacts"),
        ("terp-lc", "terp-rs/artifacts"),
        ("terp-lc2", "terp-rs/contracts/light-clients/cw-ics08-wasm-crosslink/artifacts"),
        ("hash-merchant", "terp-rs/contracts/hash-merchant/marketplace-mirror/artifacts"),
        ("loyalty", "terp-rs/contracts/hash-merchant/loyalty-verifier/artifacts"),
        ("auth-passkey", "terp-rs/contracts/smart-accounts/terp-passkey/artifacts"),
        ("auth-recovery", "terp-rs/contracts/smart-accounts/terp-recovery/artifacts"),
        ("auth-ed25519", "terp-rs/contracts/smart-accounts/terp-ed25519/artifacts"),
        ("auth-eth", "terp-rs/contracts/smart-accounts/terp-eth/artifacts"),
        ("auth-irl", "terp-rs/contracts/smart-accounts/terp-irl/artifacts"),
        ("auth-zkjwt", "terp-rs/contracts/smart-accounts/terp-zkjwt/artifacts"),
        ("auth-poseidon", "terp-rs/contracts/smart-accounts/terp-zkposiedon/artifacts"),
        ("auth-vsck", "terp-rs/contracts/smart-accounts/terp-vsck/artifacts"),
        // event-reg manifold (store_code only; cw-orch instantiate lives in event-orch)
        ("ceremony", "headstash/contracts/cw-vote-ceremony/artifacts"),
        ("reg-eligibility", "headstash/contracts/cw-reg-eligibility/artifacts"),
    ];
    for (fam, rel) in dirs {
        collect_dir(fam, &root.join(rel), &mut items);
    }
    items.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name()));
    items
}

fn extract_code_id(tx: &serde_json::Value) -> Option<u64> {
    let events = tx.get("events")?.as_array()?;
    for ev in events {
        if ev.get("type").and_then(|t| t.as_str()) != Some("store_code") {
            continue;
        }
        for attr in ev.get("attributes")?.as_array()? {
            if attr.get("key").and_then(|k| k.as_str()) == Some("code_id") {
                if let Some(s) = attr.get("value").and_then(|v| v.as_str()) {
                    return s.parse().ok();
                }
            }
        }
    }
    None
}

async fn store_code_wait(chain: &CosmosChain, container_wasm: &str) -> Result<u64, String> {
    let opts = chain.default_tx_opts().from("validator");
    let out = chain
        .chain_exec_tx_with(&["tx", "wasm", "store", container_wasm], opts)
        .await
        .map_err(|e| format!("broadcast: {e}"))?;
    let raw = out.stdout_str();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!(
            "empty store stdout stderr={}",
            out.stderr_str().trim()
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("tx json: {e} body={trimmed}"))?;
    if let Some(code) = v.get("code").and_then(|c| c.as_u64()) {
        if code != 0 {
            return Err(format!(
                "tx code={code} raw={}",
                v.get("raw_log").and_then(|x| x.as_str()).unwrap_or("")
            ));
        }
    }
    let hash = v
        .get("txhash")
        .and_then(|h| h.as_str())
        .ok_or_else(|| format!("no txhash in {trimmed}"))?;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let q = chain
            .chain_exec(&["query", "tx", hash, "--output", "json"])
            .await;
        let Ok(q) = q else { continue };
        let body = q.stdout_str();
        let Ok(tv) = serde_json::from_str::<serde_json::Value>(body.trim()) else {
            continue;
        };
        if let Some(id) = extract_code_id(&tv) {
            return Ok(id);
        }
    }
    Err(format!("timeout waiting for store tx {hash}"))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = crates_root();
    let items = inventory(&root);
    println!("=== suite_wasm_smoke ===");
    println!("crates root: {}", root.display());
    println!("artifacts: {}", items.len());
    for it in &items {
        println!(
            "  [{:<14}] {}",
            it.family,
            it.path.file_name().unwrap().to_string_lossy()
        );
    }

    if items.is_empty() {
        return Err("no wasm artifacts found".into());
    }

    println!("\n[1] DockerBackend + terp_config (zk-wasmvm)");
    let runtime: Arc<dyn RuntimeBackend> = Arc::new(DockerBackend::new(DockerConfig::default()).await?);
    let cfg = TestEnv::terp_config();
    let mut chain = CosmosChain::new(cfg, 1, 0, runtime);
    let ctx = TestContext {
        test_name: "suite-wasm-smoke".into(),
        network_id: "ict-suite-wasm-smoke".into(),
    };
    chain.initialize(&ctx).await?;
    chain.start(&[]).await?;
    let h = chain.height().await?;
    println!("    chain {} height {h}", chain.chain_id());

    println!("\n[2] store_code each artifact (key=validator)");
    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut missing_auth = 0usize;
    for fam in [
        "auth-passkey",
        "auth-recovery",
        "auth-ed25519",
        "auth-eth",
        "auth-irl",
        "auth-zkjwt",
        "auth-poseidon",
        "auth-vsck",
        "ceremony",
        "reg-eligibility",
    ] {
        if !items.iter().any(|i| i.family == fam) {
            eprintln!("  MISS  [{fam}] no artifacts/ — compile wasm before testnet");
            missing_auth += 1;
        }
    }

    for it in &items {
        let name = it.path.file_name().unwrap().to_string_lossy();
        let dest = format!("/tmp/{name}");
        let label = format!("[{:<14}] {name}", it.family);
        match chain
            .primary_node()?
            .copy_file_from_host(&it.path, &dest)
            .await
        {
            Ok(()) => {}
            Err(e) => {
                eprintln!("  COPY  {label}  ERR {e}");
                fail += 1;
                continue;
            }
        }
        match store_code_wait(&chain, &dest).await {
            Ok(code_id) => {
                println!("  OK    {label}  code_id={code_id}");
                ok += 1;
            }
            Err(e) => {
                eprintln!("  FAIL  {label}  {e}");
                fail += 1;
            }
        }
    }

    println!("\n=== result store_ok={ok} store_fail={fail} auth_missing={missing_auth} ===");
    if std::env::var("ICT_KEEP_CONTAINERS").ok().as_deref() == Some("1") {
        println!("ICT_KEEP_CONTAINERS=1 — leaving Terp node up");
    } else {
        println!("stopping chain…");
        let _ = chain.stop().await;
    }
    if fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}
