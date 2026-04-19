#[cfg(feature = "docker")]
pub mod docker;
#[cfg(feature = "kuasar")]
pub mod kuasar;
pub mod mock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::tx::ExecOutput;

/// Top-level runtime selector. Start with Docker, extend to Kuasar and beyond.
pub enum IctRuntime {
    #[cfg(feature = "docker")]
    Docker(DockerConfig),
    #[cfg(feature = "kuasar")]
    Kuasar(KuasarConfig),
}

impl IctRuntime {
    /// Convert the enum into a boxed runtime backend.
    pub async fn into_backend(self) -> Result<std::sync::Arc<dyn RuntimeBackend>> {
        match self {
            #[cfg(feature = "docker")]
            Self::Docker(config) => {
                let backend = docker::DockerBackend::new(config).await?;
                Ok(std::sync::Arc::new(backend))
            }
            #[cfg(feature = "kuasar")]
            Self::Kuasar(_config) => {
                todo!("Phase 8: construct KuasarBackend from config")
            }
        }
    }
}

/// Unique identifier for a managed container/sandbox.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainerId(pub String);

/// Unique identifier for a managed network.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct NetworkId(pub String);

/// Docker image reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerImage {
    pub repository: String,
    pub version: String,
    pub uid_gid: Option<String>,
}

impl std::fmt::Display for DockerImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.repository, self.version)
    }
}

/// Options for creating a container.
#[derive(Debug, Clone)]
pub struct ContainerOptions {
    pub image: DockerImage,
    pub name: String,
    pub network_id: Option<NetworkId>,
    pub env: Vec<(String, String)>,
    pub cmd: Vec<String>,
    pub entrypoint: Option<Vec<String>>,
    pub ports: Vec<PortBinding>,
    pub volumes: Vec<VolumeMount>,
    pub labels: Vec<(String, String)>,
    pub hostname: Option<String>,
}

/// A port mapping from host to container.
#[derive(Debug, Clone)]
pub struct PortBinding {
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
}

/// A volume mount into a container.
#[derive(Debug, Clone)]
pub struct VolumeMount {
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

/// Container exit status.
#[derive(Debug, Clone)]
pub struct ExitStatus {
    pub code: i64,
}

/// Docker-specific configuration.
#[cfg(feature = "docker")]
#[derive(Debug, Clone, Default)]
pub struct DockerConfig {
    pub socket_path: Option<String>,
}

/// Kuasar-specific configuration.
#[cfg(feature = "kuasar")]
#[derive(Debug, Clone)]
pub struct KuasarConfig {
    pub sandbox_type: KuasarSandboxType,
    pub endpoint: String,
}

/// Kuasar sandbox variants.
#[cfg(feature = "kuasar")]
#[derive(Debug, Clone)]
pub enum KuasarSandboxType {
    Runc,
    Wasm,
    Vm,
}

/// The core runtime abstraction. All container/sandbox backends implement this.
#[async_trait]
pub trait RuntimeBackend: Send + Sync {
    /// Pull an image/artifact so it's available locally.
    async fn pull_image(&self, image: &DockerImage) -> Result<()>;

    /// Create a new container (does not start it).
    async fn create_container(&self, opts: &ContainerOptions) -> Result<ContainerId>;

    /// Start a previously created container.
    async fn start_container(&self, id: &ContainerId) -> Result<()>;

    /// Stop a running container.
    async fn stop_container(&self, id: &ContainerId) -> Result<()>;

    /// Remove a container and its resources.
    async fn remove_container(&self, id: &ContainerId) -> Result<()>;

    /// Execute a command inside a running container.
    async fn exec_in_container(
        &self,
        id: &ContainerId,
        cmd: &[&str],
        env: &[(&str, &str)],
    ) -> Result<ExecOutput>;

    /// Execute a command inside a running container in detached (background) mode.
    /// The command continues running after this method returns.
    async fn exec_in_container_background(
        &self,
        id: &ContainerId,
        cmd: &[&str],
        env: &[(&str, &str)],
    ) -> Result<()>;

    /// Create an isolated network.
    async fn create_network(&self, name: &str) -> Result<NetworkId>;

    /// Remove a network.
    async fn remove_network(&self, id: &NetworkId) -> Result<()>;

    /// Remove all networks whose name starts with `prefix`.
    ///
    /// Used to clean up orphaned networks from crashed previous test runs.
    /// Default implementation is a no-op — backends that support listing
    /// networks should override this.
    async fn remove_networks_by_prefix(&self, _prefix: &str) -> Result<()> {
        Ok(())
    }

    /// Retrieve logs from a container.
    async fn container_logs(&self, id: &ContainerId) -> Result<String>;

    /// Block until a container exits.
    async fn wait_for_container(&self, id: &ContainerId) -> Result<ExitStatus>;

    /// Remove a named volume.
    async fn remove_volume(&self, name: &str) -> Result<()>;

    /// Look up the host-mapped port for a given container port after the
    /// container has been started.
    ///
    /// Returns `Ok(None)` if the port mapping cannot be determined (e.g. mock
    /// runtimes or backends that don't support inspection).
    async fn get_host_port(
        &self,
        id: &ContainerId,
        container_port: u16,
        protocol: &str,
    ) -> Result<Option<u16>> {
        let _ = (id, container_port, protocol);
        Ok(None)
    }
}
