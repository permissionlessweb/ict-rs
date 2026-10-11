//! zcash-devtool wallet for local testing exercises.
//!
//! <https://github.com/zcash/zcash-devtool>
//!
//! Compact-block light wallet (`zcash_client_sqlite`), not zakurad JSON-RPC.
//! Zakura remains consensus/P2P. This CLI is the **test wallet**: UA, UIVK,
//! sync, list-unspent. Point `-s` at a compact-tx-streamer (`host:port`);
//! localhost skips TLS. Regtest needs `--all-features` on the binary and
//! `--activation-heights` at `init`.
//!
//! Env:
//! - `ZCASH_DEVTOOL_BIN` — host path to `zcash-devtool`
//! - `ZCASH_DEVTOOL_WALLET` — wallet dir (default `/tmp/svg-mint-zcash-devtool`)
//! - `ZCASH_DEVTOOL_SERVER` — `host:port` compact server (not `zecrocks` on lab)
//! - `ZCASH_DEVTOOL_NETWORK` — `test` | `main` | `regtest` (default `regtest`)
//! - `ZCASH_DEVTOOL_ACTIVATION_HEIGHTS` — TOML for `-n regtest`

#![cfg(feature = "zakura")]

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{IctError, Result};

pub const ENV_ZCASH_DEVTOOL_BIN: &str = "ZCASH_DEVTOOL_BIN";
pub const ENV_ZCASH_DEVTOOL_WALLET: &str = "ZCASH_DEVTOOL_WALLET";
pub const ENV_ZCASH_DEVTOOL_SERVER: &str = "ZCASH_DEVTOOL_SERVER";
pub const ENV_ZCASH_DEVTOOL_NETWORK: &str = "ZCASH_DEVTOOL_NETWORK";
pub const ENV_ZCASH_DEVTOOL_ACTIVATION_HEIGHTS: &str = "ZCASH_DEVTOOL_ACTIVATION_HEIGHTS";

#[derive(Debug, Clone)]
pub struct ZcashDevtoolWallet {
    pub bin: PathBuf,
    pub wallet_dir: PathBuf,
    pub identity: PathBuf,
    pub server: Option<String>,
    pub network: String,
    pub activation_heights: Option<PathBuf>,
}

impl ZcashDevtoolWallet {
    pub fn from_env() -> Result<Self> {
        let bin = std::env::var(ENV_ZCASH_DEVTOOL_BIN)
            .map(PathBuf::from)
            .map_err(|_| {
                IctError::Config(
                    "ZCASH_DEVTOOL_BIN required: build https://github.com/zcash/zcash-devtool \
                     on groot-wan (`cargo build --release --all-features`) and export the binary"
                        .into(),
                )
            })?;
        if !bin.is_file() {
            return Err(IctError::Config(format!(
                "ZCASH_DEVTOOL_BIN is not a file: {}",
                bin.display()
            )));
        }
        let wallet_dir = std::env::var(ENV_ZCASH_DEVTOOL_WALLET)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp/svg-mint-zcash-devtool"));
        let identity = wallet_dir.join("age-identity.txt");
        let server = std::env::var(ENV_ZCASH_DEVTOOL_SERVER).ok();
        let network = std::env::var(ENV_ZCASH_DEVTOOL_NETWORK).unwrap_or_else(|_| "regtest".into());
        let activation_heights = std::env::var(ENV_ZCASH_DEVTOOL_ACTIVATION_HEIGHTS)
            .ok()
            .map(PathBuf::from);
        Ok(Self {
            bin,
            wallet_dir,
            identity,
            server,
            network,
            activation_heights,
        })
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args);
        let out = cmd
            .output()
            .map_err(|e| IctError::Config(format!("zcash-devtool: {e}")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if !out.status.success() {
            return Err(IctError::Config(format!(
                "zcash-devtool {} exit={} stdout={} stderr={}",
                args.join(" "),
                out.status,
                stdout.trim(),
                stderr.trim()
            )));
        }
        Ok(stdout)
    }

    fn wallet_args<'a>(&'a self, rest: &'a [&'a str]) -> Vec<&'a str> {
        let mut v = vec![
            "wallet",
            "-w",
            self.wallet_dir.to_str().unwrap_or("/tmp/svg-mint-zcash-devtool"),
        ];
        v.extend_from_slice(rest);
        v
    }

    /// `wallet init` (empty mnemonic via piped newline = generate). Needs compact server.
    pub fn init(&self, name: &str) -> Result<String> {
        std::fs::create_dir_all(&self.wallet_dir)
            .map_err(|e| IctError::Config(format!("wallet dir: {e}")))?;
        let w = self.wallet_dir.to_string_lossy().into_owned();
        let id = self.identity.to_string_lossy().into_owned();
        let mut args = vec![
            "wallet".into(),
            "-w".into(),
            w,
            "init".into(),
            "--name".into(),
            name.into(),
            "-i".into(),
            id,
            "-n".into(),
            self.network.clone(),
        ];
        if let Some(s) = &self.server {
            args.push("-s".into());
            args.push(s.clone());
        }
        if self.network == "regtest" {
            let heights = self.activation_heights.as_ref().ok_or_else(|| {
                IctError::Config(
                    "ZCASH_DEVTOOL_ACTIVATION_HEIGHTS required for -n regtest (TOML per zcash-devtool --activation-heights)"
                        .into(),
                )
            })?;
            args.push("--activation-heights".into());
            args.push(heights.display().to_string());
        }
        let mut cmd = Command::new(&self.bin);
        cmd.args(&args);
        cmd.stdin(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| IctError::Config(format!("zcash-devtool init spawn: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            let _ = stdin.write_all(b"\n");
        }
        let out = child
            .wait_with_output()
            .map_err(|e| IctError::Config(format!("zcash-devtool init: {e}")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if !out.status.success() {
            return Err(IctError::Config(format!(
                "zcash-devtool init failed: {stdout} {stderr}"
            )));
        }
        Ok(stdout)
    }

    pub fn list_addresses(&self) -> Result<String> {
        let args = self.wallet_args(&["list-addresses"]);
        self.run(&args)
    }

    pub fn list_accounts(&self) -> Result<String> {
        let args = self.wallet_args(&["list-accounts"]);
        self.run(&args)
    }

    pub fn sync(&self) -> Result<String> {
        let mut rest = vec!["sync".to_string()];
        if let Some(s) = &self.server {
            rest.push("-s".into());
            rest.push(s.clone());
        }
        let rest_ref: Vec<&str> = rest.iter().map(|s| s.as_str()).collect();
        let args = self.wallet_args(&rest_ref);
        self.run(&args)
    }

    pub fn list_unspent(&self) -> Result<String> {
        let args = self.wallet_args(&["list-unspent"]);
        self.run(&args)
    }

    pub fn list_tx(&self) -> Result<String> {
        let args = self.wallet_args(&["list-tx"]);
        self.run(&args)
    }

    /// Offline UA (no compact server). `wallet derive-address`.
    pub fn derive_address(&self) -> Result<String> {
        self.run(&["wallet", "derive-address", "-n", &self.network])
    }

    pub fn parse_unified_address(blob: &str) -> Option<String> {
        if let Some(i) = blob.find("uregtest1") {
            let rest = &blob[i..];
            let ua: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if ua.len() > 20 {
                return Some(ua);
            }
        }
        if let Some(i) = blob.find("utest1") {
            let rest = &blob[i..];
            let ua: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if ua.len() > 20 {
                return Some(ua);
            }
        }
        for line in blob.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("Unified Address:") {
                let ua = rest.trim();
                if !ua.is_empty() {
                    return Some(ua.to_string());
                }
            }
            if t.starts_with("u1") && t.len() > 20 {
                return Some(t.to_string());
            }
        }
        None
    }

    pub fn parse_uivk(blob: &str) -> Option<String> {
        for line in blob.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("UIVK:") {
                let v = rest.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
            if t.starts_with("uivk") {
                return Some(t.to_string());
            }
        }
        None
    }

    pub fn parse_ufvk(blob: &str) -> Option<String> {
        for line in blob.lines() {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("UFVK:") {
                let v = rest.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
        None
    }

    pub fn ensure_ready(&self, name: &str) -> Result<(String, String)> {
        let keys = self.wallet_dir.join("keys.toml");
        if !keys.is_file() {
            let _ = self.init(name)?;
        }
        let addrs = self.list_addresses()?;
        let accts = self.list_accounts()?;
        let ua = Self::parse_unified_address(&addrs)
            .or_else(|| Self::parse_unified_address(&self.derive_address().ok()?))
            .ok_or_else(|| {
                IctError::Config(format!("PARSER2026 no ua find={} blob={addrs}", addrs.contains("uregtest1")))
            })?;
        let uivk = Self::parse_uivk(&accts).unwrap_or_default();
        Ok((ua, uivk))
    }
}

pub fn write_default_regtest_heights(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| IctError::Config(e.to_string()))?;
    }
    // Matches zcash-devtool DEFAULT_REGTEST / zcash_local_net fixture.
    let toml = "\
overwinter = 1
sapling = 1
blossom = 1
heartwood = 1
canopy = 1
nu5 = 2
nu6 = 2
nu6_1 = 2
nu6_2 = 2
nu6_3 = 2
";
    std::fs::write(path, toml).map_err(|e| IctError::Config(e.to_string()))?;
    Ok(())
}
