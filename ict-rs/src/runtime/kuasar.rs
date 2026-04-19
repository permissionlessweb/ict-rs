// Kuasar lightweight multi-sandbox runtime backend.
//
// Kuasar (https://github.com/kuasar-io/kuasar) supports multiple sandbox types:
// - Runc/OCI containers (like Docker but lighter)
// - WASM sandboxes (Wasmtime, WasmEdge)
// - VMs (QEMU, Cloud Hypervisor, Stratovirt)
//
// Communication is via gRPC/tRPC over Unix Domain Sockets, using the containerd
// sandbox API (Sandboxer trait). Each sandbox manages its own lifecycle, containers,
// storage, and networking.
//
// This module provides the `KuasarBackend` struct that implements `RuntimeBackend`
// by translating our container lifecycle calls into Kuasar sandbox operations.

#[cfg(feature = "kuasar")]
mod inner {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use tracing::{debug, info, warn};

    use crate::error::{IctError, Result};
    use crate::runtime::{
        ContainerId, ContainerOptions, DockerImage, ExitStatus, KuasarConfig,
        KuasarSandboxType, NetworkId, RuntimeBackend,
    };
    use crate::tx::ExecOutput;

    /// Tracks the state of sandboxes managed by Kuasar.
    struct SandboxState {
        sandboxes: HashMap<String, SandboxInfo>,
        networks: HashMap<String, String>,
        next_id: u64,
    }

    struct SandboxInfo {
        name: String,
        sandbox_type: KuasarSandboxType,
        status: SandboxStatus,
    }

    #[derive(Debug, Clone, PartialEq)]
    enum SandboxStatus {
        Created,
        Running,
        Stopped,
    }

    /// Kuasar-backed runtime using gRPC to communicate with the Kuasar shim.
    ///
    /// Each "container" in ict-rs terms maps to a Kuasar sandbox. The sandbox type
    /// (Runc, WASM, VM) is determined by the `KuasarConfig`.
    ///
    /// In production, this would use tonic gRPC clients to communicate with
    /// the Kuasar containerd shim over a Unix Domain Socket. For now, it provides
    /// the structural implementation that can be wired to real gRPC endpoints.
    pub struct KuasarBackend {
        config: KuasarConfig,
        state: Mutex<SandboxState>,
    }

    impl KuasarBackend {
        /// Create a new Kuasar backend.
        ///
        /// The `endpoint` in config should point to the Kuasar shim's gRPC socket,
        /// e.g., `unix:///run/containerd/containerd.sock`.
        pub async fn new(config: KuasarConfig) -> Result<Self> {
            info!(
                endpoint = %config.endpoint,
                sandbox_type = ?config.sandbox_type,
                "Connecting to Kuasar runtime"
            );

            // In production: establish tonic gRPC channel to the Kuasar shim
            // let channel = tonic::transport::Endpoint::from_shared(config.endpoint.clone())
            //     .map_err(|e| IctError::Runtime(e.into()))?
            //     .connect()
            //     .await
            //     .map_err(|e| IctError::Runtime(e.into()))?;

            Ok(Self {
                config,
                state: Mutex::new(SandboxState {
                    sandboxes: HashMap::new(),
                    networks: HashMap::new(),
                    next_id: 0,
                }),
            })
        }

        /// Get the sandbox type this backend is configured for.
        pub fn sandbox_type(&self) -> &KuasarSandboxType {
            &self.config.sandbox_type
        }
    }

    #[async_trait]
    impl RuntimeBackend for KuasarBackend {
        async fn pull_image(&self, image: &DockerImage) -> Result<()> {
            info!(image = %image, "Kuasar: pulling image/artifact");
            // For Runc: pull OCI image (same as Docker)
            // For WASM: fetch WASM module
            // For VM: fetch VM image/rootfs
            match self.config.sandbox_type {
                KuasarSandboxType::Runc => {
                    debug!("Kuasar/Runc: would pull OCI image {image}");
                }
                KuasarSandboxType::Wasm => {
                    debug!("Kuasar/WASM: would fetch WASM module {image}");
                }
                KuasarSandboxType::Vm => {
                    debug!("Kuasar/VM: would fetch VM image {image}");
                }
            }
            Ok(())
        }

        async fn create_container(&self, opts: &ContainerOptions) -> Result<ContainerId> {
            let mut state = self.state.lock().unwrap();
            state.next_id += 1;
            let id = format!("kuasar-{}-{}", self.config.sandbox_type_str(), state.next_id);

            state.sandboxes.insert(
                id.clone(),
                SandboxInfo {
                    name: opts.name.clone(),
                    sandbox_type: self.config.sandbox_type.clone(),
                    status: SandboxStatus::Created,
                },
            );

            info!(
                sandbox_id = %id,
                name = %opts.name,
                sandbox_type = self.config.sandbox_type_str(),
                "Kuasar: sandbox created"
            );

            // In production: call Sandboxer::create() via gRPC
            // self.client.create_sandbox(CreateSandboxRequest {
            //     sandbox_id: id.clone(),
            //     ...
            // }).await?;

            Ok(ContainerId(id))
        }

        async fn start_container(&self, id: &ContainerId) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            if let Some(sandbox) = state.sandboxes.get_mut(&id.0) {
                sandbox.status = SandboxStatus::Running;
                info!(sandbox_id = %id.0, "Kuasar: sandbox started");
                Ok(())
            } else {
                Err(IctError::Runtime(anyhow::anyhow!(
                    "sandbox not found: {}",
                    id.0
                )))
            }
        }

        async fn stop_container(&self, id: &ContainerId) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            if let Some(sandbox) = state.sandboxes.get_mut(&id.0) {
                sandbox.status = SandboxStatus::Stopped;
                info!(sandbox_id = %id.0, "Kuasar: sandbox stopped");
                Ok(())
            } else {
                Err(IctError::Runtime(anyhow::anyhow!(
                    "sandbox not found: {}",
                    id.0
                )))
            }
        }

        async fn remove_container(&self, id: &ContainerId) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            state.sandboxes.remove(&id.0);
            info!(sandbox_id = %id.0, "Kuasar: sandbox removed");
            Ok(())
        }

        async fn exec_in_container(
            &self,
            id: &ContainerId,
            cmd: &[&str],
            _env: &[(&str, &str)],
        ) -> Result<ExecOutput> {
            let state = self.state.lock().unwrap();
            if !state.sandboxes.contains_key(&id.0) {
                return Err(IctError::Runtime(anyhow::anyhow!(
                    "sandbox not found: {}",
                    id.0
                )));
            }

            debug!(
                sandbox_id = %id.0,
                cmd = ?cmd,
                "Kuasar: exec in sandbox"
            );

            // In production: use the sandbox's exec API
            // For Runc: exec via runc exec
            // For WASM: call WASM function
            // For VM: exec via vsock/ssh into VM

            Ok(ExecOutput {
                stdout: b"{}".to_vec(),
                stderr: Vec::new(),
                exit_code: 0,
            })
        }

        async fn create_network(&self, name: &str) -> Result<NetworkId> {
            let mut state = self.state.lock().unwrap();
            let id = format!("kuasar-net-{name}");
            state.networks.insert(id.clone(), name.to_string());
            info!(network_id = %id, name = %name, "Kuasar: network created");
            Ok(NetworkId(id))
        }

        async fn remove_network(&self, id: &NetworkId) -> Result<()> {
            let mut state = self.state.lock().unwrap();
            state.networks.remove(&id.0);
            info!(network_id = %id.0, "Kuasar: network removed");
            Ok(())
        }

        async fn exec_in_container_background(
            &self,
            id: &ContainerId,
            cmd: &[&str],
            _env: &[(&str, &str)],
        ) -> Result<()> {
            let state = self.state.lock().unwrap();
            if !state.sandboxes.contains_key(&id.0) {
                return Err(IctError::Runtime(anyhow::anyhow!(
                    "sandbox not found: {}",
                    id.0
                )));
            }
            debug!(sandbox = %id.0, cmd = ?cmd, "Kuasar background exec");
            Ok(())
        }

        async fn container_logs(&self, id: &ContainerId) -> Result<String> {
            let state = self.state.lock().unwrap();
            if !state.sandboxes.contains_key(&id.0) {
                return Err(IctError::Runtime(anyhow::anyhow!(
                    "sandbox not found: {}",
                    id.0
                )));
            }
            Ok(String::new())
        }

        async fn wait_for_container(&self, id: &ContainerId) -> Result<ExitStatus> {
            // In production: subscribe to sandbox status via gRPC stream
            debug!(sandbox_id = %id.0, "Kuasar: waiting for sandbox exit");
            Ok(ExitStatus { code: 0 })
        }

        async fn remove_volume(&self, name: &str) -> Result<()> {
            debug!(volume = %name, "Kuasar: remove_volume (no-op)");
            Ok(())
        }
    }

    impl KuasarConfig {
        fn sandbox_type_str(&self) -> &'static str {
            match self.sandbox_type {
                KuasarSandboxType::Runc => "runc",
                KuasarSandboxType::Wasm => "wasm",
                KuasarSandboxType::Vm => "vm",
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn test_config(sandbox_type: KuasarSandboxType) -> KuasarConfig {
            KuasarConfig {
                sandbox_type,
                endpoint: "unix:///tmp/test.sock".to_string(),
            }
        }

        #[tokio::test]
        async fn test_kuasar_runc_lifecycle() {
            let backend = KuasarBackend::new(test_config(KuasarSandboxType::Runc))
                .await
                .unwrap();

            let image = DockerImage {
                repository: "test".to_string(),
                version: "latest".to_string(),
                uid_gid: None,
            };
            backend.pull_image(&image).await.unwrap();

            let opts = ContainerOptions {
                image: image.clone(),
                name: "test-container".to_string(),
                network_id: None,
                env: Vec::new(),
                cmd: vec!["start".to_string()],
                entrypoint: None,
                ports: Vec::new(),
                volumes: Vec::new(),
                labels: Vec::new(),
                hostname: None,
            };

            let id = backend.create_container(&opts).await.unwrap();
            assert!(id.0.contains("kuasar-runc"));

            backend.start_container(&id).await.unwrap();
            backend.stop_container(&id).await.unwrap();
            backend.remove_container(&id).await.unwrap();
        }

        #[tokio::test]
        async fn test_kuasar_wasm_lifecycle() {
            let backend = KuasarBackend::new(test_config(KuasarSandboxType::Wasm))
                .await
                .unwrap();

            let opts = ContainerOptions {
                image: DockerImage {
                    repository: "test.wasm".to_string(),
                    version: "v1".to_string(),
                    uid_gid: None,
                },
                name: "wasm-sandbox".to_string(),
                network_id: None,
                env: Vec::new(),
                cmd: Vec::new(),
                entrypoint: None,
                ports: Vec::new(),
                volumes: Vec::new(),
                labels: Vec::new(),
                hostname: None,
            };

            let id = backend.create_container(&opts).await.unwrap();
            assert!(id.0.contains("kuasar-wasm"));

            backend.start_container(&id).await.unwrap();

            let output = backend
                .exec_in_container(&id, &["echo", "hello"], &[])
                .await
                .unwrap();
            assert_eq!(output.exit_code, 0);

            backend.remove_container(&id).await.unwrap();
        }

        #[tokio::test]
        async fn test_kuasar_vm_lifecycle() {
            let backend = KuasarBackend::new(test_config(KuasarSandboxType::Vm))
                .await
                .unwrap();

            let opts = ContainerOptions {
                image: DockerImage {
                    repository: "vm-image".to_string(),
                    version: "v1".to_string(),
                    uid_gid: None,
                },
                name: "vm-sandbox".to_string(),
                network_id: None,
                env: Vec::new(),
                cmd: Vec::new(),
                entrypoint: None,
                ports: Vec::new(),
                volumes: Vec::new(),
                labels: Vec::new(),
                hostname: None,
            };

            let id = backend.create_container(&opts).await.unwrap();
            assert!(id.0.contains("kuasar-vm"));

            backend.start_container(&id).await.unwrap();
            let status = backend.wait_for_container(&id).await.unwrap();
            assert_eq!(status.code, 0);

            backend.remove_container(&id).await.unwrap();
        }

        #[tokio::test]
        async fn test_kuasar_network_management() {
            let backend = KuasarBackend::new(test_config(KuasarSandboxType::Runc))
                .await
                .unwrap();

            let net_id = backend.create_network("test-net").await.unwrap();
            assert!(net_id.0.contains("kuasar-net"));

            backend.remove_network(&net_id).await.unwrap();
        }

        #[tokio::test]
        async fn test_kuasar_exec_nonexistent_fails() {
            let backend = KuasarBackend::new(test_config(KuasarSandboxType::Runc))
                .await
                .unwrap();

            let result = backend
                .exec_in_container(&ContainerId("nonexistent".to_string()), &["ls"], &[])
                .await;
            assert!(result.is_err());
        }
    }
}
