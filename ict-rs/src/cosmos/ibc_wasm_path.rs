//! A→C IBC path over an **08-wasm** client on C (SHA-256) of A (full BLAKE3).
//!
//! Native path: Hermes with `--wasm-checksum` (Informal PR #3943 / #3958,
//! image via `HERMES_IMAGE_*` / `scripts/ibc-ops/Dockerfile.hermes-wasm`)
//! wrapping ICS-07 Tendermint in 08-wasm. Stock Hermes 1.13.x cannot create
//! or decode 08-wasm clients. `HASHER_IBC_DIY_HANDSHAKE=1` keeps the signed
//! JSON ConnOpen/ChanOpen txs as a lab fallback.

use std::path::Path;

use serde_json::{json, Value};

use crate::chain::Chain;
use crate::error::{IctError, Result};
use crate::hasher_ibc::{encode_merkle_proof, to_hex};
use crate::ibc_wasm::{IbcWasmExt, IbcWasmStoreProposal};
use crate::interchain::wait_for_blocks;
use crate::tx::TxOptions;

const UNSIGNED: &str = "/tmp/ibc_unsigned.json";
const SIGNED: &str = "/tmp/ibc_signed.json";

pub fn gzip_wasm(src: &Path) -> Result<Vec<u8>> {
    let out = std::process::Command::new("gzip")
        .args(["-c", "-9"])
        .arg(src)
        .output()
        .map_err(|e| IctError::Config(format!("gzip: {e}")))?;
    if !out.status.success() {
        return Err(IctError::Config(format!(
            "gzip failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out.stdout)
}

pub async fn put_file(chain: &dyn Chain, dest: &str, bytes: &[u8]) -> Result<()> {
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    chain
        .exec(&["sh", "-c", &format!("rm -f '{dest}' '{dest}.b64'")], &[])
        .await?;
    for chunk in b64.as_bytes().chunks(32_768) {
        let s = std::str::from_utf8(chunk).unwrap_or("");
        chain
            .exec(
                &["sh", "-c", &format!("printf '%s' '{s}' >> '{dest}.b64'")],
                &[],
            )
            .await?;
    }
    let out = chain
        .exec(
            &[
                "sh",
                "-c",
                &format!("base64 -d '{dest}.b64' > '{dest}' && rm '{dest}.b64'"),
            ],
            &[],
        )
        .await?;
    if out.exit_code != 0 {
        return Err(IctError::ExecFailed {
            exit_code: out.exit_code,
            stderr: out.stderr_str(),
        });
    }
    Ok(())
}

pub async fn gov_store_gzipped_lc(
    chain: &dyn Chain,
    wasm_in_container: &str,
) -> Result<(u64, Vec<String>)> {
    let prop = IbcWasmStoreProposal {
        wasm_path: wasm_in_container.into(),
        title: "Store cw-ics08-wasm-terp".into(),
        summary: "08-wasm LC for BLAKE3 IAVL".into(),
        deposit: "10000000uterp".into(),
        authority: None,
        metadata: None,
    };
    let stored = chain
        .gov_store_ibc_wasm_lc("validator", &prop, 60)
        .await?;
    Ok((stored.proposal_id, stored.checksums))
}

pub async fn query_status_json(chain: &dyn Chain) -> Result<Value> {
    let out = chain.chain_exec(&["status", "--output", "json"]).await?;
    let raw = out.stdout_str();
    let raw = if raw.trim().is_empty() {
        out.stderr_str()
    } else {
        raw
    };
    serde_json::from_str(raw.trim()).map_err(|e| {
        IctError::Config(format!("status json: {e}\n{raw}"))
    })
}

pub fn status_height(status: &Value) -> Result<(u64, String, u64)> {
    let h = status
        .pointer("/sync_info/latest_block_height")
        .or_else(|| status.pointer("/SyncInfo/latest_block_height"))
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| v.as_u64())
        })
        .ok_or_else(|| IctError::Config("no latest_block_height".into()))?;
    let app = status
        .pointer("/sync_info/latest_app_hash")
        .or_else(|| status.pointer("/SyncInfo/latest_app_hash"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let time_ns = 1u64; // filled from header when updating
    Ok((h, app, time_ns))
}

fn b64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

pub async fn create_wasm_client(
    host: &dyn Chain,
    from: &str,
    checksum_hex: &str,
    inner_client: &Value,
    inner_consensus: &Value,
) -> Result<String> {
    let raw = checksum_hex.trim().trim_start_matches("0x");
    let checksum = hex::decode(raw).or_else(|_| {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, raw)
    }).map_err(|e| IctError::Config(format!("checksum decode: {e}")))?;
    if checksum.len() != 32 {
        return Err(IctError::Config(format!(
            "checksum want 32 bytes, got {}",
            checksum.len()
        )));
    }
    let cs = json!({
        "@type": "/ibc.lightclients.wasm.v1.ClientState",
        "data": b64(inner_client.to_string().as_bytes()),
        "checksum": b64(&checksum),
        "latest_height": inner_client["latest_height"],
    });
    let cons = json!({
        "@type": "/ibc.lightclients.wasm.v1.ConsensusState",
        "data": b64(inner_consensus.to_string().as_bytes()),
    });
    put_file(host, "/tmp/wasm_cs.json", cs.to_string().as_bytes()).await?;
    put_file(host, "/tmp/wasm_cons.json", cons.to_string().as_bytes()).await?;
    let opts = host.default_tx_opts().from(from).gas_adjustment(2.5);
    let out = host
        .chain_exec_tx_with(
            &[
                "tx",
                "ibc",
                "client",
                "create",
                "/tmp/wasm_cs.json",
                "/tmp/wasm_cons.json",
            ],
            opts,
        )
        .await?;
    if out.exit_code != 0 {
        return Err(IctError::Config(format!(
            "create wasm client: {}",
            trunc(&out.stderr_str(), 800)
        )));
    }
    wait_for_blocks(host, 2).await?;
    let txhash = json_txhash(&out.stdout_str())?;
    let q = host
        .chain_exec(&["query", "tx", &txhash, "--output", "json"])
        .await?;
    let v: Value = serde_json::from_str(q.stdout_str().trim()).unwrap_or(Value::Null);
    extract_attr(&v, "client_id")
        .or_else(|| extract_attr(&v, "client-id"))
        .ok_or_else(|| IctError::Config(format!("no client_id in create client: {v}")))
}

pub async fn update_wasm_client_json(
    host: &dyn Chain,
    from: &str,
    client_id: &str,
    header: &Value,
) -> Result<()> {
    let msg = json!({
        "@type": "/ibc.lightclients.wasm.v1.ClientMessage",
        "data": b64(header.to_string().as_bytes()),
    });
    put_file(host, "/tmp/wasm_hdr.json", msg.to_string().as_bytes()).await?;
    let opts = host.default_tx_opts().from(from).gas_adjustment(2.5);
    let out = host
        .chain_exec_tx_with(
            &["tx", "ibc", "client", "update", client_id, "/tmp/wasm_hdr.json"],
            opts,
        )
        .await?;
    if out.exit_code != 0 {
        return Err(IctError::Config(format!(
            "update wasm client: {}",
            trunc(&out.stderr_str(), 800)
        )));
    }
    let v: Value = serde_json::from_str(out.stdout_str().trim()).unwrap_or(Value::Null);
    if v["code"].as_u64().unwrap_or(1) != 0 {
        return Err(IctError::Config(format!(
            "update wasm client code!=0: {}",
            v["raw_log"].as_str().unwrap_or(&v.to_string())
        )));
    }
    wait_for_blocks(host, 1).await?;
    Ok(())
}

pub async fn broadcast_msg(chain: &dyn Chain, from: &str, msg: Value) -> Result<Value> {
    let unsigned = json!({
        "body": {
            "messages": [msg],
            "memo": "",
            "timeout_height": "0",
            "extension_options": [],
            "non_critical_extension_options": []
        },
        "auth_info": {
            "signer_infos": [],
            "fee": {"amount": [], "gas_limit": "800000", "payer": "", "granter": ""}
        },
        "signatures": []
    });
    put_file(chain, UNSIGNED, unsigned.to_string().as_bytes()).await?;
    let opts = chain.default_tx_opts().from(from);
    let signed = chain
        .chain_exec_tx_with(&["tx", "sign", UNSIGNED], opts.clone())
        .await?;
    if signed.exit_code != 0 {
        return Err(IctError::Config(format!(
            "tx sign: {}",
            trunc(&signed.stderr_str(), 800)
        )));
    }
    let signed_json = signed.stdout_str();
    put_file(chain, SIGNED, signed_json.as_bytes()).await?;
    let bc = chain
        .chain_exec_tx_with(&["tx", "broadcast", SIGNED], opts)
        .await?;
    if bc.exit_code != 0 {
        return Err(IctError::Config(format!(
            "tx broadcast: {}",
            trunc(&bc.stderr_str(), 800)
        )));
    }
    let v: Value = serde_json::from_str(bc.stdout_str().trim()).unwrap_or(Value::Null);
    if v["code"].as_u64().unwrap_or(1) != 0 {
        return Err(IctError::Config(format!(
            "broadcast code!=0: {}",
            v["raw_log"].as_str().unwrap_or(&v.to_string())
        )));
    }
    eprintln!(
        "broadcast ok txhash={} type={} log={}",
        v["txhash"].as_str().unwrap_or(""),
        msg.get("@type").and_then(|t| t.as_str()).unwrap_or(""),
        trunc(v["raw_log"].as_str().unwrap_or(""), 400)
    );
    wait_for_blocks(chain, 2).await?;
    Ok(v)
}

pub async fn merkle_of_ibc_key(chain: &dyn Chain, key: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    merkle_of_ibc_key_at(chain, key, None).await
}

pub async fn merkle_of_ibc_key_at(
    chain: &dyn Chain,
    key: &[u8],
    height: Option<u64>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let rpc = chain.host_rpc_address();
    let mut params = json!({
        "path": "/store/ibc/key",
        "data": to_hex(key),
        "prove": true
    });
    if let Some(h) = height {
        if h > 0 {
            params["height"] = json!(h.to_string());
        }
    }
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "abci_query",
        "params": params
    })
    .to_string();
    let out = tokio::process::Command::new("curl")
        .args([
            "-fsS",
            "--max-time",
            "15",
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
            &rpc,
        ])
        .output()
        .await
        .map_err(|e| IctError::Config(format!("abci curl: {e}")))?;
    if !out.status.success() {
        return Err(IctError::Config(format!(
            "abci curl fail: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| IctError::Config(format!("abci json: {e}")))?;
    let p = crate::hasher_ibc::parse_abci_store_proof("ibc", &v)?;
    let merkle = encode_merkle_proof(&p.iavl_proof, &p.simple_proof)?;
    Ok((merkle, p.value))
}

pub fn ibc_prefix_b64() -> String {
    b64(b"ibc")
}

fn json_txhash(raw: &str) -> Result<String> {
    let v: Value = serde_json::from_str(raw.trim()).map_err(|e| {
        IctError::Config(format!("tx json: {e}"))
    })?;
    v["txhash"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| IctError::Config("no txhash".into()))
}

fn extract_attr(v: &Value, key: &str) -> Option<String> {
    fn walk(v: &Value, key: &str, out: &mut Option<String>) {
        match v {
            Value::Object(m) => {
                if m.get("key").and_then(|k| k.as_str()) == Some(key) {
                    if let Some(s) = m.get("value").and_then(|x| x.as_str()) {
                        *out = Some(s.to_string());
                    }
                }
                for x in m.values() {
                    walk(x, key, out);
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk(x, key, out);
                }
            }
            _ => {}
        }
    }
    let mut found = None;
    walk(v, key, &mut found);
    found
}

fn trunc(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.len() <= n {
        t.to_string()
    } else {
        format!("{}…", &t[..n])
    }
}

pub fn revision_of(chain_id: &str) -> u64 {
    chain_id
        .rsplit('-')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

pub async fn bech32_addr(chain: &dyn Chain, key: &str) -> Result<String> {
    let out = chain
        .chain_exec(&["keys", "show", key, "-a", "--keyring-backend", "test"])
        .await?;
    let addr = out.stdout_str().trim().to_string();
    if addr.is_empty() {
        return Err(IctError::Config(format!("empty addr {key}")));
    }
    Ok(addr)
}

/// ConnOpenInit on `src` (TM client of dst) toward `wasm_client` on dst.
pub async fn conn_open_init(
    src: &dyn Chain,
    from: &str,
    src_client: &str,
    dst_wasm_client: &str,
) -> Result<String> {
    let signer = bech32_addr(src, from).await?;
    let msg = json!({
        "@type": "/ibc.core.connection.v1.MsgConnectionOpenInit",
        "client_id": src_client,
        "counterparty": {
            "client_id": dst_wasm_client,
            "connection_id": "",
            "prefix": {"key_prefix": ibc_prefix_b64()}
        },
        "version": {
            "identifier": "1",
            "features": ["ORDER_ORDERED", "ORDER_UNORDERED"]
        },
        "delay_period": "0",
        "signer": signer
    });
    let tx = broadcast_msg(src, from, msg).await?;
    Ok(extract_attr(&tx, "connection_id").unwrap_or_else(|| "connection-0".into()))
}

pub async fn header_json_for_update(
    chain: &dyn Chain,
    revision_number: u64,
) -> Result<Value> {
    let st = query_status_json(chain).await?;
    let (h, app, _) = status_height(&st)?;
    let app_hex = if app.chars().all(|c| c.is_ascii_hexdigit()) && app.len() == 64 {
        app.to_lowercase()
    } else {
        // base64 app hash from status
        let raw = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &app)
            .unwrap_or_default();
        to_hex(&raw)
    };
    Ok(json!({
        "root_hex": app_hex,
        "timestamp_ns": 1_000_000_000u64,
        "revision_number": revision_number,
        "revision_height": h,
        "chain_id": chain.chain_id(),
    }))
}

pub fn connection_key(id: &str) -> Vec<u8> {
    format!("connections/{id}").into_bytes()
}

pub fn client_state_key(id: &str) -> Vec<u8> {
    format!("clients/{id}/clientState").into_bytes()
}

pub fn consensus_state_key(id: &str, rev: u64, height: u64) -> Vec<u8> {
    format!("clients/{id}/consensusStates/{rev}-{height}").into_bytes()
}

pub fn packet_commitment_key(port: &str, channel: &str, seq: u64) -> Vec<u8> {
    format!("commitments/ports/{port}/channels/{channel}/sequences/{seq}").into_bytes()
}

pub async fn conn_open_try(
    dst: &dyn Chain,
    src: &dyn Chain,
    from: &str,
    dst_wasm_client: &str,
    src_client: &str,
    src_conn: &str,
    proof_height: u64,
    src_rev: u64,
) -> Result<String> {
    let signer = bech32_addr(dst, from).await?;
    let (proof_init, _) = merkle_of_ibc_key(src, &connection_key(src_conn)).await?;
    let (proof_client, _) = merkle_of_ibc_key(src, &client_state_key(src_client)).await?;
    // Consensus of C on A: A's TM client latest of C. Use proof_height as a stand-in
    // for the trusted consensus height stored on src_client — query it.
    let cons_h = query_client_latest_height(src, src_client).await.unwrap_or(proof_height);
    let dst_rev = revision_of(dst.chain_id());
    let (proof_cons, _) = match merkle_of_ibc_key(
        src,
        &consensus_state_key(src_client, dst_rev, cons_h),
    )
    .await
    {
        Ok(p) => p,
        Err(_) => {
            merkle_of_ibc_key(src, &consensus_state_key(src_client, 0, cons_h)).await?
        }
    };
    let msg = json!({
        "@type": "/ibc.core.connection.v1.MsgConnectionOpenTry",
        "client_id": dst_wasm_client,
        "counterparty": {
            "client_id": src_client,
            "connection_id": src_conn,
            "prefix": {"key_prefix": ibc_prefix_b64()}
        },
        "delay_period": "0",
        "counterparty_versions": [{
            "identifier": "1",
            "features": ["ORDER_ORDERED", "ORDER_UNORDERED"]
        }],
        "proof_height": {"revision_number": src_rev.to_string(), "revision_height": proof_height.to_string()},
        "proof_init": b64(&proof_init),
        "proof_client": b64(&proof_client),
        "proof_consensus": b64(&proof_cons),
        "consensus_height": {"revision_number": dst_rev.to_string(), "revision_height": cons_h.to_string()},
        "signer": signer
    });
    let tx = broadcast_msg(dst, from, msg).await?;
    Ok(extract_attr(&tx, "connection_id").unwrap_or_else(|| "connection-0".into()))
}

pub async fn conn_open_ack(
    src: &dyn Chain,
    dst: &dyn Chain,
    from: &str,
    src_conn: &str,
    dst_conn: &str,
    dst_wasm_client: &str,
    proof_height: u64,
    dst_rev: u64,
    src_rev: u64,
) -> Result<()> {
    let signer = bech32_addr(src, from).await?;
    let (proof_try, _) = merkle_of_ibc_key(dst, &connection_key(dst_conn)).await?;
    let (proof_client, _) = merkle_of_ibc_key(dst, &client_state_key(dst_wasm_client)).await?;
    let cons_h = query_client_latest_height(dst, dst_wasm_client)
        .await
        .unwrap_or(proof_height);
    let (proof_cons, _) = merkle_of_ibc_key(
        dst,
        &consensus_state_key(dst_wasm_client, src_rev, cons_h),
    )
    .await?;
    let msg = json!({
        "@type": "/ibc.core.connection.v1.MsgConnectionOpenAck",
        "connection_id": src_conn,
        "counterparty_connection_id": dst_conn,
        "version": {
            "identifier": "1",
            "features": ["ORDER_ORDERED", "ORDER_UNORDERED"]
        },
        "proof_height": {"revision_number": dst_rev.to_string(), "revision_height": proof_height.to_string()},
        "proof_try": b64(&proof_try),
        "proof_client": b64(&proof_client),
        "proof_consensus": b64(&proof_cons),
        "consensus_height": {"revision_number": src_rev.to_string(), "revision_height": cons_h.to_string()},
        "signer": signer
    });
    broadcast_msg(src, from, msg).await?;
    Ok(())
}

pub async fn conn_open_confirm(
    dst: &dyn Chain,
    src: &dyn Chain,
    from: &str,
    dst_conn: &str,
    src_conn: &str,
    proof_height: u64,
    src_rev: u64,
) -> Result<()> {
    let signer = bech32_addr(dst, from).await?;
    let (proof_ack, _) = merkle_of_ibc_key(src, &connection_key(src_conn)).await?;
    let msg = json!({
        "@type": "/ibc.core.connection.v1.MsgConnectionOpenConfirm",
        "connection_id": dst_conn,
        "proof_ack": b64(&proof_ack),
        "proof_height": {"revision_number": src_rev.to_string(), "revision_height": proof_height.to_string()},
        "signer": signer
    });
    broadcast_msg(dst, from, msg).await?;
    Ok(())
}

pub async fn chan_open_init(
    src: &dyn Chain,
    from: &str,
    src_conn: &str,
) -> Result<String> {
    let signer = bech32_addr(src, from).await?;
    let msg = json!({
        "@type": "/ibc.core.channel.v1.MsgChannelOpenInit",
        "port_id": "transfer",
        "channel": {
            "state": "STATE_INIT",
            "ordering": "ORDER_UNORDERED",
            "counterparty": {"port_id": "transfer", "channel_id": ""},
            "connection_hops": [src_conn],
            "version": "ics20-1"
        },
        "signer": signer
    });
    let tx = broadcast_msg(src, from, msg).await?;
    Ok(extract_attr(&tx, "channel_id").unwrap_or_else(|| "channel-0".into()))
}

pub fn channel_key(port: &str, channel: &str) -> Vec<u8> {
    format!("channelEnds/ports/{port}/channels/{channel}").into_bytes()
}

pub async fn chan_open_try(
    dst: &dyn Chain,
    src: &dyn Chain,
    from: &str,
    dst_conn: &str,
    src_channel: &str,
    proof_height: u64,
    src_rev: u64,
) -> Result<String> {
    let signer = bech32_addr(dst, from).await?;
    let (proof_init, _) = merkle_of_ibc_key(src, &channel_key("transfer", src_channel)).await?;
    let msg = json!({
        "@type": "/ibc.core.channel.v1.MsgChannelOpenTry",
        "port_id": "transfer",
        "channel": {
            "state": "STATE_TRYOPEN",
            "ordering": "ORDER_UNORDERED",
            "counterparty": {"port_id": "transfer", "channel_id": src_channel},
            "connection_hops": [dst_conn],
            "version": "ics20-1"
        },
        "counterparty_version": "ics20-1",
        "proof_init": b64(&proof_init),
        "proof_height": {"revision_number": src_rev.to_string(), "revision_height": proof_height.to_string()},
        "signer": signer
    });
    let tx = broadcast_msg(dst, from, msg).await?;
    Ok(extract_attr(&tx, "channel_id").unwrap_or_else(|| "channel-0".into()))
}

pub async fn chan_open_ack(
    src: &dyn Chain,
    dst: &dyn Chain,
    from: &str,
    src_channel: &str,
    dst_channel: &str,
    proof_height: u64,
    dst_rev: u64,
) -> Result<()> {
    let signer = bech32_addr(src, from).await?;
    let (proof_try, _) = merkle_of_ibc_key(dst, &channel_key("transfer", dst_channel)).await?;
    let msg = json!({
        "@type": "/ibc.core.channel.v1.MsgChannelOpenAck",
        "port_id": "transfer",
        "channel_id": src_channel,
        "counterparty_channel_id": dst_channel,
        "counterparty_version": "ics20-1",
        "proof_try": b64(&proof_try),
        "proof_height": {"revision_number": dst_rev.to_string(), "revision_height": proof_height.to_string()},
        "signer": signer
    });
    broadcast_msg(src, from, msg).await?;
    Ok(())
}

pub async fn chan_open_confirm(
    dst: &dyn Chain,
    src: &dyn Chain,
    from: &str,
    dst_channel: &str,
    src_channel: &str,
    proof_height: u64,
    src_rev: u64,
) -> Result<()> {
    let signer = bech32_addr(dst, from).await?;
    let (proof_ack, _) = merkle_of_ibc_key(src, &channel_key("transfer", src_channel)).await?;
    let msg = json!({
        "@type": "/ibc.core.channel.v1.MsgChannelOpenConfirm",
        "port_id": "transfer",
        "channel_id": dst_channel,
        "proof_ack": b64(&proof_ack),
        "proof_height": {"revision_number": src_rev.to_string(), "revision_height": proof_height.to_string()},
        "signer": signer
    });
    broadcast_msg(dst, from, msg).await?;
    Ok(())
}

pub async fn recv_packet(
    dst: &dyn Chain,
    src: &dyn Chain,
    from: &str,
    packet: &Value,
    src_port: &str,
    src_channel: &str,
    seq: u64,
    proof_height: u64,
    src_rev: u64,
) -> Result<Value> {
    let signer = bech32_addr(dst, from).await?;
    let (proof, _) = merkle_of_ibc_key_at(
        src,
        &packet_commitment_key(src_port, src_channel, seq),
        Some(proof_height),
    )
    .await?;
    let msg = json!({
        "@type": "/ibc.core.channel.v1.MsgRecvPacket",
        "packet": packet,
        "proof_commitment": b64(&proof),
        "proof_height": {
            "revision_number": src_rev.to_string(),
            "revision_height": proof_height.to_string()
        },
        "signer": signer
    });
    broadcast_msg(dst, from, msg).await
}

pub async fn query_channels_json(chain: &dyn Chain) -> Result<Value> {
    let out = chain
        .chain_exec(&["query", "ibc", "channel", "channels", "--output", "json"])
        .await?;
    serde_json::from_str(out.stdout_str().trim()).map_err(|e| {
        IctError::Config(format!("channels json: {e}"))
    })
}

fn newest_prefixed_id(v: &Value, field: &str, prefix: &str) -> Option<String> {
    let mut best: Option<(u64, String)> = None;
    fn walk(
        v: &Value,
        field: &str,
        prefix: &str,
        best: &mut Option<(u64, String)>,
    ) {
        match v {
            Value::Object(m) => {
                if let Some(id) = m.get(field).and_then(|x| x.as_str()) {
                    if let Some(n) = id.strip_prefix(prefix).and_then(|s| s.parse().ok()) {
                        match *best {
                            Some((b, _)) if b >= n => {}
                            _ => *best = Some((n, id.to_string())),
                        }
                    }
                }
                for x in m.values() {
                    walk(x, field, prefix, best);
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk(x, field, prefix, best);
                }
            }
            _ => {}
        }
    }
    walk(v, field, prefix, &mut best);
    best.map(|(_, id)| id)
}

/// Highest `channel-N` on this chain (after a handshake tx).
pub async fn newest_wasm_client_id(chain: &dyn Chain) -> Result<String> {
    let out = chain
        .chain_exec(&["query", "ibc", "client", "states", "--output", "json"])
        .await?;
    let v: Value = serde_json::from_str(out.stdout_str().trim()).unwrap_or(Value::Null);
    newest_prefixed_id(&v, "client_id", "08-wasm-")
        .ok_or_else(|| IctError::Config(format!("no 08-wasm client_id in {v}")))
}

/// Highest local `channel-N`. Ignores nested `counterparty.channel_id`.
pub fn newest_local_channel_id(v: &Value) -> Option<String> {
    let arr = v.get("channels").and_then(|c| c.as_array())?;
    let mut best: Option<(u64, String)> = None;
    for ch in arr {
        let Some(id) = ch.get("channel_id").and_then(|x| x.as_str()) else {
            continue;
        };
        let Some(n) = id.strip_prefix("channel-").and_then(|s| s.parse().ok()) else {
            continue;
        };
        match best {
            Some((b, _)) if b >= n => {}
            _ => best = Some((n, id.to_string())),
        }
    }
    best.map(|(_, id)| id)
}

pub async fn newest_channel_id(chain: &dyn Chain) -> Result<String> {
    let v = query_channels_json(chain).await?;
    newest_local_channel_id(&v)
        .ok_or_else(|| IctError::Config(format!("no local channel_id in {v}")))
}

pub async fn newest_connection_id(chain: &dyn Chain) -> Result<String> {
    let out = chain
        .chain_exec(&["query", "ibc", "connection", "connections", "--output", "json"])
        .await?;
    let v: Value = serde_json::from_str(out.stdout_str().trim()).unwrap_or(Value::Null);
    newest_prefixed_id(&v, "id", "connection-")
        .or_else(|| newest_prefixed_id(&v, "connection_id", "connection-"))
        .ok_or_else(|| IctError::Config(format!("no connection_id in {v}")))
}

pub async fn channel_state(chain: &dyn Chain, port: &str, channel: &str) -> Result<String> {
    let out = chain
        .chain_exec(&[
            "query",
            "ibc",
            "channel",
            "end",
            port,
            channel,
            "--output",
            "json",
        ])
        .await?;
    let raw = out.stdout_str();
    let v: Value = serde_json::from_str(raw.trim()).unwrap_or(Value::Null);
    v.pointer("/channel/state")
        .or_else(|| v.pointer("/channel/State"))
        .or_else(|| v.get("state"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            IctError::Config(format!(
                "no channel state for {port}/{channel}: json={v} stdout={raw}"
            ))
        })
}

pub async fn query_client_latest_height(chain: &dyn Chain, client_id: &str) -> Result<u64> {
    let out = chain
        .chain_exec(&[
            "query",
            "ibc",
            "client",
            "state",
            client_id,
            "--output",
            "json",
        ])
        .await?;
    let v: Value = serde_json::from_str(out.stdout_str().trim()).unwrap_or(Value::Null);
    v.pointer("/client_state/latest_height/revision_height")
        .or_else(|| v.pointer("/client_state/latestHeight/revisionHeight"))
        .and_then(|x| {
            x.as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| x.as_u64())
        })
        .ok_or_else(|| IctError::Config(format!("no latest_height for {client_id}: {v}")))
}

#[allow(dead_code)]
fn _opts_ty(_: TxOptions) {}
