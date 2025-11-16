// src/bin/controller.rs

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tracing::{debug, error, info, warn};

// Results directory for v0.6.4+
use sai3_bench::results_dir::ResultsDir;
// Import BUCKET_LABELS from metrics module
use sai3_bench::metrics::BUCKET_LABELS;

pub mod pb {
    pub mod iobench {
        include!("../pb/iobench.rs");
    }
}

use pb::iobench::agent_client::AgentClient;
use pb::iobench::{Empty, LiveStats, PrepareSummary, RunGetRequest, RunPutRequest, RunWorkloadRequest, WorkloadSummary};

/// v0.7.5: Aggregator for live stats from multiple agents
/// 
/// Collects LiveStats messages from all agent streams and computes weighted aggregate metrics.
/// Uses weighted averaging for latencies (weighted by operation count).
struct LiveStatsAggregator {
    agent_stats: std::collections::HashMap<String, LiveStats>,
}

impl LiveStatsAggregator {
    fn new() -> Self {
        Self {
            agent_stats: std::collections::HashMap::new(),
        }
    }

    /// Update stats for a specific agent
    fn update(&mut self, stats: LiveStats) {
        self.agent_stats.insert(stats.agent_id.clone(), stats);
    }

    /// Mark an agent as completed
    fn mark_completed(&mut self, agent_id: &str) {
        if let Some(stats) = self.agent_stats.get_mut(agent_id) {
            stats.completed = true;
        }
    }
    
    /// Reset all accumulated stats (v0.7.9+)
    /// Call this when transitioning from prepare to workload phase
    fn reset_stats(&mut self) {
        self.agent_stats.clear();
    }

    /// Check if all agents have completed
    fn all_completed(&self) -> bool {
        !self.agent_stats.is_empty() && self.agent_stats.values().all(|s| s.completed)
    }

    /// Get aggregate stats across all agents (weighted latency averaging)
    fn aggregate(&self) -> AggregateStats {
        let mut total_get_ops = 0u64;
        let mut total_get_bytes = 0u64;
        let mut total_put_ops = 0u64;
        let mut total_put_bytes = 0u64;
        let mut total_meta_ops = 0u64;
        let mut max_elapsed = 0.0f64;

        // Weighted sums for latency averaging
        let mut get_mean_weighted = 0.0f64;
        let mut get_p50_weighted = 0.0f64;
        let mut get_p95_weighted = 0.0f64;
        let mut put_mean_weighted = 0.0f64;
        let mut put_p50_weighted = 0.0f64;
        let mut put_p95_weighted = 0.0f64;
        let mut meta_mean_weighted = 0.0f64;

        for stats in self.agent_stats.values() {
            // Sum operations and bytes
            total_get_ops += stats.get_ops;
            total_get_bytes += stats.get_bytes;
            total_put_ops += stats.put_ops;
            total_put_bytes += stats.put_bytes;
            total_meta_ops += stats.meta_ops;
            max_elapsed = max_elapsed.max(stats.elapsed_s);

            // Weighted latency sums (weight = operation count)
            if stats.get_ops > 0 {
                let weight = stats.get_ops as f64;
                get_mean_weighted += stats.get_mean_us * weight;
                get_p50_weighted += stats.get_p50_us * weight;
                get_p95_weighted += stats.get_p95_us * weight;
            }
            if stats.put_ops > 0 {
                let weight = stats.put_ops as f64;
                put_mean_weighted += stats.put_mean_us * weight;
                put_p50_weighted += stats.put_p50_us * weight;
                put_p95_weighted += stats.put_p95_us * weight;
            }
            if stats.meta_ops > 0 {
                let weight = stats.meta_ops as f64;
                meta_mean_weighted += stats.meta_mean_us * weight;
            }
        }

        // Calculate weighted averages
        let get_mean_us = if total_get_ops > 0 {
            get_mean_weighted / total_get_ops as f64
        } else {
            0.0
        };
        let get_p50_us = if total_get_ops > 0 {
            get_p50_weighted / total_get_ops as f64
        } else {
            0.0
        };
        let get_p95_us = if total_get_ops > 0 {
            get_p95_weighted / total_get_ops as f64
        } else {
            0.0
        };
        let put_mean_us = if total_put_ops > 0 {
            put_mean_weighted / total_put_ops as f64
        } else {
            0.0
        };
        let put_p50_us = if total_put_ops > 0 {
            put_p50_weighted / total_put_ops as f64
        } else {
            0.0
        };
        let put_p95_us = if total_put_ops > 0 {
            put_p95_weighted / total_put_ops as f64
        } else {
            0.0
        };
        let meta_mean_us = if total_meta_ops > 0 {
            meta_mean_weighted / total_meta_ops as f64
        } else {
            0.0
        };

        AggregateStats {
            num_agents: self.agent_stats.len(),
            total_get_ops,
            total_get_bytes,
            get_mean_us,
            get_p50_us,
            get_p95_us,
            total_put_ops,
            total_put_bytes,
            put_mean_us,
            put_p50_us,
            put_p95_us,
            total_meta_ops,
            meta_mean_us,
            elapsed_s: max_elapsed,
        }
    }
}

/// Aggregate statistics across all agents
#[derive(Debug, Clone)]
struct AggregateStats {
    num_agents: usize,
    total_get_ops: u64,
    total_get_bytes: u64,
    get_mean_us: f64,
    get_p50_us: f64,
    get_p95_us: f64,
    total_put_ops: u64,
    total_put_bytes: u64,
    put_mean_us: f64,
    put_p50_us: f64,
    put_p95_us: f64,
    total_meta_ops: u64,
    meta_mean_us: f64,
    elapsed_s: f64,
}

impl AggregateStats {
    /// Format multi-line progress display (GET on line 1, PUT on line 2, META on line 3 if present)
    fn format_progress(&self) -> String {
        let get_ops_s = if self.elapsed_s > 0.0 {
            self.total_get_ops as f64 / self.elapsed_s
        } else {
            0.0
        };
        let get_bandwidth = format_bandwidth(self.total_get_bytes, self.elapsed_s);
        
        let put_ops_s = if self.elapsed_s > 0.0 {
            self.total_put_ops as f64 / self.elapsed_s
        } else {
            0.0
        };
        let put_bandwidth = format_bandwidth(self.total_put_bytes, self.elapsed_s);
        
        let meta_ops_s = if self.elapsed_s > 0.0 {
            self.total_meta_ops as f64 / self.elapsed_s
        } else {
            0.0
        };

        // Only show META line if there are META operations
        if self.total_meta_ops > 0 {
            format!(
                "{} agents\n  GET: {:.0} ops/s, {} (mean: {:.0}µs, p50: {:.0}µs, p95: {:.0}µs)\n  PUT: {:.0} ops/s, {} (mean: {:.0}µs, p50: {:.0}µs, p95: {:.0}µs)\n  META: {:.0} ops/s (mean: {:.0}µs)",
                self.num_agents,
                get_ops_s,
                get_bandwidth,
                self.get_mean_us,
                self.get_p50_us,
                self.get_p95_us,
                put_ops_s,
                put_bandwidth,
                self.put_mean_us,
                self.put_p50_us,
                self.put_p95_us,
                meta_ops_s,
                self.meta_mean_us,
            )
        } else {
            format!(
                "{} agents\n  GET: {:.0} ops/s, {} (mean: {:.0}µs, p50: {:.0}µs, p95: {:.0}µs)\n  PUT: {:.0} ops/s, {} (mean: {:.0}µs, p50: {:.0}µs, p95: {:.0}µs)",
                self.num_agents,
                get_ops_s,
                get_bandwidth,
                self.get_mean_us,
                self.get_p50_us,
                self.get_p95_us,
                put_ops_s,
                put_bandwidth,
                self.put_mean_us,
                self.put_p50_us,
                self.put_p95_us,
            )
        }
    }
}

/// Format bytes/sec as human-readable bandwidth
fn format_bandwidth(bytes: u64, seconds: f64) -> String {
    if seconds <= 0.0 {
        return "0 B/s".to_string();
    }
    
    let bytes_per_sec = bytes as f64 / seconds;
    
    if bytes_per_sec >= 1_073_741_824.0 {
        // >= 1 GiB/s
        format!("{:.2} GiB/s", bytes_per_sec / 1_073_741_824.0)
    } else if bytes_per_sec >= 1_048_576.0 {
        // >= 1 MiB/s
        format!("{:.1} MiB/s", bytes_per_sec / 1_048_576.0)
    } else if bytes_per_sec >= 1024.0 {
        // >= 1 KiB/s
        format!("{:.1} KiB/s", bytes_per_sec / 1024.0)
    } else {
        format!("{:.0} B/s", bytes_per_sec)
    }
}

#[derive(Parser)]
#[command(name = "sai3bench-ctl", version, about = "SAI3 Benchmark Controller (gRPC)")]
struct Cli {
    /// Increase verbosity (-v = info, -vv = debug, -vvv = trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Comma-separated agent addresses (host:port)
    #[arg(long)]
    agents: String,

    /// Enable TLS for secure connections (requires --agent-ca)
    /// Default is plaintext HTTP (no TLS)
    #[arg(long, default_value_t = false)]
    tls: bool,

    /// Path to PEM file containing the agent's self-signed certificate (to trust)
    /// Required when --tls is enabled
    #[arg(long)]
    agent_ca: Option<PathBuf>,

    /// Override SNI / domain name for TLS (default "localhost").
    /// For IP addresses, you probably generated the agent cert with "DNS:localhost".
    #[arg(long, default_value = "localhost")]
    agent_domain: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Simple reachability check (ping) against all agents
    Ping,
    
    /// SSH setup wizard for distributed testing (v0.6.11+)
    /// Automates SSH key generation, distribution, and verification
    SshSetup {
        /// Hosts to configure (format: user@host or just host, comma-separated)
        /// Example: "ubuntu@vm1.example.com,ubuntu@vm2.example.com"
        #[arg(long)]
        hosts: String,
        
        /// Default SSH user if not specified in host (default: current user)
        #[arg(long)]
        user: Option<String>,
        
        /// SSH key path (default: ~/.ssh/sai3bench_id_rsa)
        #[arg(long)]
        key_path: Option<PathBuf>,
        
        /// Interactive mode: prompt for passwords (default: true)
        #[arg(long, default_value_t = true)]
        interactive: bool,
        
        /// Test connectivity only (don't setup)
        #[arg(long, default_value_t = false)]
        test_only: bool,
    },
    
    /// Distributed GET
    Get {
        #[arg(long)]
        uri: String,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Distributed PUT
    Put {
        #[arg(long)]
        bucket: String,
        #[arg(long, default_value = "bench/")]
        prefix: String,
        #[arg(long, default_value_t = 1024)]
        object_size: u64,
        #[arg(long, default_value_t = 1)]
        objects: u64,
        #[arg(long, default_value_t = 4)]
        concurrency: u64,
    },
    /// Distributed workload from YAML configuration (v0.6.0)
    Run {
        /// Path to YAML workload configuration file
        #[arg(long)]
        config: PathBuf,
        
        /// Validate configuration without executing workload
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        
        /// Path prefix template for agent isolation (e.g., "agent-{id}/")
        /// Use {id} as placeholder for agent number (1, 2, 3, ...)
        /// Default: "agent-{id}/"
        #[arg(long, default_value = "agent-{id}/")]
        path_template: String,
        
        /// Explicit agent IDs (comma-separated, optional)
        /// If not provided, agents will be numbered: agent-1, agent-2, agent-3, etc.
        /// Example: "node-a,node-b,node-c"
        #[arg(long)]
        agent_ids: Option<String>,
        
        /// Delay in seconds before coordinated start (default: 2)
        /// Allows time for all agents to receive config and prepare
        #[arg(long, default_value_t = 2)]
        start_delay: u64,
        
        /// Use shared storage for prepare phase (default: auto-detect)
        /// When true: prepare phase runs once, all agents use same data (S3, GCS, Azure, NFS)
        /// When false: each agent prepares its own local data (file://, direct://)
        /// If not specified, auto-detects based on URI scheme (s3://, az://, gs:// = shared)
        #[arg(long)]
        shared_prepare: Option<bool>,
    },
}

async fn mk_client(
    target: &str,
    insecure: bool,
    ca_path: Option<&PathBuf>,
    sni_domain: &str,
) -> Result<AgentClient<Channel>> {
    if insecure {
        // Plain HTTP
        let ep = format!("http://{}", target);
        let channel = Endpoint::try_from(ep)?
            .connect_timeout(Duration::from_secs(5))
            .tcp_nodelay(true)
            .connect()
            .await?;
        return Ok(AgentClient::new(channel));
    }

    // HTTPS + custom root (self-signed cert from the agent)
    let ca_pem = match ca_path {
        Some(p) => fs::read(p).context("reading --agent-ca")?,
        None => anyhow::bail!("TLS is enabled (not --insecure), but --agent-ca was not provided"),
    };
    let ca = Certificate::from_pem(ca_pem);

    let tls = ClientTlsConfig::new()
        .ca_certificate(ca)
        .domain_name(sni_domain);

    let ep = format!("https://{}", target);
    let channel = Endpoint::try_from(ep)?
        .tls_config(tls)?
        .connect_timeout(Duration::from_secs(5))
        .tcp_nodelay(true)
        .connect()
        .await?;
    Ok(AgentClient::new(channel))
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    dotenv().ok();
    let cli = Cli::parse();

    // Initialize logging based on verbosity level
    let level = match cli.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::new(format!("sai3bench_ctl={}", level));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    debug!("Logging initialized at level: {}", level);

    let agents: Vec<String> = cli
        .agents
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    info!("Controller starting with {} agent(s)", agents.len());
    debug!("Agent addresses: {:?}", agents);

    match &cli.command {
        Commands::SshSetup { hosts, user, key_path, interactive, test_only } => {
            use sai3_bench::ssh_setup::{SshSetup, test_connectivity, print_setup_instructions};
            
            // Parse hosts
            let default_user = user.clone().unwrap_or_else(|| {
                std::env::var("USER").unwrap_or_else(|_| "ubuntu".to_string())
            });
            
            let parsed_hosts: Vec<(String, String)> = hosts
                .split(',')
                .map(|h| h.trim())
                .filter(|h| !h.is_empty())
                .map(|h| {
                    if h.contains('@') {
                        let parts: Vec<&str> = h.split('@').collect();
                        (parts[1].to_string(), parts[0].to_string())
                    } else {
                        (h.to_string(), default_user.clone())
                    }
                })
                .collect();
            
            if parsed_hosts.is_empty() {
                bail!("No hosts specified. Use --hosts user@host1,user@host2");
            }
            
            let mut setup = SshSetup::default();
            if let Some(kp) = key_path {
                setup.key_path = kp.clone();
            }
            
            if *test_only {
                // Test connectivity only
                test_connectivity(&parsed_hosts, &setup.key_path)?;
            } else {
                // Full setup
                match setup.setup_hosts(&parsed_hosts, *interactive) {
                    Ok(_) => info!("SSH setup completed successfully"),
                    Err(e) => {
                        error!("SSH setup failed: {}", e);
                        print_setup_instructions();
                        bail!(e);
                    }
                }
            }
            
            return Ok(());
        }
        
        Commands::Ping => {
            for a in &agents {
                let mut c = mk_client(a, !cli.tls, cli.agent_ca.as_ref(), &cli.agent_domain)
                    .await
                    .with_context(|| format!("connect to {}", a))?;
                let r = c.ping(Empty {}).await?.into_inner();
                eprintln!("connected to {} (agent version {})", a, r.version);
            }
        }
        Commands::Get { uri, jobs } => {
            for a in &agents {
                let mut c = mk_client(a, !cli.tls, cli.agent_ca.as_ref(), &cli.agent_domain)
                    .await
                    .with_context(|| format!("connect to {}", a))?;
                let r = c
                    .run_get(RunGetRequest {
                        uri: uri.clone(),
                        jobs: *jobs as u32,
                    })
                    .await?
                    .into_inner();
                eprintln!(
                    "[{}] GET: {:.2} MB in {:.3}s -> {:.2} MB/s",
                    a,
                    r.total_bytes as f64 / 1_048_576.0,
                    r.seconds,
                    (r.total_bytes as f64 / 1_048_576.0) / r.seconds.max(1e-6)
                );
            }
        }
        Commands::Put {
            bucket,
            prefix,
            object_size,
            objects,
            concurrency,
        } => {
            for a in &agents {
                let mut c = mk_client(a, !cli.tls, cli.agent_ca.as_ref(), &cli.agent_domain)
                    .await
                    .with_context(|| format!("connect to {}", a))?;
                let r = c
                    .run_put(RunPutRequest {
                        bucket: bucket.clone(),
                        prefix: prefix.clone(),
                        object_size: *object_size,
                        objects: *objects as u32,         // <-- proto expects u32
                        concurrency: *concurrency as u32, // <-- proto expects u32
                    })
                    .await?
                    .into_inner();
                eprintln!(
                    "[{}] PUT: {:.2} MB in {:.3}s -> {:.2} MB/s",
                    a,
                    r.total_bytes as f64 / 1_048_576.0,
                    r.seconds,
                    (r.total_bytes as f64 / 1_048_576.0) / r.seconds.max(1e-6)
                );
            }
        }
        Commands::Run {
            config,
            dry_run,
            path_template,
            agent_ids,
            start_delay,
            shared_prepare,
        } => {
            // Read config to check for distributed.agents
            let config_yaml = fs::read_to_string(config)
                .with_context(|| format!("Failed to read config file: {}", config.display()))?;
            let parsed_config: sai3_bench::config::Config = serde_yaml::from_str(&config_yaml)
                .context("Failed to parse YAML config")?;
            
            // If dry-run, just validate and exit
            if *dry_run {
                println!("✅ Configuration validated successfully");
                println!("   Config file: {}", config.display());
                if let Some(ref dist) = parsed_config.distributed {
                    println!("   Agents: {}", dist.agents.len());
                    println!("   Shared filesystem: {}", dist.shared_filesystem);
                    println!("   Tree creation: {:?}", dist.tree_creation_mode);
                }
                println!("\nTo execute, run without --dry-run");
                return Ok(());
            }
            
            // Determine agent addresses: config.distributed.agents takes precedence over CLI --agents
            let (agent_addrs, ssh_deployment) = if let Some(ref dist_config) = parsed_config.distributed {
                // Use agents from config
                let addrs: Vec<String> = dist_config.agents.iter()
                    .map(|a| {
                        // If SSH deployment, use address without port (we'll add listen_port)
                        if dist_config.ssh.as_ref().map(|s| s.enabled).unwrap_or(false) {
                            // SSH mode: address might be just hostname
                            if a.address.contains(':') {
                                a.address.clone()
                            } else {
                                format!("{}:{}", a.address, a.listen_port)
                            }
                        } else {
                            // Direct gRPC mode: use address as-is
                            a.address.clone()
                        }
                    })
                    .collect();
                
                info!("Using {} agents from config.distributed.agents", addrs.len());
                
                // Check if SSH deployment is enabled
                let ssh_enabled = dist_config.ssh.as_ref().map(|s| s.enabled).unwrap_or(false);
                
                (addrs, if ssh_enabled { Some(dist_config.clone()) } else { None })
            } else if !agents.is_empty() {
                // Fallback to CLI --agents (backward compatibility)
                info!("Using {} agents from CLI --agents argument", agents.len());
                (agents.clone(), None)
            } else {
                bail!("No agents specified. Use --agents on CLI or distributed.agents in config YAML");
            };
            
            run_distributed_workload(
                &agent_addrs,
                config,
                path_template,
                agent_ids.as_deref(),
                *start_delay,
                *shared_prepare,
                ssh_deployment,
                !cli.tls,
                cli.agent_ca.as_ref(),
                &cli.agent_domain,
            )
            .await?;
        }
    }

    Ok(())
}

/// Execute a distributed workload across multiple agents
async fn run_distributed_workload(
    agent_addrs: &[String],
    config_path: &PathBuf,
    path_template: &str,
    agent_ids: Option<&str>,
    start_delay_secs: u64,
    shared_prepare: Option<bool>,
    ssh_deployment: Option<sai3_bench::config::DistributedConfig>,
    insecure: bool,
    agent_ca: Option<&PathBuf>,
    agent_domain: &str,
) -> Result<()> {
    info!("Starting distributed workload execution");
    debug!("Config file: {}", config_path.display());
    debug!("Path template: {}", path_template);
    debug!("Start delay: {}s", start_delay_secs);
    
    // Create results directory for distributed run (v0.6.4+)
    let mut results_dir = ResultsDir::create(config_path, None, None)
        .context("Failed to create results directory for distributed run")?;
    
    // Mark as distributed and create agents subdirectory
    let agents_dir = results_dir.create_agents_dir()?;
    info!("Created agents directory: {}", agents_dir.display());
    
    // SSH Deployment: Start agent containers if enabled (v0.6.11+)
    let mut deployments: Vec<sai3_bench::ssh_deploy::AgentDeployment> = Vec::new();
    if let Some(ref dist_config) = ssh_deployment {
        if let (Some(ref ssh_config), Some(ref deployment_config)) = 
            (&dist_config.ssh, &dist_config.deployment) {
            
            if ssh_config.enabled {
                info!("SSH deployment enabled - starting agent containers");
                
                let deploy_msg = format!("Deploying {} agents via SSH + Docker...", dist_config.agents.len());
                println!("{}", deploy_msg);
                results_dir.write_console(&deploy_msg)?;
                
                // Deploy agents
                use sai3_bench::ssh_deploy;
                match ssh_deploy::deploy_agents(&dist_config.agents, deployment_config, ssh_config).await {
                    Ok(deployed) => {
                        info!("Successfully deployed {} agents", deployed.len());
                        
                        let success_msg = format!("✓ All {} agents deployed and ready", deployed.len());
                        println!("{}", success_msg);
                        results_dir.write_console(&success_msg)?;
                        
                        deployments = deployed;
                    }
                    Err(e) => {
                        error!("Agent deployment failed: {}", e);
                        bail!("Failed to deploy agents via SSH: {}", e);
                    }
                }
                
                // Wait a moment for agents to be fully ready
                info!("Waiting 2s for agents to initialize...");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    
    // Read YAML configuration
    let config_yaml = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

    debug!("Config YAML loaded, {} bytes", config_yaml.len());
    
    // Parse config early so we can use it for progress bar and storage detection
    let config: sai3_bench::config::Config = serde_yaml::from_str(&config_yaml)
        .context("Failed to parse config")?;

    // Determine if storage is shared:
    // 1. CLI flag --shared-prepare takes highest priority (explicit override)
    // 2. Config file's distributed.shared_filesystem setting
    // 3. Auto-detect from URI scheme (fallback for backward compatibility)
    let is_shared_storage = if let Some(explicit) = shared_prepare {
        debug!("Using explicit --shared-prepare CLI flag: {}", explicit);
        explicit
    } else if let Some(ref distributed) = config.distributed {
        debug!("Using config file's shared_filesystem setting: {}", distributed.shared_filesystem);
        distributed.shared_filesystem
    } else {
        let is_shared = detect_shared_storage(&config);
        debug!("Auto-detected shared storage: {} (based on URI scheme - no config setting found)", is_shared);
        is_shared
    };

    let header = "=== Distributed Workload ===";
    println!("{}", header);
    results_dir.write_console(header)?;
    
    let config_msg = format!("Config: {}", config_path.display());
    println!("{}", config_msg);
    results_dir.write_console(&config_msg)?;
    
    let agents_msg = format!("Agents: {}", agent_addrs.len());
    println!("{}", agents_msg);
    results_dir.write_console(&agents_msg)?;
    
    let delay_msg = format!("Start delay: {}s", start_delay_secs);
    println!("{}", delay_msg);
    results_dir.write_console(&delay_msg)?;
    
    let storage_msg = format!("Storage mode: {}", if is_shared_storage { "shared (S3/GCS/Azure/NFS)" } else { "local (per-agent)" });
    println!("{}", storage_msg);
    results_dir.write_console(&storage_msg)?;
    
    println!();
    results_dir.write_console("")?;

    // Generate agent IDs
    let ids: Vec<String> = if let Some(custom_ids) = agent_ids {
        debug!("Using custom agent IDs: {}", custom_ids);
        custom_ids
            .split(',')
            .map(|s| s.trim().to_string())
            .collect()
    } else {
        let default_ids: Vec<String> = (1..=agent_addrs.len())
            .map(|i| format!("agent-{}", i))
            .collect();
        debug!("Using default agent IDs: {:?}", default_ids);
        default_ids
    };

    if ids.len() != agent_addrs.len() {
        anyhow::bail!(
            "Number of agent IDs ({}) doesn't match number of agent addresses ({})",
            ids.len(),
            agent_addrs.len()
        );
    }
    
    // Register agents in metadata
    for (idx, agent_id) in ids.iter().enumerate() {
        results_dir.add_agent(format!("{} ({})", agent_id, agent_addrs[idx]));
    }

    // Calculate coordinated start time (N seconds in the future)
    // v0.7.6: Add extra time for validation (3s) + user's start_delay
    let validation_time_secs = 3;
    let total_delay_secs = validation_time_secs + start_delay_secs;
    let start_time = SystemTime::now() + Duration::from_secs(total_delay_secs);
    let start_ns = start_time
        .duration_since(UNIX_EPOCH)?
        .as_nanos() as i64;

    debug!("Coordinated start time: {} ns since epoch (validation: {}s, user delay: {}s)", 
           start_ns, validation_time_secs, start_delay_secs);

    // v0.7.5: Use streaming RPC for live progress updates
    let msg = format!("Starting workload on {} agents with live stats...", agent_addrs.len());
    println!("{}", msg);
    results_dir.write_console(&msg)?;
    println!();  // Blank line before progress display
    
    // Create progress display using indicatif
    use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
    let multi_progress = MultiProgress::new();
    
    // v0.7.6: Use progress bar with duration (not spinner) for better visibility
    let duration_secs = config.duration.as_secs();
    let progress_bar = multi_progress.add(ProgressBar::new(duration_secs));
    progress_bar.set_style(
        ProgressStyle::default_bar()
            .template("{bar:40.cyan/blue} {pos:>7}/{len:7} {unit}\n{msg}")
            .expect("Invalid progress template")
            .progress_chars("█▓▒░ ")
    );
    progress_bar.enable_steady_tick(std::time::Duration::from_millis(100));
    
    // v0.7.6: Create channel BEFORE spawning tasks (tasks need tx)
    let (tx_stats, mut rx_stats) = tokio::sync::mpsc::channel::<LiveStats>(100);
    
    // Spawn tasks to consume agent streams
    let mut stream_handles = Vec::new();
    for (idx, agent_addr) in agent_addrs.iter().enumerate() {
        let agent_id = ids[idx].clone();
        let path_prefix = path_template.replace("{id}", &(idx + 1).to_string());
        
        // v0.7.9: For shared storage (GCS/S3/Azure), don't use path prefix - all agents access same data
        let effective_prefix = if is_shared_storage {
            String::new()  // Empty prefix for shared storage
        } else {
            path_prefix.clone()  // Use agent-specific prefix for local storage
        };
        
        debug!("Agent {}: ID='{}', prefix='{}', effective_prefix='{}', address='{}', shared_storage={}", 
               idx + 1, agent_id, path_prefix, effective_prefix, agent_addr, is_shared_storage);
        
        let config = config_yaml.clone();
        let addr = agent_addr.clone();
        let insecure = insecure;
        let ca = agent_ca.cloned();
        let domain = agent_domain.to_string();
        let shared = is_shared_storage;
        let tx = tx_stats.clone();  // v0.7.6: Clone sender for this task

        let handle = tokio::spawn(async move {
            debug!("Connecting to agent at {}", addr);
            
            // Connect to agent
            let mut client = mk_client(&addr, insecure, ca.as_ref(), &domain)
                .await
                .with_context(|| format!("connect to agent {}", addr))?;

            debug!("Connected to agent {}, starting streaming workload", addr);

            // Send streaming workload request
            let mut stream = client
                .run_workload_with_live_stats(RunWorkloadRequest {
                    config_yaml: config,
                    agent_id: agent_id.clone(),
                    path_prefix: effective_prefix.clone(),
                    start_timestamp_ns: start_ns,
                    shared_storage: shared,
                })
                .await
                .with_context(|| format!("run_workload_with_live_stats on agent {}", addr))?
                .into_inner();

            debug!("Agent {} streaming started", addr);
            
            // v0.7.6: Forward messages immediately as they arrive (don't wait for stream completion)
            let mut stats_count = 0;
            while let Some(stats_result) = stream.message().await? {
                stats_count += 1;
                debug!("Agent {} sent stats update #{}: {} ops, {} bytes", addr, stats_count, 
                       stats_result.get_ops + stats_result.put_ops + stats_result.meta_ops,
                       stats_result.get_bytes + stats_result.put_bytes);
                if let Err(e) = tx.send(stats_result).await {
                    error!("Failed to forward stats from agent {}: {}", addr, e);
                    break;
                }
            }
            
            info!("Agent {} stream completed with {} updates", addr, stats_count);
            if stats_count == 0 {
                warn!("⚠️  Agent {} stream ended with ZERO updates - possible connection issue!", addr);
            }
            Ok::<(), anyhow::Error>(())
        });

        stream_handles.push(handle);
    }

    info!("Streaming from {} agents", stream_handles.len());
    drop(tx_stats);  // v0.7.6: Drop original sender so channel closes when all tasks complete
    
    // v0.7.6: STARTUP HANDSHAKE - Track agent readiness
    let expected_agent_count = stream_handles.len();
    let mut agent_status: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut ready_agents = std::collections::HashSet::new();
    let mut error_agents: Vec<(String, String)> = Vec::new();
    
    // v0.7.6: Stream forwarding tasks are already running (messages forwarded immediately)
    // Just wait for them to complete in background
    for handle in stream_handles {
        tokio::spawn(async move {
            if let Err(e) = handle.await {
                error!("Stream task error: {:?}", e);
            }
        });
    }
    
    // Setup Ctrl+C handler
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    
    eprintln!("⏳ Waiting for agents to validate configuration...");
    let startup_timeout_secs = validation_time_secs - 2;  // Leave 2s buffer before workload starts
    let startup_timeout = tokio::time::Duration::from_secs(startup_timeout_secs);
    let startup_deadline = tokio::time::Instant::now() + startup_timeout;
    
    // v0.7.6: Wait for READY/ERROR status from all agents
    'startup: loop {
        tokio::select! {
            Some(stats) = rx_stats.recv() => {
                // Check status field
                match stats.status {
                    1 => {  // READY
                        agent_status.insert(stats.agent_id.clone(), "READY".to_string());
                        ready_agents.insert(stats.agent_id.clone());
                        eprintln!("  ✅ {} ready", stats.agent_id);
                    }
                    3 => {  // ERROR
                        agent_status.insert(stats.agent_id.clone(), "ERROR".to_string());
                        error_agents.push((stats.agent_id.clone(), stats.error_message.clone()));
                        eprintln!("  ❌ {} error: {}", stats.agent_id, stats.error_message);
                    }
                    _ => {
                        // Unexpected status during startup (RUNNING/COMPLETED)
                        // This shouldn't happen, but treat as ready
                        if !ready_agents.contains(&stats.agent_id) && !error_agents.iter().any(|(a, _)| a == &stats.agent_id) {
                            agent_status.insert(stats.agent_id.clone(), "READY".to_string());
                            ready_agents.insert(stats.agent_id.clone());
                        }
                    }
                }
                
                // Check if all agents have responded
                if agent_status.len() >= expected_agent_count {
                    break 'startup;
                }
            }
            _ = tokio::time::sleep_until(startup_deadline) => {
                eprintln!("\n❌ Startup timeout: Not all agents responded within {}s", startup_timeout.as_secs());
                eprintln!("Agent status ({}/{}):", agent_status.len(), expected_agent_count);
                for (agent_id, status) in &agent_status {
                    let icon = if status == "READY" { "✅" } else { "❌" };
                    eprintln!("  {} {}: {}", icon, agent_id, status);
                }
                let missing_count = expected_agent_count - agent_status.len();
                if missing_count > 0 {
                    eprintln!("  ⏱️  {} agent(s) did not respond", missing_count);
                }
                anyhow::bail!("Agent startup validation timeout");
            }
            _ = &mut ctrl_c => {
                eprintln!("\n⚠️  Interrupted during startup");
                anyhow::bail!("Interrupted by user");
            }
        }
    }
    
    // Check if any agents reported errors
    if !error_agents.is_empty() {
        eprintln!("\n❌ {} agent(s) failed configuration validation:", error_agents.len());
        for (agent_id, error_msg) in &error_agents {
            eprintln!("  ❌ {}: {}", agent_id, error_msg);
        }
        eprintln!("\nReady agents: {}/{}", ready_agents.len(), expected_agent_count);
        for agent_id in &ready_agents {
            eprintln!("  ✅ {}", agent_id);
        }
        anyhow::bail!("{} agent(s) failed startup validation", error_agents.len());
    }
    
    eprintln!("✅ All {} agents ready - starting workload execution\n", ready_agents.len());
    
    // v0.7.6: Track workload start time for progress bar position
    // Note: This will be reset when prepare phase completes to measure actual workload time
    let mut workload_start = std::time::Instant::now();
    
    // Aggregator for live stats display
    let mut aggregator = LiveStatsAggregator::new();
    let mut last_update = std::time::Instant::now();
    let mut completed_agents = std::collections::HashSet::new();
    
    // v0.7.5: Resilience - track per-agent activity for timeout detection
    let mut agent_last_seen: std::collections::HashMap<String, std::time::Instant> = std::collections::HashMap::new();
    let mut dead_agents = std::collections::HashSet::new();
    let timeout_warn_secs = 5.0;
    let timeout_dead_secs = 10.0;
    
    // v0.7.5: Collect final summaries for persistence (extracted from completed LiveStats messages)
    let mut agent_summaries: Vec<WorkloadSummary> = Vec::new();
    
    // v0.7.9: Collect prepare summaries for persistence  
    let mut prepare_summaries: Vec<PrepareSummary> = Vec::new();
    
    // v0.7.5: Track last console.log write time (write every 1 second)
    let mut last_console_log = std::time::Instant::now();
    
    // v0.7.9: Track prepare phase state to detect transition to workload
    let mut was_in_prepare_phase = false;
    
    // Process live stats stream
    loop {
        // v0.7.5: Check for stalled agents (timeout detection)
        let now = std::time::Instant::now();
        for (agent_id, last_seen) in &agent_last_seen {
            if dead_agents.contains(agent_id) || completed_agents.contains(agent_id) {
                continue;  // Skip already dead or completed agents
            }
            
            let elapsed = now.duration_since(*last_seen).as_secs_f64();
            if elapsed >= timeout_dead_secs {
                if !dead_agents.contains(agent_id) {
                    error!("❌ Agent {} STALLED (no updates for {:.1}s) - marking as DEAD", agent_id, elapsed);
                    dead_agents.insert(agent_id.clone());
                    aggregator.mark_completed(agent_id);  // Remove from active count
                    progress_bar.set_message(format!("{} (⚠️ {} dead)", aggregator.aggregate().format_progress(), dead_agents.len()));
                }
            } else if elapsed >= timeout_warn_secs {
                warn!("⚠️  Agent {} delayed: no updates for {:.1}s", agent_id, elapsed);
            }
        }
        
        tokio::select! {
            stats_opt = rx_stats.recv() => {
                match stats_opt {
                    Some(stats) => {
                        // v0.7.5: Update last seen timestamp for resilience
                        agent_last_seen.insert(stats.agent_id.clone(), std::time::Instant::now());
                        
                        // v0.7.9: Capture prepare phase info BEFORE moving stats
                        let in_prepare = stats.in_prepare_phase;
                        let prepare_created = stats.prepare_objects_created;
                        let prepare_total = stats.prepare_objects_total;
                        
                        // v0.7.9: Detect transition from prepare to workload and reset aggregator
                        if was_in_prepare_phase && !in_prepare {
                            debug!("Prepare phase completed, resetting aggregator for workload-only stats");
                            aggregator.reset_stats();
                            // Reset workload timer to measure actual workload duration (not including prepare)
                            workload_start = std::time::Instant::now();
                            was_in_prepare_phase = false;
                        } else if in_prepare {
                            was_in_prepare_phase = true;
                        }
                        
                        // v0.7.5: Extract final summary if completed
                        if stats.completed {
                            completed_agents.insert(stats.agent_id.clone());
                            aggregator.mark_completed(&stats.agent_id);
                            
                            // Extract and store final summary for persistence
                            if let Some(summary) = stats.final_summary {
                                debug!("Collected final summary from agent {}", summary.agent_id);
                                agent_summaries.push(summary);
                            } else {
                                warn!("Agent {} completed but did not provide final summary", stats.agent_id);
                            }
                            
                            // v0.7.9: Extract and store prepare summary for persistence
                            if let Some(prep_summary) = stats.prepare_summary {
                                debug!("Collected prepare summary from agent {}", prep_summary.agent_id);
                                prepare_summaries.push(prep_summary);
                            }
                        } else {
                            // Regular update (not completed yet)
                            aggregator.update(stats);
                        }
                        
                        // Update display every 100ms (rate limiting)
                        if last_update.elapsed() > std::time::Duration::from_millis(100) {
                            let agg = aggregator.aggregate();
                            
                            // v0.7.9: Use captured prepare phase info
                            let msg = if in_prepare && prepare_total > 0 {
                                // Show prepare progress as "created/total (percentage)"
                                let pct = (prepare_created as f64 / prepare_total as f64 * 100.0) as u32;
                                if dead_agents.is_empty() {
                                    format!("📦 Preparing: {}/{} objects ({}%)\n{}", 
                                            prepare_created, prepare_total, pct, agg.format_progress())
                                } else {
                                    format!("📦 Preparing: {}/{} objects ({}
%) (⚠️ {} dead)\n{}", 
                                            prepare_created, prepare_total, pct, dead_agents.len(), agg.format_progress())
                                }
                            } else if dead_agents.is_empty() {
                                agg.format_progress()
                            } else {
                                format!("{} (⚠️ {} dead)", agg.format_progress(), dead_agents.len())
                            };
                            progress_bar.set_message(msg.clone());
                            
                            // v0.7.9: Update progress bar based on phase
                            if in_prepare && prepare_total > 0 {
                                // During prepare: show objects created out of total
                                progress_bar.set_length(prepare_total);
                                progress_bar.set_position(prepare_created);
                                // Update style to show 'objects' unit
                                progress_bar.set_style(
                                    ProgressStyle::default_bar()
                                        .template("{bar:40.cyan/blue} {pos:>7}/{len:7} objects\n{msg}")
                                        .expect("Invalid progress template")
                                        .progress_chars("█▓▒░ ")
                                );
                            } else {
                                // During workload: show elapsed time out of duration
                                progress_bar.set_length(duration_secs);
                                let elapsed_secs = workload_start.elapsed().as_secs().min(duration_secs);
                                progress_bar.set_position(elapsed_secs);
                                // Update style to show 's' (seconds) unit
                                progress_bar.set_style(
                                    ProgressStyle::default_bar()
                                        .template("{bar:40.cyan/blue} {pos:>7}/{len:7}s\n{msg}")
                                        .expect("Invalid progress template")
                                        .progress_chars("█▓▒░ ")
                                );
                            }
                            
                            last_update = std::time::Instant::now();
                            
                            // v0.7.5: Write live stats to console.log every 1 second
                            if last_console_log.elapsed() >= std::time::Duration::from_secs(1) {
                                let timestamp = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                let log_line = format!("[{}] {}", timestamp, msg);
                                if let Err(e) = results_dir.write_console(&log_line) {
                                    warn!("Failed to write live stats to console.log: {}", e);
                                }
                                last_console_log = std::time::Instant::now();
                            }
                        }
                        
                        // v0.7.5: Check if all agents completed or dead (graceful degradation)
                        if aggregator.all_completed() {
                            break;
                        }
                    }
                    None => {
                        // Channel closed - all agent streams finished
                        warn!("Stats channel closed unexpectedly - all agent streams ended");
                        break;
                    }
                }
            }
            
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                // v0.7.5: Periodic timeout check (every 1 second)
                // Check logic is at top of loop
            }
            
            _ = &mut ctrl_c => {
                warn!("Ctrl+C received, interrupting workload");
                progress_bar.finish_with_message("Interrupted by user");
                
                // Cleanup deployments if any
                if !deployments.is_empty() {
                    warn!("Cleaning up agents...");
                    let _ = sai3_bench::ssh_deploy::cleanup_agents(deployments);
                }
                
                anyhow::bail!("Interrupted by Ctrl+C");
            }
        }
    }
    
    // Final aggregation
    let final_stats = aggregator.aggregate();
    progress_bar.finish_with_message(format!("✓ All {} agents completed", final_stats.num_agents));
    
    // v0.7.6: Add blank lines to preserve the last stats display before printing results
    println!("\n\n");  // Preserve GET/PUT stats lines from being overwritten
    
    // v0.7.5: Print live aggregate stats for immediate visibility
    println!("=== Live Aggregate Stats (from streaming) ===");
    println!("Total operations: {} GET, {} PUT, {} META", 
             final_stats.total_get_ops, final_stats.total_put_ops, final_stats.total_meta_ops);
    println!("GET: {:.0} ops/s, {} (mean: {:.0}µs, p50: {:.0}µs, p95: {:.0}µs)",
             final_stats.total_get_ops as f64 / final_stats.elapsed_s,
             format_bandwidth(final_stats.total_get_bytes, final_stats.elapsed_s),
             final_stats.get_mean_us,
             final_stats.get_p50_us,
             final_stats.get_p95_us);
    println!("PUT: {:.0} ops/s, {} (mean: {:.0}µs, p50: {:.0}µs, p95: {:.0}µs)",
             final_stats.total_put_ops as f64 / final_stats.elapsed_s,
             format_bandwidth(final_stats.total_put_bytes, final_stats.elapsed_s),
             final_stats.put_mean_us,
             final_stats.put_p50_us,
             final_stats.put_p95_us);
    println!("Elapsed: {:.2}s", final_stats.elapsed_s);
    println!();
    
    // v0.7.5: Write per-agent results and create consolidated TSV with histogram aggregation
    if !agent_summaries.is_empty() {
        info!("Writing results for {} agents", agent_summaries.len());
        
        // Write per-agent results to agents/{agent-id}/ subdirectory
        for summary in &agent_summaries {
            if let Err(e) = write_agent_results(&agents_dir, &summary.agent_id, summary) {
                error!("Failed to write results for agent {}: {}", summary.agent_id, e);
            }
        }
        
        // Create consolidated TSV with bucket-level histogram aggregation
        if let Err(e) = create_consolidated_tsv(&agents_dir, &results_dir, &agent_summaries) {
            error!("Failed to create consolidated TSV: {}", e);
        } else {
            info!("✓ Consolidated results.tsv created with accurate histogram aggregation");
        }
        
        // Print detailed per-agent summary
        if let Err(e) = print_distributed_results(&agent_summaries, &mut results_dir) {
            error!("Failed to print distributed results: {}", e);
        }
    } else {
        warn!("No agent summaries collected - per-agent results and consolidated TSV not available");
        warn!("This may indicate agents failed to return final_summary in completed LiveStats messages");
    }
    
    // v0.7.9: Write per-agent prepare results and create consolidated prepare TSV
    if !prepare_summaries.is_empty() {
        info!("Writing prepare results for {} agents", prepare_summaries.len());
        
        // Write per-agent prepare results to agents/{agent-id}/ subdirectory
        for prep_summary in &prepare_summaries {
            if let Err(e) = write_agent_prepare_results(&agents_dir, &prep_summary.agent_id, prep_summary) {
                error!("Failed to write prepare results for agent {}: {}", prep_summary.agent_id, e);
            }
        }
        
        // Create consolidated prepare TSV with histogram aggregation
        if let Err(e) = create_consolidated_prepare_tsv(results_dir.path(), &prepare_summaries) {
            error!("Failed to create consolidated prepare TSV: {}", e);
        } else {
            info!("✓ Consolidated prepare_results.tsv created");
        }
    }
    
    // Finalize results directory with elapsed time from final stats
    results_dir.finalize(final_stats.elapsed_s)?;
    let msg = format!("\nResults saved to: {}", results_dir.path().display());
    println!("{}", msg);

    // Cleanup SSH-deployed agents (v0.6.11+)
    if !deployments.is_empty() {
        info!("Cleaning up {} deployed agents", deployments.len());
        
        let cleanup_msg = format!("Stopping {} agent containers...", deployments.len());
        println!("{}", cleanup_msg);
        
        use sai3_bench::ssh_deploy;
        match ssh_deploy::cleanup_agents(deployments) {
            Ok(_) => {
                let success_msg = "✓ All agent containers stopped and cleaned up";
                println!("{}", success_msg);
                info!("{}", success_msg);
            }
            Err(e) => {
                let err_msg = format!("⚠ Warning: Agent cleanup had errors: {}", e);
                eprintln!("{}", err_msg);
                warn!("{}", err_msg);
                // Don't fail the whole operation if cleanup has issues
            }
        }
    }

    Ok(())
}

/// Print aggregated results from all agents
fn print_distributed_results(summaries: &[WorkloadSummary], results_dir: &mut ResultsDir) -> anyhow::Result<()> {
    let msg = "\n=== Distributed Results ===".to_string();
    println!("{}", msg);
    results_dir.write_console(&msg)?;
    
    let msg = format!("Total agents: {}", summaries.len());
    println!("{}", msg);
    results_dir.write_console(&msg)?;

    // Per-agent results
    for summary in summaries {
        let msg = format!("\n--- Agent: {} ---", summary.agent_id);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("  Wall time: {:.2}s", summary.wall_seconds);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("  Total ops: {} ({:.2} ops/s)", 
                 summary.total_ops, 
                 summary.total_ops as f64 / summary.wall_seconds);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("  Total bytes: {:.2} MB ({:.2} MiB/s)", 
                 summary.total_bytes as f64 / 1_048_576.0,
                 (summary.total_bytes as f64 / 1_048_576.0) / summary.wall_seconds);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        if let Some(ref get) = summary.get {
            if get.ops > 0 {
                let msg = format!("  GET: {} ops, {:.2} MB, mean: {}µs, p95: {}µs", 
                         get.ops,
                         get.bytes as f64 / 1_048_576.0,
                         get.mean_us,
                         get.p95_us);
                println!("{}", msg);
                results_dir.write_console(&msg)?;
            }
        }
        
        if let Some(ref put) = summary.put {
            if put.ops > 0 {
                let msg = format!("  PUT: {} ops, {:.2} MB, mean: {}µs, p95: {}µs", 
                         put.ops,
                         put.bytes as f64 / 1_048_576.0,
                         put.mean_us,
                         put.p95_us);
                println!("{}", msg);
                results_dir.write_console(&msg)?;
            }
        }
        
        if let Some(ref meta) = summary.meta {
            if meta.ops > 0 {
                let msg = format!("  META: {} ops, {:.2} MB, mean: {}µs, p95: {}µs", 
                         meta.ops,
                         meta.bytes as f64 / 1_048_576.0,
                         meta.mean_us,
                         meta.p95_us);
                println!("{}", msg);
                results_dir.write_console(&msg)?;
            }
        }
    }

    // Aggregated totals
    let total_ops: u64 = summaries.iter().map(|s| s.total_ops).sum();
    let total_bytes: u64 = summaries.iter().map(|s| s.total_bytes).sum();
    let max_wall = summaries
        .iter()
        .map(|s| s.wall_seconds)
        .fold(0.0f64, f64::max);

    let get_ops: u64 = summaries.iter().filter_map(|s| s.get.as_ref().map(|g| g.ops)).sum();
    let get_bytes: u64 = summaries.iter().filter_map(|s| s.get.as_ref().map(|g| g.bytes)).sum();
    
    let put_ops: u64 = summaries.iter().filter_map(|s| s.put.as_ref().map(|p| p.ops)).sum();
    let put_bytes: u64 = summaries.iter().filter_map(|s| s.put.as_ref().map(|p| p.bytes)).sum();

    let meta_ops: u64 = summaries.iter().filter_map(|s| s.meta.as_ref().map(|m| m.ops)).sum();

    let msg = "\n=== Aggregate Totals ===".to_string();
    println!("{}", msg);
    results_dir.write_console(&msg)?;
    
    let msg = format!("Total ops: {} ({:.2} ops/s)", 
             total_ops, 
             total_ops as f64 / max_wall);
    println!("{}", msg);
    results_dir.write_console(&msg)?;
    
    let msg = format!("Total bytes: {:.2} MB ({:.2} MiB/s)", 
             total_bytes as f64 / 1_048_576.0,
             (total_bytes as f64 / 1_048_576.0) / max_wall);
    println!("{}", msg);
    results_dir.write_console(&msg)?;
    
    if get_ops > 0 {
        let msg = "\nGET aggregate:".to_string();
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("  Total ops: {} ({:.2} ops/s)", get_ops, get_ops as f64 / max_wall);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("  Total bytes: {:.2} MB ({:.2} MiB/s)", 
                 get_bytes as f64 / 1_048_576.0,
                 (get_bytes as f64 / 1_048_576.0) / max_wall);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
    }
    
    if put_ops > 0 {
        let msg = "\nPUT aggregate:".to_string();
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("  Total ops: {} ({:.2} ops/s)", put_ops, put_ops as f64 / max_wall);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("  Total bytes: {:.2} MB ({:.2} MiB/s)", 
                 put_bytes as f64 / 1_048_576.0,
                 (put_bytes as f64 / 1_048_576.0) / max_wall);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
    }
    
    if meta_ops > 0 {
        let msg = "\nMETA aggregate:".to_string();
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("  Total ops: {} ({:.2} ops/s)", meta_ops, meta_ops as f64 / max_wall);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
    }

    let msg = "\n✅ Distributed workload complete!".to_string();
    println!("{}", msg);
    results_dir.write_console(&msg)?;
    
    Ok(())
}

/// Write agent results to agents/{agent-id}/ subdirectory (v0.6.4)
fn write_agent_results(
    agents_dir: &std::path::Path,
    agent_id: &str,
    summary: &WorkloadSummary,
) -> anyhow::Result<()> {
    use std::fs;
    
    // Create subdirectory for this agent
    let agent_dir = agents_dir.join(agent_id);
    fs::create_dir_all(&agent_dir)
        .with_context(|| format!("Failed to create agent directory: {}", agent_dir.display()))?;
    
    // Write metadata.json
    if !summary.metadata_json.is_empty() {
        let metadata_path = agent_dir.join("metadata.json");
        fs::write(&metadata_path, &summary.metadata_json)
            .with_context(|| format!("Failed to write agent metadata: {}", metadata_path.display()))?;
    }
    
    // Write results.tsv
    if !summary.tsv_content.is_empty() {
        let tsv_path = agent_dir.join("results.tsv");
        fs::write(&tsv_path, &summary.tsv_content)
            .with_context(|| format!("Failed to write agent TSV: {}", tsv_path.display()))?;
    }
    
    // Write console.log (if provided - currently agents don't generate this)
    if !summary.console_log.is_empty() {
        let console_path = agent_dir.join("console.log");
        fs::write(&console_path, &summary.console_log)
            .with_context(|| format!("Failed to write agent console log: {}", console_path.display()))?;
    }
    
    // Write a note about the agent's local results path
    if !summary.results_path.is_empty() {
        let note_path = agent_dir.join("agent_local_path.txt");
        fs::write(&note_path, &summary.results_path)
            .with_context(|| format!("Failed to write agent path note: {}", note_path.display()))?;
    }
    
    // Write a note about operation log (if it exists on agent)
    if !summary.op_log_path.is_empty() {
        let oplog_note_path = agent_dir.join("agent_op_log_path.txt");
        let note = format!(
            "Operation log on agent (not transferred):\n{}\n\n\
             Note: Operation logs (.tsv.zst files) remain on the agent's local filesystem.\n\
             They are not transferred via gRPC due to their size.",
            summary.op_log_path
        );
        fs::write(&oplog_note_path, note)
            .with_context(|| format!("Failed to write op-log path note: {}", oplog_note_path.display()))?;
    }
    
    info!("Wrote agent {} results to: {}", agent_id, agent_dir.display());
    Ok(())
}

/// Write per-agent prepare results to agents/{agent-id}/ subdirectory (v0.7.9)
fn write_agent_prepare_results(
    agents_dir: &std::path::Path,
    agent_id: &str,
    summary: &PrepareSummary,
) -> anyhow::Result<()> {
    use std::fs;
    
    // Create subdirectory for this agent
    let agent_dir = agents_dir.join(agent_id);
    fs::create_dir_all(&agent_dir)
        .with_context(|| format!("Failed to create agent directory: {}", agent_dir.display()))?;
    
    // Write prepare_results.tsv
    if !summary.tsv_content.is_empty() {
        let tsv_path = agent_dir.join("prepare_results.tsv");
        fs::write(&tsv_path, &summary.tsv_content)
            .with_context(|| format!("Failed to write agent prepare TSV: {}", tsv_path.display()))?;
    }
    
    // Write a note about the agent's local results path
    if !summary.results_path.is_empty() {
        let note_path = agent_dir.join("prepare_local_path.txt");
        fs::write(&note_path, &summary.results_path)
            .with_context(|| format!("Failed to write prepare path note: {}", note_path.display()))?;
    }
    
    Ok(())
}

/// Create consolidated prepare_results.tsv from agent histograms (v0.7.9)
/// Mirrors create_consolidated_tsv logic but for prepare phase
fn create_consolidated_prepare_tsv(
    results_dir: &std::path::Path,
    summaries: &[PrepareSummary],
) -> anyhow::Result<()> {
    use hdrhistogram::Histogram;
    use hdrhistogram::serialization::Deserializer;
    use std::fs::File;
    use std::io::{BufWriter, Write};
    
    info!("Creating consolidated prepare_results.tsv from {} agent histograms", summaries.len());
    
    // Define histogram parameters (must match agent configuration)
    const MIN: u64 = 1;
    const MAX: u64 = 3_600_000_000;  // 1 hour in microseconds
    const SIGFIG: u8 = 3;
    const NUM_BUCKETS: usize = 9;
    
    // Create accumulators for PUT operations (prepare phase only does PUTs)
    let mut put_accumulators = Vec::new();
    for _ in 0..NUM_BUCKETS {
        put_accumulators.push(Histogram::<u64>::new_with_bounds(MIN, MAX, SIGFIG)?);
    }
    
    // Deserialize and merge all PUT histograms from agents
    let mut deserializer = Deserializer::new();
    
    for (agent_idx, summary) in summaries.iter().enumerate() {
        info!("Merging prepare histograms from agent {} ({})", agent_idx + 1, summary.agent_id);
        
        if summary.histogram_put.is_empty() {
            continue;
        }
        
        // Deserialize PUT histograms (one per bucket)
        let mut cursor = &summary.histogram_put[..];
        for bucket_idx in 0..NUM_BUCKETS {
            match deserializer.deserialize::<u64, _>(&mut cursor) {
                Ok(hist) => {
                    put_accumulators[bucket_idx].add(&hist)
                        .context("Failed to merge PUT histograms")?;
                }
                Err(e) => {
                    warn!("Failed to deserialize PUT histogram bucket {} from agent {}: {}", 
                          bucket_idx, summary.agent_id, e);
                    break;  // Stop if deserialization fails
                }
            }
        }
    }
    
    // Calculate aggregate metrics from summaries
    let mut total_ops: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_objects_created: u64 = 0;
    let mut total_objects_existed: u64 = 0;
    let total_wall_seconds = summaries.iter().map(|s| s.wall_seconds).fold(0.0f64, f64::max);
    
    for summary in summaries {
        if let Some(ref put) = summary.put {
            total_ops += put.ops;
            total_bytes += put.bytes;
        }
        total_objects_created += summary.objects_created;
        total_objects_existed += summary.objects_existed;
    }
    
    // Write consolidated TSV
    let tsv_path = results_dir.join("prepare_results.tsv");
    let mut writer = BufWriter::new(File::create(&tsv_path)?);
    
    // Write header
    writeln!(writer, "operation\tsize_bucket\tbucket_idx\tmean_us\tp50_us\tp90_us\tp95_us\tp99_us\tmax_us\tavg_bytes\tops_per_sec\tthroughput_mibps\tcount")?;
    
    // Write PUT bucket rows (from merged histograms)
    for (idx, hist) in put_accumulators.iter().enumerate() {
        if hist.len() == 0 {
            continue;
        }
        
        let mean_us = hist.mean();
        let p50_us = hist.value_at_quantile(0.50);
        let p90_us = hist.value_at_quantile(0.90);
        let p95_us = hist.value_at_quantile(0.95);
        let p99_us = hist.value_at_quantile(0.99);
        let max_us = hist.max();
        let count = hist.len();
        
        // Estimate avg_bytes and throughput per bucket (approximate)
        let avg_bytes = if total_ops > 0 { total_bytes / total_ops } else { 0 };
        let ops_per_sec = count as f64 / total_wall_seconds;
        let throughput_mibps = (count as f64 * avg_bytes as f64) / total_wall_seconds / 1024.0 / 1024.0;
        
        let bucket_label = sai3_bench::metrics::BUCKET_LABELS[idx];
        
        writeln!(
            writer,
            "PUT\t{}\t{}\t{:.2}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.2}\t{:.2}\t{}",
            bucket_label, idx, mean_us, p50_us, p90_us, p95_us, p99_us, max_us,
            avg_bytes, ops_per_sec, throughput_mibps, count
        )?;
    }
    
    // Write ALL summary row (combines all buckets)
    if total_ops > 0 {
        // Merge all bucket histograms into one for overall stats
        let mut all_hist = hdrhistogram::Histogram::<u64>::new(3)?;
        for bucket_hist in put_accumulators.iter() {
            if bucket_hist.len() > 0 {
                all_hist.add(bucket_hist)?;
            }
        }
        
        if all_hist.len() > 0 {
            let mean_us = all_hist.mean();
            let p50_us = all_hist.value_at_quantile(0.50);
            let p90_us = all_hist.value_at_quantile(0.90);
            let p95_us = all_hist.value_at_quantile(0.95);
            let p99_us = all_hist.value_at_quantile(0.99);
            let max_us = all_hist.max();
            let count = all_hist.len();
            
            let avg_bytes = total_bytes / total_ops;
            let ops_per_sec = count as f64 / total_wall_seconds;
            let throughput_mibps = (count as f64 * avg_bytes as f64) / total_wall_seconds / 1024.0 / 1024.0;
            
            writeln!(
                writer,
                "PUT\tALL\t{}\t{:.2}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.2}\t{:.2}\t{}",
                99, mean_us, p50_us, p90_us, p95_us, p99_us, max_us,
                avg_bytes, ops_per_sec, throughput_mibps, count
            )?;
        }
    }
    
    writer.flush()?;
    info!("Wrote consolidated prepare_results.tsv to {}", tsv_path.display());
    info!("Prepare phase summary: {} objects created, {} existed, {:.2}s elapsed",
          total_objects_created, total_objects_existed, total_wall_seconds);
    
    Ok(())
}

/// Create consolidated results.tsv from agent histograms (v0.6.4)
/// Uses HDR histogram merging for mathematically accurate percentile aggregation
fn create_consolidated_tsv(
    _agents_dir: &std::path::Path,
    results_dir: &ResultsDir,
    summaries: &[WorkloadSummary],
) -> anyhow::Result<()> {
    use hdrhistogram::Histogram;
    use hdrhistogram::serialization::Deserializer;
    use std::fs::File;
    use std::io::Write;
    
    info!("Creating consolidated results.tsv from {} agent histograms", summaries.len());
    
    // Define histogram parameters (must match agent configuration in metrics.rs)
    // Range: 1µs to 1 hour (3.6e9 µs), 3 significant figures
    const MIN: u64 = 1;
    const MAX: u64 = 3_600_000_000;  // 1 hour in microseconds
    const SIGFIG: u8 = 3;
    const NUM_BUCKETS: usize = 9;
    
    // Create accumulators for each operation type and size bucket
    let mut get_accumulators = Vec::new();
    let mut put_accumulators = Vec::new();
    let mut meta_accumulators = Vec::new();
    
    for _ in 0..NUM_BUCKETS {
        get_accumulators.push(Histogram::<u64>::new_with_bounds(MIN, MAX, SIGFIG)?);
        put_accumulators.push(Histogram::<u64>::new_with_bounds(MIN, MAX, SIGFIG)?);
        meta_accumulators.push(Histogram::<u64>::new_with_bounds(MIN, MAX, SIGFIG)?);
    }
    
    // Deserialize and merge histograms from all agents
    let mut deserializer = Deserializer::new();
    
    for (agent_idx, summary) in summaries.iter().enumerate() {
        info!("Merging histograms from agent {} ({})", agent_idx + 1, summary.agent_id);
        
        // Deserialize GET histograms (one per bucket)
        let mut cursor = &summary.histogram_get[..];
        for bucket_idx in 0..NUM_BUCKETS {
            let hist: Histogram<u64> = deserializer.deserialize(&mut cursor)
                .with_context(|| format!("Failed to deserialize GET histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
            get_accumulators[bucket_idx].add(hist)
                .with_context(|| format!("Failed to merge GET histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
        }
        
        // Deserialize PUT histograms
        let mut cursor = &summary.histogram_put[..];
        for bucket_idx in 0..NUM_BUCKETS {
            let hist: Histogram<u64> = deserializer.deserialize(&mut cursor)
                .with_context(|| format!("Failed to deserialize PUT histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
            put_accumulators[bucket_idx].add(hist)
                .with_context(|| format!("Failed to merge PUT histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
        }
        
        // Deserialize META histograms
        let mut cursor = &summary.histogram_meta[..];
        for bucket_idx in 0..NUM_BUCKETS {
            let hist: Histogram<u64> = deserializer.deserialize(&mut cursor)
                .with_context(|| format!("Failed to deserialize META histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
            meta_accumulators[bucket_idx].add(hist)
                .with_context(|| format!("Failed to merge META histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
        }
    }
    
    // Calculate total wall time (max of all agents)
    let wall_seconds = summaries.iter()
        .map(|s| s.wall_seconds)
        .fold(0.0f64, f64::max);
    
    // Calculate aggregate operation counts and bytes
    let total_get_ops: u64 = summaries.iter()
        .filter_map(|s| s.get.as_ref())
        .map(|g| g.ops)
        .sum();
    let total_get_bytes: u64 = summaries.iter()
        .filter_map(|s| s.get.as_ref())
        .map(|g| g.bytes)
        .sum();
    
    let total_put_ops: u64 = summaries.iter()
        .filter_map(|s| s.put.as_ref())
        .map(|p| p.ops)
        .sum();
    let total_put_bytes: u64 = summaries.iter()
        .filter_map(|s| s.put.as_ref())
        .map(|p| p.bytes)
        .sum();
    
    let total_meta_ops: u64 = summaries.iter()
        .filter_map(|s| s.meta.as_ref())
        .map(|m| m.ops)
        .sum();
    let total_meta_bytes: u64 = summaries.iter()
        .filter_map(|s| s.meta.as_ref())
        .map(|m| m.bytes)
        .sum();
    
    // Write consolidated TSV
    let tsv_path = results_dir.path().join("results.tsv");
    let mut f = File::create(&tsv_path)
        .with_context(|| format!("Failed to create consolidated TSV: {}", tsv_path.display()))?;
    
    // Write header
    writeln!(f, "operation\tsize_bucket\tbucket_idx\tmean_us\tp50_us\tp90_us\tp95_us\tp99_us\tmax_us\tavg_bytes\tops_per_sec\tthroughput_mibps\tcount")?;
    
    // Collect all rows for sorting
    let mut rows = Vec::new();
    
    // Write per-bucket rows (merged across all agents)
    collect_op_rows(&mut rows, "GET", &get_accumulators, total_get_ops, total_get_bytes, wall_seconds)?;
    collect_op_rows(&mut rows, "PUT", &put_accumulators, total_put_ops, total_put_bytes, wall_seconds)?;
    collect_op_rows(&mut rows, "META", &meta_accumulators, total_meta_ops, total_meta_bytes, wall_seconds)?;
    
    // Write aggregate rows (overall totals across all agents and size buckets)
    // META=97, GET=98, PUT=99 for natural sorting
    collect_aggregate_row(&mut rows, "META", 97, &meta_accumulators, total_meta_ops, total_meta_bytes, wall_seconds)?;
    collect_aggregate_row(&mut rows, "GET", 98, &get_accumulators, total_get_ops, total_get_bytes, wall_seconds)?;
    collect_aggregate_row(&mut rows, "PUT", 99, &put_accumulators, total_put_ops, total_put_bytes, wall_seconds)?;
    
    // Sort by bucket_idx
    rows.sort_by_key(|(bucket_idx, _)| *bucket_idx);
    
    // Write sorted rows
    for (_, row) in rows {
        writeln!(f, "{}", row)?;
    }
    
    info!("Consolidated results.tsv written to: {}", tsv_path.display());
    Ok(())
}

/// Helper to collect rows for one operation type
fn collect_op_rows(
    rows: &mut Vec<(usize, String)>,
    op_name: &str,
    accumulators: &[hdrhistogram::Histogram<u64>],
    total_ops: u64,
    total_bytes: u64,
    wall_seconds: f64,
) -> anyhow::Result<()> {
    for (bucket_idx, hist) in accumulators.iter().enumerate() {
        let count = hist.len();
        if count == 0 {
            continue;
        }
        
        // Calculate percentiles (histograms store microseconds directly)
        let mean_us = hist.mean();
        let p50_us = hist.value_at_quantile(0.50) as f64;
        let p90_us = hist.value_at_quantile(0.90) as f64;
        let p95_us = hist.value_at_quantile(0.95) as f64;
        let p99_us = hist.value_at_quantile(0.99) as f64;
        let max_us = hist.max() as f64;
        
        // Calculate average bytes (approximate - we don't track per-bucket bytes in distributed mode)
        let avg_bytes = if total_ops > 0 {
            total_bytes as f64 / total_ops as f64
        } else {
            0.0
        };
        
        // Calculate throughput
        let ops_per_sec = count as f64 / wall_seconds;
        let bucket_bytes = count as f64 * avg_bytes;  // Approximation
        let throughput_mibps = (bucket_bytes / 1_048_576.0) / wall_seconds;
        
        let row = format!(
            "{}\t{}\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.0}\t{:.2}\t{:.2}\t{}",
            op_name,
            BUCKET_LABELS[bucket_idx],
            bucket_idx,
            mean_us, p50_us, p90_us, p95_us, p99_us, max_us,
            avg_bytes,
            ops_per_sec,
            throughput_mibps,
            count
        );
        
        rows.push((bucket_idx, row));
    }
    
    Ok(())
}

/// Helper to collect an aggregate row combining all size buckets for one operation type
fn collect_aggregate_row(
    rows: &mut Vec<(usize, String)>,
    op_name: &str,
    bucket_idx: usize,
    accumulators: &[hdrhistogram::Histogram<u64>],
    total_ops: u64,
    total_bytes: u64,
    wall_seconds: f64,
) -> anyhow::Result<()> {
    // Combine all size bucket histograms
    const MIN: u64 = 1;
    const MAX: u64 = 3_600_000_000;
    const SIGFIG: u8 = 3;
    
    let mut combined = hdrhistogram::Histogram::<u64>::new_with_bounds(MIN, MAX, SIGFIG)?;
    
    for hist in accumulators {
        if hist.len() > 0 {
            combined.add(hist)?;
        }
    }
    
    let count = combined.len();
    if count == 0 {
        return Ok(());
    }
    
    // Calculate percentiles from combined histogram
    let mean_us = combined.mean();
    let p50_us = combined.value_at_quantile(0.50) as f64;
    let p90_us = combined.value_at_quantile(0.90) as f64;
    let p95_us = combined.value_at_quantile(0.95) as f64;
    let p99_us = combined.value_at_quantile(0.99) as f64;
    let max_us = combined.max() as f64;
    
    // Calculate totals
    let avg_bytes = if total_ops > 0 {
        total_bytes as f64 / total_ops as f64
    } else {
        0.0
    };
    
    let ops_per_sec = count as f64 / wall_seconds;
    let throughput_mibps = (total_bytes as f64 / 1_048_576.0) / wall_seconds;
    
    let row = format!(
        "{}\tALL\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.0}\t{:.2}\t{:.2}\t{}",
        op_name,
        bucket_idx,
        mean_us, p50_us, p90_us, p95_us, p99_us, max_us,
        avg_bytes,
        ops_per_sec,
        throughput_mibps,
        count
    );
    
    rows.push((bucket_idx, row));
    Ok(())
}

/// Detect if storage is shared based on URI scheme
/// Shared storage: s3://, az://, gs://, and potentially file:// (if NFS-mounted)
/// Local storage: file://, direct://
fn detect_shared_storage(config: &sai3_bench::config::Config) -> bool {
    // Check target URI if present
    if let Some(ref target) = config.target {
        return is_shared_uri(target);
    }
    
    // Check prepare config URIs
    if let Some(ref prepare) = config.prepare {
        for ensure_spec in &prepare.ensure_objects {
            if is_shared_uri(&ensure_spec.base_uri) {
                return true;
            }
        }
    }
    
    // Default to local if no clear indication
    false
}

/// Check if a URI represents shared storage
fn is_shared_uri(uri: &str) -> bool {
    uri.starts_with("s3://") 
        || uri.starts_with("az://") 
        || uri.starts_with("gs://")
        // Note: file:// could be shared (NFS) or local - we assume local by default
        // Users can override with --shared-prepare if using NFS
}

