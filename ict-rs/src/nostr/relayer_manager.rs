//! NostrRelayerManager — manages a local Nostr relay Docker container for integration testing.
//!
//! Uses `RuntimeBackend` to pull, create, start, and clean up a Nostr relay container.
//! The relay exposes a NIP-01 WebSocket endpoint. Port auto-assignment is supported so
//! parallel test runs don't conflict.
//!
//! ## Usage
//!
//! ```ignore
//! use ict_rs::runtime::IctRuntime;
//! use ict_rs::nostr::NostrRelayerManager;
//!
//! let runtime = IctRuntime::Docker(Default::default()).into_backend().await.unwrap();
//! let mut relay = NostrRelayerManager::new(runtime, "my-test");
//! let ws_url = relay.start().await.unwrap();
//! // ... run tests ...
//! relay.stop().await.unwrap();
//! ```

use std::sync::Arc;

use tracing::{info, debug, warn};

use crate::error::Result;
use crate::runtime::{
    ContainerId, ContainerOptions, DockerImage, PortBinding, RuntimeBackend,
};

/// Manages a local Nostr relay Docker container for integration testing.
///
/// Creates a container from `mattn/nostr-relay:latest`, auto-assigns the host port,
/// and provides the WebSocket URL for clients to connect.
#[derive(Clone)]
pub struct NostrRelayerManager {
    runtime: Arc<dyn RuntimeBackend>,
    image: DockerImage,
    container_name: String,
    container_id: Option<ContainerId>,
    relay_port: u16,
    host_port: u16,
}

impl NostrRelayerManager {
    /// Default Nostr relay port inside the container.
    pub const DEFAULT_RELAY_PORT: u16 = 7777;

    /// Create a new manager. Does **not** start the container — call [`start`] for that.
    ///
    /// `runtime` is the ict-rs runtime backend (Docker, mock, etc.).
    /// `name` is used to derive the container name and should be unique per test.
    pub fn new(runtime: Arc<dyn RuntimeBackend>, name: &str) -> Self {
        Self {
            runtime,
            image: DockerImage {
                repository: "mattn/nostr-relay".to_string(),
                version: "latest".to_string(),
                uid_gid: None,
            },
            container_name: format!("nostr-relay-{}-{}", name, std::process::id()),
            container_id: None,
            relay_port: Self::DEFAULT_RELAY_PORT,
            host_port: 0,
        }
    }

    /// Create a new manager with a custom Docker image.
    pub fn with_image(
        runtime: Arc<dyn RuntimeBackend>,
        image: DockerImage,
        name: &str,
    ) -> Self {
        Self {
            runtime,
            image,
            container_name: format!("nostr-relay-{}-{}", name, std::process::id()),
            container_id: None,
            relay_port: Self::DEFAULT_RELAY_PORT,
            host_port: 0,
        }
    }

    /// Pull the image, create and start the container.
    ///
    /// Returns the WebSocket URL (e.g. `ws://127.0.0.1:37371`) that tests
    /// can pass to `NostrClient::connect()`.
    pub async fn start(&mut self) -> Result<String> {
        info!(
            container = %self.container_name,
            image = %self.image,
            "Pulling Nostr relay image"
        );
        self.runtime.pull_image(&self.image).await?;

        let opts = ContainerOptions {
            image: self.image.clone(),
            name: self.container_name.clone(),
            network_id: None,
            env: vec![],
            cmd: vec![],
            entrypoint: None,
            ports: vec![PortBinding {
                host_port: 0, // auto-assign to avoid port conflicts
                container_port: self.relay_port,
                protocol: "tcp".to_string(),
            }],
            volumes: vec![],
            labels: vec![
                ("ict-rs.nostr".to_string(), "true".to_string()),
            ],
            hostname: None,
        };

        let id = self.runtime.create_container(&opts).await?;
        self.runtime.start_container(&id).await?;
        self.container_id = Some(id.clone());
        debug!(container_id = %id.0, "Nostr relay container started");

        // Resolve the auto-assigned host port
        self.host_port = self.runtime
            .get_host_port(&id, self.relay_port, "tcp")
            .await?
            .unwrap_or(self.relay_port);

        let url = format!("ws://127.0.0.1:{}", self.host_port);
        info!(host_port = self.host_port, ws_url = %url, "Nostr relay ready");
        Ok(url)
    }

    /// The WebSocket URL clients use to connect to the relay.
    /// Only valid after [`start`] has been called.
    pub fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.host_port)
    }

    /// The host port the relay is bound to (auto-assigned).
    pub fn host_port(&self) -> u16 {
        self.host_port
    }

    /// Stop and remove the relay container. Idempotent — safe to call multiple times.
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(id) = self.container_id.take() {
            info!(container_id = %id.0, "Stopping Nostr relay container");
            if let Err(e) = self.runtime.stop_container(&id).await {
                warn!(error = %e, "Failed to stop Nostr relay container");
            }
            if let Err(e) = self.runtime.remove_container(&id).await {
                warn!(error = %e, "Failed to remove Nostr relay container");
            }
        }
        Ok(())
    }
}

impl Drop for NostrRelayerManager {
    fn drop(&mut self) {
        if self.container_id.is_some() {
            warn!(
                container = %self.container_name,
                "NostrRelayerManager dropped without calling stop() — container may leak. \
                 Call .stop().await before dropping or use a lifecycle wrapper."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::mock::MockBackend;

    #[tokio::test]
    async fn test_relayer_manager_create() {
        let runtime = Arc::new(MockBackend::new());
        let mut relay = NostrRelayerManager::new(runtime, "test-relayer");
        assert_eq!(relay.host_port, 0);

        let url = relay.start().await.unwrap();
        assert!(url.starts_with("ws://127.0.0.1:"), "URL should start with ws://127.0.0.1:<port>");
        assert!(relay.host_port > 0, "host_port should be set after start");

        relay.stop().await.unwrap();
        assert!(relay.container_id.is_none(), "container_id should be None after stop");
    }

    #[tokio::test]
    async fn test_relayer_ws_url() {
        let runtime = Arc::new(MockBackend::new());
        let mut relay = NostrRelayerManager::new(runtime, "ws-url-test");
        relay.start().await.unwrap();

        assert_eq!(relay.ws_url(), format!("ws://127.0.0.1:{}", relay.host_port()));
        relay.stop().await.unwrap();
    }
}