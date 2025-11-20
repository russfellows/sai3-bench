// src/ssh_deploy.rs
//! SSH-based deployment automation for distributed agents
//! 
//! Enables zero-touch deployment:
//! 1. SSH to remote hosts
//! 2. Pull/verify Docker image
//! 3. Start agent containers with --net=host
//! 4. Health check agent readiness
//! 5. Track container IDs for cleanup

use anyhow::{Context, Result, bail};
use ssh2::Session;
use std::io::Read;
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;
use tracing::{debug, info, warn, error};

use crate::config::{AgentConfig, SshConfig, DeploymentConfig};

/// SSH session wrapper with automatic cleanup
pub struct SshSession {
    session: Session,
    host: String,
}

impl SshSession {
    /// Connect to remote host via SSH
    pub fn connect(host: &str, ssh_config: &SshConfig) -> Result<Self> {
        let user = ssh_config.user.clone().unwrap_or_else(|| {
            std::env::var("USER").unwrap_or_else(|_| "ubuntu".to_string())
        });
        
        let addr = if host.contains(':') {
            host.to_string()
        } else {
            format!("{}:22", host)
        };
        
        info!("Connecting to {}@{}", user, addr);
        
        let tcp = TcpStream::connect_timeout(
            &addr.parse()?,
            Duration::from_secs(ssh_config.timeout)
        ).context(format!("Failed to connect to {}", addr))?;
        
        let mut sess = Session::new()?;
        sess.set_tcp_stream(tcp);
        sess.set_timeout((ssh_config.timeout * 1000) as u32);  // milliseconds
        sess.handshake()?;
        
        // Authenticate with key
        let key_path = ssh_config.key_path.as_deref()
            .unwrap_or("~/.ssh/id_rsa");
        let expanded_key = shellexpand::tilde(key_path);
        let key_file = Path::new(expanded_key.as_ref());
        
        if !key_file.exists() {
            bail!("SSH key not found: {}", key_file.display());
        }
        
        debug!("Authenticating with key: {}", key_file.display());
        sess.userauth_pubkey_file(&user, None, key_file, None)
            .context("SSH authentication failed")?;
        
        if !sess.authenticated() {
            bail!("SSH authentication failed for {}@{}", user, addr);
        }
        
        info!("SSH connected to {}", addr);
        
        Ok(SshSession {
            session: sess,
            host: addr,
        })
    }
    
    /// Execute command and return stdout
    pub fn exec(&self, cmd: &str) -> Result<String> {
        debug!("SSH exec on {}: {}", self.host, cmd);
        
        let mut channel = self.session.channel_session()?;
        channel.exec(cmd)?;
        
        let mut stdout = String::new();
        channel.read_to_string(&mut stdout)?;
        
        channel.wait_close()?;
        let exit_status = channel.exit_status()?;
        
        if exit_status != 0 {
            // Try to get stderr
            let mut stderr = String::new();
            let _ = channel.stderr().read_to_string(&mut stderr);
            bail!("Command failed (exit {}): {}\nStderr: {}", 
                exit_status, cmd, stderr);
        }
        
        Ok(stdout.trim().to_string())
    }
    
    /// Execute command without waiting for completion (background process)
    pub fn exec_background(&self, cmd: &str) -> Result<()> {
        debug!("SSH exec background on {}: {}", self.host, cmd);
        
        let mut channel = self.session.channel_session()?;
        channel.exec(cmd)?;
        
        // Don't wait for completion - let it run in background
        // The channel will be dropped, but process continues
        
        Ok(())
    }
    
    /// Check if container runtime is available
    pub fn check_runtime(&self, runtime: &str) -> Result<bool> {
        let cmd = format!("{} --version", runtime);
        match self.exec(&cmd) {
            Ok(version) => {
                info!("{} available on {}: {}", runtime, self.host, version);
                Ok(true)
            }
            Err(_) => {
                warn!("{} not found on {}", runtime, self.host);
                Ok(false)
            }
        }
    }
    
    /// Pull container image if needed
    pub fn pull_image(&self, runtime: &str, image: &str, policy: &str) -> Result<()> {
        match policy {
            "never" => {
                debug!("Pull policy 'never' - skipping image pull");
                return Ok(());
            }
            "if_not_present" => {
                // Check if image exists
                let check_cmd = format!("{} image inspect {} >/dev/null 2>&1", runtime, image);
                if self.exec(&check_cmd).is_ok() {
                    debug!("Image {} already present on {}", image, self.host);
                    return Ok(());
                }
            }
            "always" => {
                debug!("Pull policy 'always' - pulling image");
            }
            _ => {
                warn!("Unknown pull policy '{}', defaulting to 'if_not_present'", policy);
            }
        }
        
        info!("Pulling container image {} on {} using {}", image, self.host, runtime);
        let pull_cmd = format!("{} pull {}", runtime, image);
        self.exec(&pull_cmd)
            .context(format!("Failed to pull image {}", image))?;
        
        Ok(())
    }
}

/// Agent deployment manager
pub struct AgentDeployment {
    /// Container ID for cleanup
    pub container_id: Option<String>,
    /// SSH session (kept alive for cleanup)
    pub ssh_session: Option<SshSession>,
    /// Container runtime used (docker/podman)
    pub container_runtime: String,
    /// Agent identifier
    pub agent_id: String,
    /// Agent address (for gRPC connection)
    pub address: String,
}

impl AgentDeployment {
    /// Deploy agent via container runtime (docker/podman) on remote host
    pub fn deploy_docker(
        agent: &AgentConfig,
        deployment: &DeploymentConfig,
        ssh_config: &SshConfig,
        agent_id: &str,
    ) -> Result<Self> {
        // Extract hostname from address (remove :port if present)
        let host = agent.address.split(':').next()
            .context("Invalid agent address")?;
        
        let session = SshSession::connect(host, ssh_config)?;
        
        let runtime = &deployment.container_runtime;
        
        // Verify container runtime is available
        if !session.check_runtime(runtime)? {
            bail!("{} is not available on {}", runtime, host);
        }
        
        // Pull image if needed
        session.pull_image(runtime, &deployment.image, &deployment.pull_policy)?;
        
        // Build container run command (works for both docker and podman)
        let mut container_cmd = vec![
            format!("{} run -d", runtime),  // Detached mode
            "--rm".to_string(),  // Auto-remove on stop
            format!("--name sai3bench-agent-{}", agent_id),
            format!("--network {}", deployment.network_mode),
        ];
        
        // Add environment variables
        for (key, value) in &agent.env {
            container_cmd.push(format!("-e {}={}", key, value));
        }
        
        // Add volume mounts
        for volume in &agent.volumes {
            container_cmd.push(format!("-v {}", volume));
        }
        
        // Add any additional container args
        for arg in &deployment.docker_args {
            container_cmd.push(arg.clone());
        }
        
        // Image and command
        container_cmd.push(deployment.image.clone());
        container_cmd.push("sai3bench-agent".to_string());
        container_cmd.push("--listen".to_string());
        container_cmd.push(format!("0.0.0.0:{}", agent.listen_port));
        
        let full_cmd = container_cmd.join(" ");
        info!("Starting agent container on {} using {}: {}", host, runtime, full_cmd);
        
        let container_id = session.exec(&full_cmd)
            .context("Failed to start agent container")?;
        
        info!("Agent container started on {}: {}", host, container_id);
        
        // Wait a moment for container to start
        std::thread::sleep(Duration::from_secs(crate::constants::CONTAINER_STARTUP_WAIT_SECS));
        
        // Verify container is running
        let check_cmd = format!("{} ps -q --filter id={}", runtime, container_id);
        let running = session.exec(&check_cmd)?;
        if running.is_empty() {
            // Get container logs for debugging
            let logs_cmd = format!("{} logs {}", runtime, container_id);
            let logs = session.exec(&logs_cmd).unwrap_or_default();
            bail!("Container failed to start. Logs:\n{}", logs);
        }
        
        let address = if agent.address.contains(':') {
            agent.address.clone()
        } else {
            format!("{}:{}", agent.address, agent.listen_port)
        };
        
        Ok(AgentDeployment {
            container_id: Some(container_id),
            ssh_session: Some(session),
            container_runtime: runtime.clone(),
            agent_id: agent_id.to_string(),
            address,
        })
    }
    
    /// Stop and cleanup agent container
    pub fn cleanup(&mut self) -> Result<()> {
        if let (Some(container_id), Some(session)) = (&self.container_id, &self.ssh_session) {
            info!("Stopping agent container: {}", container_id);
            
            let stop_cmd = format!("{} stop {} || true", self.container_runtime, container_id);
            match session.exec(&stop_cmd) {
                Ok(_) => {
                    info!("Agent container stopped: {}", container_id);
                    self.container_id = None;
                }
                Err(e) => {
                    error!("Failed to stop container {}: {}", container_id, e);
                    return Err(e);
                }
            }
        }
        
        Ok(())
    }
}

impl Drop for AgentDeployment {
    fn drop(&mut self) {
        if self.container_id.is_some() {
            if let Err(e) = self.cleanup() {
                error!("Error during agent cleanup: {}", e);
            }
        }
    }
}

/// Deploy multiple agents in parallel
pub async fn deploy_agents(
    agents: &[AgentConfig],
    deployment: &DeploymentConfig,
    ssh_config: &SshConfig,
) -> Result<Vec<AgentDeployment>> {
    if !ssh_config.enabled {
        bail!("SSH deployment not enabled in configuration");
    }
    
    info!("Deploying {} agents via SSH + Docker", agents.len());
    
    let mut deployments = Vec::new();
    let mut errors = Vec::new();
    
    // Deploy agents sequentially (could parallelize with tokio::task::spawn_blocking)
    for (idx, agent) in agents.iter().enumerate() {
        let agent_id = agent.id.clone()
            .unwrap_or_else(|| format!("{}", idx + 1));
        
        match AgentDeployment::deploy_docker(agent, deployment, ssh_config, &agent_id) {
            Ok(deployment) => {
                info!("✓ Agent {} deployed: {}", agent_id, deployment.address);
                deployments.push(deployment);
            }
            Err(e) => {
                error!("✗ Failed to deploy agent {}: {}", agent_id, e);
                errors.push(format!("Agent {}: {}", agent_id, e));
            }
        }
    }
    
    if !errors.is_empty() {
        // Cleanup successful deployments
        for mut deployment in deployments {
            let _ = deployment.cleanup();
        }
        bail!("Failed to deploy {} agents:\n{}", errors.len(), errors.join("\n"));
    }
    
    info!("✓ All {} agents deployed successfully", deployments.len());
    
    Ok(deployments)
}

/// Cleanup all agent deployments
pub fn cleanup_agents(mut deployments: Vec<AgentDeployment>) -> Result<()> {
    info!("Cleaning up {} agent deployments", deployments.len());
    
    let mut errors = Vec::new();
    
    for mut deployment in deployments.drain(..) {
        if let Err(e) = deployment.cleanup() {
            errors.push(format!("Agent {}: {}", deployment.agent_id, e));
        }
    }
    
    if !errors.is_empty() {
        bail!("Failed to cleanup {} agents:\n{}", errors.len(), errors.join("\n"));
    }
    
    info!("✓ All agents cleaned up successfully");
    
    Ok(())
}
