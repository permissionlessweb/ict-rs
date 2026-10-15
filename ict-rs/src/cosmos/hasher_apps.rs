//! App-level IBC benches on an **already-built** Interchain.
//!
//! Spawn vs attach is the caller's job (`Interchain::build` or reuse). These
//! functions only need chains, a relayer, and existing clients/connections.

use std::path::{Path, PathBuf};

use crate::chain::Chain;
use crate::error::{IctError, Result};
use crate::hasher_ibc::{
    collect_ibc_msgs, first_of, rows_from_ibc_msgs, HasherActionRow, IbcMsgKind,
};
use crate::ibc::{ibc_denom_multi_hop, ChannelOptions};
use crate::ibc_wasm_path;
use crate::interchain::wait_for_blocks;
use crate::relayer::Relayer;
use crate::tx::{TransferOptions, WalletAmount};

fn contracts_dir() -> PathBuf {
    if let Ok(p) = std::env::var("TERP_CORE_ROOT").or_else(|_| std::env::var("TERP_CORE")) {
        return PathBuf::from(p).join("artifacts");
    }
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.pop();
    p.join("artifacts")
}

async fn put_host_wasm(chain: &dyn Chain, host: &Path, dest_name: &str) -> Result<String> {
    let bz = std::fs::read(host).map_err(|e| {
        IctError::Config(format!("read {}: {e}", host.display()))
    })?;
    let dest = format!("/tmp/{dest_name}");
    ibc_wasm_path::put_file(chain, &dest, &bz).await?;
    Ok(dest)
}

async fn code_ids(chain: &dyn Chain) -> Result<Vec<String>> {
    let out = chain
        .chain_exec(&["query", "wasm", "list-code", "--output", "json"])
        .await?;
    let v: serde_json::Value =
        serde_json::from_str(out.stdout_str().trim()).unwrap_or(serde_json::Value::Null);
    let mut ids = Vec::new();
    if let Some(arr) = v.get("code_infos").and_then(|c| c.as_array()) {
        for c in arr {
            if let Some(id) = c
                .get("code_id")
                .and_then(|x| x.as_str().map(|s| s.to_string()).or_else(|| x.as_u64().map(|n| n.to_string())))
            {
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

async fn wasm_tx(
    chain: &dyn Chain,
    args: &[&str],
) -> Result<serde_json::Value> {
    let out = chain.chain_exec(args).await?;
    if out.exit_code != 0 {
        return Err(IctError::Config(format!(
            "wasm tx exit {}: {}",
            out.exit_code,
            out.stderr_str()
        )));
    }
    serde_json::from_str(out.stdout_str().trim()).map_err(|e| {
        IctError::Config(format!("wasm tx json: {e} {}", out.stdout_str()))
    })
}

async fn store_new_code(chain: &dyn Chain, key: &str, container_path: &str) -> Result<String> {
    let before = code_ids(chain).await?;
    let v = wasm_tx(
        chain,
        &[
            "tx",
            "wasm",
            "store",
            container_path,
            "--from",
            key,
            "--gas-prices",
            "0uterp",
            "--chain-id",
            chain.chain_id(),
            "--keyring-backend",
            "test",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )
    .await?;
    if v.get("code").and_then(|c| c.as_u64()).unwrap_or(999) != 0 {
        return Err(IctError::Config(format!("store {container_path}: {v}")));
    }
    wait_for_blocks(chain, 2).await?;
    let after = code_ids(chain).await?;
    after
        .into_iter()
        .find(|id| !before.contains(id))
        .ok_or_else(|| IctError::Config(format!("no new code_id after store {container_path}")))
}

async fn instantiate(
    chain: &dyn Chain,
    key: &str,
    code_id: &str,
    msg: &str,
    label: &str,
) -> Result<String> {
    let v = wasm_tx(
        chain,
        &[
            "tx",
            "wasm",
            "instantiate",
            code_id,
            msg,
            "--label",
            label,
            "--no-admin",
            "--from",
            key,
            "--gas-prices",
            "0uterp",
            "--chain-id",
            chain.chain_id(),
            "--keyring-backend",
            "test",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )
    .await?;
    if v.get("code").and_then(|c| c.as_u64()).unwrap_or(999) != 0 {
        return Err(IctError::Config(format!("instantiate {label}: {v}")));
    }
    wait_for_blocks(chain, 2).await?;
    let q = chain
        .chain_exec(&[
            "query",
            "wasm",
            "list-contract-by-code",
            code_id,
            "--output",
            "json",
        ])
        .await?;
    let j: serde_json::Value =
        serde_json::from_str(q.stdout_str().trim()).unwrap_or(serde_json::Value::Null);
    j.get("contracts")
        .and_then(|c| c.as_array())
        .and_then(|a| a.last())
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| IctError::Config(format!("no contract for {label} code {code_id}: {j}")))
}

async fn execute(chain: &dyn Chain, key: &str, contract: &str, msg: &str) -> Result<()> {
    let v = wasm_tx(
        chain,
        &[
            "tx",
            "wasm",
            "execute",
            contract,
            msg,
            "--from",
            key,
            "--gas-prices",
            "0uterp",
            "--chain-id",
            chain.chain_id(),
            "--keyring-backend",
            "test",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )
    .await?;
    if v.get("code").and_then(|c| c.as_u64()).unwrap_or(999) != 0 {
        return Err(IctError::Config(format!("execute {contract}: {v}")));
    }
    Ok(())
}

async fn smart_query(chain: &dyn Chain, contract: &str, msg: &str) -> (serde_json::Value, String) {
    let out = match chain
        .chain_exec(&[
            "query",
            "wasm",
            "contract-state",
            "smart",
            contract,
            msg,
            "--output",
            "json",
        ])
        .await
    {
        Ok(o) => o,
        Err(e) => return (serde_json::Value::Null, e.to_string()),
    };
    let stdout = out.stdout_str();
    let err = out.stderr_str();
    let v = serde_json::from_str(stdout.trim()).unwrap_or(serde_json::Value::Null);
    (v, format!("stdout={stdout} stderr={err}"))
}

/// Official ibc-hooks counter: missing key → `Counter not found`; first
/// `increment` for a new sender stores **0** (`None => 0`), second stores 1.
fn parsed_count(q: &serde_json::Value, raw: &str) -> Option<i64> {
    if raw.contains("Counter not found") {
        return None;
    }
    q.pointer("/data/count")
        .or_else(|| q.get("count"))
        .and_then(|c| {
            c.as_i64()
                .or_else(|| c.as_u64().map(|n| n as i64))
                .or_else(|| c.as_str().and_then(|s| s.parse().ok()))
        })
}

pub async fn query_txs_json(chain: &dyn Chain, min_height: u64) -> Result<serde_json::Value> {
    let q = format!("tx.height>={min_height}");
    let out = chain
        .chain_exec(&[
            "query",
            "txs",
            "--query",
            &q,
            "--limit",
            "50",
            "--page",
            "1",
            "--output",
            "json",
        ])
        .await?;
    let raw = out.stdout_str();
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({"txs": []}));
    }
    Ok(serde_json::from_str(raw.trim()).unwrap_or_else(|_| serde_json::json!({"txs": []})))
}

/// Handshake / packet IBC msgs on one chain since `min_height`.
pub async fn lifecycle_rows(
    chain: &dyn Chain,
    min_height: u64,
    path: &str,
    packets_on: &str,
) -> Result<Vec<HasherActionRow>> {
    let j = query_txs_json(chain, min_height).await?;
    Ok(rows_from_ibc_msgs(
        &collect_ibc_msgs(&j, min_height),
        path,
        packets_on,
    ))
}

async fn recv_gas(dst: &dyn Chain, min_height: u64) -> Result<u64> {
    let j = query_txs_json(dst, min_height).await?;
    Ok(first_of(&collect_ibc_msgs(&j, min_height), IbcMsgKind::RecvPacket)
        .map(|m| m.gas)
        .unwrap_or(0))
}

/// Expired ICS-20 packet → `MsgTimeout` (VerifyNonMembership on the wasm host).
pub async fn bench_timeout_packet(
    src: &dyn Chain,
    dst: &dyn Chain,
    relayer: &dyn Relayer,
    path: &str,
    src_ch: &str,
    src_key: &str,
    dest_addr: &str,
    packets_on: &str,
) -> Result<HasherActionRow> {
    let h0 = src.height().await.unwrap_or(1);
    let timeout_h = h0.saturating_add(2);
    let timeout_height = format!("1-{timeout_h}");
    let v = wasm_tx(
        src,
        &[
            "tx",
            "ibc-transfer",
            "transfer",
            "transfer",
            src_ch,
            dest_addr,
            "1000uterp",
            "--from",
            src_key,
            "--absolute-timeouts",
            "--packet-timeout-height",
            &timeout_height,
            "--packet-timeout-timestamp",
            "0",
            "--gas-prices",
            "0uterp",
            "--chain-id",
            src.chain_id(),
            "--keyring-backend",
            "test",
            "--gas",
            "auto",
            "--gas-adjustment",
            "2.0",
            "--broadcast-mode",
            "sync",
            "--output",
            "json",
            "-y",
        ],
    )
    .await;
    if let Err(e) = v {
        return Ok(HasherActionRow::fail(
            "Timeout",
            path,
            packets_on,
            format!("expired transfer: {e}"),
        ));
    }
    wait_for_blocks(src, 4).await?;
    wait_for_blocks(dst, 4).await?;
    let _ = relayer.flush(path, src_ch).await;
    wait_for_blocks(src, 2).await?;
    let j = query_txs_json(src, h0).await?;
    let gas = first_of(&collect_ibc_msgs(&j, h0), IbcMsgKind::Timeout)
        .map(|m| m.gas)
        .unwrap_or(0);
    if gas == 0 {
        return Ok(HasherActionRow::fail(
            "Timeout",
            path,
            packets_on,
            "no MsgTimeout in query txs (packet may still be in-flight)".into(),
        ));
    }
    Ok(HasherActionRow::ok("Timeout", path, packets_on, gas, 1))
}

fn recv_raw_log(txs: &serde_json::Value, min_height: u64) -> String {
    let samples = collect_ibc_msgs(txs, min_height);
    let Some(recv) = first_of(&samples, IbcMsgKind::RecvPacket) else {
        return "no RecvPacket".into();
    };
    let arr = txs
        .get("tx_responses")
        .or_else(|| txs.get("txs"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for tx in arr {
        let hash = tx
            .get("txhash")
            .or_else(|| tx.get("hash"))
            .and_then(|h| h.as_str())
            .unwrap_or("");
        if hash == recv.txhash {
            let log = tx
                .get("raw_log")
                .and_then(|l| l.as_str())
                .unwrap_or("");
            return log.chars().take(800).collect();
        }
    }
    format!("txhash={} (no raw_log)", recv.txhash)
}

/// IBC-hooks wasm memo increment on an existing transfer channel (A→C).
async fn ibc_hooks_wasm_sender(dst: &dyn Chain, dst_ch: &str, src_addr: &str) -> Result<String> {
    let out = dst
        .chain_exec(&["query", "ibchooks", "wasm-sender", dst_ch, src_addr])
        .await?;
    let stdout = out.stdout_str();
    let raw = stdout.trim();
    if raw.starts_with("terp1") || raw.starts_with("osmo1") {
        return Ok(raw.lines().next().unwrap_or(raw).to_string());
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
        if let Some(s) = v
            .as_str()
            .or_else(|| v.get("wasm_sender").and_then(|x| x.as_str()))
            .or_else(|| v.get("address").and_then(|x| x.as_str()))
        {
            return Ok(s.to_string());
        }
    }
    Err(IctError::Config(format!(
        "ibchooks wasm-sender {dst_ch} {src_addr}: {raw} stderr={}",
        out.stderr_str()
    )))
}

pub async fn bench_ibc_hooks(
    src: &dyn Chain,
    dst: &dyn Chain,
    relayer: &dyn Relayer,
    path: &str,
    src_ch: &str,
    dst_ch: &str,
    src_key: &str,
    src_addr: &str,
    dest_addr: &str,
) -> Result<HasherActionRow> {
    let wasm = contracts_dir().join("ibchooks_counter.wasm");
    if !wasm.is_file() {
        return Ok(HasherActionRow::fail(
            "ibc-hooks increment",
            "A→C",
            "08-wasm",
            format!("missing {}", wasm.display()),
        ));
    }
    let dest = put_host_wasm(dst, &wasm, "ibchooks_counter.wasm").await?;
    let code_id = store_new_code(dst, "validator", &dest).await?;
    let contract = instantiate(dst, "validator", &code_id, r#"{"count":0}"#, "ibchooks-counter").await?;
    wait_for_blocks(dst, 1).await?;
    let _ = dest_addr;
    let memo = serde_json::json!({
        "wasm": { "contract": contract, "msg": { "increment": {} } }
    });
    let h0 = dst.height().await.unwrap_or(1);
    // Receiver MUST equal wasm.contract or ibc-hooks rejects the memo.
    // Two packets: this counter's first increment for a new sender is 0, second is 1.
    for _ in 0..2 {
        src.send_ibc_transfer(
            src_ch,
            src_key,
            &WalletAmount {
                address: contract.clone(),
                denom: "uterp".into(),
                amount: 1000,
            },
            &TransferOptions {
                memo: Some(memo.to_string()),
                ..Default::default()
            },
        )
        .await?;
        relayer.flush(path, src_ch).await?;
        wait_for_blocks(dst, 2).await?;
    }
    let gas = recv_gas(dst, h0).await?;
    let hook_addr = ibc_hooks_wasm_sender(dst, dst_ch, src_addr).await?;
    let qmsg = format!(r#"{{"get_count":{{"addr":"{hook_addr}"}}}}"#);
    let (q, raw) = smart_query(dst, &contract, &qmsg).await;
    let wasm_n = parsed_count(&q, &raw);
    if wasm_n != Some(1) {
        let qh = format!("tx.height>={h0}");
        let out = dst
            .chain_exec(&[
                "query",
                "txs",
                "--query",
                &qh,
                "--limit",
                "20",
                "--page",
                "1",
                "--output",
                "json",
            ])
            .await;
        let raw_log = match out {
            Ok(o) => {
                let j: serde_json::Value =
                    serde_json::from_str(o.stdout_str().trim()).unwrap_or(serde_json::json!({}));
                recv_raw_log(&j, h0)
            }
            Err(e) => format!("txs query: {e}"),
        };
        return Ok(HasherActionRow::fail(
            "ibc-hooks increment",
            "A→C",
            "08-wasm",
            format!("wasm-sender={hook_addr} count={wasm_n:?} (want 1 after 2 packets) gas={gas} {raw} raw_log={raw_log}"),
        ));
    }
    let mut row = HasherActionRow::ok("ibc-hooks increment", "A→C", "08-wasm", gas, 2);
    row.note = format!("wasm-sender={hook_addr} count=1");
    Ok(row)
}

/// Packet-forward A→B→C on existing transfer channels.
pub async fn bench_pfm(
    a: &dyn Chain,
    b: &dyn Chain,
    c: &dyn Chain,
    relayer: &dyn Relayer,
    path_ab: &str,
    path_bc: &str,
    a_ch_to_b: &str,
    b_ch_to_c: &str,
    c_ch_from_b: &str,
    b_ch_from_a: &str,
    a_key: &str,
    b_recv: &str,
    c_dest: &str,
) -> Result<HasherActionRow> {
    let memo = serde_json::json!({
        "forward": {
            "receiver": c_dest,
            "port": "transfer",
            "channel": b_ch_to_c
        }
    });
    let denom = ibc_denom_multi_hop(
        &[("transfer", c_ch_from_b), ("transfer", b_ch_from_a)],
        "uterp",
    );
    let before = c.get_balance(c_dest, &denom).await.unwrap_or(0);
    let h0 = c.height().await.unwrap_or(1);
    a.send_ibc_transfer(
        a_ch_to_b,
        a_key,
        &WalletAmount {
            address: b_recv.to_string(),
            denom: "uterp".into(),
            amount: 1000,
        },
        &TransferOptions {
            memo: Some(memo.to_string()),
            ..Default::default()
        },
    )
    .await?;
    relayer.flush(path_ab, a_ch_to_b).await?;
    wait_for_blocks(b, 2).await?;
    relayer.flush(path_bc, b_ch_to_c).await?;
    wait_for_blocks(c, 2).await?;
    let after = c.get_balance(c_dest, &denom).await.unwrap_or(0);
    let gas = recv_gas(c, h0).await?;
    if after < before.saturating_add(1000) {
        return Ok(HasherActionRow::fail(
            "PFM A→B→C",
            "A→B→C",
            "mixed",
            format!("denom {denom} before={before} after={after} gas={gas}"),
        ));
    }
    Ok(HasherActionRow::ok("PFM A→B→C", "A→B→C", "mixed", gas, 1))
}

fn b64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

async fn find_port_channel(chain: &dyn Chain, port: &str) -> Result<String> {
    let v = ibc_wasm_path::query_channels_json(chain).await?;
    let arr = v
        .get("channels")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    for ch in arr {
        if ch.get("port_id").and_then(|p| p.as_str()) == Some(port) {
            if let Some(id) = ch.get("channel_id").and_then(|x| x.as_str()) {
                return Ok(id.to_string());
            }
        }
    }
    Err(IctError::Config(format!("no channel on port {port} in {v}")))
}

/// One-way polytone note(A) → voice(C) on the existing A-C connection.
pub async fn bench_polytone(
    a: &dyn Chain,
    c: &dyn Chain,
    relayer: &dyn Relayer,
    path_ac: &str,
    a_key: &str,
) -> Result<HasherActionRow> {
    let dir = contracts_dir();
    let note = dir.join("polytone_note.wasm");
    let voice = dir.join("polytone_voice.wasm");
    let proxy = dir.join("polytone_proxy.wasm");
    let tester = dir.join("polytone_tester.wasm");
    for p in [&note, &voice, &proxy, &tester] {
        if !p.is_file() {
            return Ok(HasherActionRow::fail(
                "polytone note→voice",
                "A→C",
                "08-wasm",
                format!("missing {}", p.display()),
            ));
        }
    }

    let note_p = put_host_wasm(a, &note, "polytone_note.wasm").await?;
    let tester_a_p = put_host_wasm(a, &tester, "polytone_tester.wasm").await?;
    let voice_p = put_host_wasm(c, &voice, "polytone_voice.wasm").await?;
    let proxy_p = put_host_wasm(c, &proxy, "polytone_proxy.wasm").await?;
    let tester_c_p = put_host_wasm(c, &tester, "polytone_tester.wasm").await?;

    let note_code = store_new_code(a, a_key, &note_p).await?;
    let tester_a_code = store_new_code(a, a_key, &tester_a_p).await?;
    let voice_code = store_new_code(c, "validator", &voice_p).await?;
    let proxy_code = store_new_code(c, "validator", &proxy_p).await?;
    let tester_c_code = store_new_code(c, "validator", &tester_c_p).await?;

    let note_addr = instantiate(
        a,
        a_key,
        &note_code,
        r#"{"block_max_gas":"100000000"}"#,
        "polytone-note",
    )
    .await?;
    let tester_a = instantiate(a, a_key, &tester_a_code, "{}", "polytone-tester").await?;
    let voice_init = format!(
        r#"{{"proxy_code_id":"{proxy_code}","block_max_gas":"100000000","contract_addr_len":32}}"#
    );
    let voice_addr =
        instantiate(c, "validator", &voice_code, &voice_init, "polytone-voice").await?;
    let tester_c = instantiate(c, "validator", &tester_c_code, "{}", "polytone-tester").await?;
    wait_for_blocks(a, 1).await?;

    let src_port = format!("wasm.{note_addr}");
    let dst_port = format!("wasm.{voice_addr}");
    relayer
        .create_channel(
            path_ac,
            &ChannelOptions {
                src_port: src_port.clone(),
                dst_port: dst_port.clone(),
                version: "polytone-1".into(),
                ..Default::default()
            },
        )
        .await?;
    wait_for_blocks(a, 2).await?;
    let poly_ch = find_port_channel(a, &src_port).await?;

    let hello_inner = b64(br#"{"hello":{"data":"aGVsbG8="}}"#);
    let callback_msg = b64(b"hello\n");
    let execute_msg = serde_json::json!({
        "execute": {
            "msgs": [{
                "wasm": {
                    "execute": {
                        "contract_addr": tester_c,
                        "msg": hello_inner,
                        "funds": []
                    }
                }
            }],
            "timeout_seconds": "600",
            "callback": { "receiver": tester_a, "msg": callback_msg }
        }
    });
    let h0 = c.height().await.unwrap_or(1);
    execute(a, a_key, &note_addr, &execute_msg.to_string()).await?;
    wait_for_blocks(a, 2).await?;
    relayer.flush(path_ac, &poly_ch).await?;
    wait_for_blocks(c, 2).await?;
    relayer.flush(path_ac, &poly_ch).await?;
    wait_for_blocks(a, 2).await?;

    let mut hello_ok = false;
    let mut cb_ok = false;
    for _ in 0..8 {
        relayer.flush(path_ac, &poly_ch).await?;
        wait_for_blocks(c, 1).await?;
        let (hello, _) = smart_query(c, &tester_c, r#"{"hello_history":{}}"#).await;
        let (cb, _) = smart_query(a, &tester_a, r#"{"history":{}}"#).await;
        hello_ok = hello
            .pointer("/data/history")
            .and_then(|v| v.as_array())
            .map(|h| !h.is_empty())
            .unwrap_or(false);
        cb_ok = cb
            .pointer("/data/history")
            .and_then(|v| v.as_array())
            .map(|h| !h.is_empty())
            .unwrap_or(false);
        if hello_ok && cb_ok {
            break;
        }
    }
    let gas = recv_gas(c, h0).await?;
    if !hello_ok {
        return Ok(HasherActionRow::fail(
            "polytone note→voice",
            "A→C",
            "08-wasm",
            format!("no hello_history gas={gas} ch={poly_ch}"),
        ));
    }
    let mut row = HasherActionRow::ok("polytone note→voice", "A→C", "08-wasm", gas, 1);
    row.note = format!("callback={} ch={poly_ch}", cb_ok);
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::parsed_count;

    #[test]
    fn hooks_counter_missing_is_none() {
        assert_eq!(
            parsed_count(&serde_json::Value::Null, "Counter not found"),
            None
        );
    }

    #[test]
    fn hooks_counter_first_increment_is_zero() {
        let q = serde_json::json!({"data": {"count": 0}});
        assert_eq!(parsed_count(&q, "stdout={\"data\":{\"count\":0}}"), Some(0));
    }

    #[test]
    fn hooks_counter_second_increment_is_one() {
        let q = serde_json::json!({"data": {"count": 1}});
        assert_eq!(parsed_count(&q, ""), Some(1));
    }
}
