//! Four-node Zakura mesh: 2 PoW miners + 2 Crosslink/tenderlink finalizers.
//!
//! All four run `zakurad --features crosslink` in Docker on one ict-rs network.

use std::path::PathBuf;
use std::sync::Arc;

use tracing::{info, warn};

use crate::error::{IctError, Result};
use crate::runtime::{NetworkId, RuntimeBackend};

use super::zakura::{
    spawn_zakura_local, wait_rpc_ready, zakurad_is_linux_elf, ZakuraNode, ZakuraNodeConfig,
    ENV_ZAKURAD_BIN, REGTEST_MINER_DEST, ZAKURA_P2P_PORT, ZAKURA_RPC_PORT,
};

/// Tenderlink UDP port inside each container.
pub const ZAKURA_TFL_PORT: u16 = 24834;

/// One member of the 2+2 mesh.
#[derive(Debug, Clone)]
pub struct MeshMember {
    /// Docker hostname / tenderlink identity seed.
    pub name: &'static str,
    /// Host RPC publish port.
    pub rpc_host_port: u16,
    /// This node is asked to `generate` blocks.
    pub mines: bool,
}

impl MeshMember {
    /// Tenderlink / roster identity. Must match `malachite_peers` entries.
    pub fn identity(&self) -> String {
        format!("{}:{ZAKURA_TFL_PORT}", self.name)
    }

    pub fn container_name(&self) -> String {
        format!("ict-{}-zakura-{}", self.name, self.rpc_host_port)
    }
}

/// Default topology: two miners, two finalizer-only nodes. All four vote.
pub fn default_quad_members() -> [MeshMember; 4] {
    [
        MeshMember {
            name: "pow-a",
            rpc_host_port: 18252,
            mines: true,
        },
        MeshMember {
            name: "pow-b",
            rpc_host_port: 18253,
            mines: true,
        },
        MeshMember {
            name: "fin-a",
            rpc_host_port: 18254,
            mines: false,
        },
        MeshMember {
            name: "fin-b",
            rpc_host_port: 18255,
            mines: false,
        },
    ]
}

/// Running 2 PoW + 2 finalizer mesh.
pub struct ZakuraQuad {
    pub members: Vec<(MeshMember, ZakuraNode)>,
    pub network_id: NetworkId,
    runtime: Arc<dyn RuntimeBackend>,
}

impl ZakuraQuad {
    pub fn pow_nodes(&self) -> impl Iterator<Item = &ZakuraNode> {
        self.members.iter().filter(|(m, _)| m.mines).map(|(_, n)| n)
    }

    pub fn finalizers(&self) -> impl Iterator<Item = &ZakuraNode> {
        self.members.iter().map(|(_, n)| n)
    }

    pub async fn teardown(&mut self) -> Result<()> {
        for (_, node) in &mut self.members {
            let _ = node.teardown().await;
        }
        let _ = self.runtime.remove_network(&self.network_id).await;
        Ok(())
    }
}

/// Per-node corridor TOML: shared Regtest + P2P mesh + tenderlink roster.
pub fn corridor_mesh_toml(me: &MeshMember, all: &[MeshMember]) -> String {
    let p2p_peers: Vec<String> = all
        .iter()
        .filter(|o| o.name != me.name)
        .map(|o| format!("{}:{ZAKURA_P2P_PORT}", o.name))
        .collect();
    let tfl_peers: Vec<String> = all
        .iter()
        .filter(|o| o.name != me.name)
        .map(|o| o.identity())
        .collect();
    let p2p_list = p2p_peers
        .iter()
        .map(|p| format!("    \"{p}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    let tfl_list = tfl_peers
        .iter()
        .map(|p| format!("    \"{p}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    let identity = me.identity();

    format!(
        r#"# ict-rs 2 PoW + 2 Crosslink finalizers (all zakurad).
[network]
network = "Regtest"
listen_addr = "0.0.0.0:{ZAKURA_P2P_PORT}"
p2p_stack = "legacy"
cache_dir = false
initial_testnet_peers = [
{p2p_list}
]
max_connections_per_ip = 16

[state]
ephemeral = true

[rpc]
listen_addr = "0.0.0.0:{ZAKURA_RPC_PORT}"
enable_cookie_auth = false

[metrics]
endpoint_addr = "0.0.0.0:19901"

[mining]
internal_miner = false
miner_address = "{REGTEST_MINER_DEST}"

[tracing]
filter = "info"

[crosslink]
enabled = true
activation_height = 3
confirmation_depth_sigma = 3
insecure_user_name = "{identity}"
listen_address = "0.0.0.0:{ZAKURA_TFL_PORT}"
public_address = "{identity}"
malachite_peers = [
{tfl_list}
]
do_not_manipulate_config = false
"#
    )
}

/// Preferred Linux binary for this suite (must be built with `--features crosslink`).
pub fn resolve_crosslink_zakurad_bin() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(ENV_ZAKURAD_BIN) {
        let pb = PathBuf::from(p.trim());
        if pb.is_file() {
            return Ok(pb);
        }
    }
    let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("zakura/target/zakura-regtest-e2e-linux-crosslink/zakurad"));
    if let Some(pb) = crates {
        if pb.is_file() {
            return Ok(pb);
        }
    }
    Err(IctError::Config(
        "need Linux zakurad built with --features crosslink \
         (ZAKURAD_BIN or crates/zakura/target/zakura-regtest-e2e-linux-crosslink/zakurad)"
            .into(),
    ))
}

/// Spawn the 2+2 mesh on a private Docker network.
///
/// All four containers are started before any RPC wait so Docker DNS can
/// resolve `pow-*` / `fin-*` hostnames. `zakurad` otherwise retries seed
/// DNS forever and never opens RPC.
pub async fn spawn_zakura_quad(
    runtime: Arc<dyn RuntimeBackend>,
    zakurad_bin: PathBuf,
) -> Result<ZakuraQuad> {
    if !zakurad_bin.is_file() {
        return Err(IctError::Config(format!(
            "zakurad missing at {zakurad_bin:?}"
        )));
    }
    if !zakurad_is_linux_elf(&zakurad_bin) {
        return Err(IctError::Config(format!(
            "zakurad at {zakurad_bin:?} is not a Linux ELF"
        )));
    }

    let members = default_quad_members();
    let net_name = format!("ict-zakura-xl-quad-{}", std::process::id());
    let network_id = runtime.create_network(&net_name).await?;

    let mut running = Vec::new();
    for me in &members {
        let toml = corridor_mesh_toml(me, &members);
        let mut cfg = ZakuraNodeConfig::corridor(&zakurad_bin);
        cfg.test_name = me.name.into();
        cfg.rpc_host_port = me.rpc_host_port;
        cfg.rpc_container_port = ZAKURA_RPC_PORT;
        cfg.network_id = Some(network_id.clone());
        cfg.reuse_if_ready = false;
        cfg.wait_for_rpc = false;
        cfg.crosslink = true;
        cfg.ready_timeout_secs = 120;
        cfg.extra_toml = Some(toml);
        cfg.container_hostname = Some(me.name.to_string());

        info!(
            name = me.name,
            rpc = me.rpc_host_port,
            "spawning mesh zakurad"
        );
        let node = spawn_zakura_local(runtime.clone(), cfg).await?;
        running.push((me.clone(), node));
    }

    for (me, node) in &running {
        info!(name = me.name, rpc = %node.rpc_url(), "waiting for mesh RPC");
        wait_rpc_ready(node.rpc_url(), 120).await?;
    }

    // Belt: if crawler has not dialed yet, inject container IPs via addnode.
    mesh_addnode(&running).await;

    Ok(ZakuraQuad {
        members: running,
        network_id,
        runtime,
    })
}

async fn mesh_addnode(running: &[(MeshMember, ZakuraNode)]) {
    let mut ips = Vec::new();
    for (me, _) in running {
        match docker_container_ip(&me.container_name()).await {
            Ok(ip) if !ip.is_empty() => ips.push((me.name, ip)),
            Ok(_) => warn!(name = me.name, "docker inspect returned empty IP"),
            Err(e) => warn!(name = me.name, error = %e, "docker inspect IP failed"),
        }
    }
    for (me, node) in running {
        for (peer_name, ip) in &ips {
            if *peer_name == me.name {
                continue;
            }
            let addr = format!("{ip}:{ZAKURA_P2P_PORT}");
            match rpc_call(node.rpc_url(), "addnode", &format!("[\"{addr}\",\"add\"]")).await {
                Ok(body) => info!(from = me.name, to = %addr, %body, "addnode"),
                Err(e) => warn!(from = me.name, to = %addr, error = %e, "addnode failed"),
            }
        }
    }
}

async fn docker_container_ip(name: &str) -> Result<String> {
    let out = tokio::process::Command::new("docker")
        .args([
            "inspect",
            "-f",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            name,
        ])
        .output()
        .await
        .map_err(|e| IctError::Runtime(e.into()))?;
    if !out.status.success() {
        return Err(IctError::Runtime(anyhow::anyhow!(
            "docker inspect {name}: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// JSON-RPC helper (curl).
pub async fn rpc_call(rpc_url: &str, method: &str, params: &str) -> Result<String> {
    let url = rpc_url.trim_end_matches('/').to_string() + "/";
    let body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}"#);
    let out = tokio::process::Command::new("curl")
        .args([
            "-sf",
            "--max-time",
            "8",
            "-X",
            "POST",
            &url,
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
        ])
        .output()
        .await
        .map_err(|e| IctError::Runtime(e.into()))?;
    if !out.status.success() {
        return Err(IctError::Runtime(anyhow::anyhow!(
            "curl {method} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Mine `n` regtest blocks on a node (`generate`).
pub async fn generate_blocks(rpc_url: &str, n: u32) -> Result<String> {
    rpc_call(rpc_url, "generate", &format!("[{n}]")).await
}

/// Best-effort chain height from `getblockchaininfo`.
pub fn parse_blocks_height(body: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("result")?.get("blocks")?.as_u64()
}

/// True when `getpeerinfo` reports at least one connected peer.
pub fn peerinfo_connected(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    v.get("result")
        .and_then(|r| r.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_toml_has_four_roles_and_tenderlink() {
        let all = default_quad_members();
        let t = corridor_mesh_toml(&all[0], &all);
        assert!(t.contains("[crosslink]"));
        assert!(t.contains("enabled = true"));
        assert!(t.contains("malachite_peers"));
        assert!(t.contains("pow-b:18233"));
        assert!(t.contains("fin-a:24834"));
        assert!(t.contains("insecure_user_name = \"pow-a:24834\""));
        assert!(t.contains("public_address = \"pow-a:24834\""));
        assert!(!t.contains("\"pow-a:18233\""), "must not peer with self");
        let miners = all.iter().filter(|m| m.mines).count();
        let fins = all.iter().filter(|m| !m.mines).count();
        assert_eq!(miners, 2);
        assert_eq!(fins, 2);
    }
}
