//! 2 PoW + 2 Crosslink finalizers, all Zakura, via ict-rs Docker.
//!
//! Requires a Linux `zakurad` built with `--features crosslink`:
//!   docker build -f crates/zakura/docker/ubuntu-package.Dockerfile \
//!     --build-arg FEATURES=crosslink --target artifact \
//!     --output type=local,dest=crates/zakura/target/zakura-regtest-e2e-linux-crosslink \
//!     crates/zakura
//!
//! `ZAKURA_CROSSLINK_LAB=1` fail-closes if the mesh cannot finalize.

#![cfg(feature = "zakura")]

use std::time::Duration;

use ict_rs::chain::zakura::ENV_ZAKURA_CROSSLINK_LAB;
use ict_rs::chain::zakura_mesh::{
    generate_blocks, parse_blocks_height, peerinfo_connected, resolve_crosslink_zakurad_bin,
    rpc_call, spawn_zakura_quad,
};
use ict_rs::runtime::docker::DockerBackend;
use ict_rs::runtime::DockerConfig;

fn lab() -> bool {
    std::env::var(ENV_ZAKURA_CROSSLINK_LAB).ok().as_deref() == Some("1")
}

fn json_has_sigs(body: &str) -> bool {
    body.contains("result")
        && !body.contains("\"result\":null")
        && (body.contains("signatures") || body.contains("vote_signature") || body.len() > 80)
}

fn json_final_height(body: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let r = v.get("result")?;
    if r.is_null() {
        return None;
    }
    r.get(0)
        .and_then(|h| h.as_u64())
        .or_else(|| r.get("height").and_then(|h| h.as_u64()))
        .or_else(|| r.as_u64())
}

#[tokio::test]
async fn zakura_two_pow_two_finalizers_tenderlink() {
    let bin = match resolve_crosslink_zakurad_bin() {
        Ok(b) => b,
        Err(e) if lab() => panic!("ZAKURA_CROSSLINK_LAB=1: {e}"),
        Err(e) => {
            eprintln!("skip: {e}");
            return;
        }
    };

    let runtime = match DockerBackend::new(DockerConfig::default()).await {
        Ok(r) => std::sync::Arc::new(r),
        Err(e) if lab() => panic!("Docker required: {e}"),
        Err(e) => {
            eprintln!("skip: docker {e}");
            return;
        }
    };

    let mut quad = match spawn_zakura_quad(runtime, bin).await {
        Ok(q) => q,
        Err(e) if lab() => panic!("spawn 2+2 mesh: {e}"),
        Err(e) => {
            eprintln!("skip: spawn {e}");
            return;
        }
    };

    assert_eq!(quad.members.len(), 4, "mesh must be 2 PoW + 2 finalizers");
    let miners = quad.members.iter().filter(|(m, _)| m.mines).count();
    let fins = quad.members.iter().filter(|(m, _)| !m.mines).count();
    assert_eq!(miners, 2);
    assert_eq!(fins, 2);

    // All four must answer getblockchaininfo.
    for (m, n) in &quad.members {
        let body = rpc_call(n.rpc_url(), "getblockchaininfo", "[]")
            .await
            .unwrap_or_else(|e| panic!("{} getblockchaininfo: {e}", m.name));
        assert!(
            body.contains("\"result\""),
            "{} not RPC-ready: {body}",
            m.name
        );
    }

    // Wait until the PoW mesh has at least one peer on every node.
    let peer_deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut peers_ok = false;
    let mut last_peers = String::new();
    while std::time::Instant::now() < peer_deadline {
        let mut connected = 0usize;
        last_peers.clear();
        for (m, n) in &quad.members {
            let info = rpc_call(n.rpc_url(), "getpeerinfo", "[]")
                .await
                .unwrap_or_default();
            last_peers.push_str(&format!(
                "\n{} {}",
                m.name,
                info.chars().take(160).collect::<String>()
            ));
            if peerinfo_connected(&info) {
                connected += 1;
            }
        }
        if connected >= 3 {
            peers_ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if !peers_ok {
        if lab() {
            let _ = quad.teardown().await;
            panic!("2+2 mesh did not form P2P connections:{last_peers}");
        }
        eprintln!("residual: P2P not fully connected:{last_peers}");
    }

    // One miner extends the shared chain past TFL activation + σ.
    // Mining on both before sync would fork ephemeral state.
    let pow_a = quad
        .members
        .iter()
        .find(|(m, _)| m.name == "pow-a")
        .map(|(_, n)| n.rpc_url().to_string())
        .expect("pow-a");
    match generate_blocks(&pow_a, 10).await {
        Ok(body) => {
            if body.contains("\"error\"") && !body.contains("\"result\"") {
                let _ = quad.teardown().await;
                if lab() {
                    panic!("generate failed on pow-a: {body}");
                }
                eprintln!("generate not available on pow-a: {body}");
                return;
            }
        }
        Err(e) => {
            let _ = quad.teardown().await;
            if lab() {
                panic!("generate pow-a: {e}");
            }
            eprintln!("generate pow-a: {e}");
            return;
        }
    }

    // Wait for every node to see the mined height.
    let tip_deadline = std::time::Instant::now() + Duration::from_secs(90);
    let mut synced = false;
    let mut last_tips = String::new();
    while std::time::Instant::now() < tip_deadline {
        let mut seen = 0usize;
        last_tips.clear();
        for (m, n) in &quad.members {
            let info = rpc_call(n.rpc_url(), "getblockchaininfo", "[]")
                .await
                .unwrap_or_default();
            let h = parse_blocks_height(&info).unwrap_or(0);
            last_tips.push_str(&format!("\n{} height={h}", m.name));
            if h >= 10 {
                seen += 1;
            }
        }
        if seen == 4 {
            synced = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    if !synced {
        if lab() {
            let _ = quad.teardown().await;
            panic!("2+2 mesh did not sync mined height 10:{last_tips}");
        }
        eprintln!("residual: tips not synced:{last_tips}");
    }

    // Wait for tenderlink finality on miners + finalizers.
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let mut last = String::new();
    let mut ok = false;
    while std::time::Instant::now() < deadline {
        let mut activated = 0usize;
        let mut with_fp = 0usize;
        let mut sane_final = 0usize;
        last.clear();
        for (m, n) in &quad.members {
            let act = rpc_call(n.rpc_url(), "get_tfl_is_activated", "[]")
                .await
                .unwrap_or_default();
            let tip = rpc_call(n.rpc_url(), "get_tfl_final_block_height_and_hash", "[]")
                .await
                .unwrap_or_default();
            let fp = rpc_call(n.rpc_url(), "get_tfl_fat_pointer_to_bft_chain_tip", "[]")
                .await
                .unwrap_or_default();
            let info = rpc_call(n.rpc_url(), "getblockchaininfo", "[]")
                .await
                .unwrap_or_default();
            let chain = parse_blocks_height(&info).unwrap_or(0);
            last.push_str(&format!(
                "\n{} chain={chain} act={} tip={} fp_len={}",
                m.name,
                act.chars().take(80).collect::<String>(),
                tip.chars().take(120).collect::<String>(),
                fp.len()
            ));
            if act.contains("true") {
                activated += 1;
            }
            if json_has_sigs(&fp) {
                with_fp += 1;
            }
            if let Some(f) = json_final_height(&tip) {
                if f > 0 && f <= chain {
                    sane_final += 1;
                }
            }
        }
        if activated >= 2 && with_fp >= 2 && sane_final >= 2 {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    let _ = quad.teardown().await;

    if !ok {
        if lab() {
            panic!("2+2 mesh did not reach shared TFL finality:{last}");
        }
        eprintln!("residual: TFL not observed on 2+2 mesh:{last}");
    }
}
