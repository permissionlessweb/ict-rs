//! Thin CI dispatcher: run prebuilt example binaries without a second cargo build.
//!
//! ```text
//! cargo build -p ict-rs --release --features docker,terp,testing \
//!   --example ibc_transfer --example polytone --bin ict-ci
//! ICT_CI_BIN_DIR=target/release/examples ict-ci list
//! ICT_CI_BIN_DIR=target/release/examples ict-ci run ibc_transfer
//! ```

use std::env;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

const SUITES: &[&str] = &[
    "ibc_transfer",
    "ibc_transfer_e2e",
    "polytone",
    "cosmos_upgrade",
    "suite_wasm_smoke",
    "basic_cosmos",
];

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
    eprintln!("ict-ci list");
    eprintln!("ict-ci run <suite>");
    eprintln!("suites: {}", SUITES.join(", "));
    eprintln!("ICT_CI_BIN_DIR={}", bin_dir().display());
}

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("list") => {
            for s in SUITES {
                println!("{s}");
            }
            ExitCode::SUCCESS
        }
        Some("run") => {
            let Some(suite) = args.next() else {
                usage();
                return ExitCode::from(2);
            };
            if !SUITES.contains(&suite.as_str()) {
                eprintln!("unknown suite {suite}");
                usage();
                return ExitCode::from(2);
            }
            let dir = bin_dir();
            let bin = dir.join(&suite);
            if !bin.is_file() {
                eprintln!(
                    "missing {} (build examples into ICT_CI_BIN_DIR)",
                    bin.display()
                );
                return ExitCode::from(2);
            }
            eprintln!("ict-ci run {suite} ({})", bin.display());
            let status = Command::new(&bin).args(args).envs(env::vars()).status();
            match status {
                Ok(s) if s.success() => ExitCode::SUCCESS,
                Ok(s) => ExitCode::from(s.code().unwrap_or(1) as u8),
                Err(e) => {
                    eprintln!("exec {}: {e}", bin.display());
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            usage();
            ExitCode::from(2)
        }
    }
}
