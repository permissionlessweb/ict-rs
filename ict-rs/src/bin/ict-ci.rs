//! Thin CI dispatcher: run prebuilt example binaries without a second cargo build.
//!
//! Mock suites need no Docker. Docker suites use TERP_IMAGE_*=local like polytone.
//!
//! Not dispatched (extra image / feature / live net): cosmos_upgrade, no_rick,
//! polytone_any_norick (zk image + any-proxy wasm + proof fixture),
//! headstash, lean_terpz, lean_production_e2e, bootstrap_mainnet, snapshot,
//! hashmerchant*, loyalty*, suite_wasm_smoke, ibc_transfer_e2e (dup).

use std::env;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

const MOCK_SUITES: &[&str] = &[
    "basic_cosmos",
    "integration_test",
    "ibc_hooks",
    "pfm",
    "circuit_deposit",
];
const DOCKER_SUITES: &[&str] = &["ibc_transfer", "polytone", "state_sync"];

fn all_suites() -> Vec<&'static str> {
    let mut v = Vec::from(MOCK_SUITES);
    v.extend_from_slice(DOCKER_SUITES);
    v
}

fn bin_dir() -> PathBuf {
    if let Ok(d) = env::var("ICT_CI_BIN_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            let examples = parent.join("examples");
            if examples.is_dir() {
                return examples;
            }
            return parent.to_path_buf();
        }
    }
    PathBuf::from("target/release/examples")
}

fn usage() {
    eprintln!("ict-ci list [mock|docker]");
    eprintln!("ict-ci run <suite>");
    eprintln!("ict-ci run-group <mock|docker>");
    eprintln!("mock:   {}", MOCK_SUITES.join(", "));
    eprintln!("docker: {}", DOCKER_SUITES.join(", "));
    eprintln!("ICT_CI_BIN_DIR={}", bin_dir().display());
}

fn group(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "mock" => Some(MOCK_SUITES),
        "docker" => Some(DOCKER_SUITES),
        _ => None,
    }
}

fn run_one(suite: &str) -> ExitCode {
    if !all_suites().iter().any(|s| *s == suite) {
        eprintln!("unknown suite {suite}");
        usage();
        return ExitCode::from(2);
    }
    let dir = bin_dir();
    let bin = dir.join(suite);
    if !bin.is_file() {
        eprintln!(
            "missing {} (build examples into ICT_CI_BIN_DIR)",
            bin.display()
        );
        return ExitCode::from(2);
    }
    eprintln!("ict-ci run {suite} ({})", bin.display());
    let extra: Vec<String> = env::args().skip(3).collect();
    let status = Command::new(&bin).args(&extra).envs(env::vars()).status();
    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => ExitCode::from(s.code().unwrap_or(1) as u8),
        Err(e) => {
            eprintln!("exec {}: {e}", bin.display());
            ExitCode::from(1)
        }
    }
}

fn run_group(name: &str) -> ExitCode {
    let Some(suites) = group(name) else {
        usage();
        return ExitCode::from(2);
    };
    for suite in suites {
        let code = run_one(suite);
        if code != ExitCode::SUCCESS {
            return code;
        }
    }
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("list") => {
            let suites = match args.next().as_deref() {
                Some(g) => match group(g) {
                    Some(s) => s.to_vec(),
                    None => {
                        usage();
                        return ExitCode::from(2);
                    }
                },
                None => all_suites(),
            };
            for s in suites {
                println!("{s}");
            }
            ExitCode::SUCCESS
        }
        Some("run") => {
            let Some(suite) = args.next() else {
                usage();
                return ExitCode::from(2);
            };
            run_one(&suite)
        }
        Some("run-group") => {
            let Some(g) = args.next() else {
                usage();
                return ExitCode::from(2);
            };
            run_group(&g)
        }
        _ => {
            usage();
            ExitCode::from(2)
        }
    }
}
