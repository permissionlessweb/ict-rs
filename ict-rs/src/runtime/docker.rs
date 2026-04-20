// Docker runtime backend using bollard — Phase 1.

use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, LogsOptions, RemoveContainerOptions, StopContainerOptions,
    WaitContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, PortBinding as BollardPortBinding, RestartPolicy, RestartPolicyNameEnum};
use bollard::network::CreateNetworkOptions;
use bollard::Docker;
use futures::StreamExt;
use std::collections::HashMap;
use tracing::{debug, info, warn};

use crate::error::{IctError, Result};
use crate::runtime::{
    ContainerId, ContainerOptions, DockerConfig, DockerImage, ExitStatus, NetworkId,
    RuntimeBackend,
};
use crate::tx::ExecOutput;

/// Docker-backed runtime using the bollard crate.
pub struct DockerBackend {
    client: Docker,
}

impl DockerBackend {
    /// Create a new `DockerBackend` from a `DockerConfig`.
    ///
    /// If `config.socket_path` is set, connects to that socket; otherwise uses
    /// the platform default (`/var/run/docker.sock` on Linux/macOS, named pipe
    /// on Windows).
    pub async fn new(config: DockerConfig) -> Result<Self> {
        let client = match config.socket_path {
            Some(ref path) => {
                Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)?
            }
            None => Docker::connect_with_socket_defaults()?,
        };

        // Verify the daemon is reachable.
        client.ping().await?;
        info!("Connected to Docker daemon");

        Ok(Self { client })
    }

    /// Wrap an already-constructed bollard `Docker` client.
    pub fn from_client(client: Docker) -> Self {
        Self { client }
    }
}

#[async_trait]
impl RuntimeBackend for DockerBackend {
    async fn pull_image(&self, image: &DockerImage) -> Result<()> {
        let reference = image.to_string();

        // Check if the image already exists locally before pulling.
        // This is essential for local-only images (e.g. `terpnetwork/terp-core:local`)
        // that were built with `docker build` and don't exist on any registry.
        if self.client.inspect_image(&reference).await.is_ok() {
            info!(image = %reference, "Image already exists locally, skipping pull");
            return Ok(());
        }

        info!(image = %reference, "Pulling image");

        let opts = CreateImageOptions {
            from_image: reference.as_str(),
            ..Default::default()
        };

        let mut stream = self.client.create_image(Some(opts), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = &info.status {
                        debug!(status = %status, "pull progress");
                    }
                }
                Err(e) => return Err(IctError::Docker(e)),
            }
        }

        info!(image = %image, "Image pulled successfully");
        Ok(())
    }

    async fn create_container(&self, opts: &ContainerOptions) -> Result<ContainerId> {
        let image_ref = opts.image.to_string();

        // --- Environment variables ---
        let env: Vec<String> = opts
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();

        // --- Labels ---
        let labels: HashMap<String, String> = opts
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        // --- Port bindings & exposed ports ---
        let mut port_bindings: HashMap<String, Option<Vec<BollardPortBinding>>> = HashMap::new();
        let mut exposed_ports: HashMap<String, HashMap<(), ()>> = HashMap::new();

        for pb in &opts.ports {
            let container_key = format!("{}/{}", pb.container_port, pb.protocol);
            exposed_ports.insert(container_key.clone(), HashMap::new());
            port_bindings.insert(
                container_key,
                Some(vec![BollardPortBinding {
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(pb.host_port.to_string()),
                }]),
            );
        }

        // --- Volume binds ---
        let binds: Vec<String> = opts
            .volumes
            .iter()
            .map(|v| {
                let mode = if v.read_only { "ro" } else { "rw" };
                format!("{}:{}:{}", v.source, v.target, mode)
            })
            .collect();

        // --- Host config ---
        let host_config = HostConfig {
            binds: if binds.is_empty() {
                None
            } else {
                Some(binds)
            },
            port_bindings: if port_bindings.is_empty() {
                None
            } else {
                Some(port_bindings)
            },
            network_mode: opts.network_id.as_ref().map(|n| n.0.clone()),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::NO),
                maximum_retry_count: None,
            }),
            ..Default::default()
        };

        // --- Container config ---
        let config = Config {
            image: Some(image_ref.clone()),
            hostname: opts.hostname.as_deref().map(String::from),
            env: if env.is_empty() {
                None
            } else {
                Some(env.clone())
            },
            cmd: if opts.cmd.is_empty() {
                None
            } else {
                Some(opts.cmd.clone())
            },
            entrypoint: opts.entrypoint.clone(),
            labels: if labels.is_empty() {
                None
            } else {
                Some(labels)
            },
            exposed_ports: if exposed_ports.is_empty() {
                None
            } else {
                Some(exposed_ports)
            },
            host_config: Some(host_config),
            user: opts.image.uid_gid.as_deref().map(String::from),
            ..Default::default()
        };

        let create_opts = CreateContainerOptions {
            name: opts.name.as_str(),
            platform: None,
        };

        let response = self.client.create_container(Some(create_opts), config).await?;
        let id = ContainerId(response.id);
        info!(container_id = %id.0, name = %opts.name, "Container created");
        Ok(id)
    }

    async fn start_container(&self, id: &ContainerId) -> Result<()> {
        self.client
            .start_container::<String>(&id.0, None)
            .await?;
        info!(container_id = %id.0, "Container started");
        Ok(())
    }

    async fn stop_container(&self, id: &ContainerId) -> Result<()> {
        let opts = StopContainerOptions { t: 30 };
        self.client.stop_container(&id.0, Some(opts)).await?;
        info!(container_id = %id.0, "Container stopped");
        Ok(())
    }

    async fn pause_container(&self, id: &ContainerId) -> Result<()> {
        self.client.pause_container(&id.0).await?;
        info!(container_id = %id.0, "Container paused (cgroup freeze)");
        Ok(())
    }

    async fn unpause_container(&self, id: &ContainerId) -> Result<()> {
        self.client.unpause_container(&id.0).await?;
        info!(container_id = %id.0, "Container unpaused");
        Ok(())
    }

    async fn remove_container(&self, id: &ContainerId) -> Result<()> {
        let opts = RemoveContainerOptions {
            force: true,
            v: true, // also remove anonymous volumes
            ..Default::default()
        };
        self.client.remove_container(&id.0, Some(opts)).await?;
        info!(container_id = %id.0, "Container removed");
        Ok(())
    }

    async fn exec_in_container(
        &self,
        id: &ContainerId,
        cmd: &[&str],
        env: &[(&str, &str)],
    ) -> Result<ExecOutput> {
        let env_strs: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        let exec_config = CreateExecOptions {
            cmd: Some(cmd.to_vec()),
            env: if env_strs.is_empty() {
                None
            } else {
                Some(env_strs.iter().map(|s| s.as_str()).collect())
            },
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };

        let exec = self.client.create_exec(&id.0, exec_config).await?;
        debug!(exec_id = %exec.id, container = %id.0, "Exec instance created");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let start_result = self.client.start_exec(&exec.id, None).await?;
        match start_result {
            StartExecResults::Attached { mut output, .. } => {
                while let Some(msg) = output.next().await {
                    match msg {
                        Ok(bollard::container::LogOutput::StdOut { message }) => {
                            stdout.extend_from_slice(&message);
                        }
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            stderr.extend_from_slice(&message);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(error = %e, "Error reading exec output stream");
                            return Err(IctError::Docker(e));
                        }
                    }
                }
            }
            StartExecResults::Detached => {
                warn!("Exec started in detached mode unexpectedly");
            }
        }

        // Inspect the exec to get the exit code.
        let inspect = self.client.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code.unwrap_or(-1);

        if exit_code != 0 {
            let stdout_s = String::from_utf8_lossy(&stdout);
            let stderr_s = String::from_utf8_lossy(&stderr);
            warn!(
                exec_id = %exec.id,
                exit_code = exit_code,
                stdout = %stdout_s,
                stderr = %stderr_s,
                "Exec failed"
            );
        } else {
            let stdout_s = String::from_utf8_lossy(&stdout);
            let stderr_s = String::from_utf8_lossy(&stderr);
            debug!(
                exec_id = %exec.id,
                exit_code = exit_code,
                stdout = %stdout_s,
                stderr = %stderr_s,
                "Exec completed"
            );
        }

        Ok(ExecOutput {
            stdout,
            stderr,
            exit_code,
        })
    }

    async fn exec_in_container_background(
        &self,
        id: &ContainerId,
        cmd: &[&str],
        env: &[(&str, &str)],
    ) -> Result<()> {
        let env_strs: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        let exec_config = CreateExecOptions {
            cmd: Some(cmd.to_vec()),
            env: if env_strs.is_empty() {
                None
            } else {
                Some(env_strs.iter().map(|s| s.as_str()).collect())
            },
            attach_stdout: Some(false),
            attach_stderr: Some(false),
            ..Default::default()
        };

        let exec = self.client.create_exec(&id.0, exec_config).await?;
        debug!(exec_id = %exec.id, container = %id.0, "Background exec created");

        let start_opts = bollard::exec::StartExecOptions { detach: true, ..Default::default() };
        self.client.start_exec(&exec.id, Some(start_opts)).await?;
        debug!(exec_id = %exec.id, "Background exec started (detached)");

        Ok(())
    }

    async fn create_network(&self, name: &str) -> Result<NetworkId> {
        // Idempotent: if the network already exists, return its ID.
        if let Ok(net) = self.client.inspect_network::<String>(name, None).await {
            if let Some(id) = net.id {
                info!(network_id = %id, name = %name, "Network already exists, reusing");
                return Ok(NetworkId(id));
            }
        }

        // Generate a unique /16 subnet from a hash of the network name to reduce
        // collisions across concurrent test runs.
        let hash = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            name.hash(&mut h);
            h.finish()
        };
        let mut second_octet = ((hash % 240) + 10) as u8; // 10..249

        // Retry with different subnets on "Pool overlaps" collision.
        for attempt in 0u64..5 {
            let subnet = format!("172.{second_octet}.0.0/16");
            let gateway = format!("172.{second_octet}.0.1");

            let ipam_config = bollard::models::IpamConfig {
                subnet: Some(subnet.clone()),
                gateway: Some(gateway),
                ..Default::default()
            };

            let ipam = bollard::models::Ipam {
                driver: Some("default".to_string()),
                config: Some(vec![ipam_config]),
                ..Default::default()
            };

            let config = CreateNetworkOptions {
                name: name.to_string(),
                driver: "bridge".to_string(),
                ipam,
                labels: HashMap::from([("ict-rs".to_string(), "true".to_string())]),
                ..Default::default()
            };

            match self.client.create_network(config).await {
                Ok(response) => {
                    let id = response.id;
                    info!(network_id = %id, name = %name, "Network created");
                    return Ok(NetworkId(id));
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("Pool overlaps") && attempt < 4 {
                        warn!(
                            attempt = attempt,
                            subnet = %subnet,
                            "Subnet collision, retrying with different octet"
                        );
                        // Deterministic fallback using PID + attempt to pick a new octet.
                        second_octet = ((std::process::id() as u64 + attempt * 37 + hash) % 240 + 10) as u8;
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }
        unreachable!("retry loop should have returned")
    }

    async fn remove_network(&self, id: &NetworkId) -> Result<()> {
        self.client.remove_network(&id.0).await?;
        info!(network_id = %id.0, "Network removed");
        Ok(())
    }

    async fn remove_networks_by_prefix(&self, prefix: &str) -> Result<()> {
        use bollard::network::{ListNetworksOptions, DisconnectNetworkOptions};
        let opts = ListNetworksOptions::<String> {
            ..Default::default()
        };
        let networks = self.client.list_networks(Some(opts)).await?;
        for net in networks {
            if let Some(ref name) = net.name {
                if name.starts_with(prefix) {
                    if let Some(ref id) = net.id {
                        // Force-disconnect any lingering containers before removing
                        // the network, otherwise remove_network silently fails.
                        if let Ok(info) = self.client.inspect_network::<String>(id, None).await {
                            if let Some(ref containers) = info.containers {
                                for (cid, _) in containers {
                                    let _ = self.client.disconnect_network(
                                        id,
                                        DisconnectNetworkOptions::<String> {
                                            container: cid.clone(),
                                            force: true,
                                        },
                                    ).await;
                                }
                            }
                        }
                        let _ = self.client.remove_network(id).await;
                        info!(network = %name, "Removed orphaned network");
                    }
                }
            }
        }
        Ok(())
    }

    async fn container_logs(&self, id: &ContainerId) -> Result<String> {
        let opts = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            follow: false,
            timestamps: false,
            ..Default::default()
        };

        let mut stream = self.client.logs(&id.0, Some(opts));
        let mut output = String::new();

        while let Some(result) = stream.next().await {
            match result {
                Ok(log_output) => {
                    output.push_str(&log_output.to_string());
                }
                Err(e) => {
                    warn!(error = %e, "Error reading container logs");
                    return Err(IctError::Docker(e));
                }
            }
        }

        Ok(output)
    }

    async fn wait_for_container(&self, id: &ContainerId) -> Result<ExitStatus> {
        let opts = WaitContainerOptions {
            condition: "not-running",
        };

        let mut stream = self.client.wait_container(&id.0, Some(opts));

        while let Some(result) = stream.next().await {
            match result {
                Ok(response) => {
                    let code = response.status_code;
                    info!(container_id = %id.0, exit_code = code, "Container exited");
                    return Ok(ExitStatus { code });
                }
                Err(e) => {
                    return Err(IctError::Docker(e));
                }
            }
        }

        // Stream ended without producing a result — treat as abnormal.
        Err(IctError::Runtime(anyhow::anyhow!(
            "wait stream ended without an exit status for container {}",
            id.0
        )))
    }

    async fn remove_volume(&self, name: &str) -> Result<()> {
        self.client.remove_volume(name, None).await?;
        info!(volume = %name, "Volume removed");
        Ok(())
    }

    async fn copy_from_container(
        &self,
        id: &ContainerId,
        container_path: &str,
    ) -> Result<Vec<u8>> {
        use bollard::container::DownloadFromContainerOptions;

        let opts = DownloadFromContainerOptions {
            path: container_path,
        };

        let mut stream = self.client.download_from_container(&id.0, Some(opts));
        let mut tar_bytes = Vec::new();

        while let Some(result) = stream.next().await {
            match result {
                Ok(chunk) => tar_bytes.extend_from_slice(&chunk),
                Err(e) => return Err(IctError::Docker(e)),
            }
        }

        info!(
            container_id = %id.0,
            path = %container_path,
            size_bytes = tar_bytes.len(),
            "Copied from container"
        );
        Ok(tar_bytes)
    }

    async fn get_host_port(
        &self,
        id: &ContainerId,
        container_port: u16,
        protocol: &str,
    ) -> Result<Option<u16>> {
        let inspect = self.client.inspect_container(&id.0, None).await?;

        let ports = inspect
            .network_settings
            .and_then(|ns| ns.ports)
            .unwrap_or_default();

        let key = format!("{container_port}/{protocol}");
        if let Some(Some(bindings)) = ports.get(&key) {
            for binding in bindings {
                if let Some(ref host_port_str) = binding.host_port {
                    if let Ok(port) = host_port_str.parse::<u16>() {
                        debug!(
                            container_id = %id.0,
                            container_port = container_port,
                            host_port = port,
                            "Resolved host port"
                        );
                        return Ok(Some(port));
                    }
                }
            }
        }

        Ok(None)
    }
}
