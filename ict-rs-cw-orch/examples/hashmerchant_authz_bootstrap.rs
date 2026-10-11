//! Authz grant: runtime account → temp HashMerchant.
//!
//! `GenericAuthorization` for `MsgGrantAllowance` and `MsgRevokeAllowance` only.
//! Signer is the runtime mnemonic. HashMerchant is the grantee, never the signer.
//! Addresses are derived from mnemonic files (do not type bech32).
//!
//! Dry-run (default):
//! ```sh
//! cargo run --example hashmerchant_authz_bootstrap -p ict-rs-cw-orch
//! ```
//!
//! Broadcast:
//! ```sh
//! AUTHZ_BROADCAST=1 cargo run --example hashmerchant_authz_bootstrap -p ict-rs-cw-orch
//! ```
//!
//! Mnemonics (groot2):
//! `~/.terp/onboard-runtime.mnemonic` and `~/.terp/hashmerchant.mnemonic`

use cw_orch::prelude::*;
use cw_orch_core::environment::{ChainInfoOwned, ChainKind, NetworkInfoOwned};
use cw_orch_traits::Stargate;
use dregg_svg_mint::hd::HdEngine;
use prost::Message;
use prost_types::Any;
use serde_json::json;
use std::path::PathBuf;
use terp_onboard_authz::{dao_authz_grants, OnboardRoles, AUTHZ_MSG_TYPES};

fn morocco_info() -> ChainInfoOwned {
    let grpc = std::env::var("TERP_MAIN_GRPC").unwrap_or_else(|_| "http://127.0.0.1:9090".into());
    let denom = std::env::var("GAS_DENOM").unwrap_or_else(|_| "uthiol".into());
    ChainInfoOwned {
        chain_id: std::env::var("ONBOARD_CHAIN").unwrap_or_else(|_| "morocco-1".into()),
        gas_denom: denom,
        gas_price: 0.05,
        grpc_urls: vec![grpc],
        lcd_url: Some(
            std::env::var("TERP_MAIN_LCD").unwrap_or_else(|_| "https://api.terp.network".into()),
        ),
        fcd_url: None,
        network_info: NetworkInfoOwned {
            chain_name: "terp".into(),
            pub_address_prefix: "terp".into(),
            coin_type: 118,
        },
        kind: ChainKind::Mainnet,
    }
}

fn home_file(name: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".terp").join(name)
}

fn read_mnemonic(path: &PathBuf) -> Result<String, String> {
    let s = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    let t = s.trim().to_string();
    if t.split_whitespace().count() < 12 {
        return Err(format!("{} does not look like a mnemonic", path.display()));
    }
    Ok(t)
}

#[derive(Clone, PartialEq, Message)]
struct GenericAuthorization {
    #[prost(string, tag = "1")]
    msg: String,
}

#[derive(Clone, PartialEq, Message)]
struct Grant {
    #[prost(message, optional, tag = "1")]
    authorization: Option<Any>,
    #[prost(message, optional, tag = "2")]
    expiration: Option<prost_types::Timestamp>,
}

#[derive(Clone, PartialEq, Message)]
struct MsgGrant {
    #[prost(string, tag = "1")]
    granter: String,
    #[prost(string, tag = "2")]
    grantee: String,
    #[prost(message, optional, tag = "3")]
    grant: Option<Grant>,
}

fn authz_grant_any(granter: &str, grantee: &str, msg_type: &str) -> Any {
    let authorization = Any {
        type_url: "/cosmos.authz.v1beta1.GenericAuthorization".into(),
        value: GenericAuthorization {
            msg: msg_type.to_string(),
        }
        .encode_to_vec(),
    };
    let msg = MsgGrant {
        granter: granter.to_string(),
        grantee: grantee.to_string(),
        grant: Some(Grant {
            authorization: Some(authorization),
            expiration: None,
        }),
    };
    Any {
        type_url: "/cosmos.authz.v1beta1.MsgGrant".into(),
        value: msg.encode_to_vec(),
    }
}

fn main() -> Result<(), String> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let runtime_mn_path = std::env::var("ONBOARD_RUNTIME_MNEMONIC")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_file("onboard-runtime.mnemonic"));
    let hm_mn_path = std::env::var("ONBOARD_HASHMERCHANT_MNEMONIC")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_file("hashmerchant.mnemonic"));

    let runtime_mn = read_mnemonic(&runtime_mn_path)?;
    let hm_mn = read_mnemonic(&hm_mn_path)?;
    let runtime = HdEngine::terp1_from_mnemonic(&runtime_mn)?;
    let hashmerchant = HdEngine::terp1_from_mnemonic(&hm_mn)?;
    if runtime == hashmerchant {
        return Err("collapsed keys: runtime mnemonic and hashmerchant mnemonic derived the same terp1".into());
    }

    let roles = OnboardRoles {
        runtime: runtime.clone(),
        hashmerchant: hashmerchant.clone(),
        chain_id: std::env::var("ONBOARD_CHAIN").unwrap_or_else(|_| "morocco-1".into()),
    };
    if !roles.is_sound() {
        return Err("OnboardRoles::is_sound failed".into());
    }
    let bundle = dao_authz_grants(&roles);
    for m in bundle["msgs"].as_array().unwrap() {
        if m["granter"] != runtime || m["grantee"] != hashmerchant {
            return Err("msg is not runtime→hashmerchant".into());
        }
    }

    let anys: Vec<Any> = AUTHZ_MSG_TYPES
        .iter()
        .map(|t| authz_grant_any(&runtime, &hashmerchant, t))
        .collect();

    let preview = json!({
        "ok": true,
        "chain": roles.chain_id,
        "grpc": morocco_info().grpc_urls,
        "runtime": runtime,
        "hashmerchant": hashmerchant,
        "authz_granter": runtime,
        "fee_granter": hashmerchant,
        "authorized_msgs": AUTHZ_MSG_TYPES,
        "msgs": bundle["msgs"],
        "broadcast": std::env::var("AUTHZ_BROADCAST").ok().as_deref() == Some("1"),
        "note": "runtime signs MsgGrant; HashMerchant is grantee only",
    });
    println!("{preview}");

    if std::env::var("AUTHZ_BROADCAST").ok().as_deref() != Some("1") {
        eprintln!("dry-run. AUTHZ_BROADCAST=1 to commit with cw-orch Daemon (runtime mnemonic).");
        return Ok(());
    }

    let mut builder = DaemonBuilder::new(morocco_info());
    builder.mnemonic(runtime_mn);
    let daemon = builder.build().map_err(|e| e.to_string())?;
    let sender = daemon.sender_addr().to_string();
    if sender != runtime {
        return Err(format!(
            "Daemon sender {sender} != derived runtime {runtime}"
        ));
    }
    let res = daemon.commit_any(anys, Some("runtime authz HashMerchant feegrant")).map_err(|e| e.to_string())?;
    println!(
        "{}",
        json!({
            "ok": true,
            "broadcast": true,
            "txhash": format!("{res:?}"),
            "runtime": runtime,
            "hashmerchant": hashmerchant,
        })
    );
    Ok(())
}
