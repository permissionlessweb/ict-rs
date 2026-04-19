//! CLI binary for generating ict-rs module trait files from `.proto` definitions.
//!
//! Usage:
//! ```sh
//! cargo run -p ict-rs-codegen --bin generate -- \
//!   --proto-dir /path/to/terp-core/proto \
//!   --out-dir /path/to/ict-rs/src/modules
//! ```

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut proto_dir: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--proto-dir" => {
                i += 1;
                proto_dir = Some(PathBuf::from(&args[i]));
            }
            "--out-dir" => {
                i += 1;
                out_dir = Some(PathBuf::from(&args[i]));
            }
            "--help" | "-h" => {
                eprintln!("Usage: generate --proto-dir <DIR> --out-dir <DIR>");
                eprintln!();
                eprintln!("Discovers tx.proto and query.proto files recursively under --proto-dir,");
                eprintln!("parses them, and generates Rust trait files in --out-dir.");
                std::process::exit(0);
            }
            other => {
                eprintln!("Unknown argument: {other}");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let proto_dir = proto_dir.unwrap_or_else(|| {
        eprintln!("Error: --proto-dir is required");
        std::process::exit(1);
    });

    let out_dir = out_dir.unwrap_or_else(|| {
        eprintln!("Error: --out-dir is required");
        std::process::exit(1);
    });

    eprintln!("Proto dir: {}", proto_dir.display());
    eprintln!("Output dir: {}", out_dir.display());

    let modules = ict_rs_codegen::TextCodegenBuilder::new()
        .proto_dirs(&[&proto_dir])
        .out_dir(&out_dir)
        .generate()
        .unwrap_or_else(|e| {
            eprintln!("Error: {e}");
            std::process::exit(1);
        });

    eprintln!("Generated {} modules:", modules.len());
    for module in &modules {
        eprintln!("  - {module}.rs");
    }
    eprintln!("  - mod.rs");
}
