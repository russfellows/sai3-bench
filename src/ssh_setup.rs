// src/ssh_setup.rs
//! SSH key setup automation for distributed testing
//! 
//! Simplifies the painful process of:
//! 1. Generating SSH key pairs
//! 2. Distributing public keys to remote hosts
//! 3. Verifying passwordless SSH access
//! 4. Setting up known_hosts entries

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn, error};

/// SSH key setup configuration
pub struct SshSetup {
    pub key_path: PathBuf,
    pub key_type: String,
    pub key_bits: u32,
}

impl Default for SshSetup {
    fn default() -> Self {
        let home = std::env::var("HOME").expect("HOME not set");
        Self {
            key_path: PathBuf::from(format!("{}/.ssh/sai3bench_id_rsa", home)),
            key_type: "rsa".to_string(),
            key_bits: 4096,
        }
    }
}

impl SshSetup {
    /// Generate SSH key pair if it doesn't exist
    pub fn generate_key(&self) -> Result<()> {
        if self.key_path.exists() {
            info!("SSH key already exists: {}", self.key_path.display());
            return Ok(());
        }
        
        info!("Generating SSH key pair: {}", self.key_path.display());
        
        // Create .ssh directory if it doesn't exist
        if let Some(parent) = self.key_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create .ssh directory")?;
            
            // Set proper permissions on .ssh directory
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = fs::metadata(parent)?.permissions();
                perms.set_mode(0o700);
                fs::set_permissions(parent, perms)?;
            }
        }
        
        // Generate key with ssh-keygen
        let output = Command::new("ssh-keygen")
            .arg("-t").arg(&self.key_type)
            .arg("-b").arg(self.key_bits.to_string())
            .arg("-f").arg(&self.key_path)
            .arg("-N").arg("")  // No passphrase for automation
            .arg("-C").arg("sai3bench-automation")
            .output()
            .context("Failed to execute ssh-keygen. Is it installed?")?;
        
        if !output.status.success() {
            bail!("ssh-keygen failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        
        // Set proper permissions on private key
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&self.key_path)?.permissions();
            perms.set_mode(0o600);
            fs::set_permissions(&self.key_path, perms)?;
        }
        
        info!("✓ SSH key generated: {}", self.key_path.display());
        info!("  Public key: {}.pub", self.key_path.display());
        
        Ok(())
    }
    
    /// Get public key content
    pub fn get_public_key(&self) -> Result<String> {
        let pub_key_path = format!("{}.pub", self.key_path.display());
        fs::read_to_string(&pub_key_path)
            .context(format!("Failed to read public key: {}", pub_key_path))
    }
    
    /// Copy public key to remote host using ssh-copy-id
    pub fn copy_key_to_host(&self, host: &str, user: &str, password_prompt: bool) -> Result<()> {
        info!("Copying SSH key to {}@{}", user, host);
        
        let pub_key_path = format!("{}.pub", self.key_path.display());
        
        if password_prompt {
            // Interactive mode - use ssh-copy-id for password prompt
            info!("You will be prompted for the password for {}@{}", user, host);
            
            let status = Command::new("ssh-copy-id")
                .arg("-i").arg(&pub_key_path)
                .arg(format!("{}@{}", user, host))
                .status()
                .context("Failed to execute ssh-copy-id. Is it installed?")?;
            
            if !status.success() {
                bail!("ssh-copy-id failed for {}@{}", user, host);
            }
        } else {
            // Non-interactive mode - manual copy
            warn!("Non-interactive mode: You must manually copy the public key to the remote host");
            warn!("Run this command:");
            warn!("  ssh-copy-id -i {} {}@{}", pub_key_path, user, host);
            return Ok(());
        }
        
        info!("✓ SSH key copied to {}@{}", user, host);
        
        Ok(())
    }
    
    /// Verify passwordless SSH access to host
    pub fn verify_access(&self, host: &str, user: &str) -> Result<bool> {
        info!("Verifying passwordless SSH access to {}@{}", user, host);
        
        let output = Command::new("ssh")
            .arg("-i").arg(&self.key_path)
            .arg("-o").arg("BatchMode=yes")  // Fail if password is required
            .arg("-o").arg("StrictHostKeyChecking=no")  // Auto-accept host key
            .arg("-o").arg("ConnectTimeout=5")
            .arg(format!("{}@{}", user, host))
            .arg("echo OK")
            .output()
            .context("Failed to execute ssh")?;
        
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.trim() == "OK" {
                info!("✓ Passwordless SSH access verified for {}@{}", user, host);
                return Ok(true);
            }
        }
        
        warn!("✗ Passwordless SSH access failed for {}@{}", user, host);
        warn!("  Error: {}", String::from_utf8_lossy(&output.stderr));
        
        Ok(false)
    }
    
    /// Test Docker availability on remote host
    pub fn verify_docker(&self, host: &str, user: &str) -> Result<bool> {
        info!("Checking Docker availability on {}@{}", user, host);
        
        let output = Command::new("ssh")
            .arg("-i").arg(&self.key_path)
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("ConnectTimeout=5")
            .arg(format!("{}@{}", user, host))
            .arg("docker --version")
            .output()
            .context("Failed to execute ssh")?;
        
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout);
            info!("✓ Docker found on {}: {}", host, version.trim());
            return Ok(true);
        }
        
        warn!("✗ Docker not found on {}", host);
        warn!("  Install Docker: curl -fsSL https://get.docker.com | sh");
        
        Ok(false)
    }
    
    /// Full automated setup for a single host
    pub fn setup_host(&self, host: &str, user: &str, password_prompt: bool) -> Result<()> {
        println!("\n=== Setting up SSH access to {}@{} ===", user, host);
        
        // Generate key if needed
        self.generate_key()?;
        
        // Copy key to host
        self.copy_key_to_host(host, user, password_prompt)?;
        
        // Wait a moment for key to propagate
        std::thread::sleep(std::time::Duration::from_secs(crate::constants::SSH_KEY_PROPAGATION_WAIT_SECS));
        
        // Verify access
        if !self.verify_access(host, user)? {
            bail!("Failed to verify passwordless SSH access to {}@{}", user, host);
        }
        
        // Verify Docker
        if !self.verify_docker(host, user)? {
            warn!("Docker is not available on {}. You may need to install it.", host);
        }
        
        println!("✓ Host {} is ready for distributed testing", host);
        
        Ok(())
    }
    
    /// Setup multiple hosts in sequence
    pub fn setup_hosts(&self, hosts: &[(String, String)], password_prompt: bool) -> Result<()> {
        println!("\n=== SSH Setup for Distributed Testing ===");
        println!("Setting up {} hosts for sai3-bench distributed testing\n", hosts.len());
        
        // Generate key once
        self.generate_key()?;
        
        let mut success_count = 0;
        let mut failed_hosts = Vec::new();
        
        for (host, user) in hosts {
            match self.setup_host(host, user, password_prompt) {
                Ok(_) => success_count += 1,
                Err(e) => {
                    error!("Failed to setup {}@{}: {}", user, host, e);
                    failed_hosts.push(format!("{}@{}", user, host));
                }
            }
        }
        
        println!("\n=== Setup Summary ===");
        println!("✓ Successfully configured: {}/{}", success_count, hosts.len());
        
        if !failed_hosts.is_empty() {
            println!("✗ Failed hosts: {}", failed_hosts.join(", "));
            bail!("Some hosts failed to configure. See errors above.");
        }
        
        println!("\n=== Next Steps ===");
        println!("1. Update your workload YAML with:");
        println!("   distributed:");
        println!("     ssh:");
        println!("       enabled: true");
        println!("       user: {}", hosts[0].1);  // Use first user as example
        println!("       key_path: {}", self.key_path.display());
        println!("     agents:");
        for (host, _) in hosts {
            println!("       - address: {}", host);
        }
        println!("\n2. Run distributed test:");
        println!("   sai3bench-ctl run --config your-workload.yaml");
        
        Ok(())
    }
}

/// Print SSH setup instructions
pub fn print_setup_instructions() {
    println!("\n=== Manual SSH Setup Instructions ===\n");
    println!("If automatic setup fails, follow these manual steps:\n");
    println!("1. Generate SSH key (if you don't have one):");
    println!("   ssh-keygen -t rsa -b 4096 -f ~/.ssh/sai3bench_id_rsa -N '' -C 'sai3bench-automation'\n");
    println!("2. Copy key to each remote host:");
    println!("   ssh-copy-id -i ~/.ssh/sai3bench_id_rsa.pub user@hostname\n");
    println!("3. Verify passwordless access:");
    println!("   ssh -i ~/.ssh/sai3bench_id_rsa user@hostname echo OK\n");
    println!("4. Verify Docker is installed:");
    println!("   ssh -i ~/.ssh/sai3bench_id_rsa user@hostname docker --version\n");
    println!("5. If Docker is missing, install it:");
    println!("   ssh user@hostname 'curl -fsSL https://get.docker.com | sh'\n");
    println!("6. Update your workload YAML with the agent hosts and SSH config\n");
}

/// Quick test of SSH connectivity to all configured agents
pub fn test_connectivity(hosts: &[(String, String)], key_path: &Path) -> Result<()> {
    println!("\n=== Testing SSH Connectivity ===");
    
    let mut all_ok = true;
    
    for (host, user) in hosts {
        print!("Testing {}@{}... ", user, host);
        
        let output = Command::new("ssh")
            .arg("-i").arg(key_path)
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("ConnectTimeout=5")
            .arg(format!("{}@{}", user, host))
            .arg("echo OK && docker --version")
            .output()?;
        
        if output.status.success() {
            println!("✓ OK");
        } else {
            println!("✗ FAILED");
            all_ok = false;
        }
    }
    
    if all_ok {
        println!("\n✓ All hosts are ready for distributed testing");
        Ok(())
    } else {
        bail!("Some hosts failed connectivity test. Run 'ssh-setup' to configure them.");
    }
}
