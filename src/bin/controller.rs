// src/bin/controller.rs

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tracing::{debug, error, info};

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
            shared_prepare,
        } => {
            run_distributed_workload(
                &agents,
                config,
                path_template,
                agent_ids.as_deref(),
                *start_delay,
                *shared_prepare,
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
    shared_prepare: Option<bool>,
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
    
    // Read YAML configuration
    let config_yaml = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

    debug!("Config YAML loaded, {} bytes", config_yaml.len());

    // Auto-detect if storage is shared based on URI scheme
    let is_shared_storage = if let Some(explicit) = shared_prepare {
        debug!("Using explicit shared_prepare setting: {}", explicit);
        explicit
    } else {
        // Parse config to detect storage type
        let config: sai3_bench::config::Config = serde_yaml::from_str(&config_yaml)
            .context("Failed to parse config for storage detection")?;
        
        let is_shared = detect_shared_storage(&config);
        debug!("Auto-detected shared storage: {} (based on URI scheme)", is_shared);
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
    let start_time = SystemTime::now() + Duration::from_secs(start_delay_secs);
    let start_ns = start_time
        .duration_since(UNIX_EPOCH)?
        .as_nanos() as i64;

    debug!("Coordinated start time: {} ns since epoch", start_ns);

    // Send workload to all agents in parallel
    let msg = format!("Sending workload to {} agents...", agent_addrs.len());
    println!("{}", msg);
    results_dir.write_console(&msg)?;
    
    let mut handles = Vec::new();
    for (idx, agent_addr) in agent_addrs.iter().enumerate() {
        let agent_id = ids[idx].clone();
        let path_prefix = path_template.replace("{id}", &(idx + 1).to_string());
        
        debug!("Agent {}: ID='{}', prefix='{}', address='{}', shared_storage={}", 
               idx + 1, agent_id, path_prefix, agent_addr, is_shared_storage);
        
        let config = config_yaml.clone();
        let addr = agent_addr.clone();
        let insecure = insecure;
        let ca = agent_ca.cloned();
        let domain = agent_domain.to_string();
        let shared = is_shared_storage;

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
                    shared_storage: shared,
                })
                .await
                .with_context(|| format!("run_workload on agent {}", addr))?;

            debug!("Agent {} completed workload successfully", addr);
            Ok::<WorkloadSummary, anyhow::Error>(response.into_inner())
        });

        handles.push(handle);
    }

    info!("Waiting for {} agents to complete workload", handles.len());
    
    // Wait for all agents to complete
    let mut summaries = Vec::new();
    for (idx, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok(summary)) => {
                info!("Agent {} ({}) completed successfully", ids[idx], agent_addrs[idx]);
                let msg = format!("✓ Agent {} completed", ids[idx]);
                println!("{}", msg);
                results_dir.write_console(&msg)?;
                
                // Write agent results to agents/{id}/ subdirectory (v0.6.4)
                write_agent_results(&agents_dir, &ids[idx], &summary)?;
                
                summaries.push(summary);
            }
            Ok(Err(e)) => {
                error!("Agent {} ({}) failed: {:?}", ids[idx], agent_addrs[idx], e);
                let msg = format!("✗ Agent {} failed: {}", ids[idx], e);
                eprintln!("{}", msg);
                results_dir.write_console(&msg)?;
                anyhow::bail!("Agent {} failed: {}", ids[idx], e);
            }
            Err(e) => {
                error!("Agent {} ({}) join error: {:?}", ids[idx], agent_addrs[idx], e);
                let msg = format!("✗ Agent {} join error: {}", ids[idx], e);
                eprintln!("{}", msg);
                results_dir.write_console(&msg)?;
                anyhow::bail!("Agent {} join error: {}", ids[idx], e);
            }
        }
    }

    let msg = format!("All {} agents completed successfully", summaries.len());
    info!("{}", msg);
    results_dir.write_console(&msg)?;

    // Display results
    print_distributed_results(&summaries, &mut results_dir)?;
    
    // Generate consolidated results.tsv from individual agent histograms (v0.6.4)
    create_consolidated_tsv(&agents_dir, &results_dir, &summaries)?;

    // Finalize results directory with max wall time
    let max_wall = summaries
        .iter()
        .map(|s| s.wall_seconds)
        .fold(0.0f64, f64::max);
    
    results_dir.finalize(max_wall)?;
    let msg = format!("\nResults saved to: {}", results_dir.path().display());
    println!("{}", msg);

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

