// tests/nostr-sidecar-test.rs
// TDD RED test for nostr_relay_config() helper (Task 1 of 2026-05-21-nostr-sidecar-robust-client-tdd-plan.md)
// Per plan: asserts key fields for first-class SidecarProcess for local Nostr relay (mattn/nostr-relay on 7777).
// This test will fail to compile until GREEN impl (RED phase).
// cargo test --features nostr --test nostr-sidecar-test -- --nocapture

#![cfg(feature = "nostr")]
use ict_rs::cosmos::docker_sidecar::nostr_relay_config;
use ict_rs::chain::SidecarConfig;

#[test]
fn test_nostr_relay_config() {
    let config: SidecarConfig = nostr_relay_config();

    assert_eq!(config.name, "nostr-relay");
    assert_eq!(config.image.repository, "mattn/nostr-relay");
    assert_eq!(config.image.version, "latest");
    assert_eq!(config.ports, vec!["7777".to_string()]);
    assert!(!config.validator_process);
    assert!(!config.pre_start);
    assert_eq!(config.health_endpoint, None);
    // Additional fields per SidecarConfig and plan example (home_dir, env empty, cmd empty, ready_timeout)
    assert_eq!(config.home_dir, "/data".to_string());
    assert!(config.env.is_empty());
    assert!(config.cmd.is_empty());
    assert_eq!(config.ready_timeout_secs, 10);
}