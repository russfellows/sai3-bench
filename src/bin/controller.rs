// src/bin/controller.rs

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tracing::{debug, error, info};

pub mod pb {
    pub mod iobench {
        include!("../pb/iobench.rs");
    }
}

use pb::iobench::agent_client::AgentClient;
use pb::iobench::{Empty, RunGetRequest, RunPutRequest, RunWorkloadRequest, WorkloadSummary};

#[derive(Parser)]
#[command(name = "sai3bench-ctl", version, about = "SAI3 Benchmark Controller (gRPC)")]
struct Cli {
    /// Increase verbosity (-v = info, -vv = debug, -vvv = trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Comma-separated agent addresses (host:port)
    #[arg(long)]
    agents: String,

    /// If set, connect without TLS (http)
    #[arg(long, default_value_t = false)]
    insecure: bool,

    /// Path to PEM file containing the agent's self-signed certificate (to trust)
    /// Only used when not --insecure
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
        Commands::Ping => {
            for a in &agents {
                let mut c = mk_client(a, cli.insecure, cli.agent_ca.as_ref(), &cli.agent_domain)
                    .await
                    .with_context(|| format!("connect to {}", a))?;
                let r = c.ping(Empty {}).await?.into_inner();
                eprintln!("connected to {} (agent version {})", a, r.version);
            }
        }
        Commands::Get { uri, jobs } => {
            for a in &agents {
                let mut c = mk_client(a, cli.insecure, cli.agent_ca.as_ref(), &cli.agent_domain)
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
                let mut c = mk_client(a, cli.insecure, cli.agent_ca.as_ref(), &cli.agent_domain)
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
            path_template,
            agent_ids,
            start_delay,
        } => {
            run_distributed_workload(
                &agents,
                config,
                path_template,
                agent_ids.as_deref(),
                *start_delay,
                cli.insecure,
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
    insecure: bool,
    agent_ca: Option<&PathBuf>,
    agent_domain: &str,
) -> Result<()> {
    info!("Starting distributed workload execution");
    debug!("Config file: {}", config_path.display());
    debug!("Path template: {}", path_template);
    debug!("Start delay: {}s", start_delay_secs);
    
    // Read YAML configuration
    let config_yaml = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

    debug!("Config YAML loaded, {} bytes", config_yaml.len());

    println!("=== Distributed Workload ===");
    println!("Config: {}", config_path.display());
    println!("Agents: {}", agent_addrs.len());
    println!("Start delay: {}s", start_delay_secs);
    println!();

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

    // Calculate coordinated start time (N seconds in the future)
    let start_time = SystemTime::now() + Duration::from_secs(start_delay_secs);
    let start_ns = start_time
        .duration_since(UNIX_EPOCH)?
        .as_nanos() as i64;

    debug!("Coordinated start time: {} ns since epoch", start_ns);

    // Send workload to all agents in parallel
    let mut handles = Vec::new();
    for (idx, agent_addr) in agent_addrs.iter().enumerate() {
        let agent_id = ids[idx].clone();
        let path_prefix = path_template.replace("{id}", &(idx + 1).to_string());
        
        debug!("Agent {}: ID='{}', prefix='{}', address='{}'", 
               idx + 1, agent_id, path_prefix, agent_addr);
        
        let config = config_yaml.clone();
        let addr = agent_addr.clone();
        let insecure = insecure;
        let ca = agent_ca.cloned();
        let domain = agent_domain.to_string();

        let handle = tokio::spawn(async move {
            debug!("Connecting to agent at {}", addr);
            
            // Connect to agent
            let mut client = mk_client(&addr, insecure, ca.as_ref(), &domain)
                .await
                .with_context(|| format!("connect to agent {}", addr))?;

            debug!("Connected to agent {}, sending workload request", addr);

            // Send workload request
            let response = client
                .run_workload(RunWorkloadRequest {
                    config_yaml: config,
                    agent_id: agent_id.clone(),
                    path_prefix: path_prefix.clone(),
                    start_timestamp_ns: start_ns,
                })
                .await
                .with_context(|| format!("run_workload on agent {}", addr))?;

            debug!("Agent {} completed workload successfully", addr);
            Ok::<WorkloadSummary, anyhow::Error>(response.into_inner())
        });

        handles.push(handle);
    }

    println!("Sending workload to {} agents...", handles.len());
    info!("Waiting for {} agents to complete workload", handles.len());
    
    // Wait for all agents to complete
    let mut summaries = Vec::new();
    for (idx, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok(summary)) => {
                info!("Agent {} ({}) completed successfully", ids[idx], agent_addrs[idx]);
                println!("✓ Agent {} completed", ids[idx]);
                summaries.push(summary);
            }
            Ok(Err(e)) => {
                error!("Agent {} ({}) failed: {:?}", ids[idx], agent_addrs[idx], e);
                eprintln!("✗ Agent {} failed: {}", ids[idx], e);
                anyhow::bail!("Agent {} failed: {}", ids[idx], e);
            }
            Err(e) => {
                error!("Agent {} ({}) join error: {:?}", ids[idx], agent_addrs[idx], e);
                eprintln!("✗ Agent {} join error: {}", ids[idx], e);
                anyhow::bail!("Agent {} join error: {}", ids[idx], e);
            }
        }
    }

    info!("All {} agents completed successfully", summaries.len());

    // Display results
    print_distributed_results(&summaries);

    Ok(())
}

/// Print aggregated results from all agents
fn print_distributed_results(summaries: &[WorkloadSummary]) {
    println!("\n=== Distributed Results ===");
    println!("Total agents: {}", summaries.len());

    // Per-agent results
    for summary in summaries {
        println!("\n--- Agent: {} ---", summary.agent_id);
        println!("  Wall time: {:.2}s", summary.wall_seconds);
        println!("  Total ops: {} ({:.2} ops/s)", 
                 summary.total_ops, 
                 summary.total_ops as f64 / summary.wall_seconds);
        println!("  Total bytes: {:.2} MB ({:.2} MiB/s)", 
                 summary.total_bytes as f64 / 1_048_576.0,
                 (summary.total_bytes as f64 / 1_048_576.0) / summary.wall_seconds);
        
        if let Some(ref get) = summary.get {
            if get.ops > 0 {
                println!("  GET: {} ops, {:.2} MB, mean: {}µs, p95: {}µs", 
                         get.ops,
                         get.bytes as f64 / 1_048_576.0,
                         get.mean_us,
                         get.p95_us);
            }
        }
        
        if let Some(ref put) = summary.put {
            if put.ops > 0 {
                println!("  PUT: {} ops, {:.2} MB, mean: {}µs, p95: {}µs", 
                         put.ops,
                         put.bytes as f64 / 1_048_576.0,
                         put.mean_us,
                         put.p95_us);
            }
        }
        
        if let Some(ref meta) = summary.meta {
            if meta.ops > 0 {
                println!("  META: {} ops, {:.2} MB, mean: {}µs, p95: {}µs", 
                         meta.ops,
                         meta.bytes as f64 / 1_048_576.0,
                         meta.mean_us,
                         meta.p95_us);
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

    println!("\n=== Aggregate Totals ===");
    println!("Total ops: {} ({:.2} ops/s)", 
             total_ops, 
             total_ops as f64 / max_wall);
    println!("Total bytes: {:.2} MB ({:.2} MiB/s)", 
             total_bytes as f64 / 1_048_576.0,
             (total_bytes as f64 / 1_048_576.0) / max_wall);
    
    if get_ops > 0 {
        println!("\nGET aggregate:");
        println!("  Total ops: {} ({:.2} ops/s)", get_ops, get_ops as f64 / max_wall);
        println!("  Total bytes: {:.2} MB ({:.2} MiB/s)", 
                 get_bytes as f64 / 1_048_576.0,
                 (get_bytes as f64 / 1_048_576.0) / max_wall);
    }
    
    if put_ops > 0 {
        println!("\nPUT aggregate:");
        println!("  Total ops: {} ({:.2} ops/s)", put_ops, put_ops as f64 / max_wall);
        println!("  Total bytes: {:.2} MB ({:.2} MiB/s)", 
                 put_bytes as f64 / 1_048_576.0,
                 (put_bytes as f64 / 1_048_576.0) / max_wall);
    }
    
    if meta_ops > 0 {
        println!("\nMETA aggregate:");
        println!("  Total ops: {} ({:.2} ops/s)", meta_ops, meta_ops as f64 / max_wall);
    }

    println!("\n✅ Distributed workload complete!");
}

