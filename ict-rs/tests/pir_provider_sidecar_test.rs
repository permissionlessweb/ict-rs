//! ict-rs sidecar for the re-zerve provider fork.
//!
//! Config test always runs. Live spawn (`PIR_SIDECAR_E2E=1`) starts
//! `akash-provider-pir:local` with `/var/run/docker.sock`, execs `pir serve`,
//! and checks the sibling lightweight container is running and serving HTTP.
//!
//!   PIR_SIDECAR_E2E=1 cargo test --test pir_provider_sidecar_test -- --nocapture

#![cfg(all(feature = "akash", feature = "docker"))]

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use ict_rs::cosmos::docker_sidecar::pir_provider_sidecar_config;
use ict_rs::runtime::docker::DockerBackend;
use ict_rs::runtime::{DockerConfig, RuntimeBackend, VolumeMount};
use ict_rs::sidecar::SidecarProcess;

const LEASE: &str = "lease-ict";
const LEASE_CONTAINER: &str = "pir-lease-lease-ict";
const MATCH_CONTAINER: &str = "pir-cw-commit-match-e2e";
const ASK: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const SESSION: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const SERVE_PORT: &str = "18766";

fn live_required() -> bool {
    std::env::var("PIR_SIDECAR_E2E").ok().as_deref() == Some("1")
}

fn host_docker(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("docker").args(args).output()
}

fn docker_ok() -> bool {
    host_docker(&["info"])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn image_ref() -> String {
    std::env::var("PIR_PROVIDER_IMAGE").unwrap_or_else(|_| "akash-provider-pir:local".into())
}

fn image_present() -> bool {
    let img = image_ref();
    host_docker(&["image", "inspect", &img])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct DockerRm(Vec<String>);

impl Drop for DockerRm {
    fn drop(&mut self) {
        let mut cmd = Command::new("docker");
        cmd.arg("rm").arg("-f");
        for n in &self.0 {
            cmd.arg(n);
        }
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = cmd.status();
    }
}

#[test]
fn pir_provider_sidecar_config_mounts_socket() {
    let cfg = pir_provider_sidecar_config();
    assert_eq!(cfg.name, "pir-provider");
    assert_eq!(cfg.cmd, vec!["pir".to_string(), "sidecar".to_string()]);
    assert!(cfg.ports.contains(&"8444".to_string()));
    assert_eq!(cfg.health_endpoint.as_deref(), Some("/health"));
    assert!(
        cfg.volumes
            .iter()
            .any(|v| v.source == "/var/run/docker.sock" && v.target == "/var/run/docker.sock"),
        "must mount docker.sock so pir serve can spawn sibling containers"
    );
    assert!(
        cfg.env.iter().any(|(k, _)| k == "PIR_REQUIRE_COMMIT"),
        "commit gate env must be forwarded"
    );
    assert!(
        cfg.env.iter().any(|(k, _)| k == "PIR_COMMIT_MATCH_URL"),
        "cw-pir-commit match URL must be forwarded"
    );
}

#[tokio::test]
async fn pir_sidecar_spawns_and_serves_lightweight_container() {
    if !docker_ok() {
        if live_required() {
            panic!("PIR_SIDECAR_E2E=1 but docker is not running");
        }
        eprintln!("skip: docker not running");
        return;
    }
    if !image_present() {
        if live_required() {
            panic!(
                "PIR_SIDECAR_E2E=1 but image {} is missing (build Dockerfile.pir)",
                image_ref()
            );
        }
        eprintln!("skip: {} not built", image_ref());
        return;
    }

    let sidecar_name = "pir-local-pir-provider-0-pir-sc-e2e";
    let _rm = DockerRm(vec![
        sidecar_name.to_string(),
        LEASE_CONTAINER.to_string(),
        MATCH_CONTAINER.to_string(),
    ]);
    let _ = host_docker(&["rm", "-f", sidecar_name, LEASE_CONTAINER, MATCH_CONTAINER]);

    let img = image_ref();
    let seal = host_docker(&[
        "run",
        "--rm",
        &img,
        "pir",
        "seal",
        "--ask-id",
        "ask-1",
        "--bidder",
        "prov",
        "--price",
        "9000",
        "--endpoint",
        "oob://p",
        "--session-key",
        SESSION,
    ])
    .expect("pir seal");
    let seal_out = String::from_utf8_lossy(&seal.stdout).trim().to_string();
    assert!(
        seal.status.success(),
        "pir seal failed: {seal_out} {}",
        String::from_utf8_lossy(&seal.stderr)
    );
    let seal_json: serde_json::Value =
        serde_json::from_str(&seal_out).expect("seal JSON");
    let bid = seal_json["bid_commitment"]
        .as_str()
        .expect("bid_commitment")
        .to_string();
    let bearer = seal_json["bearer"].as_str().expect("bearer").to_string();
    let envelope = serde_json::to_vec(&seal_json["envelope"]).expect("envelope bytes");
    let tmp = std::env::temp_dir().join("pir-sidecar-e2e");
    std::fs::create_dir_all(&tmp).expect("tmp");
    let envelope_host = tmp.join("envelope.json");
    std::fs::write(&envelope_host, envelope).expect("write envelope");

    let runtime: Arc<dyn RuntimeBackend> = Arc::new(
        DockerBackend::new(DockerConfig::default())
            .await
            .expect("docker backend"),
    );
    let _ = runtime.remove_networks_by_prefix("ict-pir-sidecar").await;
    let net = runtime
        .create_network("ict-pir-sidecar-e2e")
        .await
        .expect("network");

    let ask_e = format!("PIR_ASK_COMMIT={ASK}");
    let bid_e = format!("PIR_BID_COMMIT={bid}");
    let match_run = host_docker(&[
        "run",
        "-d",
        "--name",
        MATCH_CONTAINER,
        "--network",
        &net.0,
        "-e",
        &ask_e,
        "-e",
        &bid_e,
        &img,
        "pir",
        "match-serve",
        "--addr",
        ":8080",
    ])
    .expect("match-serve");
    assert!(
        match_run.status.success(),
        "match-serve start failed: {}{}",
        String::from_utf8_lossy(&match_run.stdout),
        String::from_utf8_lossy(&match_run.stderr)
    );
    let mut match_up = false;
    for _ in 0..40 {
        let insp = host_docker(&["inspect", "-f", "{{.State.Running}}", MATCH_CONTAINER])
            .expect("match inspect");
        if String::from_utf8_lossy(&insp.stdout).trim() == "true" {
            match_up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(match_up, "local cw-pir-commit match-serve must be running");
    std::thread::sleep(Duration::from_millis(250));

    // Sidecar e2e points at the local commit query (cw-pir-commit Match JSON).
    let match_url = format!("http://{MATCH_CONTAINER}:8080/match");
    let mut cfg = pir_provider_sidecar_config();
    cfg.env.retain(|(k, _)| {
        k != "PIR_ASK_COMMIT"
            && k != "PIR_BID_COMMIT"
            && k != "PIR_REQUIRE_COMMIT"
            && k != "PIR_COMMIT_MATCH_URL"
            && k != "PIR_BID_ENVELOPE_FILE"
            && k != "PIR_REQUIRE_ENVELOPE"
            && k != "PIR_SESSION_KEY"
    });
    cfg.env.push(("PIR_REQUIRE_COMMIT".into(), "1".into()));
    cfg.env.push(("PIR_REQUIRE_ENVELOPE".into(), "1".into()));
    cfg.env.push(("PIR_ASK_COMMIT".into(), ASK.into()));
    cfg.env.push(("PIR_BID_COMMIT".into(), bid.clone()));
    cfg.env.push(("PIR_COMMIT_MATCH_URL".into(), match_url));
    cfg.env.push((
        "PIR_BID_ENVELOPE_FILE".into(),
        "/tmp/pir/envelope.json".into(),
    ));
    cfg.env.push(("PIR_SESSION_KEY".into(), SESSION.into()));
    cfg.volumes.push(VolumeMount {
        source: envelope_host.to_string_lossy().into_owned(),
        target: "/tmp/pir/envelope.json".into(),
        read_only: true,
    });

    let mut sidecar = SidecarProcess::new(
        cfg,
        0,
        "pir-local",
        "pir-sc-e2e",
        runtime.clone(),
        net.clone(),
    );
    sidecar
        .create_container()
        .await
        .expect("create pir sidecar");
    sidecar.start_container().await.expect("start pir sidecar");

    let outcome = tokio::time::timeout(Duration::from_secs(120), async {
        let mut check_out = String::new();
        let mut allocated = false;
        for _ in 0..40 {
            let check = sidecar
                .exec(&["provider-services", "pir", "check"], &[])
                .await
                .expect("pir check");
            check_out = format!("{}{}", check.stdout_str(), check.stderr_str());
            if check.exit_code == 0
                && (check_out.contains("\"allocate\":true")
                    || check_out.contains("\"allocate\": true"))
            {
                allocated = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(
            allocated,
            "sidecar must allocate after ChaCha envelope + cw-pir-commit matches:true, got {check_out}"
        );
        assert!(
            check_out.contains("\"match_url\":true") || check_out.contains("\"match_url\": true"),
            "sidecar e2e must point PIR_COMMIT_MATCH_URL at the local commit query, got {check_out}"
        );
        assert!(
            check_out.contains("\"envelope\":true") || check_out.contains("\"envelope\": true"),
            "sidecar must ingest ChaCha envelope, got {check_out}"
        );
        assert!(
            check_out.contains("\"matches\":true") || check_out.contains("\"matches\": true"),
            "live cw-pir-commit Match JSON must be {{matches:true}}, got {check_out}"
        );

        let serve = sidecar
            .exec(
                &[
                    "provider-services",
                    "pir",
                    "serve",
                    "--lease",
                    LEASE,
                    "--bearer",
                    &bearer,
                    "--port",
                    SERVE_PORT,
                ],
                &[],
            )
            .await
            .expect("pir serve exec");
        let serve_out = format!("{}{}", serve.stdout_str(), serve.stderr_str());
        assert_eq!(serve.exit_code, 0, "pir serve failed: {serve_out}");
        assert!(
            serve_out.contains("\"served\":true")
                || serve_out.contains("\"served\": true")
                || serve_out.contains("\"running\":true"),
            "serve JSON must report served/running, got {serve_out}"
        );

        let inspect = host_docker(&[
            "inspect",
            "-f",
            "{{.State.Running}} {{.Config.Image}} {{.Name}}",
            LEASE_CONTAINER,
        ])
        .expect("docker inspect");
        let inspect_s = format!(
            "{}{}",
            String::from_utf8_lossy(&inspect.stdout),
            String::from_utf8_lossy(&inspect.stderr)
        );
        assert!(
            inspect.status.success(),
            "lease container must exist: {inspect_s}"
        );
        assert!(
            inspect_s.contains("true") && inspect_s.contains("python:3.12-alpine"),
            "lease container must be running python:3.12-alpine, got {inspect_s}"
        );

        let http = host_docker(&[
            "exec",
            LEASE_CONTAINER,
            "python",
            "-c",
            "import urllib.request; r=urllib.request.urlopen('http://127.0.0.1:8080/'); print(r.status)",
        ])
        .expect("docker exec http");
        let http_s = String::from_utf8_lossy(&http.stdout).trim().to_string();
        assert!(
            http.status.success() && http_s.contains("200"),
            "lease must serve HTTP 200, got status={} body={} err={}",
            http.status,
            http_s,
            String::from_utf8_lossy(&http.stderr)
        );

        let stop = sidecar
            .exec(&["provider-services", "pir", "stop", "--lease", LEASE], &[])
            .await
            .expect("pir stop");
        assert_eq!(
            stop.exit_code,
            0,
            "pir stop failed: {}{}",
            stop.stdout_str(),
            stop.stderr_str()
        );

        let gone = host_docker(&["inspect", LEASE_CONTAINER]).expect("inspect after stop");
        assert!(
            !gone.status.success(),
            "lease container must be gone after pir stop"
        );
        eprintln!("PIR_SIDECAR_SPAWN_SERVE_OK inspect={inspect_s} http={http_s}");
    })
    .await;

    let _ = sidecar
        .exec(&["provider-services", "pir", "stop", "--lease", LEASE], &[])
        .await;
    let _ = sidecar.stop_container().await;
    let _ = runtime.remove_network(&net).await;

    match outcome {
        Ok(()) => {}
        Err(_) => panic!("pir sidecar spawn/serve timed out"),
    }
}
