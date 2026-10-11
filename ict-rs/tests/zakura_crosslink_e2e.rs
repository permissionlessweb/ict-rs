//! Zakura × Crosslink ict-rs e2e (SSOT: `ict-rs/docs/ZAKURA_CROSSLINK_LOOP.md`).
//!
//! CX0 Pass: fork at `crates/zakura` with `ORIGIN.md` (upstream URL + tag/commit).
//! Stock `zakurad` still builds. ict-rs spawn of that binary still RPC-ready.
//! Docs do **not** claim Crosslink.
//!
//! CX4+ names (`zakura_crosslink_reports_finality`,
//! `zakura_crosslink_rejects_bad_fat_pointer`) live in later cards and must
//! only be added when groot-wan quotes `fork_present` / `lab_ok`. This file
//! is **not** 08-wasm / IBC client-update proof (CX7 is terp-rs).
//! `ZAKURA_CROSSLINK_LAB=1` is fail-closed for live Docker (missing bin / Mach-O).

#![cfg(feature = "zakura")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use ict_rs::chain::zakura::{
    corridor_node_toml, spawn_zakura_local, zakurad_is_linux_elf, ZakuraNodeConfig,
    ENV_ZAKURAD_BIN, ENV_ZAKURA_CROSSLINK_LAB,
};
use ict_rs::runtime::docker::DockerBackend;
use ict_rs::runtime::DockerConfig;

fn crates_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("ict-rs crate lives at crates/ict-rs/ict-rs")
        .to_path_buf()
}

fn zakura_fork_root() -> PathBuf {
    crates_root().join("zakura")
}

fn lab_fail_closed() -> bool {
    std::env::var(ENV_ZAKURA_CROSSLINK_LAB)
        .ok()
        .as_deref()
        == Some("1")
}

fn origin_md_path() -> PathBuf {
    zakura_fork_root().join("ORIGIN.md")
}

/// `- tag / commit: <pin>` from the CX0 ORIGIN.md template.
fn origin_tag_or_commit(origin: &str) -> Option<String> {
    for line in origin.lines() {
        let line = line.trim().trim_start_matches('-').trim();
        let Some(rest) = line
            .strip_prefix("tag / commit:")
            .or_else(|| line.strip_prefix("tag/commit:"))
        else {
            continue;
        };
        let pin = rest.trim();
        if !pin.is_empty() {
            return Some(pin.to_string());
        }
    }
    None
}

fn pin_looks_real(pin: &str) -> bool {
    let pin = pin.trim();
    if pin.is_empty() || pin.contains('<') || pin.contains("fill") {
        return false;
    }
    if pin.starts_with('v') {
        // prefer v1.2.0+
        return pin.len() >= 2;
    }
    let hex: String = pin.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    hex.len() >= 7
}

fn origin_has_upstream(origin: &str) -> bool {
    origin.contains("https://github.com/zakura-core/zakura")
}

fn forbidden_crosslink_claims(text: &str) -> Vec<&'static str> {
    const FORBIDDEN: &[&str] = &[
        "mainnet Crosslink",
        "mainnet crosslink",
        "IBC Crosslink",
        "IBC crosslink",
        "Orchard-note finality",
        "ZIP-complete",
        "crosslink-finalized heights",
        "this fork ships Crosslink",
        "this fork ships crosslink",
        "reports crosslink-finalized",
    ];
    FORBIDDEN
        .iter()
        .copied()
        .filter(|n| text.contains(n))
        .collect()
}

fn cargo_metadata_has_zakurad(manifest: &Path) -> Result<(), String> {
    let out = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(manifest)
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("parse cargo metadata: {e}"))?;
    let packages = meta
        .get("packages")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "cargo metadata missing packages".to_string())?;
    let has = packages.iter().any(|pkg| {
        pkg.get("targets")
            .and_then(|t| t.as_array())
            .map(|targets| {
                targets.iter().any(|t| {
                    t.get("name").and_then(|n| n.as_str()) == Some("zakurad")
                        && t.get("kind")
                            .and_then(|k| k.as_array())
                            .map(|ks| ks.iter().any(|x| x.as_str() == Some("bin")))
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });
    if has {
        Ok(())
    } else {
        Err("cargo metadata has no bin target named zakurad".into())
    }
}

/// CX0: `crates/zakura/ORIGIN.md` pins zakura-core/zakura + tag/commit (not a placeholder).
#[test]
fn cx0_origin_md_pins_upstream() {
    let path = origin_md_path();
    assert!(
        path.is_file(),
        "CX0: missing {}; clone https://github.com/zakura-core/zakura into crates/zakura \
         and write ORIGIN.md (upstream URL + tag/commit, prefer v1.2.0+)",
        path.display()
    );
    let text = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        origin_has_upstream(&text),
        "CX0: ORIGIN.md must pin upstream https://github.com/zakura-core/zakura\n---\n{text}"
    );
    let pin = origin_tag_or_commit(&text).unwrap_or_default();
    assert!(
        pin_looks_real(&pin),
        "CX0: ORIGIN.md tag/commit must be a real pin (not `<fill>`), got {pin:?}\n---\n{text}"
    );
}

/// CX0: stock fork tree is a Cargo workspace/package that still exposes `zakurad`.
#[test]
fn cx0_stock_zakurad_still_builds() {
    let root = zakura_fork_root();
    let manifest = root.join("Cargo.toml");
    assert!(
        manifest.is_file(),
        "CX0: missing {}; fork zakura-core/zakura here so stock zakurad still builds",
        manifest.display()
    );
    cargo_metadata_has_zakurad(&manifest)
        .unwrap_or_else(|e| panic!("CX0: stock zakurad manifest check failed: {e}"));
}

/// CX0: ORIGIN + fork README + stock corridor must not claim shipped Crosslink.
#[test]
fn cx0_docs_do_not_claim_crosslink() {
    let origin_path = origin_md_path();
    assert!(
        origin_path.is_file(),
        "CX0: missing {}; cannot assert honesty without ORIGIN.md",
        origin_path.display()
    );
    let origin = fs::read_to_string(&origin_path).unwrap_or_default();
    assert!(
        origin.contains("stock Zakura until CX1+"),
        "CX0: ORIGIN.md status must be stock Zakura until CX1+ (do not claim Crosslink)\n---\n{origin}"
    );
    let mut scanned = origin.clone();
    let readme = zakura_fork_root().join("README.md");
    if readme.is_file() {
        scanned.push_str(&fs::read_to_string(&readme).unwrap_or_default());
    }
    let hits = forbidden_crosslink_claims(&scanned);
    assert!(
        hits.is_empty(),
        "CX0: fork docs must not claim shipped Crosslink; found {hits:?}"
    );

    let toml = corridor_node_toml();
    assert!(
        !toml.contains("[crosslink]"),
        "CX0: stock corridor TOML must stay valid for PIR (no [crosslink] section)"
    );
}

/// Spawn stock `zakurad` and require JSON-RPC `getblockchaininfo` `"result"`.
///
/// Skip is allowed on CX0 when Docker/`ZAKURAD_BIN` is missing.
/// `ZAKURA_CROSSLINK_LAB=1` fail-closes. Never bind-mount Mach-O.
#[tokio::test]
async fn zakura_crosslink_rpc_ready() {
    let lab = lab_fail_closed();
    let toml = corridor_node_toml();
    assert!(
        !toml.contains("[crosslink]"),
        "stock spawn uses corridor_node_toml(); [crosslink] is CX4+"
    );

    let bin = match std::env::var(ENV_ZAKURAD_BIN) {
        Ok(p) if Path::new(p.trim()).is_file() => PathBuf::from(p.trim()),
        other => {
            if lab {
                panic!(
                    "{ENV_ZAKURA_CROSSLINK_LAB}=1 but {ENV_ZAKURAD_BIN} is missing or not a file ({other:?})"
                );
            }
            eprintln!("skip: CX0 spawn needs {ENV_ZAKURAD_BIN} (Linux zakurad)");
            return;
        }
    };

    assert!(
        zakurad_is_linux_elf(&bin),
        "never bind-mount a Mach-O zakurad into debian:trixie-slim: {} \
         (build/use an aarch64/x86_64 Linux binary; groot cache or cross)",
        bin.display()
    );

    let docker = match DockerBackend::new(DockerConfig::default()).await {
        Ok(d) => d,
        Err(e) if lab => panic!("{ENV_ZAKURA_CROSSLINK_LAB}=1 but Docker is unreachable: {e}"),
        Err(e) => {
            eprintln!("skip: Docker not reachable ({e})");
            return;
        }
    };

    let mut cfg = ZakuraNodeConfig::corridor(&bin);
    cfg.test_name = "cx0-stock-rpc".into();
    cfg.reuse_if_ready = false;
    cfg.ready_timeout_secs = 90;
    // PIR corridor (`pir-zakura-local`) binds host 18232. Spawn a fresh
    // stock node on a distinct host port; container RPC stays 18232.
    cfg.rpc_host_port = 18242;

    let mut node = spawn_zakura_local(Arc::new(docker), cfg)
        .await
        .unwrap_or_else(|e| panic!("CX0: spawn stock zakurad must be RPC-ready: {e}"));
    let ready = node.rpc_ready().await;
    let _ = node.teardown().await;
    assert!(
        ready,
        "CX0: getblockchaininfo must contain JSON-RPC \"result\" at {}",
        node.rpc_url()
    );
}
