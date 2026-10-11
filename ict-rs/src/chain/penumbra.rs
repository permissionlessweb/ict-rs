//! Local Penumbra devnet support.
//!
//! `examples/penumbra_kms_local.rs` runs `scripts/penumbra_kms_local.sh`:
//! `pd network generate`, consensus key removed from the node, CometBFT 0.37
//! kms dials `127.0.0.1:${PRIVVAL_PORT}` (default 27659). Refuses rotko /
//! penumbra.zone / penumbra-1 join URLs.
//!
//! `examples/penumbra_terp_relay.rs` keeps that pd, adds a Docker Terp node,
//! and points the Rotko Hermes fork at local pd gRPC + CometBFT RPC for a
//! round-trip: ICS-20 terp → pd, then `pcli tx withdraw` back. Still refuses
//! public join URLs.

/// Marker checked by the unit test so the public-network refusal stays in the script.
pub const LOCAL_KMS_SCRIPT: &str = include_str!("../../../scripts/penumbra_kms_local.sh");

/// Terp ↔ local pd packet-relay suite (Rotko Hermes, host pd).
pub const LOCAL_RELAY_SCRIPT: &str = include_str!("../../../scripts/penumbra_terp_relay.sh");

#[cfg(test)]
mod tests {
    use super::{LOCAL_KMS_SCRIPT, LOCAL_RELAY_SCRIPT};

    #[test]
    fn local_kms_suite_is_loopback_only() {
        assert!(LOCAL_KMS_SCRIPT.contains("refusing public penumbra endpoint"));
        assert!(LOCAL_KMS_SCRIPT.contains("PRIVVAL_PORT:-27659"));
        assert!(LOCAL_KMS_SCRIPT.contains("rotko"));
        assert!(!LOCAL_KMS_SCRIPT.contains("pd network join"));
        assert!(LOCAL_KMS_SCRIPT.contains("KEEP_RUNNING"));
        assert!(LOCAL_KMS_SCRIPT.contains("allocation-address"));
    }

    #[test]
    fn local_relay_suite_points_hermes_at_local_pd() {
        assert!(LOCAL_RELAY_SCRIPT.contains("refusing public penumbra endpoint"));
        assert!(LOCAL_RELAY_SCRIPT.contains("type = 'Penumbra'"));
        assert!(LOCAL_RELAY_SCRIPT.contains("kms_config"));
        assert!(LOCAL_RELAY_SCRIPT.contains("view_service_storage_dir"));
        assert!(LOCAL_RELAY_SCRIPT.contains("create channel"));
        assert!(LOCAL_RELAY_SCRIPT.contains("ibc-transfer"));
        assert!(LOCAL_RELAY_SCRIPT.contains("pcli tx withdraw"));
        assert!(LOCAL_RELAY_SCRIPT.contains("compat_mode = '0.37'"));
        assert!(LOCAL_RELAY_SCRIPT.contains("HERMES_BIN"));
        assert!(!LOCAL_RELAY_SCRIPT.contains("pd network join"));
        assert!(!LOCAL_RELAY_SCRIPT.contains("hermes keys add --chain penumbra"));
        assert!(!LOCAL_RELAY_SCRIPT.contains("hermes tx ft-transfer"));
    }
}
