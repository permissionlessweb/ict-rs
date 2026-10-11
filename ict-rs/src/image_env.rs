//! Runtime image identity. No hardcoded `local` / `local-zk` tag.
//!
//! Set `TERP_IMAGE_REPO` + `TERP_IMAGE_VERSION` (aliases: `ICT_IMAGE_REPO`,
//! `ICT_IMAGE_VERSION`) in the shell / Makefile before Docker suites.

use crate::runtime::DockerImage;

const DEFAULT_REPO: &str = "registry.terp.network/terp-core";

pub fn terp_image_repo() -> String {
    std::env::var("TERP_IMAGE_REPO")
        .or_else(|_| std::env::var("ICT_IMAGE_REPO"))
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

pub fn terp_image_version() -> String {
    let raw = std::env::var("TERP_IMAGE_VERSION")
        .or_else(|_| std::env::var("ICT_IMAGE_VERSION"))
        .unwrap_or_default();
    if raw == "local" || raw == "local-zk" {
        panic!(
            "TERP_IMAGE_VERSION={raw} is retired. Set a real tag (git sha, v6.1.0-dev, …). \
             Do not publish or pull a registry tag named local / local-zk."
        );
    }
    if !raw.is_empty() {
        return raw;
    }
    if std::env::var("ICT_MOCK").ok().as_deref() == Some("1") {
        return "unused-mock".to_string();
    }
    panic!(
        "set TERP_IMAGE_VERSION (or ICT_IMAGE_VERSION) before Docker runs; \
         the local / local-zk image tag is gone"
    );
}

pub fn terp_docker_image() -> DockerImage {
    DockerImage {
        repository: terp_image_repo(),
        version: terp_image_version(),
        uid_gid: None,
    }
}
