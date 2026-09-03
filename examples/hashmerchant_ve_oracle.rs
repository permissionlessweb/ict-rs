//! Elevate `hashmerchant.rs` to 2 validators × 2 hash-market servers.
//!
//! Neutron-class check: the two sidecars hold the **same** oracle attestations
//! in **opposite** JSON order. Sidecar aggregate roots must match. After VE
//! enable height, `terpd query hashmerchant root` must return that same root
//! (fail-closed — empty / missing root is a failure) and the chain must keep
//! producing blocks (no AppHash halt).
//!
//! Same waist as `examples/hashmerchant.rs`: host `hash-market-server` process,
//! Terp Docker, `GET /vote-extension`, genesis VE height 2, CLI root query.
//! Anvil is omitted — this case is attestation-order determinism, not ERC20.
//!
//! ```sh
//! cd tools/hash-market && cargo build
//! cargo run --example hashmerchant_ve_oracle --features docker,hashmerchant
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use ict_rs::chain::cosmos::CosmosChain;
use ict_rs::chain::{Chain, ChainType, SigningAlgorithm, TestContext};
use ict_rs::runtime::docker::DockerBackend;
use ict_rs::runtime::{DockerConfig, DockerImage, RuntimeBackend};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn find_server_binary() -> PathBuf {
    if let Ok(p) = std::env::var("HASHMERCHANT_SERVER") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb.canonicalize().unwrap();
        }
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let cwd = std::env::current_dir().unwrap();
    let candidates = [
        manifest.join("../../terp-rs/tools/hash-market/target/debug/hash-market-server"),
        manifest.join("../../../terp-rs/tools/hash-market/target/debug/hash-market-server"),
        cwd.join("../terp-rs/tools/hash-market/target/debug/hash-market-server"),
        cwd.join("crates/terp-rs/tools/hash-market/target/debug/hash-market-server"),
        cwd.join("tools/hash-market/target/debug/hash-market-server"),
    ];
    for c in candidates {
        if c.exists() {
            return c.canonicalize().unwrap();
        }
    }
    panic!(
        "hash-market-server not found. Build it first:\n  \
         cd crates/terp-rs/tools/hash-market && cargo build --features server,ve"
    );
}

fn keep_containers() -> bool {
    std::env::var("ICT_KEEP_CONTAINERS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Custody scope + ed25519 keys (seed 0x11 / 0x22). Must match genesis oracle_sources.
const CUSTODY_SCOPE: &str = "ict-ve";
const PUB_ALPHA_B64: &str = "0EqyMnQrtKs6E2i9RhXk5tAiSrcaAWuvhSCjMsl3hzc=";
const PUB_ZETA_B64: &str = "oJql9HpnWYAv+VX43C0qFKXJnSO+l/hkEn/5ODRVpPA=";
const SIG_ALPHA: &str = "be4816fe38e46c56584f44b8fa6fcd8d7b8ba4584f4fc6aeb1c147d2d8c9408c9c9e21130e6bb7207357540f4affa96244597aa532623f408ab99bb101dbb106";
const SIG_ZETA: &str = "91b99b9346c1432696242f00ff8a8f7d33fdcfc8b21f1c4c217ad0528a81ba34c9492e414b145acabd0033d83594f9c77fde5b381134fa4c977b4e30d5d75800";
const GOLDEN_ROOT_HEX: &str = "a2e9f599d262ed589375679f66d65a5a127b0c10492b54ecf396ce4f63edabd2";

fn fixture_attestations() -> serde_json::Value {
    // Go hex-decodes `value` then hashes those bytes. Sigs over custodyPayload.
    serde_json::json!([
        {
            "source_id": "src-zeta",
            "value": "313030",
            "height": 42,
            "timestamp": 1700000002,
            "custody_signature": SIG_ZETA
        },
        {
            "source_id": "src-alpha",
            "value": "3939",
            "height": 41,
            "timestamp": 1700000001,
            "custody_signature": SIG_ALPHA
        }
    ])
}

fn patch_val_sidecar_url(val_index: usize, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let list = std::process::Command::new("docker")
        .args(["ps", "--format", "{{.Names}}"])
        .output()?;
    if !list.status.success() {
        return Err("docker ps failed".into());
    }
    let needle = format!("-val-{val_index}");
    let names = String::from_utf8_lossy(&list.stdout);
    let name = names
        .lines()
        .find(|n| n.contains(&needle))
        .ok_or_else(|| format!("container *{needle} not found"))?;
    println!("    {name} sidecar-url = {url}");
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
"#,
        url = url
    );
    let out = std::process::Command::new("docker")
        .args(["exec", name, "sh", "-c", &script])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "patch {name}: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(())
}

fn docker_exec_wget(val_index: usize, url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let list = std::process::Command::new("docker")
        .args(["ps", "--format", "{{.Names}}"])
        .output()?;
    let needle = format!("-val-{val_index}");
    let names = String::from_utf8_lossy(&list.stdout);
    let name = names
        .lines()
        .find(|n| n.contains(&needle))
        .ok_or_else(|| format!("container *{needle} not found"))?;
    let cmd = format!("wget -qO- {url} || curl -sf {url} || echo UNREACHABLE");
    let out = std::process::Command::new("docker")
        .args(["exec", name, "sh", "-c", &cmd])
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn http_get(addr: &str, path: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut stream = TcpStream::connect(addr).await?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let response = String::from_utf8_lossy(&response).to_string();
    let body_start = response.find("\r\n\r\n").unwrap_or(0) + 4;
    Ok(response[body_start..].to_string())
}

async fn wait_healthy(addr: &str) -> Result<String, Box<dyn std::error::Error>> {
    let mut last = String::new();
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        match http_get(addr, "/health").await {
            Ok(body) => return Ok(body),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!("sidecar {addr} never healthy: {last}").into())
}

fn write_sidecar_config(dir: &std::path::Path, port: u16) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join(format!("hm-{port}.toml"));
    std::fs::write(
        &path,
        format!(
            r#"bind = "127.0.0.1:{port}"
chain_id = "terp-ve-pair-1"
signing_key = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
ve_enabled = true
data_dir = "{data}"

[[providers]]
name = "lab-static"
chain_uid = "oracle-eth"
algo = "sha256"
mode = "static"
address = "lab"
static_root = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
static_height = 42
"#,
            data = dir.join(format!("data-{port}")).display()
        ),
    )?;
    Ok(path)
}

fn spawn_sidecar(
    bin: &std::path::Path,
    config: &std::path::Path,
    reverse: bool,
) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(bin)
        .args(["-c", config.to_str().unwrap()])
        .env("HASHMERCHANT_ONBOARD", "0")
        .env("HASHMERCHANT_ATTESTATIONS", fixture_attestations().to_string())
        .env(
            "HASHMERCHANT_ATTESTATION_REVERSE",
            if reverse { "1" } else { "0" },
        )
        .env("HASHMERCHANT_CHAIN_UID", "oracle-eth")
        .env("HASHMERCHANT_ALGO", "sha256")
        .env("HASHMERCHANT_RUNTIME_ID", "ict-ve-pair")
        .env("RUST_LOG", "info")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

fn attestation_ids(body: &serde_json::Value) -> Vec<String> {
    body.get("present_attestations")
        .or_else(|| body.get("attestations"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| {
                    x.get("source_id")
                        .and_then(|s| s.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn json_array_len(body: &serde_json::Value, key: &str) -> usize {
    body.get(key)
        .and_then(|a| a.as_array())
        .map(|a| a.len())
        .unwrap_or(0)
}

fn decode_root_bytes(s: &str) -> Option<Vec<u8>> {
    let t = s.trim().trim_start_matches("0x");
    if t.is_empty() {
        return None;
    }
    if let Ok(b) = hex::decode(t) {
        if !b.is_empty() && t.len() % 2 == 0 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(b);
        }
    }
    // Comet/gogo proto bytes render as standard base64 on CLI.
    b64_decode(t)
}

fn b64_decode(input: &str) -> Option<Vec<u8>> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [0xffu8; 256];
    for (i, &c) in T.iter().enumerate() {
        table[c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut n = 0;
    for &c in input.as_bytes() {
        if c == b'=' {
            break;
        }
        let v = table[c as usize];
        if v == 0xff {
            return None;
        }
        buf = (buf << 6) | u32::from(v);
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((buf >> n) as u8);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn extract_on_chain_root(root_json: &serde_json::Value) -> Option<String> {
    let root_data = if root_json.get("root").is_some() && root_json["root"].is_object() {
        &root_json["root"]
    } else {
        root_json
    };
    root_data
        .get("root")
        .and_then(|r| r.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

async fn cleanup(servers: &mut [Option<tokio::process::Child>], terp: &mut Option<CosmosChain>) {
    if keep_containers() {
        println!("\n[cleanup] ICT_KEEP_CONTAINERS=1 — leaving Terp containers");
        for proc in servers.iter_mut().flatten() {
            proc.kill().await.ok();
            proc.wait().await.ok();
        }
        return;
    }
    println!("\n[cleanup] Stopping resources...");
    for proc in servers.iter_mut().flatten() {
        proc.kill().await.ok();
        proc.wait().await.ok();
    }
    if let Some(ref mut t) = terp {
        if let Err(e) = t.stop().await {
            eprintln!("    Terp stop error (non-fatal): {e}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    println!("=== Hashmerchant VE oracle: 2 validators × 2 hash-market servers ===\n");

    let docker = match DockerBackend::new(DockerConfig::default()).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ERROR: Docker daemon: {e}");
            std::process::exit(1);
        }
    };
    let runtime: Arc<dyn RuntimeBackend> = Arc::new(docker);

    let mut servers: [Option<tokio::process::Child>; 2] = [None, None];
    let mut terp_handle: Option<CosmosChain> = None;
    let result = run_test(runtime, &mut servers, &mut terp_handle).await;
    cleanup(&mut servers, &mut terp_handle).await;
    result
}

async fn run_test(
    runtime: Arc<dyn RuntimeBackend>,
    servers: &mut [Option<tokio::process::Child>],
    terp_handle: &mut Option<CosmosChain>,
) -> Result<(), Box<dyn std::error::Error>> {
    let bin = find_server_binary();
    println!("[1] hash-market-server: {}", bin.display());

    // Avoid 19090/19091 — a local terpd on groot already binds those.
    let port_a = 29190u16;
    let port_b = 29191u16;
    let tmp = tempfile::tempdir()?;
    let cfg_a = write_sidecar_config(tmp.path(), port_a)?;
    let cfg_b = write_sidecar_config(tmp.path(), port_b)?;
    // keep tmp dir alive for process lifetime
    std::mem::forget(tmp);
    servers[0] = Some(spawn_sidecar(&bin, &cfg_a, false)?);
    servers[1] = Some(spawn_sidecar(&bin, &cfg_b, true)?);

    let addr_a = format!("127.0.0.1:{port_a}");
    let addr_b = format!("127.0.0.1:{port_b}");
    println!("    A health: {}", wait_healthy(&addr_a).await?);
    println!("    B health: {}", wait_healthy(&addr_b).await?);

    println!("\n[2] Opposite attestation order, one sidecar root");
    let ve_a: serde_json::Value = serde_json::from_str(&http_get(&addr_a, "/vote-extension").await?)?;
    let ve_b: serde_json::Value = serde_json::from_str(&http_get(&addr_b, "/vote-extension").await?)?;
    let ids_a = attestation_ids(&ve_a);
    let ids_b = attestation_ids(&ve_b);
    println!("    A order={ids_a:?} root={}", ve_a["root"]);
    println!("    B order={ids_b:?} root={}", ve_b["root"]);

    if json_array_len(&ve_a, "attestations") < 2 || json_array_len(&ve_b, "attestations") < 2 {
        return Err(
            "GET /vote-extension must put the reversed set on attestations[] (the field Go unmarshals)"
                .into(),
        );
    }
    if ids_a.len() < 2 || ids_b.len() < 2 {
        return Err("each sidecar must emit ≥2 attestations".into());
    }
    if ve_a["root"].as_str() != Some(GOLDEN_ROOT_HEX) {
        return Err(format!(
            "sidecar root {} != golden {GOLDEN_ROOT_HEX}",
            ve_a["root"]
        )
        .into());
    }
    let mut rev = ids_a.clone();
    rev.reverse();
    if ids_b != rev {
        return Err(format!("expected reversed present-order, A={ids_a:?} B={ids_b:?}").into());
    }
    let root_a = ve_a["root"].as_str().unwrap_or("");
    let root_b = ve_b["root"].as_str().unwrap_or("");
    if root_a.is_empty() || root_a != root_b {
        return Err(format!("sidecar roots diverged: A={root_a} B={root_b}").into());
    }
    if ve_a["present_reversed"] == ve_b["present_reversed"] {
        return Err("both sidecars used the same present_reversed flag".into());
    }
    let sidecar_root = root_a.to_string();
    println!("    sidecar aggregate root = {sidecar_root}");

    let chain_uid = "oracle-eth";
    let algo = "sha256";
    let terp_chain_id = "terp-ve-pair-1";
    let sidecar_url_a = format!("http://host.docker.internal:{port_a}");
    let sidecar_url_b = format!("http://host.docker.internal:{port_b}");
    if sidecar_url_a == sidecar_url_b {
        return Err("val sidecar URLs must differ".into());
    }

    println!("\n[3] Terp chain: 2 validators, VE enable height 2");
    let terp_config = ict_rs::chain::ChainConfig {
        chain_type: ChainType::Cosmos,
        name: "terp".into(),
        chain_id: terp_chain_id.into(),
        images: vec![DockerImage {
            repository: std::env::var("ICT_UPGRADE_REPO")
                .unwrap_or_else(|_| "terpnetwork/terp-core".into()),
            version: std::env::var("ICT_LEAN_IMAGE")
                .or_else(|_| std::env::var("ICT_UPGRADE_TO"))
                .unwrap_or_else(|_| "local-zk".into()),
            uid_gid: None,
        }],
        bin: "terpd".into(),
        bech32_prefix: "terp".into(),
        denom: "uterp".into(),
        coin_type: 118,
        signing_algorithm: SigningAlgorithm::Secp256k1,
        gas_prices: "0.025uterp".into(),
        gas_adjustment: 1.5,
        trusting_period: "336h".into(),
        block_time: "1s".into(),
        genesis: None,
        // After app.toml overrides (same blob on every home). Do not set
        // HASHMERCHANT_SIDECAR_URL — env overrides toml and is process-wide.
        pre_genesis: Some({
            let a = sidecar_url_a.clone();
            let b = sidecar_url_b.clone();
            Box::new(move |_chain| {
                patch_val_sidecar_url(0, &a)
                    .and_then(|_| patch_val_sidecar_url(1, &b))
                    .map_err(|e| ict_rs::error::IctError::Config(e.to_string()))
            })
        }),
        additional_start_args: Vec::new(),
        sidecar_configs: Vec::new(),
        faucet: None,
        genesis_style: Default::default(),
        env: vec![],
        config_file_overrides: {
            let mut m = std::collections::HashMap::new();
            m.insert(
                "config/app.toml".into(),
                serde_json::json!({
                    "hashmerchant": {
                        "sidecar-url": sidecar_url_a,
                        "sidecar-timeout": "2s"
                    }
                }),
            );
            m
        },
        modify_genesis: Some(Box::new(move |_cfg, genesis_bytes| {
            let mut genesis: serde_json::Value = serde_json::from_slice(&genesis_bytes)
                .map_err(|e| ict_rs::error::IctError::Config(e.to_string()))?;
            genesis["consensus"]["params"]["abci"]["vote_extensions_enable_height"] =
                serde_json::json!("2");
            genesis["app_state"]["hashmerchant"]["registered_chains"] = serde_json::json!([
                {
                    "chain_uid": "oracle-eth",
                    "name": "Oracle attestation pair",
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
                                "pubkey": PUB_ALPHA_B64,
                                "algorithm": "ed25519",
                                "scope": CUSTODY_SCOPE
                            }
                        },
                        {
                            "source_id": "src-zeta",
                            "kind": 4,
                            "endpoint": "lab",
                            "enabled": true,
                            "authenticator": {
                                "pubkey": PUB_ZETA_B64,
                                "algorithm": "ed25519",
                                "scope": CUSTODY_SCOPE
                            }
                        }
                    ]
                }
            ]);
            genesis["app_state"]["hashmerchant"]["params"]["quorum_fraction"] =
                serde_json::json!("0.667000000000000000");
            Ok(serde_json::to_vec_pretty(&genesis)
                .map_err(|e| ict_rs::error::IctError::Config(e.to_string()))?)
        })),
    };

    let ctx = TestContext {
        test_name: "hm-ve-oracle".into(),
        network_id: "ict-hm-ve-oracle".into(),
    };
    {
        let terp = CosmosChain::new(terp_config, 2, 0, runtime);
        *terp_handle = Some(terp);
    }
    let terp = terp_handle.as_mut().unwrap();
    terp.initialize(&ctx).await?;
    terp.start(&[]).await?;

    let probe_a = docker_exec_wget(0, &format!("{sidecar_url_a}/health"))?;
    let probe_b = docker_exec_wget(1, &format!("{sidecar_url_b}/health"))?;
    println!("    val-0 → A: {probe_a}");
    println!("    val-1 → B: {probe_b}");
    if probe_a.contains("UNREACHABLE") || probe_b.contains("UNREACHABLE") {
        return Err("validator could not reach its sidecar".into());
    }
    let ve_from_0 = docker_exec_wget(0, &format!("{sidecar_url_a}/vote-extension"))?;
    let ve_from_1 = docker_exec_wget(1, &format!("{sidecar_url_b}/vote-extension"))?;
    let j0: serde_json::Value = serde_json::from_str(&ve_from_0)?;
    let j1: serde_json::Value = serde_json::from_str(&ve_from_1)?;
    let o0 = attestation_ids(&j0);
    let o1 = attestation_ids(&j1);
    println!("    val-0 fetch order={o0:?}");
    println!("    val-1 fetch order={o1:?}");
    let mut rev0 = o0.clone();
    rev0.reverse();
    if o1 != rev0 {
        return Err(format!(
            "validators did not fetch opposite present-order: val0={o0:?} val1={o1:?}"
        )
        .into());
    }

    let genesis = terp.read_genesis().await?;
    let ve_height = genesis["consensus"]["params"]["abci"]["vote_extensions_enable_height"]
        .as_str()
        .unwrap_or("0");
    assert_eq!(ve_height, "2", "vote extensions must enable at height 2");
    let registered = genesis["app_state"]["hashmerchant"]["registered_chains"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let has_oracle = registered.iter().any(|c| {
        c.get("chain_uid").and_then(|s| s.as_str()) == Some(chain_uid)
            && c.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false)
    });
    if !has_oracle {
        return Err(format!(
            "genesis missing enabled registered chain {chain_uid}: {registered:?}"
        )
        .into());
    }
    println!("    vote_extensions_enable_height={ve_height} validators=2 registered={chain_uid}");

    println!("\n[4] Wait for VE apply (height ≥ 8)");
    let target = 8u64;
    let mut current = terp.height().await?;
    for _ in 0..60 {
        if current >= target {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        current = terp.height().await?;
    }
    if current < target {
        return Err(format!("chain stalled at {current}, need {target}").into());
    }
    println!("    height={current}");

    println!("\n[5] On-chain HashRoot (fail-closed)");
    let mut raw = String::new();
    let mut last_err = String::new();
    for attempt in 0..8 {
        let query_out = terp
            .exec(
                &[
                    "terpd",
                    "query",
                    "hashmerchant",
                    "root",
                    chain_uid,
                    algo,
                    "--output",
                    "json",
                    "--home",
                    terp.home_dir(),
                ],
                &[],
            )
            .await?;
        raw = query_out.stdout_str().trim().to_string();
        if raw.is_empty() {
            last_err = query_out.stderr_str().trim().to_string();
            println!("    query attempt {attempt}: empty stdout ({last_err})");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        break;
    }
    println!("    query: {raw}");
    if raw.is_empty() {
        return Err(format!("hashmerchant root query empty: {last_err}").into());
    }
    let root_json: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|_| format!("hashmerchant root query not JSON: {raw}"))?;
    let on_chain = extract_on_chain_root(&root_json)
        .ok_or_else(|| format!("no HashRoot on-chain (empty / missing): {raw}"))?;

    // Sidecar emits hex; chain may store hex or base64. Compare after normalize.
    let matches = decode_root_bytes(&on_chain)
        .zip(decode_root_bytes(&sidecar_root))
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if !matches {
        return Err(format!("on-chain root {on_chain} != sidecar {sidecar_root}").into());
    }
    println!("    ON-CHAIN ROOT MATCHES SIDECAR ✓");

    let h0 = terp.height().await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let h1 = terp.height().await?;
    if h1 < h0 {
        return Err("height went backwards — likely AppHash halt".into());
    }
    println!("    still advancing {h0} → {h1}");

    println!("\n=== PASS: reversed sidecar attestations, one root, no fork ===");
    Ok(())
}
