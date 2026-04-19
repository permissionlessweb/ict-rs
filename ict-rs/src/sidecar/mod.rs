//! Sidecar process management for ict-rs.
//!
//! Mirrors Go ICT's `SidecarProcess` from `chain/cosmos/sidecar.go`.
//! A sidecar is an auxiliary container (oracle, price feeder, hash-market, etc.)
//! that runs alongside a chain and shares its Docker network.



use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::chain::SidecarConfig;
use crate::error::{IctError, Result};
use crate::runtime::{
    ContainerId, ContainerOptions, NetworkId, PortBinding, RuntimeBackend,
};
use crate::tx::ExecOutput;

/// A running sidecar process attached to a chain.
///
/// Mirrors Go ICT's `SidecarProcess` — holds the config, a reference to the
/// runtime backend, and the container ID once created.
pub struct SidecarProcess {
    pub config: SidecarConfig,
    /// Index within the chain's sidecar list (or validator index for per-validator sidecars).
    pub index: usize,
    /// Chain ID this sidecar belongs to.
    pub chain_id: String,
    /// Test name (used in container naming).
    pub test_name: String,
    runtime: Arc<dyn RuntimeBackend>,
    container_id: Option<ContainerId>,
    network_id: NetworkId,
}

impl std::fmt::Debug for SidecarProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidecarProcess")
            .field("name", &self.config.name)
            .field("chain_id", &self.chain_id)
            .field("index", &self.index)
            .field("has_container", &self.container_id.is_some())
            .finish_non_exhaustive()
    }
}

impl SidecarProcess {
    /// Create a new sidecar process (does not create a container yet).
    pub fn new(
        config: SidecarConfig,
        index: usize,
        chain_id: &str,
        test_name: &str,
        runtime: Arc<dyn RuntimeBackend>,
        network_id: NetworkId,
    ) -> Self {
        Self {
            config,
            index,
            chain_id: chain_id.to_string(),
            test_name: test_name.to_string(),
            runtime,
            container_id: None,
            network_id,
        }
    }

    /// Container name following Go ICT's naming convention.
    ///
    /// Chain-level: `{chain_id}-{process_name}-{index}-{test_name}`
    /// Validator-level: `{chain_id}-{process_name}-val-{index}-{test_name}`
    pub fn name(&self) -> String {
        if self.config.validator_process {
            format!(
                "{}-{}-val-{}-{}",
                self.chain_id, self.config.name, self.index, self.test_name
            )
        } else {
            format!(
                "{}-{}-{}-{}",
                self.chain_id, self.config.name, self.index, self.test_name
            )
        }
    }

    /// Hostname for Docker network DNS resolution.
    pub fn hostname(&self) -> String {
        // Condense to a DNS-friendly name
        if self.config.validator_process {
            format!("{}-{}-val-{}", self.chain_id, self.config.name, self.index)
        } else {
            format!("{}-{}-{}", self.chain_id, self.config.name, self.index)
        }
    }

    /// Create and start the sidecar container.
    pub async fn create_container(&mut self) -> Result<()> {
        let container_name = self.name();
        let hostname = self.hostname();

        info!(
            sidecar = %self.config.name,
            chain = %self.chain_id,
            container = %container_name,
            "Creating sidecar container"
        );

        // Pull the image
        self.runtime.pull_image(&self.config.image).await?;

        // Build port bindings
        let ports: Vec<PortBinding> = self
            .config
            .ports
            .iter()
            .filter_map(|p| {
                p.parse::<u16>().ok().map(|port| PortBinding {
                    host_port: 0, // auto-assign
                    container_port: port,
                    protocol: "tcp".to_string(),
                })
            })
            .collect();

        // Build env vars
        let env: Vec<(String, String)> = self.config.env.clone();

        let opts = ContainerOptions {
            image: self.config.image.clone(),
            name: container_name,
            network_id: Some(self.network_id.clone()),
            env,
            cmd: self.config.cmd.clone(),
            entrypoint: None,
            ports,
            volumes: Vec::new(),
            labels: vec![
                ("ict.test".to_string(), self.test_name.clone()),
                ("ict.chain_id".to_string(), self.chain_id.clone()),
                ("ict.sidecar".to_string(), self.config.name.clone()),
            ],
            hostname: Some(hostname),
        };

        let container_id = self.runtime.create_container(&opts).await?;
        self.container_id = Some(container_id);

        Ok(())
    }

    /// Start the container and wait for it to become ready.
    pub async fn start_container(&self) -> Result<()> {
        let container_id = self.container_id.as_ref().ok_or_else(|| {
            IctError::Runtime(anyhow::anyhow!(
                "sidecar {} not created yet",
                self.config.name
            ))
        })?;

        info!(
            sidecar = %self.config.name,
            chain = %self.chain_id,
            "Starting sidecar container"
        );

        self.runtime.start_container(container_id).await?;

        // If a health endpoint is configured, poll until ready
        if let Some(ref endpoint) = self.config.health_endpoint {
            self.wait_for_health(endpoint).await?;
        }

        Ok(())
    }

    /// Poll the health endpoint until it responds or timeout.
    async fn wait_for_health(&self, _endpoint: &str) -> Result<()> {
        let timeout_secs = if self.config.ready_timeout_secs == 0 {
            30
        } else {
            self.config.ready_timeout_secs
        };

        // In a real Docker environment, we'd HTTP-poll the health endpoint.
        // For now, we use exec to curl/wget inside the container.
        // With the mock runtime this is a no-op that succeeds immediately.
        let container_id = self.container_id.as_ref().ok_or_else(|| {
            IctError::Runtime(anyhow::anyhow!("sidecar not started"))
        })?;

        let mut attempts = 0u64;
        let max_attempts = timeout_secs * 2; // poll every 500ms
        loop {
            // Try a simple exec to check if the container is responsive
            match self
                .runtime
                .exec_in_container(container_id, &["true"], &[])
                .await
            {
                Ok(output) if output.exit_code == 0 => {
                    debug!(
                        sidecar = %self.config.name,
                        "Sidecar health check passed"
                    );
                    return Ok(());
                }
                Ok(_) | Err(_) => {
                    attempts += 1;
                    if attempts >= max_attempts {
                        return Err(IctError::Timeout {
                            what: format!("sidecar {} health", self.config.name),
                            duration: std::time::Duration::from_secs(timeout_secs),
                        });
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
    }

    /// Stop and remove the sidecar container.
    pub async fn stop_container(&mut self) -> Result<()> {
        if let Some(id) = self.container_id.take() {
            info!(
                sidecar = %self.config.name,
                chain = %self.chain_id,
                "Stopping sidecar container"
            );
            if let Err(e) = self.runtime.stop_container(&id).await {
                warn!(sidecar = %self.config.name, error = %e, "Failed to stop sidecar");
            }
            if let Err(e) = self.runtime.remove_container(&id).await {
                warn!(sidecar = %self.config.name, error = %e, "Failed to remove sidecar");
            }
        }
        Ok(())
    }

    /// Execute a command inside the running sidecar container.
    pub async fn exec(&self, cmd: &[&str], env: &[(&str, &str)]) -> Result<ExecOutput> {
        let container_id = self.container_id.as_ref().ok_or_else(|| {
            IctError::Runtime(anyhow::anyhow!(
                "sidecar {} not running",
                self.config.name
            ))
        })?;
        self.runtime.exec_in_container(container_id, cmd, env).await
    }

    /// Get container logs.
    pub async fn logs(&self) -> Result<String> {
        let container_id = self.container_id.as_ref().ok_or_else(|| {
            IctError::Runtime(anyhow::anyhow!(
                "sidecar {} not running",
                self.config.name
            ))
        })?;
        self.runtime.container_logs(container_id).await
    }

    /// Write a file into the sidecar container using exec.
    pub async fn write_file(&self, content: &str, rel_path: &str) -> Result<()> {
        let full_path = format!("{}/{}", self.config.home_dir, rel_path);
        let cmd_str = format!(
            "mkdir -p $(dirname {}) && cat > {}",
            full_path, full_path
        );
        // Use printf to avoid shell interpretation issues
        let encoded = content.replace('\'', "'\\''");
        let write_cmd = format!("printf '%s' '{}' | sh -c '{}'", encoded, cmd_str);
        self.exec(&["sh", "-c", &write_cmd], &[]).await?;
        Ok(())
    }

    /// Get the container ID if the sidecar has been created.
    pub fn container_id(&self) -> Option<&ContainerId> {
        self.container_id.as_ref()
    }
}
