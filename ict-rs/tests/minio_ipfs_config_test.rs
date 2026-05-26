// tests/minio_ipfs_config_test.rs
// Unit test for minio_ipfs_config() helper (analogous to nostr-sidecar-test.rs).
// Validates the SidecarConfig struct is properly constructed with correct
// ports, env vars, and health endpoint.
//
// Run: cargo test --test minio_ipfs_config_test -- --nocapture

use ict_rs::cosmos::docker_sidecar::minio_ipfs_config;
use ict_rs::chain::SidecarConfig;

#[test]
fn test_minio_ipfs_config_defaults() {
    let config: SidecarConfig = minio_ipfs_config(
        "testadmin",
        "testpass",
        "test-webhook-token",
        "test-bucket-one,test-bucket-two",
    );

    assert_eq!(config.name, "minio-ipfs");
    assert_eq!(config.image.repository, "minio-ipfs");
    assert_eq!(config.image.version, "latest");

    // Check required ports
    assert!(config.ports.contains(&"9000".to_string()), "MinIO S3 API port");
    assert!(config.ports.contains(&"9002".to_string()), "MinIO Console port");
    assert!(config.ports.contains(&"8081".to_string()), "IPFS Gateway port");
    assert!(config.ports.contains(&"9100".to_string()), "Webhook collector port");
    assert!(config.ports.contains(&"9443".to_string()), "Push daemon port");

    // Check env vars
    assert_eq!(config.env.len(), 4);
    assert!(config.env.contains(&(
        "MINIO_ROOT_USER".to_string(),
        "testadmin".to_string(),
    )));
    assert!(config.env.contains(&(
        "WEBHOOK_AUTH_TOKEN".to_string(),
        "test-webhook-token".to_string(),
    )));
    assert!(config.env.contains(&(
        "AUTOPIN_BUCKETS".to_string(),
        "test-bucket-one,test-bucket-two".to_string(),
    )));

    // Check lifecycle flags
    assert!(!config.validator_process, "minio-ipfs is per-chain, not per-validator");
    assert!(!config.pre_start, "minio-ipfs does not need to start before chain");

    // Check health endpoint points to MinIO health check
    assert_eq!(
        config.health_endpoint,
        Some("/minio/health/live".to_string()),
        "Health check should use MinIO /minio/health/live"
    );

    // Check timeout is adequate for container startup
    assert_eq!(config.ready_timeout_secs, 30,
               "30 second timeout for MinIO + IPFS initialization");

    // Home dir is /data
    assert_eq!(config.home_dir, "/data");
}

#[test]
fn test_minio_ipfs_config_empty_buckets() {
    // Should handle empty bucket list gracefully (no autopin)
    let config: SidecarConfig = minio_ipfs_config(
        "admin", "password", "token", "",
    );

    assert!(config.env.contains(&(
        "AUTOPIN_BUCKETS".to_string(),
        "".to_string(),
    )));
    // Even with empty buckets, all services (MinIO + IPFS + webhook) still start
    assert_eq!(config.ports.len(), 5, "All service ports present even with no buckets");
}

#[test]
fn test_minio_ipfs_config_different_credentials() {
    // Verify different credential sets create distinct configs
    let config_a = minio_ipfs_config("user1", "pass1", "tok1", "b1");
    let config_b = minio_ipfs_config("user2", "pass2", "tok2", "b2");

    assert_ne!(config_a.env, config_b.env,
              "Different credentials should produce different env vars");

    // But port structure should be identical
    assert_eq!(config_a.ports, config_b.ports,
               "Port mapping is invariant regardless of credentials");
}