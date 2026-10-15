//! Store and instantiate every terp-rs smart-account wasm on a terp-core image.
//!
//! ```sh
//! cargo run -p ict-rs --example smart_accounts --features docker,terp,testing
//! ```
//!
//! Wasm is read from `crates/terp-rs/artifacts/terp_*.wasm`. Image identity is
//! `TERP_IMAGE_REPO` / `TERP_IMAGE_VERSION`.

use std::path::PathBuf;

use ict_rs::cosmwasm::CosmWasmExt;
use ict_rs::prelude::*;
use ict_rs::testing::{setup_chain, TestEnv};

struct Account {
    wasm: &'static str,
    init: fn(&str) -> String,
    /// Contract-state query. None when the shipped wasm's instantiate does not
    /// persist the field the query reads (terp-dao leaves Item "dao" empty).
    query: Option<&'static str>,
    /// Substring the query JSON must contain (the sender address, or a marker).
    marker: fn(&str) -> String,
}

fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | data[i + 2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    if i < data.len() {
        let rem = data.len() - i;
        let mut n = (data[i] as u32) << 16;
        if rem == 2 {
            n |= (data[i + 1] as u32) << 8;
        }
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if rem == 2 {
            out.push(T[((n >> 6) & 63) as usize] as char);
            out.push('=');
        } else {
            out.push('=');
            out.push('=');
        }
    }
    out
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

fn init_ed25519(addr: &str) -> String {
    let pk = b64(&[0x11; 32]);
    format!(r#"{{"owner":"{addr}","pubkey":"{pk}"}}"#)
}

fn init_eth(addr: &str) -> String {
    format!(r#"{{"owner":"{addr}","signer":"0x1111111111111111111111111111111111111111"}}"#)
}

fn init_dao(addr: &str) -> String {
    format!(r#"{{"dao_addr":"{addr}"}}"#)
}

fn init_dregg(addr: &str) -> String {
    format!(r#"{{"owner":"{addr}","mode":"embedded_head"}}"#)
}

fn init_frost(addr: &str) -> String {
    format!(
        r#"{{"group_vk":"lab-vk","t":2,"n":3,"seats":["{addr}","desk","steward"]}}"#
    )
}

fn init_passkey(addr: &str) -> String {
    let mut pk = vec![0x02];
    pk.extend_from_slice(&[0x11; 32]);
    let cred = b64(b"lab-credential");
    let pk = b64(&pk);
    format!(
        r#"{{"owner":"{addr}","registration":{{"origin":"https://lab.terp.network","rp_id":"lab.terp.network","credential_id":"{cred}","public_key":"{pk}"}}}}"#
    )
}

fn init_recovery(addr: &str) -> String {
    format!(
        r#"{{"config":{{"guardians":[{{"address":"{addr}"}}],"threshold":1,"hash_alg":"sha256","rehash_rounds":1,"domain":"terp-recovery/break-glass/v1","allow_address_only_approvals":true}}}}"#
    )
}

fn init_vsck(addr: &str) -> String {
    format!(
        r#"{{"admin":"{addr}","dao":"{addr}","circuit_vks":{{"delegation_vk":"","vote_proof_vk":"","share_reveal_vk":""}}}}"#
    )
}

fn init_zkjwt(addr: &str) -> String {
    format!(r#"{{"admin":"{addr}","issuers":[],"require_registered_claim":false}}"#)
}

fn init_zktls(addr: &str) -> String {
    format!(r#"{{"owner":"{addr}"}}"#)
}

/// IETF BLS12-381 G1 generator, compressed. Host aggregate accepts this point.
fn init_bls(addr: &str) -> String {
    const G1: &str = "97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb";
    let pk = b64(&unhex(G1));
    format!(
        r#"{{"owner":"{addr}","operator_pubkeys":["{pk}"],"threshold":1,"variant":"min_pk"}}"#
    )
}

fn marker_addr(addr: &str) -> String {
    addr.to_string()
}

fn marker_embedded(_: &str) -> String {
    "embedded_head".into()
}

fn marker_lab(_: &str) -> String {
    "lab-vk".into()
}

fn marker_passkey(_: &str) -> String {
    "lab.terp.network".into()
}

fn marker_sha(_: &str) -> String {
    "sha256".into()
}

fn accounts() -> Vec<Account> {
    vec![
        Account {
            wasm: "terp_ed25519.wasm",
            init: init_ed25519,
            query: Some(r#"{"pubkey":{}}"#),
            marker: |_| b64(&[0x11; 32]),
        },
        Account {
            wasm: "terp_eth.wasm",
            init: init_eth,
            query: Some(r#"{"signer":{}}"#),
            marker: |_| "0x1111111111111111111111111111111111111111".into(),
        },
        Account {
            wasm: "terp_dao.wasm",
            init: init_dao,
            query: None,
            marker: marker_addr,
        },
        Account {
            wasm: "terp_dregg.wasm",
            init: init_dregg,
            query: Some(r#"{"config":{}}"#),
            marker: marker_embedded,
        },
        Account {
            wasm: "terp_frost.wasm",
            init: init_frost,
            query: Some(r#"{"group_vk":{}}"#),
            marker: marker_lab,
        },
        Account {
            wasm: "terp_passkey.wasm",
            init: init_passkey,
            query: Some(r#"{"registration":{}}"#),
            marker: marker_passkey,
        },
        Account {
            wasm: "terp_recovery.wasm",
            init: init_recovery,
            query: Some(r#"{"config":{}}"#),
            marker: marker_sha,
        },
        Account {
            wasm: "terp_vsck.wasm",
            init: init_vsck,
            query: Some(r#"{"config":{}}"#),
            marker: marker_addr,
        },
        Account {
            wasm: "terp_zkjwt.wasm",
            init: init_zkjwt,
            query: Some(r#"{"config":{}}"#),
            marker: marker_addr,
        },
        Account {
            wasm: "terp_zktls.wasm",
            init: init_zktls,
            query: Some(r#"{"get_all_epoch":{}}"#),
            marker: |_| "ids".into(),
        },
        Account {
            wasm: "terp_bls.wasm",
            init: init_bls,
            query: Some(r#"{"config":{}}"#),
            marker: |_| "min_pk".into(),
        },
    ]
}

fn artifacts() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let marker = "crates/terp-rs/artifacts/terp_ed25519.wasm";
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..8 {
        if dir.join(marker).is_file() {
            return Ok(dir.join("crates/terp-rs/artifacts"));
        }
        if !dir.pop() {
            break;
        }
    }
    Err("missing crates/terp-rs/artifacts/terp_ed25519.wasm".into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return Err("smart_accounts uses the terp-core image. Unset ICT_MOCK.".into());
    }
    let artifacts = artifacts()?;
    let mut tc = setup_chain("smart-accounts", TestEnv::terp_config()).await?;
    let outcome = run(&mut tc, &artifacts).await;
    if let Err(e) = tc.cleanup().await {
        eprintln!("cleanup: {e}");
    }
    outcome?;
    println!("smart_accounts passed");
    Ok(())
}

async fn run(
    tc: &mut ict_rs::testing::TestChain,
    artifacts: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    tc.chain.create_key("acct").await?;
    let addr = tc.chain.key_address("acct").await?;
    tc.chain
        .send_funds(
            "validator",
            &WalletAmount {
                address: addr.clone(),
                denom: "uterp".into(),
                amount: 50_000_000_000,
            },
        )
        .await?;

    for account in accounts() {
        let host = artifacts.join(account.wasm);
        if !host.is_file() {
            return Err(format!("missing {}", host.display()).into());
        }
        let dest = format!("/tmp/{}", account.wasm);
        ibc_wasm_path::put_file(&tc.chain, &dest, &std::fs::read(&host)?).await?;
        let code_id = tc
            .chain
            .store_code("acct", &dest)
            .await
            .map_err(|e| format!("store {}: {e}", account.wasm))?;
        let init = (account.init)(&addr);
        let label = account.wasm.trim_end_matches(".wasm");
        let contract = tc
            .chain
            .instantiate_contract("acct", &code_id, &init, label, Some(&addr))
            .await
            .map_err(|e| format!("instantiate {}: {e}", account.wasm))?;
        let info = tc
            .chain
            .chain_exec(&["query", "wasm", "contract", &contract, "--output", "json"])
            .await?;
        let info_raw = format!("{}{}", info.stdout_str(), info.stderr_str());
        if info.exit_code != 0 || !info_raw.contains(&code_id) {
            return Err(format!(
                "{} contract info missing code_id {code_id}: {info_raw}",
                account.wasm
            )
            .into());
        }
        if let Some(query) = account.query {
            let queried = tc
                .chain
                .query_contract(&contract, query)
                .await
                .map_err(|e| format!("query {}: {e}", account.wasm))?;
            let raw = queried.to_string();
            let marker = (account.marker)(&addr);
            if !raw.contains(&marker) {
                return Err(format!("{} query missing {marker}: {raw}", account.wasm).into());
            }
        }
        println!("ok {} code_id={code_id} {contract}", account.wasm);
    }
    Ok(())
}
