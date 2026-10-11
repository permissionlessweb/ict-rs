//! Hasher IBC bench metrics (hybrid H vs full-BLAKE3 F).
//! See `networks/upgrades/v6.3/BENCH.md`. `ICT_MOCK=1` is not a result.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{IctError, Result};

/// Bench variant from `HASHER_VARIANT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HasherVariant {
    /// IBC SHA-256; 07-tendermint or stock IavlSpec 08-wasm.
    H,
    /// All IAVL BLAKE3; 08-wasm `Blake3IavlSpec`.
    F,
}

impl HasherVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::H => "H",
            Self::F => "F",
        }
    }
}

/// Parse `HASHER_VARIANT` (`H` / `F`, case-insensitive).
pub fn parse_variant(raw: &str) -> Result<HasherVariant> {
    match raw.trim().to_ascii_uppercase().as_str() {
        "H" | "HYBRID" => Ok(HasherVariant::H),
        "F" | "FULL" | "BLAKE3" => Ok(HasherVariant::F),
        other => Err(IctError::Config(format!(
            "HASHER_VARIANT={other} (want H or F)"
        ))),
    }
}

/// Fail closed: mock is not a hasher IBC bench result.
pub fn refuse_mock() -> Result<()> {
    match std::env::var("ICT_MOCK") {
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => Err(IctError::Config(
            "hasher_ibc_bench: ICT_MOCK=1 is not a result (need Docker RecvPacket)".into(),
        )),
        _ => Ok(()),
    }
}

pub fn median_u64(xs: &[u64]) -> Option<u64> {
    if xs.is_empty() {
        return None;
    }
    let mut v = xs.to_vec();
    v.sort_unstable();
    let n = v.len();
    if n % 2 == 1 {
        Some(v[n / 2])
    } else {
        Some((v[n / 2 - 1] + v[n / 2]) / 2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IbcMsgKind {
    CreateClient,
    ConnectionOpenInit,
    ConnectionOpenTry,
    ConnectionOpenAck,
    ConnectionOpenConfirm,
    ChannelOpenInit,
    ChannelOpenTry,
    ChannelOpenAck,
    ChannelOpenConfirm,
    UpdateClient,
    RecvPacket,
    Acknowledgement,
    Timeout,
}

impl IbcMsgKind {
    pub fn action(self) -> &'static str {
        match self {
            Self::CreateClient => "CreateClient",
            Self::ConnectionOpenInit => "ConnOpenInit",
            Self::ConnectionOpenTry => "ConnOpenTry",
            Self::ConnectionOpenAck => "ConnOpenAck",
            Self::ConnectionOpenConfirm => "ConnOpenConfirm",
            Self::ChannelOpenInit => "ChanOpenInit",
            Self::ChannelOpenTry => "ChanOpenTry",
            Self::ChannelOpenAck => "ChanOpenAck",
            Self::ChannelOpenConfirm => "ChanOpenConfirm",
            Self::UpdateClient => "UpdateClient",
            Self::RecvPacket => "RecvPacket",
            Self::Acknowledgement => "Acknowledgement",
            Self::Timeout => "Timeout",
        }
    }
}

#[derive(Debug, Clone)]
pub struct IbcMsgSample {
    pub kind: IbcMsgKind,
    pub height: u64,
    pub gas: u64,
    pub proof_bytes: u64,
    pub txhash: String,
}

/// One transfer sample (post-warmup).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferSample {
    pub t_round_trip_ms: u64,
    pub t_recv_packet_ms: u64,
    pub t_update_client_ms: u64,
    pub t_ack_ms: u64,
    pub gas_recv_packet: u64,
    pub gas_update_client: u64,
    pub gas_ack: u64,
    pub proof_bytes_recv: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HasherIbcReport {
    pub variant: String,
    pub image: String,
    pub n: u64,
    pub warmup: u64,
    pub packets_on: String,
    pub median_t_round_trip_ms: Option<u64>,
    pub median_t_recv_packet_ms: Option<u64>,
    pub median_gas_recv_packet: Option<u64>,
    /// Alias for HASHER.md `jq .median_gas_recv`.
    pub median_gas_recv: Option<u64>,
    pub median_gas_update_client: Option<u64>,
    pub median_proof_bytes_recv: Option<u64>,
    pub median_t_ack_ms: Option<u64>,
    pub median_gas_ack: Option<u64>,
    pub lc_store_gas: u64,
    pub lc_create_gas: u64,
    pub samples: Vec<TransferSample>,
}

impl HasherIbcReport {
    pub fn from_samples(
        variant: HasherVariant,
        image: String,
        packets_on: &str,
        warmup: u64,
        lc_store_gas: u64,
        lc_create_gas: u64,
        samples: Vec<TransferSample>,
    ) -> Self {
        let n = samples.len() as u64;
        let rt: Vec<u64> = samples.iter().map(|s| s.t_round_trip_ms).collect();
        let recv_t: Vec<u64> = samples.iter().map(|s| s.t_recv_packet_ms).collect();
        let recv_g: Vec<u64> = samples.iter().map(|s| s.gas_recv_packet).collect();
        let upd_g: Vec<u64> = samples.iter().map(|s| s.gas_update_client).collect();
        let proof: Vec<u64> = samples.iter().map(|s| s.proof_bytes_recv).collect();
        let ack_t: Vec<u64> = samples.iter().map(|s| s.t_ack_ms).collect();
        let ack_g: Vec<u64> = samples.iter().map(|s| s.gas_ack).collect();
        let med_recv = median_u64(&recv_g);
        Self {
            variant: variant.as_str().to_string(),
            image,
            n,
            warmup,
            packets_on: packets_on.to_string(),
            median_t_round_trip_ms: median_u64(&rt),
            median_t_recv_packet_ms: median_u64(&recv_t),
            median_gas_recv_packet: med_recv,
            median_gas_recv: med_recv,
            median_gas_update_client: median_u64(&upd_g),
            median_proof_bytes_recv: median_u64(&proof),
            median_t_ack_ms: median_u64(&ack_t),
            median_gas_ack: median_u64(&ack_g),
            lc_store_gas,
            lc_create_gas,
            samples,
        }
    }
}

/// One measured action (transfer RecvPacket, store-code, polytone, hooks, PFM).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HasherActionRow {
    pub action: String,
    pub path: String,
    pub packets_on: String,
    pub gas: u64,
    pub n: u64,
    pub ok: bool,
    #[serde(default)]
    pub note: String,
    /// Filled on this run when `std::env::consts::ARCH == "aarch64"`.
    #[serde(default)]
    pub gas_aarch64: Option<u64>,
    /// Reserved for amd64/x86_64 curation (overwrite on that host).
    #[serde(default)]
    pub gas_amd64: Option<u64>,
}

impl HasherActionRow {
    pub fn ok(action: &str, path: &str, packets_on: &str, gas: u64, n: u64) -> Self {
        Self {
            action: action.into(),
            path: path.into(),
            packets_on: packets_on.into(),
            gas,
            n,
            ok: true,
            note: String::new(),
            gas_aarch64: None,
            gas_amd64: None,
        }
        .with_this_arch()
    }

    pub fn fail(action: &str, path: &str, packets_on: &str, note: String) -> Self {
        Self {
            action: action.into(),
            path: path.into(),
            packets_on: packets_on.into(),
            gas: 0,
            n: 0,
            ok: false,
            note,
            gas_aarch64: None,
            gas_amd64: None,
        }
        .with_this_arch()
    }

    pub fn with_this_arch(mut self) -> Self {
        match std::env::consts::ARCH {
            "aarch64" => self.gas_aarch64 = Some(self.gas),
            "x86_64" => self.gas_amd64 = Some(self.gas),
            _ => {}
        }
        self
    }
}

/// Spawn vs attach knobs for hasher examples. Attach means the caller already
/// built `Interchain` (existing chains/channels); this crate does not discover
/// Docker containers by label.
#[derive(Debug, Clone)]
pub struct HasherIbcEnv {
    pub smoke: bool,
    pub spawn: bool,
    pub apps: bool,
}

impl HasherIbcEnv {
    pub fn from_env() -> Self {
        let truthy = |k: &str| {
            matches!(
                std::env::var(k).ok().as_deref(),
                Some("1" | "true" | "yes" | "on")
            )
        };
        let smoke = truthy("HASHER_SMOKE");
        let spawn = !truthy("HASHER_ATTACH");
        let apps = match std::env::var("HASHER_APPS").ok().as_deref() {
            Some("0" | "false" | "no" | "off") => false,
            Some(_) => true,
            None => !smoke,
        };
        Self {
            smoke,
            spawn,
            apps,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HasherThreeChainReport {
    /// RFC3339 or unix; overwritten every write.
    #[serde(default)]
    pub timestamp: String,
    /// `std::env::consts::ARCH` for this run (`aarch64`, `x86_64`, …).
    #[serde(default)]
    pub arch: String,
    pub image: String,
    pub topology: String,
    pub lc_store_gas: u64,
    /// 07-tendermint handshake A(full BLAKE3)→C; empty if not attempted.
    #[serde(default)]
    pub a_tm_handshake: String,
    /// 08-wasm ConfirmIcs23 gas (hybrid bank/acc inclusion).
    #[serde(default)]
    pub hybrid_inclusion_gas: u64,
    #[serde(default)]
    pub hybrid_exclusion_gas: u64,
    /// 08-wasm ConfirmIcs23 gas (full-BLAKE3 chain A).
    #[serde(default)]
    pub a_inclusion_gas: u64,
    #[serde(default)]
    pub wasm_lc_contract: String,
    /// `08-wasm-*` client on C of A (empty if create failed).
    #[serde(default)]
    pub a_wasm_client: String,
    /// A→C 08-wasm path result (`ok` or the error).
    #[serde(default)]
    pub a_wasm_path: String,
    /// `08-wasm-*` client on A of C (dual-wasm). Empty if A stayed 07-tendermint.
    #[serde(default)]
    pub c_wasm_client: String,
    #[serde(default)]
    pub actions: Vec<HasherActionRow>,
    pub legs: Vec<HasherIbcReport>,
}

pub fn write_three_report(report: &HasherThreeChainReport, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| {
        IctError::Config(format!("hasher-ibc mkdir {}: {e}", dir.display()))
    })?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamped = dir.join(format!("hasher-ibc-three-{ts}.json"));
    let latest = dir.join("hasher-ibc-three-latest.json");
    let mut report = report.clone();
    report.timestamp = bench_timestamp();
    report.arch = std::env::consts::ARCH.to_string();
    for row in &mut report.actions {
        *row = row.clone().with_this_arch();
    }
    let body = serde_json::to_string_pretty(&report)
        .map_err(|e| IctError::Config(format!("hasher-ibc json: {e}")))?;
    std::fs::write(&stamped, &body)
        .map_err(|e| IctError::Config(format!("write {}: {e}", stamped.display())))?;
    let _ = std::fs::write(&latest, &body);
    let md = markdown_table(&report);
    let _ = std::fs::write(dir.join("TABLE.md"), &md);
    let _ = std::fs::write(dir.join(format!("hasher-ibc-three-{ts}.md")), &md);
    Ok(stamped)
}

/// Readable gas comparison. Written next to the JSON as `TABLE.md`.
pub fn markdown_table(report: &HasherThreeChainReport) -> String {
    let mut rows: Vec<HasherActionRow> = Vec::new();
    if report.lc_store_gas > 0 {
        rows.push(HasherActionRow::ok(
            "ibc-wasm store-code",
            "A+C",
            "08-wasm",
            report.lc_store_gas,
            1,
        ));
    }
    for leg in &report.legs {
        if let Some(g) = leg.median_gas_recv {
            rows.push(HasherActionRow::ok(
                "RecvPacket",
                &format!("{} {}", leg.variant, leg.packets_on),
                &leg.packets_on,
                g,
                leg.n,
            ));
        }
        if let Some(g) = leg.median_gas_update_client.filter(|g| *g > 0) {
            rows.push(HasherActionRow::ok(
                "UpdateClient",
                &format!("{} {}", leg.variant, leg.packets_on),
                &leg.packets_on,
                g,
                leg.n,
            ));
        }
    }
    rows.extend(report.actions.iter().cloned());

    let ts = if report.timestamp.is_empty() {
        bench_timestamp()
    } else {
        report.timestamp.clone()
    };
    let arch = if report.arch.is_empty() {
        std::env::consts::ARCH.to_string()
    } else {
        report.arch.clone()
    };
    let mut out = String::new();
    out.push_str("# Hasher IBC gas\n\n");
    out.push_str(&format!("- timestamp: `{ts}` (overwritten each run)\n"));
    out.push_str(&format!("- arch: `{arch}` (this run fills `gas_{arch}`; other arch columns reserved)\n"));
    out.push_str(&format!("- image: `{}`\n", report.image));
    out.push_str(&format!("- topology: {}\n", report.topology));
    out.push_str(&format!("- A TM handshake: {}\n", report.a_tm_handshake));
    out.push_str(&format!(
        "- clients: C of A `{}`; A of C `{}`\n",
        empty_dash(&report.a_wasm_client),
        empty_dash(&report.c_wasm_client)
    ));
    out.push_str(&format!("- A→C path: {}\n\n", report.a_wasm_path));
    out.push_str("| action | path | client | n | ok | gas | gas_aarch64 | gas_amd64 | note |\n");
    out.push_str("|---|---|---|---:|---|---:|---:|---:|---|\n");
    for r in &rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            r.action,
            r.path,
            r.packets_on,
            r.n,
            if r.ok { "yes" } else { "no" },
            r.gas,
            opt_gas(r.gas_aarch64),
            opt_gas(r.gas_amd64),
            trunc_note(&r.note)
        ));
    }
    out.push_str("\n08-wasm LC verifies ICS-23 `HashOp` in software (portable). ");
    out.push_str("It does **not** import CosmWasm `blake3_256`, so the same bytecode can be stored on a foreign counterparty. ");
    out.push_str("Terp-native app contracts on A/B may use `deps.api.blake3_256` (`hash-blake`); C in this lab is a SHA-256 stand-in and must not require that host import.\n");
    out
}

pub fn bench_timestamp() -> String {
    std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs().to_string())
                .unwrap_or_default()
        })
}

fn opt_gas(g: Option<u64>) -> String {
    g.map(|n| n.to_string()).unwrap_or_else(|| "—".into())
}

fn empty_dash(s: &str) -> &str {
    if s.is_empty() {
        "—"
    } else {
        s
    }
}

fn trunc_note(s: &str) -> String {
    const MAX: usize = 120;
    let one: String = s.chars().filter(|c| *c != '\n' && *c != '|').take(MAX).collect();
    if s.chars().count() > MAX {
        format!("{one}…")
    } else {
        one
    }
}

pub fn write_report(report: &HasherIbcReport, dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| {
        IctError::Config(format!("hasher-ibc mkdir {}: {e}", dir.display()))
    })?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamped = dir.join(format!("hasher-ibc-{}-{ts}.json", report.variant));
    let latest = dir.join(format!("hasher-ibc-{}-latest.json", report.variant));
    let body = serde_json::to_string_pretty(report)
        .map_err(|e| IctError::Config(format!("hasher-ibc json: {e}")))?;
    std::fs::write(&stamped, &body)
        .map_err(|e| IctError::Config(format!("write {}: {e}", stamped.display())))?;
    let _ = std::fs::write(&latest, &body);
    Ok(stamped)
}

/// `crates/ict-rs/ict-rs` → repo `tests/benchmarks/hasher-ibc`.
pub fn default_out_dir() -> PathBuf {
    if let Ok(p) = std::env::var("HASHER_BENCH_OUT") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Ok(p) = std::env::var("TERP_CORE_ROOT") {
        return PathBuf::from(p).join("tests/benchmarks/hasher-ibc");
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.pop();
    p.join("tests/benchmarks/hasher-ibc")
}

fn b64_len(s: &str) -> u64 {
    let t = s.trim();
    if t.is_empty() {
        return 0;
    }
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, t)
        .map(|b| b.len() as u64)
        .or_else(|_| {
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD_NO_PAD, t)
                .map(|b| b.len() as u64)
        })
        .unwrap_or(t.len() as u64)
}

fn msg_kind(type_url: &str) -> Option<IbcMsgKind> {
    if type_url.contains("MsgRecvPacket") {
        Some(IbcMsgKind::RecvPacket)
    } else if type_url.contains("MsgUpdateClient") {
        Some(IbcMsgKind::UpdateClient)
    } else if type_url.contains("MsgAcknowledgement") {
        Some(IbcMsgKind::Acknowledgement)
    } else if type_url.contains("MsgTimeout") {
        Some(IbcMsgKind::Timeout)
    } else if type_url.contains("MsgCreateClient") {
        Some(IbcMsgKind::CreateClient)
    } else if type_url.contains("MsgConnectionOpenInit") {
        Some(IbcMsgKind::ConnectionOpenInit)
    } else if type_url.contains("MsgConnectionOpenTry") {
        Some(IbcMsgKind::ConnectionOpenTry)
    } else if type_url.contains("MsgConnectionOpenAck") {
        Some(IbcMsgKind::ConnectionOpenAck)
    } else if type_url.contains("MsgConnectionOpenConfirm") {
        Some(IbcMsgKind::ConnectionOpenConfirm)
    } else if type_url.contains("MsgChannelOpenInit") {
        Some(IbcMsgKind::ChannelOpenInit)
    } else if type_url.contains("MsgChannelOpenTry") {
        Some(IbcMsgKind::ChannelOpenTry)
    } else if type_url.contains("MsgChannelOpenAck") {
        Some(IbcMsgKind::ChannelOpenAck)
    } else if type_url.contains("MsgChannelOpenConfirm") {
        Some(IbcMsgKind::ChannelOpenConfirm)
    } else {
        None
    }
}

fn proof_bytes_from_msg(msg: &serde_json::Value) -> u64 {
    for key in [
        "proof_commitment",
        "proofCommitment",
        "proof_acked",
        "proofAcked",
        "proof",
    ] {
        if let Some(s) = msg.get(key).and_then(|v| v.as_str()) {
            return b64_len(s);
        }
    }
    0
}

fn walk_messages(v: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match v {
        serde_json::Value::Object(m) => {
            if m.contains_key("@type") {
                out.push(v.clone());
            }
            for val in m.values() {
                walk_messages(val, out);
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                walk_messages(x, out);
            }
        }
        _ => {}
    }
}

fn json_u64(v: &serde_json::Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
}

/// `gas_used` lives on `tx_responses[]`, sometimes under `gas_info`, almost
/// never on the companion proto `txs[]` object. Prefer the first hit.
pub fn tx_gas_used(tx: &serde_json::Value) -> u64 {
    tx.get("gas_used")
        .and_then(json_u64)
        .or_else(|| tx.pointer("/gas_info/gas_used").and_then(json_u64))
        .or_else(|| tx.pointer("/tx_response/gas_used").and_then(json_u64))
        .unwrap_or(0)
}

fn kind_from_event_type(ty: &str) -> Option<IbcMsgKind> {
    let t = ty.rsplit('.').next().unwrap_or(ty);
    match t {
        "create_client" => Some(IbcMsgKind::CreateClient),
        "connection_open_init" => Some(IbcMsgKind::ConnectionOpenInit),
        "connection_open_try" => Some(IbcMsgKind::ConnectionOpenTry),
        "connection_open_ack" => Some(IbcMsgKind::ConnectionOpenAck),
        "connection_open_confirm" => Some(IbcMsgKind::ConnectionOpenConfirm),
        "channel_open_init" => Some(IbcMsgKind::ChannelOpenInit),
        "channel_open_try" => Some(IbcMsgKind::ChannelOpenTry),
        "channel_open_ack" => Some(IbcMsgKind::ChannelOpenAck),
        "channel_open_confirm" => Some(IbcMsgKind::ChannelOpenConfirm),
        "update_client" => Some(IbcMsgKind::UpdateClient),
        "recv_packet" => Some(IbcMsgKind::RecvPacket),
        "acknowledge_packet" => Some(IbcMsgKind::Acknowledgement),
        "timeout_packet" => Some(IbcMsgKind::Timeout),
        _ => None,
    }
}

fn kinds_from_events(tx: &serde_json::Value) -> Vec<IbcMsgKind> {
    let mut set = std::collections::BTreeSet::new();
    fn walk(v: &serde_json::Value, set: &mut std::collections::BTreeSet<IbcMsgKind>) {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(ty) = m.get("type").and_then(|t| t.as_str()) {
                    if let Some(k) = kind_from_event_type(ty) {
                        set.insert(k);
                    }
                }
                if m.get("key").and_then(|k| k.as_str()) == Some("action") {
                    if let Some(k) = m.get("value").and_then(|x| x.as_str()).and_then(msg_kind) {
                        set.insert(k);
                    }
                }
                for x in m.values() {
                    walk(x, set);
                }
            }
            serde_json::Value::Array(a) => {
                for x in a {
                    walk(x, set);
                }
            }
            _ => {}
        }
    }
    walk(tx, &mut set);
    set.into_iter().collect()
}

/// Median gas per IBC message kind in a `query txs` window.
pub fn rows_from_ibc_msgs(
    samples: &[IbcMsgSample],
    path: &str,
    packets_on: &str,
) -> Vec<HasherActionRow> {
    let mut by = std::collections::BTreeMap::<IbcMsgKind, Vec<u64>>::new();
    for s in samples {
        by.entry(s.kind).or_default().push(s.gas);
    }
    by.into_iter()
        .map(|(k, mut gs)| {
            gs.sort_unstable();
            let n = gs.len() as u64;
            let med = gs[gs.len() / 2];
            HasherActionRow::ok(k.action(), path, packets_on, med, n)
        })
        .collect()
}

/// Parse `query txs` JSON; keep IBC relayer msgs at `height >= min_height`.
///
/// Cosmos SDK 0.50+ returns both `txs` (proto, often no `gas_used`) and
/// `tx_responses` (has `gas_used`). Prefer `tx_responses`. Packet-receipt
/// JSON is not a RecvPacket sample.
pub fn collect_ibc_msgs(txs_json: &serde_json::Value, min_height: u64) -> Vec<IbcMsgSample> {
    let txs = txs_json
        .get("tx_responses")
        .or_else(|| txs_json.get("txs"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for tx in txs {
        let height = tx
            .get("height")
            .and_then(json_u64)
            .unwrap_or(0);
        if height < min_height {
            continue;
        }
        let gas = tx_gas_used(&tx);
        let txhash = tx
            .get("txhash")
            .or_else(|| tx.get("hash"))
            .and_then(|h| h.as_str())
            .unwrap_or("")
            .to_string();
        let mut msgs = Vec::new();
        walk_messages(&tx, &mut msgs);
        if msgs.is_empty() {
            for kind in kinds_from_events(&tx) {
                out.push(IbcMsgSample {
                    kind,
                    height,
                    gas,
                    proof_bytes: 0,
                    txhash: txhash.clone(),
                });
            }
            continue;
        }
        for msg in msgs {
            let ty = msg.get("@type").and_then(|t| t.as_str()).unwrap_or("");
            let Some(kind) = msg_kind(ty) else {
                continue;
            };
            out.push(IbcMsgSample {
                kind,
                height,
                gas,
                proof_bytes: proof_bytes_from_msg(&msg),
                txhash: txhash.clone(),
            });
        }
    }
    out
}

/// ABCI `/store/<name>/key` prove response (IAVL + simple ops).
#[derive(Debug, Clone)]
pub struct StoreProve {
    pub store_name: String,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub iavl_proof: Vec<u8>,
    pub simple_proof: Vec<u8>,
    pub store_root: Vec<u8>,
}

fn b64_decode(s: &str) -> Vec<u8> {
    base64::Engine::decode(&base64::engine::general_purpose::STANDARD, s.trim())
        .or_else(|_| {
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD_NO_PAD, s.trim())
        })
        .unwrap_or_default()
}

/// Parse CometBFT `abci_query` JSON (`prove=true`).
pub fn parse_abci_store_proof(store_name: &str, json: &serde_json::Value) -> Result<StoreProve> {
    let resp = json
        .pointer("/result/response")
        .or_else(|| json.get("response"))
        .ok_or_else(|| IctError::Config("abci_query missing response".into()))?;
    let key = resp
        .get("key")
        .and_then(|v| v.as_str())
        .map(b64_decode)
        .unwrap_or_default();
    let value = resp
        .get("value")
        .and_then(|v| v.as_str())
        .map(b64_decode)
        .unwrap_or_default();
    let ops = resp
        .pointer("/proofOps/ops")
        .or_else(|| resp.pointer("/proof_ops/ops"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut iavl_proof = Vec::new();
    let mut simple_proof = Vec::new();
    let mut store_root = Vec::new();
    for op in ops {
        let ty = op.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let data = op.get("data").and_then(|d| d.as_str()).map(b64_decode).unwrap_or_default();
        if ty.contains("iavl") {
            iavl_proof = data;
        } else if ty.contains("simple") {
            simple_proof = data.clone();
            // simple exist leaf value is the IAVL store root
            store_root = simple_exist_value(&data);
        }
    }
    if iavl_proof.is_empty() {
        return Err(IctError::Config("abci_query missing ics23:iavl proof op".into()));
    }
    Ok(StoreProve {
        store_name: store_name.into(),
        key,
        value,
        iavl_proof,
        simple_proof,
        store_root,
    })
}

/// ICS-23 `MerkleProof` bytes (IAVL then simple) for 08-wasm VerifyMembership.
pub fn encode_merkle_proof(iavl: &[u8], simple: &[u8]) -> Result<Vec<u8>> {
    use prost::Message;
    let iavl_p = ics23::CommitmentProof::decode(iavl).map_err(|e| {
        IctError::Config(format!("merkle iavl CommitmentProof: {e}"))
    })?;
    let mut proofs = vec![iavl_p];
    if !simple.is_empty() {
        let simple_p = ics23::CommitmentProof::decode(simple).map_err(|e| {
            IctError::Config(format!("merkle simple CommitmentProof: {e}"))
        })?;
        proofs.push(simple_p);
    }
    let mp = Ics23MerkleProof { proofs };
    Ok(mp.encode_to_vec())
}

#[derive(Clone, PartialEq, prost::Message)]
struct Ics23MerkleProof {
    #[prost(message, repeated, tag = "1")]
    proofs: Vec<ics23::CommitmentProof>,
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn iavl_root_from_proof(proof_bz: &[u8], _blake3: bool) -> Option<Vec<u8>> {
    use prost::Message;
    let p = ics23::CommitmentProof::decode(proof_bz).ok()?;
    let ex = match p.proof? {
        ics23::commitment_proof::Proof::Exist(ex) => ex,
        _ => return None,
    };
    ics23::calculate_existence_root::<ics23::HostFunctionsManager>(&ex).ok()
}

fn simple_exist_value(proof_bz: &[u8]) -> Vec<u8> {
    use prost::Message;
    if let Ok(p) = ics23::CommitmentProof::decode(proof_bz) {
        if let Some(ics23::commitment_proof::Proof::Exist(ex)) = p.proof {
            return ex.value;
        }
    }
    Vec::new()
}

pub fn first_of(samples: &[IbcMsgSample], kind: IbcMsgKind) -> Option<&IbcMsgSample> {
    samples.iter().find(|s| s.kind == kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_h_f() {
        assert_eq!(parse_variant("h").unwrap(), HasherVariant::H);
        assert_eq!(parse_variant("F").unwrap(), HasherVariant::F);
        assert!(parse_variant("x").is_err());
    }

    #[test]
    fn mock_refused() {
        let prev = std::env::var("ICT_MOCK").ok();
        std::env::set_var("ICT_MOCK", "1");
        assert!(refuse_mock().is_err());
        match prev {
            Some(v) => std::env::set_var("ICT_MOCK", v),
            None => std::env::remove_var("ICT_MOCK"),
        }
        if std::env::var("ICT_MOCK").ok().as_deref() != Some("1") {
            assert!(refuse_mock().is_ok());
        }
    }

    #[test]
    fn median_odd_even() {
        assert_eq!(median_u64(&[]), None);
        assert_eq!(median_u64(&[3]), Some(3));
        assert_eq!(median_u64(&[1, 3, 100]), Some(3));
        assert_eq!(median_u64(&[2, 4]), Some(3));
    }

    #[test]
    fn collect_recv_and_update() {
        let j = serde_json::json!({
            "txs": [{
                "height": "12",
                "txhash": "AA",
                "gas_used": "111000",
                "tx": { "body": { "messages": [
                    {"@type": "/ibc.core.client.v1.MsgUpdateClient"},
                    {"@type": "/ibc.core.channel.v1.MsgRecvPacket", "proof_commitment": "AQID"}
                ]}}
            }]
        });
        let s = collect_ibc_msgs(&j, 10);
        assert_eq!(s.len(), 2);
        let recv = first_of(&s, IbcMsgKind::RecvPacket).unwrap();
        assert_eq!(recv.gas, 111000);
        assert_eq!(recv.proof_bytes, 3);
        assert!(first_of(&s, IbcMsgKind::UpdateClient).is_some());
        assert!(collect_ibc_msgs(&j, 99).is_empty());
    }

    #[test]
    fn collect_prefers_tx_responses_gas() {
        // SDK 0.50: `txs` is proto (no gas); `tx_responses` has gas_used.
        let j = serde_json::json!({
            "txs": [{
                "body": { "messages": [
                    {"@type": "/ibc.core.channel.v1.MsgRecvPacket"}
                ]}
            }],
            "tx_responses": [{
                "height": "12",
                "txhash": "BB",
                "gas_used": "174807",
                "tx": { "body": { "messages": [
                    {"@type": "/ibc.core.channel.v1.MsgRecvPacket", "proof_commitment": "AQID"}
                ]}}
            }]
        });
        let s = collect_ibc_msgs(&j, 10);
        let recv = first_of(&s, IbcMsgKind::RecvPacket).unwrap();
        assert_eq!(recv.gas, 174807);
        assert_eq!(recv.proof_bytes, 3);
    }

    #[test]
    fn collect_gas_info_and_events() {
        let j = serde_json::json!({
            "tx_responses": [{
                "height": "20",
                "txhash": "CC",
                "gas_info": { "gas_used": "162620" },
                "events": [
                    {"type": "recv_packet", "attributes": []},
                    {"type": "message", "attributes": [
                        {"key": "action", "value": "/ibc.core.channel.v1.MsgRecvPacket"}
                    ]}
                ]
            }]
        });
        let s = collect_ibc_msgs(&j, 1);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].kind, IbcMsgKind::RecvPacket);
        assert_eq!(s[0].gas, 162620);
    }

    #[test]
    fn packet_receipt_json_is_not_recvpacket() {
        let j = serde_json::json!({"received": true});
        assert!(collect_ibc_msgs(&j, 0).is_empty());
    }

    #[test]
    fn collect_create_client_and_conn_open() {
        let j = serde_json::json!({
            "tx_responses": [{
                "height": "5",
                "txhash": "CC",
                "gas_used": "81234",
                "tx": { "body": { "messages": [
                    {"@type": "/ibc.core.client.v1.MsgCreateClient"},
                    {"@type": "/ibc.core.connection.v1.MsgConnectionOpenTry"}
                ]}}
            }]
        });
        let s = collect_ibc_msgs(&j, 1);
        assert!(first_of(&s, IbcMsgKind::CreateClient).is_some());
        assert!(first_of(&s, IbcMsgKind::ConnectionOpenTry).is_some());
        let rows = rows_from_ibc_msgs(&s, "A→C", "08-wasm");
        assert!(rows.iter().any(|r| r.action == "CreateClient" && r.gas == 81234));
        assert!(rows.iter().any(|r| r.action == "ConnOpenTry"));
    }

    #[test]
    fn report_jq_alias() {
        let samples = vec![TransferSample {
            t_round_trip_ms: 10,
            t_recv_packet_ms: 8,
            t_update_client_ms: 7,
            t_ack_ms: 9,
            gas_recv_packet: 100,
            gas_update_client: 50,
            gas_ack: 40,
            proof_bytes_recv: 20,
        }];
        let r = HasherIbcReport::from_samples(
            HasherVariant::H,
            "img:tag".into(),
            "07-tendermint",
            3,
            0,
            0,
            samples,
        );
        assert_eq!(r.median_gas_recv, Some(100));
        assert_eq!(r.median_gas_recv, r.median_gas_recv_packet);
        let dir = tempfile::tempdir().unwrap();
        let p = write_report(&r, dir.path()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        assert_eq!(v["median_gas_recv"], 100);
        assert_eq!(v["variant"], "H");
    }

    #[test]
    fn markdown_table_lists_recv_and_actions() {
        let report = HasherThreeChainReport {
            timestamp: "2026-09-14T00:00:00Z".into(),
            arch: "aarch64".into(),
            image: "img:tag".into(),
            topology: "A=blake3 C=sha256; 08-wasm A↔C".into(),
            lc_store_gas: 39742,
            a_tm_handshake: "skipped".into(),
            hybrid_inclusion_gas: 0,
            hybrid_exclusion_gas: 0,
            a_inclusion_gas: 0,
            wasm_lc_contract: String::new(),
            a_wasm_client: "08-wasm-0".into(),
            a_wasm_path: "ok".into(),
            c_wasm_client: "08-wasm-0".into(),
            actions: vec![HasherActionRow::ok(
                "ibc-hooks increment",
                "A→C",
                "08-wasm",
                210000,
                1,
            )],
            legs: vec![HasherIbcReport::from_samples(
                HasherVariant::F,
                "img:tag".into(),
                "08-wasm",
                0,
                0,
                0,
                vec![TransferSample {
                    t_round_trip_ms: 0,
                    t_recv_packet_ms: 0,
                    t_update_client_ms: 0,
                    t_ack_ms: 0,
                    gas_recv_packet: 200665,
                    gas_update_client: 0,
                    gas_ack: 0,
                    proof_bytes_recv: 0,
                }],
            )],
        };
        let md = markdown_table(&report);
        assert!(md.contains("| RecvPacket |"));
        assert!(md.contains("200665"));
        assert!(md.contains("ibc-hooks increment"));
        assert!(md.contains("08-wasm-0"));
        assert!(md.contains("does **not** import CosmWasm `blake3_256`"));
        assert!(md.contains("gas_aarch64"));
        assert!(md.contains("timestamp:"));
    }
}
