// src/bin/agent.rs

use anyhow::{Context, Result};
use clap::Parser;
use dotenvy::dotenv;
use futures::{stream::FuturesUnordered, StreamExt};
use std::env;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, Mutex, Semaphore};
use tonic::{transport::Server, Request, Response, Status};
use tracing::{debug, error, info, warn};

// Use AWS async SDK directly for listing to avoid nested runtimes
use aws_config::{self, BehaviorVersion};
use aws_sdk_s3 as s3;

// Modern ObjectStore pattern (v0.9.4+)
use s3dlio::object_store::store_for_uri;

// v0.7.11: CPU utilization monitoring
use sai3_bench::cpu_monitor::{CpuMonitor, CpuUtilization};

pub mod pb {
    pub mod iobench {
        include!("../pb/iobench.rs");
    }
}
use pb::iobench::agent_server::{Agent, AgentServer};
use pb::iobench::{Empty, LiveStats, OpSummary, PingReply, PrepareSummary, RunGetRequest, RunPutRequest, RunWorkloadRequest, WorkloadSummary, OpAggregateMetrics, ControlMessage, control_message::Command};

/// Helper function to get current timestamp with optional simulated clock skew.
/// 
/// For testing clock synchronization with local agents, set SAI3_AGENT_CLOCK_SKEW_MS
/// environment variable to simulate clock skew in milliseconds (can be negative).
/// 
/// Examples:
///   SAI3_AGENT_CLOCK_SKEW_MS=5000   # Agent clock 5 seconds ahead
///   SAI3_AGENT_CLOCK_SKEW_MS=-3000  # Agent clock 3 seconds behind
/// 
/// This allows testing the distributed clock synchronization protocol without
/// needing different physical machines with actual clock skew.
fn get_agent_timestamp_ns() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    
    // Check for test clock skew (only reads once at function definition, but that's ok for testing)
    if let Ok(skew_ms_str) = env::var("SAI3_AGENT_CLOCK_SKEW_MS") {
        if let Ok(skew_ms) = skew_ms_str.parse::<i64>() {
            let skew_ns = skew_ms * 1_000_000;
            eprintln!("[TEST MODE] Simulating clock skew: {} ms ({} ns)", skew_ms, skew_ns);
            return now + skew_ns;
        }
    }
    
    now
}

#[derive(Parser)]
#[command(name = "sai3bench-agent", version, about = "SAI3 Benchmark Agent (gRPC)")]
struct Cli {
    /// Increase verbosity (-v = info, -vv = debug, -vvv = trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Listen address, e.g. 0.0.0.0:7761
    #[arg(long, default_value = "0.0.0.0:7761")]
    listen: String,

    /// Enable TLS with an ephemeral self-signed certificate
    #[arg(long, default_value_t = false)]
    tls: bool,

    /// Subject DNS name for the self-signed cert (default "localhost")
    /// Controller must use --agent-domain to match this value for SNI.
    #[arg(long, default_value = "localhost")]
    tls_domain: String,

    /// If set, write the generated cert & key (PEM) here for the controller to trust with --agent-ca
    #[arg(long)]
    tls_write_ca: Option<std::path::PathBuf>,

    /// Optional comma-separated Subject Alternative Names (DNS names and/or IPs)
    /// Example: "localhost,myhost,10.0.0.5"
    /// NOTE: With the current minimal change we treat these as DNS names; see note below.
    #[arg(long)]
    tls_sans: Option<String>,

    /// Optional operation log path (s3dlio oplog) - applies to all workloads executed by this agent
    /// Can be overridden per-workload via config YAML op_log_path field
    /// Agent appends agent_id to filename to avoid collisions (e.g., oplog-agent1.tsv.zst)
    /// Supports environment variables: S3DLIO_OPLOG_BUF=8192 (buffer size)
    /// Note: For sorted oplogs, use 'sai3-bench sort' post-processing after capture
    #[arg(long)]
    op_log: Option<std::path::PathBuf>,
}

/// Agent state for workload management and cancellation
#[derive(Clone)]
struct AgentState {
    /// Broadcast channel for abort signals (controller calls AbortWorkload)
    abort_tx: broadcast::Sender<()>,
    /// Broadcast channel for workload completion/cleanup signals
    completion_tx: broadcast::Sender<()>,
    /// Current workload state
    state: Arc<Mutex<WorkloadState>>,
    /// Error message (if state is Failed)
    error_message: Arc<Mutex<Option<String>>>,
    /// Optional operation log path (from CLI --op-log flag)
    agent_op_log_path: Option<std::path::PathBuf>,
    /// v0.8.4: Agent ID for current workload (for execute_workload RPC)
    agent_id: Arc<Mutex<Option<String>>>,
    /// v0.8.4: Config YAML for current workload (for execute_workload RPC)
    config_yaml: Arc<Mutex<Option<String>>>,
    /// v0.8.4: LiveStatsTracker for current workload (for execute_workload RPC)
    tracker: Arc<Mutex<Option<Arc<sai3_bench::live_stats::LiveStatsTracker>>>>,
    /// v0.8.4: Operation log path for current workload (for execute_workload RPC)
    op_log_path: Arc<Mutex<Option<String>>>,
    /// v0.8.7: Agent index for distributed cleanup (0-based)
    agent_index: Arc<Mutex<Option<u32>>>,
    /// v0.8.7: Total number of agents for distributed cleanup
    num_agents: Arc<Mutex<Option<u32>>>,
}

#[derive(Debug, Clone, PartialEq)]
enum WorkloadState {
    Idle,      // Ready to accept new workload
    Ready,     // Validated, waiting for coordinated start (0-60s)
    Running,   // Workload executing (includes prepare phase)
    Failed,    // Validation or workload error (auto-resets to Idle)
    Aborting,  // Abort requested, cleaning up (5-15s timeout)
}

impl AgentState {
    fn new(agent_op_log_path: Option<std::path::PathBuf>) -> Self {
        let (abort_tx, _) = broadcast::channel(16);
        let (completion_tx, _) = broadcast::channel(16);
        Self {
            abort_tx,
            completion_tx,
            state: Arc::new(Mutex::new(WorkloadState::Idle)),
            error_message: Arc::new(Mutex::new(None)),
            agent_op_log_path,
            agent_id: Arc::new(Mutex::new(None)),
            config_yaml: Arc::new(Mutex::new(None)),
            tracker: Arc::new(Mutex::new(None)),
            op_log_path: Arc::new(Mutex::new(None)),
            agent_index: Arc::new(Mutex::new(None)),
            num_agents: Arc::new(Mutex::new(None)),
        }
    }
    
    /// Validate state transition before applying (5-state model)
    fn can_transition(from: &WorkloadState, to: &WorkloadState) -> bool {
        use WorkloadState::*;
        matches!(
            (from, to),
            // Normal flow
            (Idle, Ready)           // RPC arrives, validation passes
            | (Idle, Failed)        // RPC arrives, validation fails
            | (Ready, Running)      // Start time reached, spawn workload
            | (Ready, Idle)         // Abort during coordinated start (no cleanup needed)
            | (Running, Idle)       // Workload completed successfully
            | (Running, Failed)     // Workload error
            | (Running, Aborting)   // Abort signal during execution
            | (Aborting, Idle)      // Cleanup complete
            | (Failed, Idle)        // Auto-reset after sending error
        )
    }
    
    /// Transition to new state with validation and logging
    async fn transition_to(&self, new_state: WorkloadState, reason: &str) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if !Self::can_transition(&*state, &new_state) {
            let msg = format!("Invalid state transition: {:?} → {:?} (reason: {})", *state, new_state, reason);
            error!("{}", msg);
            return Err(msg);
        }
        info!("Agent state transition: {:?} → {:?} ({})", *state, new_state, reason);
        *state = new_state;
        Ok(())
    }
    
    async fn set_state(&self, new_state: WorkloadState) {
        let mut state = self.state.lock().await;
        info!("Agent state transition: {:?} → {:?}", *state, new_state);
        *state = new_state;
    }
    
    async fn get_state(&self) -> WorkloadState {
        self.state.lock().await.clone()
    }
    
    async fn set_error(&self, error: String) {
        let mut err_msg = self.error_message.lock().await;
        *err_msg = Some(error);
    }
    
    async fn clear_error(&self) {
        let mut err_msg = self.error_message.lock().await;
        *err_msg = None;
    }
    
    /// Send abort signal to running workload
    fn send_abort(&self) {
        let _ = self.abort_tx.send(());
        info!("Abort signal broadcast to workload");
    }
    
    /// Send completion signal for cleanup tasks (progress bars, etc)
    fn send_completion(&self) {
        let _ = self.completion_tx.send(());
    }
    
    /// Subscribe to abort signals
    fn subscribe_abort(&self) -> broadcast::Receiver<()> {
        self.abort_tx.subscribe()
    }
    
    /// Subscribe to completion signals
    fn subscribe_completion(&self) -> broadcast::Receiver<()> {
        self.completion_tx.subscribe()
    }
    
    // v0.8.4: Additional state management for execute_workload RPC
    
    async fn set_agent_id(&self, id: String) {
        let mut agent_id = self.agent_id.lock().await;
        *agent_id = Some(id);
    }
    
    async fn get_agent_id(&self) -> Option<String> {
        self.agent_id.lock().await.clone()
    }
    
    async fn set_config_yaml(&self, yaml: String) {
        let mut config_yaml = self.config_yaml.lock().await;
        *config_yaml = Some(yaml);
    }
    
    async fn get_config_yaml(&self) -> Option<String> {
        self.config_yaml.lock().await.clone()
    }
    
    async fn set_tracker(&self, t: Arc<sai3_bench::live_stats::LiveStatsTracker>) {
        let mut tracker = self.tracker.lock().await;
        *tracker = Some(t);
    }
    
    async fn get_tracker(&self) -> Option<Arc<sai3_bench::live_stats::LiveStatsTracker>> {
        self.tracker.lock().await.clone()
    }
    
    async fn set_op_log_path(&self, path: Option<String>) {
        let mut op_log_path = self.op_log_path.lock().await;
        *op_log_path = path;
    }
    
    async fn get_op_log_path(&self) -> Option<String> {
        self.op_log_path.lock().await.clone()
    }
    
    async fn set_agent_index(&self, index: u32) {
        let mut agent_index = self.agent_index.lock().await;
        *agent_index = Some(index);
    }
    
    async fn get_agent_index(&self) -> Option<u32> {
        *self.agent_index.lock().await
    }
    
    async fn set_num_agents(&self, num: u32) {
        let mut num_agents = self.num_agents.lock().await;
        *num_agents = Some(num);
    }
    
    async fn get_num_agents(&self) -> Option<u32> {
        *self.num_agents.lock().await
    }
}

struct AgentSvc {
    state: AgentState,
}

impl AgentSvc {
    fn new(state: AgentState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl Agent for AgentSvc {
    async fn ping(&self, _req: Request<Empty>) -> Result<Response<PingReply>, Status> {
        Ok(Response::new(PingReply {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }

    async fn run_get(&self, req: Request<RunGetRequest>) -> Result<Response<OpSummary>, Status> {
        let RunGetRequest { uri, jobs } = req.into_inner();
        
        // Parse URI to extract base and pattern
        // For S3: s3://bucket/prefix/pattern
        // For other backends: backend://path/pattern
        let (base_uri, pattern) = if let Some(last_slash) = uri.rfind('/') {
            let base = &uri[..=last_slash];
            let pat = &uri[last_slash + 1..];
            (base, pat)
        } else {
            return Err(Status::invalid_argument("Invalid URI format"));
        };

        // Expand keys: supports exact key, prefix, or glob '*'
        let full_uris = if pattern.contains('*') {
            // Glob pattern - list directory and filter
            let keys = list_keys_for_uri(&uri)
                .await
                .map_err(to_status)?;
            keys.into_iter()
                .filter(|k| glob_match(pattern, k))
                .map(|k| format!("{}{}", base_uri, k))
                .collect::<Vec<_>>()
        } else if pattern.is_empty() {
            // List all in directory
            let keys = list_keys_for_uri(base_uri)
                .await
                .map_err(to_status)?;
            keys.into_iter()
                .map(|k| format!("{}{}", base_uri, k))
                .collect::<Vec<_>>()
        } else {
            // Exact key
            vec![uri.clone()]
        };

        if full_uris.is_empty() {
            return Err(Status::invalid_argument("No objects match given URI"));
        }

        let started = Instant::now();
        let sem = Arc::new(Semaphore::new(jobs as usize));
        let mut futs = FuturesUnordered::new();
        
        for full_uri in full_uris {
            let sem2 = sem.clone();
            futs.push(tokio::spawn(async move {
                let _p = sem2.acquire_owned().await.unwrap();
                // Use ObjectStore pattern with full URI
                let store = store_for_uri(&full_uri).map_err(|e| anyhow::anyhow!(e))?;
                let bytes = store.get(&full_uri).await?;
                Ok::<usize, anyhow::Error>(bytes.len())
            }));
        }
        
        let mut total = 0usize;
        while let Some(join_res) = futs.next().await {
            let inner = join_res.map_err(to_status)?.map_err(to_status)?;
            total += inner;
        }
        let secs = started.elapsed().as_secs_f64();
        Ok(Response::new(OpSummary {
            total_bytes: total as u64,
            seconds: secs,
            notes: String::new(),
        }))
    }

    async fn run_put(&self, req: Request<RunPutRequest>) -> Result<Response<OpSummary>, Status> {
        let RunPutRequest {
            bucket,
            prefix,
            object_size,
            objects,
            concurrency,
        } = req.into_inner();
        
        // Build base URI from bucket (assume s3:// for backward compatibility)
        let base_uri = if bucket.starts_with("s3://") || bucket.starts_with("file://") || 
                         bucket.starts_with("az://") || bucket.starts_with("gs://") {
            bucket.clone()
        } else {
            format!("s3://{}", bucket)
        };
        
        let keys: Vec<String> = (0..objects as usize)
            .map(|i| format!("{}{}obj_{}", base_uri, prefix, i))
            .collect();
        let data = vec![0u8; object_size as usize];

        let started = Instant::now();
        let sem = Arc::new(Semaphore::new(concurrency as usize));
        let mut futs = FuturesUnordered::new();
        
        for full_uri in keys {
            let d = data.clone();
            let sem2 = sem.clone();
            futs.push(tokio::spawn(async move {
                let _p = sem2.acquire_owned().await.unwrap();
                // Use ObjectStore pattern with full URI
                let store = store_for_uri(&full_uri).map_err(|e| anyhow::anyhow!(e))?;
                store.put(&full_uri, &d).await?;
                Ok::<(), anyhow::Error>(())
            }));
        }
        
        while let Some(join_res) = futs.next().await {
            join_res.map_err(to_status)?.map_err(to_status)?;
        }
        let secs = started.elapsed().as_secs_f64();
        Ok(Response::new(OpSummary {
            total_bytes: object_size * objects as u64,
            seconds: secs,
            notes: String::new(),
        }))
    }

    async fn run_workload(
        &self,
        req: Request<RunWorkloadRequest>,
    ) -> Result<Response<WorkloadSummary>, Status> {
        info!("Received run_workload request");
        
        let RunWorkloadRequest {
            config_yaml,
            agent_id,
            path_prefix,
            start_timestamp_ns,
            shared_storage,
            agent_index,
            num_agents,
        } = req.into_inner();

        debug!("Agent ID: {}, Path prefix: {}, Shared storage: {}", agent_id, path_prefix, shared_storage);
        debug!("Agent index: {}/{}", agent_index, num_agents);
        debug!("Config YAML: {} bytes", config_yaml.len());

        // Parse the YAML configuration
        let mut config: sai3_bench::config::Config = serde_yaml::from_str(&config_yaml)
            .map_err(|e| {
                error!("Failed to parse YAML config: {}", e);
                Status::invalid_argument(format!("Invalid YAML config: {}", e))
            })?;

        debug!("YAML config parsed successfully");

        // Apply agent-specific path prefix for isolation
        config
            .apply_agent_prefix(&agent_id, &path_prefix, shared_storage)
            .map_err(|e| {
                error!("Failed to apply path prefix: {}", e);
                Status::internal(format!("Failed to apply path prefix: {}", e))
            })?;

        debug!("Agent-specific path prefix applied (shared_storage={})", shared_storage);

        debug!("Path prefix applied successfully");

        // Wait until coordinated start time
        let start_time = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(start_timestamp_ns as u64);
        if let Ok(wait_duration) = start_time.duration_since(std::time::SystemTime::now()) {
            if wait_duration > std::time::Duration::from_secs(60) {
                error!("Start time is too far in the future: {:?}", wait_duration);
                return Err(Status::invalid_argument(
                    "Start time is too far in the future (>60s)",
                ));
            }
            debug!("Waiting {:?} until coordinated start", wait_duration);
            tokio::time::sleep(wait_duration).await;
        }
        // If start_time is in the past, start immediately

        info!("Starting workload execution for agent {}", agent_id);

        // v0.8.7: Detect cleanup-only mode (checks cleanup_only flag)
        let is_cleanup_only = config.prepare.as_ref()
            .map(|p| p.cleanup_only.unwrap_or(false))
            .unwrap_or(false);

        // Execute prepare phase or generate cleanup list
        let (prepared_objects, tree_manifest) = if is_cleanup_only {
            // Cleanup-only mode: Generate objects WITHOUT listing (if skip_verification=true)
            if let Some(ref prepare_config) = config.prepare {
                if prepare_config.skip_verification {
                    info!("Cleanup-only mode (skip_verification=true): Generating object list from config");
                    let objects = sai3_bench::workload::generate_cleanup_objects(
                        prepare_config,
                        agent_index as usize,
                        num_agents as usize,
                    ).map_err(|e| {
                        error!("Failed to generate cleanup objects: {}", e);
                        Status::internal(format!("Failed to generate cleanup objects: {}", e))
                    })?;
                    info!("Generated {} objects for cleanup", objects.len());
                    (objects, None)
                } else {
                    info!("Cleanup-only mode (skip_verification=false): Listing existing objects");
                    // Run normal prepare to discover what exists
                    let (prepared, manifest, _) = sai3_bench::workload::prepare_objects(
                        prepare_config,
                        Some(&config.workload),
                        None,
                        config.concurrency,
                        agent_index as usize,
                        num_agents as usize,
                    ).await.map_err(|e| {
                        error!("Prepare phase failed: {}", e);
                        Status::internal(format!("Prepare phase failed: {}", e))
                    })?;
                    (prepared, manifest)
                }
            } else {
                (Vec::new(), None)
            }
        } else if let Some(ref prepare_config) = config.prepare {
            // Normal mode: Execute prepare phase
            debug!("Executing prepare phase");
            let (prepared, manifest, prepare_metrics) = sai3_bench::workload::prepare_objects(
                prepare_config,
                Some(&config.workload),
                None,
                config.concurrency,
                agent_index as usize,
                num_agents as usize,
            ).await.map_err(|e| {
                error!("Prepare phase failed: {}", e);
                Status::internal(format!("Prepare phase failed: {}", e))
            })?;
            info!("Prepared {} objects ({} created, {} existed) in {:.2}s",
                prepared.len(), prepare_metrics.objects_created, prepare_metrics.objects_existed, prepare_metrics.wall_seconds);
            
            // Print prepare performance summary
            if prepare_metrics.put.ops > 0 {
                let put_ops_s = prepare_metrics.put.ops as f64 / prepare_metrics.wall_seconds;
                let put_mib_s = (prepare_metrics.put.bytes as f64 / 1_048_576.0) / prepare_metrics.wall_seconds;
                
                info!("Prepare Performance:");
                info!("  Total ops: {} ({:.2} ops/s)", prepare_metrics.put.ops, put_ops_s);
                info!("  Total bytes: {} ({:.2} MiB)", prepare_metrics.put.bytes, prepare_metrics.put.bytes as f64 / 1_048_576.0);
                info!("  Throughput: {:.2} MiB/s", put_mib_s);
                info!("  Latency: mean={:.2}ms, p50={:.2}ms, p95={:.2}ms, p99={:.2}ms",
                    prepare_metrics.put.mean_us as f64 / 1000.0,
                    prepare_metrics.put.p50_us as f64 / 1000.0,
                    prepare_metrics.put.p95_us as f64 / 1000.0,
                    prepare_metrics.put.p99_us as f64 / 1000.0);
            }
            
            if prepare_metrics.mkdir_count > 0 {
                info!("  MKDIR: {} directories created", prepare_metrics.mkdir_count);
            }
            
            // Export prepare metrics to TSV if any operations were performed
            if prepare_metrics.put.ops > 0 {
                use sai3_bench::tsv_export::TsvExporter;
                let results_dir = std::path::Path::new("./sai3-agent-results");
                std::fs::create_dir_all(results_dir).ok();
                let prepare_tsv_path = results_dir.join("prepare_results.tsv");
                let exporter = TsvExporter::with_path(&prepare_tsv_path)
                    .map_err(|e| Status::internal(format!("Failed to create prepare TSV exporter: {}", e)))?;
                exporter.export_prepare_metrics(&prepare_metrics)
                    .map_err(|e| Status::internal(format!("Failed to export prepare metrics: {}", e)))?;
                info!("Prepare metrics exported to: {}", prepare_tsv_path.display());
            }
            
            // Use configurable delay from YAML (only if objects were created)
            if prepared.iter().any(|p| p.created) && prepare_config.post_prepare_delay > 0 {
                let delay_secs = prepare_config.post_prepare_delay;
                info!("Waiting {}s for object propagation (configured delay)...", delay_secs);
                tokio::time::sleep(tokio::time::Duration::from_secs(delay_secs)).await;
            }
            
            (prepared, manifest)
        } else {
            debug!("No prepare phase configured");
            (Vec::new(), None)
        };

        // Initialize s3dlio oplog if configured (v0.8.2+)
        // Priority: config YAML op_log_path > agent CLI --op-log > None
        let final_op_log_path = config.op_log_path.as_ref()
            .or(self.state.agent_op_log_path.as_ref());
        
        if let Some(op_log_base) = final_op_log_path {
            // Append agent_id to filename to avoid collisions
            let op_log_path = if let Some(parent) = op_log_base.parent() {
                let filename = op_log_base.file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("oplog.tsv.zst");
                
                // Insert agent_id before extension (e.g., oplog.tsv.zst -> oplog-agent1.tsv.zst)
                let base_name = if filename.ends_with(".tsv.zst") {
                    filename.strip_suffix(".tsv.zst").unwrap()
                } else if filename.ends_with(".tsv") {
                    filename.strip_suffix(".tsv").unwrap()
                } else {
                    filename
                };
                
                let extension = if filename.ends_with(".tsv.zst") {
                    ".tsv.zst"
                } else if filename.ends_with(".tsv") {
                    ".tsv"
                } else {
                    ""
                };
                
                parent.join(format!("{}-{}{}", base_name, agent_id, extension))
            } else {
                // No parent directory, just append agent_id
                let filename = op_log_base.file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("oplog.tsv.zst");
                std::path::PathBuf::from(format!("{}-{}", filename, agent_id))
            };
            
            info!("Initializing s3dlio operation logger: {}", op_log_path.display());
            sai3_bench::workload::init_operation_logger(&op_log_path)
                .map_err(|e| {
                    error!("Failed to initialize operation logger: {}", e);
                    Status::internal(format!("Failed to initialize operation logger: {}", e))
                })?;
        }

        // Execute the workload using existing workload::run function
        // In cleanup-only mode, run cleanup AS the workload
        let is_cleanup_only = config.prepare.as_ref()
            .map(|p| p.cleanup_only.unwrap_or(false))
            .unwrap_or(false);
        
        let summary = if is_cleanup_only {
            info!("Cleanup-only mode: Running cleanup as workload");
            
            // Get tracker for stats reporting
            let tracker = self.state.get_tracker().await;
            let start_time = std::time::Instant::now();
            
            // Get cleanup mode from config
            let cleanup_mode = config.prepare.as_ref()
                .map(|p| p.cleanup_mode)
                .unwrap_or(sai3_bench::config::CleanupMode::Tolerant);
            
            // Run cleanup with stats tracking
            sai3_bench::workload::cleanup_prepared_objects(
                &prepared_objects,
                tree_manifest.as_ref(),
                agent_index as usize,
                num_agents as usize,
                cleanup_mode,
                tracker.clone(),
            ).await.map_err(|e| {
                error!("Cleanup execution failed: {}", e);
                Status::internal(format!("Cleanup execution failed: {}", e))
            })?;
            
            // Generate summary from tracker stats
            if let Some(t) = tracker {
                let snapshot = t.snapshot();
                let elapsed = start_time.elapsed().as_secs_f64();
                
                sai3_bench::workload::Summary {
                    wall_seconds: elapsed,
                    total_ops: snapshot.meta_ops,
                    total_bytes: 0,  // DELETE operations don't transfer bytes
                    p50_us: 0,
                    p95_us: 0,
                    p99_us: 0,
                    get: Default::default(),
                    put: Default::default(),
                    meta: sai3_bench::workload::OpAgg {
                        ops: snapshot.meta_ops,
                        bytes: 0,
                        mean_us: snapshot.meta_mean_us,
                        p50_us: 0,
                        p95_us: 0,
                        p99_us: 0,
                    },
                    get_bins: Default::default(),
                    put_bins: Default::default(),
                    meta_bins: Default::default(),
                    get_hists: Default::default(),
                    put_hists: Default::default(),
                    meta_hists: Default::default(),
                    total_errors: 0,
                    error_rate: 0.0,
                }
            } else {
                // No tracker - return empty summary
                sai3_bench::workload::Summary {
                    wall_seconds: start_time.elapsed().as_secs_f64(),
                    total_ops: 0,
                    total_bytes: 0,
                    p50_us: 0,
                    p95_us: 0,
                    p99_us: 0,
                    get: Default::default(),
                    put: Default::default(),
                    meta: Default::default(),
                    get_bins: Default::default(),
                    put_bins: Default::default(),
                    meta_bins: Default::default(),
                    get_hists: Default::default(),
                    put_hists: Default::default(),
                    meta_hists: Default::default(),
                    total_errors: 0,
                    error_rate: 0.0,
                }
            }
        } else {
            sai3_bench::workload::run(&config, tree_manifest.clone())
                .await
                .map_err(|e| {
                    error!("Workload execution failed: {}", e);
                    // Finalize oplog on error
                    if final_op_log_path.is_some() {
                        if let Err(finalize_err) = sai3_bench::workload::finalize_operation_logger() {
                            error!("Failed to finalize operation logger: {}", finalize_err);
                        }
                    }
                    Status::internal(format!("Workload execution failed: {}", e))
                })?
        };

        // Finalize s3dlio oplog if initialized
        if final_op_log_path.is_some() {
            info!("Finalizing s3dlio operation logger");
            if let Err(e) = sai3_bench::workload::finalize_operation_logger() {
                error!("Failed to finalize operation logger: {}", e);
            }
        }
        
        // v0.8.7: Execute cleanup phase if configured (skip if cleanup-only since we already did it)
        if !is_cleanup_only {
            if let Some(ref prepare_config) = config.prepare {
                if prepare_config.cleanup {
                    let cleanup_mode = prepare_config.cleanup_mode;
                    info!("Starting cleanup phase (agent {}/{}, mode: {:?})", 
                          agent_index, num_agents, cleanup_mode);
                    
                    // Get tracker for live stats reporting during cleanup
                    let tracker = self.state.get_tracker().await;
                    
                    if let Err(e) = sai3_bench::workload::cleanup_prepared_objects(
                        &prepared_objects,
                        tree_manifest.as_ref(),
                        agent_index as usize,
                        num_agents as usize,
                        cleanup_mode,
                        tracker,
                    ).await {
                        error!("Cleanup phase failed for agent {}: {}", agent_id, e);
                    } else {
                        info!("Cleanup phase completed for agent {}", agent_id);
                    }
                }
            }
        }

        info!("Workload completed successfully for agent {}", agent_id);
        debug!("Summary: {} ops, {} bytes, {:.2}s", 
               summary.total_ops, summary.total_bytes, summary.wall_seconds);

        // v0.7.5: Convert Summary to WorkloadSummary protobuf using helper
        // Pass oplog path to summary (for protobuf field population)
        let op_log_path_str = final_op_log_path.map(|p| p.display().to_string());
        let proto_summary = summary_to_proto(&agent_id, &config_yaml, &summary, op_log_path_str).await?;
        Ok(Response::new(proto_summary))
    }

    /// v0.7.5: Server streaming RPC for live progress updates during distributed execution
    type RunWorkloadWithLiveStatsStream = 
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<LiveStats, Status>> + Send>>;
    
    // v0.8.4: Bidirectional streaming type
    type ExecuteWorkloadStream = 
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<LiveStats, Status>> + Send>>;

    async fn run_workload_with_live_stats(
        &self,
        req: Request<RunWorkloadRequest>,
    ) -> Result<Response<Self::RunWorkloadWithLiveStatsStream>, Status> {
        info!("Received run_workload_with_live_stats request (streaming mode)");
        
        let RunWorkloadRequest {
            config_yaml,
            agent_id,
            path_prefix,
            start_timestamp_ns,
            shared_storage,
            agent_index,
            num_agents,
        } = req.into_inner();

        // Parse and apply config (same as run_workload)
        let mut config: sai3_bench::config::Config = serde_yaml::from_str(&config_yaml)
            .map_err(|e| Status::invalid_argument(format!("Invalid YAML config: {}", e)))?;

        config
            .apply_agent_prefix(&agent_id, &path_prefix, shared_storage)
            .map_err(|e| Status::internal(format!("Failed to apply path prefix: {}", e)))?;

        // v0.7.6: Parse coordinated start time (will wait inside stream after READY)
        // v0.8.4: Coordinated start with clock synchronization
        // 
        // Protocol:
        // 1. Agent sends READY with agent_timestamp_ns (agent's current time)
        // 2. Controller calculates offset = agent_time - controller_time
        // 3. Controller sends start_timestamp_ns in controller's clock
        // 4. Agent adjusts: start_time_agent = start_time_controller (no adjustment needed!)
        //
        // Key insight: Both clocks measure time since UNIX_EPOCH. Even if agent clock
        // is skewed (e.g., +5 seconds ahead), when controller says "start at epoch time X",
        // agent checks its own clock against the same epoch time X. The skew cancels out
        // because both are measuring against the same absolute reference point.
        //
        // Example:
        //   Controller time: 1000s since epoch, says "start at 1010s"
        //   Agent time: 1005s since epoch (5s ahead), sees "start at 1010s"
        //   Agent waits: 1010 - 1005 = 5 seconds ✓
        //   Controller waits: 1010 - 1000 = 10 seconds ✓
        //   Both start at the same absolute epoch time (1010s)
        //
        // Why this works: SystemTime measures elapsed time from UNIX_EPOCH, not wall-clock time.
        // Clock skew affects the current reading but not the measurement basis.
        //
        // Previous bug: Used minimum wait when start_time was in the past.
        // Now: Always use calculated duration, even if negative (means agent already past start time).
        
        let start_time = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(start_timestamp_ns as u64);
        let wait_duration = match start_time.duration_since(std::time::SystemTime::now()) {
            Ok(d) if d > std::time::Duration::from_secs(60) => {
                return Err(Status::invalid_argument("Start time is too far in the future (>60s)"));
            }
            Ok(d) => d,
            Err(_) => {
                // Start time is in the past - this should not happen with proper coordination
                // but we handle it gracefully by starting immediately
                warn!("Start time already passed - starting immediately");
                std::time::Duration::from_secs(0)
            }
        };

        info!("Preparing workload execution with live stats for agent {}", agent_id);

        // v0.8.2: Clone agent_op_log_path early for 'static lifetime requirement
        let agent_op_log_path_clone = self.state.agent_op_log_path.clone();

        // Create live stats tracker
        let tracker = Arc::new(sai3_bench::live_stats::LiveStatsTracker::new());
        
        // v0.7.5: Capture config_yaml for final summary conversion
        let config_yaml_for_summary = config_yaml.clone();
        
        // Channel to signal completion with workload summary (v0.7.5)
        let (tx_done, mut rx_done) = tokio::sync::mpsc::channel::<Result<sai3_bench::workload::Summary, String>>(1);
        
        // v0.7.9: Channel to send prepare metrics from workload task to stream task
        let (tx_prepare, mut rx_prepare) = tokio::sync::mpsc::channel::<sai3_bench::workload::PrepareMetrics>(1);
        
        // v0.7.8: Channel to signal cancellation (Ctrl+C propagation from controller)
        let (tx_cancel, mut rx_cancel) = tokio::sync::oneshot::channel::<()>();
        
        // v0.7.11: Clone state for use in async contexts
        let agent_state = self.state.clone();
        
        // v0.7.6: VALIDATION PHASE - Check config before starting workload
        // This prevents silent failures and provides immediate feedback to controller
        if let Err(e) = validate_workload_config(&config).await {
            // v0.7.13: Transition to Failed state
            agent_state.transition_to(WorkloadState::Failed, "validation failed").await.ok();
            agent_state.set_error(e.to_string()).await;
            
            // Send ERROR status immediately and exit stream
            let error_stats = LiveStats {
                agent_id: agent_id.clone(),
                timestamp_s: 0.0,
                get_ops: 0,
                get_bytes: 0,
                get_mean_us: 0.0,
                get_p50_us: 0.0,
                get_p95_us: 0.0,
                put_ops: 0,
                put_bytes: 0,
                put_mean_us: 0.0,
                put_p50_us: 0.0,
                put_p95_us: 0.0,
                meta_ops: 0,
                meta_mean_us: 0.0,
                elapsed_s: 0.0,
                completed: false,
                final_summary: None,
                status: 3,  // ERROR
                error_message: format!("Configuration validation failed: {}", e),
                in_prepare_phase: false,
                prepare_objects_created: 0,
                prepare_objects_total: 0,
                prepare_summary: None,
                cpu_user_percent: 0.0,
                cpu_system_percent: 0.0,
                cpu_iowait_percent: 0.0,
                cpu_total_percent: 0.0,
                agent_timestamp_ns: 0,
                sequence: 0,
            };
            
            let stream = async_stream::stream! {
                yield Ok(error_stats);
                // v0.7.13: Auto-reset to Idle after sending error
                let _ = agent_state.transition_to(WorkloadState::Idle, "error sent, auto-reset").await;
                agent_state.clear_error().await;
            };
            return Ok(Response::new(Box::pin(stream) as Self::RunWorkloadWithLiveStatsStream));
        }
        
        // v0.7.12: Check current state - only accept new workload if Idle
        let current_state = agent_state.get_state().await;
        if current_state != WorkloadState::Idle {
            warn!("Rejecting workload request - agent in {:?} state (not Idle)", current_state);
            return Err(Status::failed_precondition(
                format!("Agent busy: current state is {:?}, expected Idle", current_state)
            ));
        }
        
        // v0.7.13: Transition to Ready state (validation passed)
        agent_state.transition_to(WorkloadState::Ready, "validation passed").await
            .map_err(|e| Status::internal(format!("State transition failed: {}", e)))?;
        
        // v0.7.11: Initialize CPU monitor for utilization tracking
        let mut cpu_monitor = CpuMonitor::new();
        
        // Create stream that sends stats every 1 second
        let agent_id_stream = agent_id.clone();
        let config_yaml_stream = config_yaml_for_summary;
        let tracker_spawn = tracker.clone();
        let config_spawn = config.clone();
        let agent_state_stream = agent_state.clone();
        let stream = async_stream::stream! {
            // v0.7.12: Subscribe to abort signals inside stream (needed for coordinated start + workload)
            let mut abort_rx = agent_state_stream.subscribe_abort();
            
            // v0.7.6: Send READY status first (validation passed)
            // v0.8.4: Include agent timestamp for clock synchronization
            let agent_timestamp_ns = get_agent_timestamp_ns();
            
            let ready_msg = LiveStats {
                agent_id: agent_id_stream.clone(),
                timestamp_s: 0.0,
                get_ops: 0,
                get_bytes: 0,
                get_mean_us: 0.0,
                get_p50_us: 0.0,
                get_p95_us: 0.0,
                put_ops: 0,
                put_bytes: 0,
                put_mean_us: 0.0,
                put_p50_us: 0.0,
                put_p95_us: 0.0,
                meta_ops: 0,
                meta_mean_us: 0.0,
                elapsed_s: 0.0,
                completed: false,
                final_summary: None,
                status: 1,  // READY
                error_message: String::new(),
                in_prepare_phase: false,
                prepare_objects_created: 0,
                prepare_objects_total: 0,
                prepare_summary: None,
                cpu_user_percent: 0.0,
                cpu_system_percent: 0.0,
                cpu_iowait_percent: 0.0,
                cpu_total_percent: 0.0,
                agent_timestamp_ns: agent_timestamp_ns,
                sequence: 0,
            };
            yield Ok(ready_msg);
            
            // v0.8.4: Wait for coordinated start while sending periodic keepalive stats
            // CRITICAL: Don't block the stream during wait - send stats every 1s to keep controller informed
            info!("Agent {} waiting {:?} for coordinated start", agent_id_stream, wait_duration);
            
            let target_start_time = tokio::time::Instant::now() + wait_duration;
            let mut keepalive_interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            keepalive_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            
            loop {
                tokio::select! {
                    _ = &mut rx_cancel => {
                        warn!("Agent {} cancelled during coordinated start (controller disconnected)", agent_id_stream);
                        let _ = agent_state_stream.transition_to(WorkloadState::Idle, "disconnect during wait").await;
                        yield Err(Status::cancelled("Controller disconnected during coordinated start"));
                        return;
                    }
                    _ = abort_rx.recv() => {
                        warn!("Agent {} aborted during coordinated start", agent_id_stream);
                        let _ = agent_state_stream.transition_to(WorkloadState::Idle, "abort during wait").await;
                        yield Err(Status::aborted("Aborted during coordinated start"));
                        return;
                    }
                    _ = keepalive_interval.tick() => {
                        // Send keepalive stats to controller (agent still waiting for coordinated start)
                        // Status=1 (READY) indicates agent is validated and waiting
                        let keepalive_stats = LiveStats {
                            agent_id: agent_id_stream.clone(),
                            timestamp_s: 0.0,
                            get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p95_us: 0.0,
                            put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p95_us: 0.0,
                            meta_ops: 0, meta_mean_us: 0.0,
                            elapsed_s: 0.0,
                            completed: false,
                            final_summary: None,
                            status: 1,  // READY (waiting for start)
                            error_message: String::new(),
                            in_prepare_phase: false,
                            prepare_objects_created: 0,
                            prepare_objects_total: 0,
                            prepare_summary: None,
                            cpu_user_percent: 0.0,
                            cpu_system_percent: 0.0,
                            cpu_iowait_percent: 0.0,
                            cpu_total_percent: 0.0,
                            agent_timestamp_ns: 0,
                            sequence: 0,
                        };
                        debug!("Sending keepalive stats during coordinated start wait");
                        yield Ok(keepalive_stats);
                        
                        // Check if we've reached the start time
                        if tokio::time::Instant::now() >= target_start_time {
                            break;
                        }
                    }
                }
            }
            
            // v0.7.13: Transition to Running state (start time reached, spawn workload)
            if let Err(e) = agent_state_stream.transition_to(WorkloadState::Running, "start time reached").await {
                error!("Failed to transition to Running: {}", e);
                yield Err(Status::internal(format!("State transition failed: {}", e)));
                return;
            }
            
            info!("Starting workload execution for agent {}", agent_id_stream);
            
            // v0.8.2: Capture final oplog path for summary_to_proto (before spawning task)
            let final_op_log_path_for_summary = config_spawn.op_log_path.as_ref()
                .or(agent_op_log_path_clone.as_ref())
                .map(|p| p.display().to_string());
            
            // v0.8.2: Clone agent_op_log_path for use in spawned task
            let agent_op_log_path_for_task = agent_op_log_path_clone.clone();
            
            // v0.7.6: NOW spawn the workload task (after READY sent and coordinated start)
            let tracker_exec = tracker_spawn.clone();
            let mut config_exec = config_spawn.clone();
            let agent_id_exec = agent_id_stream.clone();
            let rx_cancel_exec = rx_cancel;
            // v0.7.12: Subscribe again for the workload task (separate receiver)
            let mut abort_rx_exec = agent_state_stream.subscribe_abort();
            
            // v0.8.3: Spawn LOCAL PROGRESS BAR task to display agent console progress
            // This runs INDEPENDENTLY of gRPC streaming - agents MUST show progress locally
            // Each agent displays its OWN progress bar on its OWN console (separate terminal window)
            let tracker_progress = tracker_spawn.clone();
            let agent_id_progress = agent_id_stream.clone();
            let mut rx_completion = agent_state_stream.subscribe_completion();
            let mut rx_abort = agent_state_stream.subscribe_abort();
            tokio::spawn(async move {
                use indicatif::{ProgressBar, ProgressStyle};
                use std::time::{Duration as StdDuration, Instant};
                
                let mut interval = tokio::time::interval(StdDuration::from_millis(100));
                let mut prepare_pb: Option<ProgressBar> = None;
                let mut workload_pb: Option<ProgressBar> = None;
                let mut last_prepare_total = 0u64;
                let mut last_stats_update = Instant::now();
                let mut last_ops = 0u64;
                let mut last_bytes = 0u64;
                
                loop {
                    tokio::select! {
                        _ = rx_completion.recv() => {
                            // Workload completed successfully - clean up progress bars
                            if let Some(pb) = prepare_pb.as_ref() {
                                pb.finish_and_clear();
                            }
                            if let Some(pb) = workload_pb.as_ref() {
                                pb.finish_and_clear();
                            }
                            break;
                        }
                        _ = rx_abort.recv() => {
                            // Workload aborted - clean up progress bars immediately
                            if let Some(pb) = prepare_pb.as_ref() {
                                pb.abandon_with_message(format!("[{}] aborted", agent_id_progress));
                            }
                            if let Some(pb) = workload_pb.as_ref() {
                                pb.abandon_with_message(format!("[{}] aborted", agent_id_progress));
                            }
                            break;
                        }
                        _ = interval.tick() => {
                            let snapshot = tracker_progress.snapshot();
                    
                            // Prepare phase progress bar (stays visible after completion)
                            if snapshot.in_prepare_phase {
                                if prepare_pb.is_none() || last_prepare_total != snapshot.prepare_objects_total {
                                    // Create prepare progress bar with exact same style as standalone mode
                                    let pb = ProgressBar::new(snapshot.prepare_objects_total);
                                    pb.set_style(
                                        ProgressStyle::with_template(
                                            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects {msg}"
                                        ).unwrap()
                                    );
                                    pb.set_message(format!("[{}] preparing...", agent_id_progress));
                                    prepare_pb = Some(pb);
                                    last_prepare_total = snapshot.prepare_objects_total;
                                }
                                
                                if let Some(ref pb) = prepare_pb {
                                    pb.set_position(snapshot.prepare_objects_created);
                                    
                                    // Update stats message every 0.5s
                                    let elapsed = last_stats_update.elapsed();
                                    if elapsed.as_secs_f64() >= 0.5 {
                                        let ops_delta = snapshot.put_ops.saturating_sub(last_ops);
                                        let bytes_delta = snapshot.put_bytes.saturating_sub(last_bytes);
                                        let time_delta = elapsed.as_secs_f64();
                                        
                                        if ops_delta > 0 {
                                            let ops_per_sec = ops_delta as f64 / time_delta;
                                            let mib_per_sec = (bytes_delta as f64 / 1_048_576.0) / time_delta;
                                            // Estimate avg latency from throughput (rough approximation)
                                            let avg_latency_ms = if snapshot.put_ops > 0 {
                                                snapshot.put_mean_us as f64 / 1000.0
                                            } else {
                                                0.0
                                            };
                                            
                                            pb.set_message(format!(
                                                "[{}] {:.0} ops/s | {:.1} MiB/s | avg {:.2}ms",
                                                agent_id_progress, ops_per_sec, mib_per_sec, avg_latency_ms
                                            ));
                                        }
                                        
                                        last_ops = snapshot.put_ops;
                                        last_bytes = snapshot.put_bytes;
                                        last_stats_update = Instant::now();
                                    }
                                }
                            } else if snapshot.in_prepare_phase == false && prepare_pb.is_some() {
                                // Prepare phase just finished - finalize the bar
                                if let Some(pb) = prepare_pb.as_ref() {
                                    pb.finish_with_message(format!("[{}] prepare complete", agent_id_progress));
                                }
                            }
                            
                            // Workload execution phase progress bar (separate from prepare)
                            if !snapshot.in_prepare_phase && (snapshot.put_ops > 0 || snapshot.get_ops > 0) {
                                if workload_pb.is_none() {
                                    // Create workload progress bar (time-based, like standalone mode)
                                    let pb = ProgressBar::new_spinner();
                                    pb.set_style(
                                        ProgressStyle::with_template(
                                            "{spinner:.green} [{elapsed_precise}] {msg}"
                                        ).unwrap()
                                    );
                                    pb.set_message(format!("[{}] workload running...", agent_id_progress));
                                    workload_pb = Some(pb);
                                    // Reset stats tracking for workload phase
                                    last_ops = 0;
                                    last_bytes = 0;
                                    last_stats_update = Instant::now();
                                }
                                
                                if let Some(ref pb) = workload_pb {
                                    pb.tick();
                                    
                                    // Update stats message every 0.5s
                                    let elapsed = last_stats_update.elapsed();
                                    if elapsed.as_secs_f64() >= 0.5 {
                                        let ops_delta = (snapshot.get_ops + snapshot.put_ops).saturating_sub(last_ops);
                                        let bytes_delta = (snapshot.get_bytes + snapshot.put_bytes).saturating_sub(last_bytes);
                                        let time_delta = elapsed.as_secs_f64();
                                        
                                        if ops_delta > 0 {
                                            let ops_per_sec = ops_delta as f64 / time_delta;
                                            let mib_per_sec = (bytes_delta as f64 / 1_048_576.0) / time_delta;
                                            let avg_latency_ms = if snapshot.get_ops > 0 && snapshot.put_ops > 0 {
                                                // Mixed workload - average of both
                                                ((snapshot.get_mean_us + snapshot.put_mean_us) as f64 / 2.0) / 1000.0
                                            } else if snapshot.get_ops > 0 {
                                                snapshot.get_mean_us as f64 / 1000.0
                                            } else {
                                                snapshot.put_mean_us as f64 / 1000.0
                                            };
                                            
                                            pb.set_message(format!(
                                                "[{}] {:.0} ops/s | {:.1} MiB/s | avg {:.2}ms",
                                                agent_id_progress, ops_per_sec, mib_per_sec, avg_latency_ms
                                            ));
                                        }
                                        
                                        last_ops = snapshot.get_ops + snapshot.put_ops;
                                        last_bytes = snapshot.get_bytes + snapshot.put_bytes;
                                        last_stats_update = Instant::now();
                                    }
                                }
                            }
                        }
                    }
                }
            });
            
            tokio::spawn(async move {
                // Wire live stats tracker for distributed operation recording (v0.7.5+)
                let tracker_for_prepare = tracker_exec.clone();
                config_exec.live_stats_tracker = Some(tracker_exec);
                
                // v0.7.11: Execute prepare/workload with cancellation support (controller disconnect OR abort RPC)
                tokio::select! {
                    _ = rx_cancel_exec => {
                        warn!("Agent {} received cancellation signal (controller disconnected)", agent_id_exec);
                        let _ = tx_done.send(Err("Cancelled by controller disconnect".to_string())).await;
                    }
                    _ = abort_rx_exec.recv() => {
                        warn!("Agent {} received ABORT signal via AbortWorkload RPC", agent_id_exec);
                        let _ = tx_done.send(Err("Aborted by controller request".to_string())).await;
                    }
                    result = async {
                        // Execute prepare phase if configured
                        let tree_manifest = if let Some(ref prepare_config) = config_exec.prepare {
                            match sai3_bench::workload::prepare_objects(
                                prepare_config, 
                                Some(&config_exec.workload), 
                                Some(tracker_for_prepare.clone()), 
                                config_exec.concurrency,
                                agent_index as usize,
                                num_agents as usize,
                            ).await {
                                Ok((prepared, manifest, prepare_metrics)) => {
                                    info!("Prepared {} objects for agent {}", prepared.len(), agent_id_exec);
                                    
                                    // v0.7.9: Export prepare metrics to TSV and JSON
                                    if prepare_metrics.put.ops > 0 {
                                        use sai3_bench::tsv_export::TsvExporter;
                                        let results_dir = std::path::Path::new("./sai3-agent-results");
                                        if let Err(e) = std::fs::create_dir_all(results_dir) {
                                            warn!("Failed to create agent results directory: {}", e);
                                        } else {
                                            let prepare_tsv_path = results_dir.join("prepare_results.tsv");
                                            match TsvExporter::with_path(&prepare_tsv_path) {
                                                Ok(exporter) => {
                                                    if let Err(e) = exporter.export_prepare_metrics(&prepare_metrics) {
                                                        warn!("Failed to export prepare metrics: {}", e);
                                                    } else {
                                                        info!("Prepare metrics exported to: {}", prepare_tsv_path.display());
                                                    }
                                                }
                                                Err(e) => warn!("Failed to create prepare TSV exporter: {}", e),
                                            }
                                            
                                            // v0.7.9: Send prepare metrics to stream task
                                            let _ = tx_prepare.send(prepare_metrics).await;
                                        }
                                    }
                                    
                                    // v0.7.9: Reset stats counters before workload to clear prepare phase PUT operations
                                    tracker_for_prepare.reset_for_workload();
                                    
                                    if prepared.iter().any(|p| p.created) && prepare_config.post_prepare_delay > 0 {
                                        tokio::time::sleep(tokio::time::Duration::from_secs(prepare_config.post_prepare_delay)).await;
                                    }
                                    manifest
                                }
                                Err(e) => {
                                    let _ = tx_done.send(Err(format!("Prepare phase failed: {}", e))).await;
                                    return;
                                }
                            }
                        } else {
                            None
                        };

                        // v0.8.2: Initialize s3dlio oplog if configured (same logic as run_workload)
                        let final_op_log_path = config_exec.op_log_path.as_ref()
                            .or(agent_op_log_path_for_task.as_ref());
                        
                        if let Some(op_log_base) = final_op_log_path {
                            // Append agent_id to filename to avoid collisions
                            let op_log_path = if let Some(parent) = op_log_base.parent() {
                                let filename = op_log_base.file_name()
                                    .and_then(|f| f.to_str())
                                    .unwrap_or("oplog.tsv.zst");
                                
                                let base_name = if filename.ends_with(".tsv.zst") {
                                    filename.strip_suffix(".tsv.zst").unwrap()
                                } else if filename.ends_with(".tsv") {
                                    filename.strip_suffix(".tsv").unwrap()
                                } else {
                                    filename
                                };
                                
                                let extension = if filename.ends_with(".tsv.zst") {
                                    ".tsv.zst"
                                } else if filename.ends_with(".tsv") {
                                    ".tsv"
                                } else {
                                    ""
                                };
                                
                                parent.join(format!("{}-{}{}", base_name, agent_id_exec, extension))
                            } else {
                                let filename = op_log_base.file_name()
                                    .and_then(|f| f.to_str())
                                    .unwrap_or("oplog.tsv.zst");
                                std::path::PathBuf::from(format!("{}-{}", filename, agent_id_exec))
                            };
                            
                            info!("Initializing s3dlio operation logger: {}", op_log_path.display());
                            if let Err(e) = sai3_bench::workload::init_operation_logger(&op_log_path) {
                                error!("Failed to initialize operation logger: {}", e);
                                let _ = tx_done.send(Err(format!("Failed to initialize oplog: {}", e))).await;
                                return;
                            }
                        }

                        // Execute workload (tracker wired via config for live stats)
                        match sai3_bench::workload::run(&config_exec, tree_manifest).await {
                            Ok(summary) => {
                                // v0.8.2: Finalize oplog if initialized
                                if final_op_log_path.is_some() {
                                    info!("Finalizing s3dlio operation logger");
                                    if let Err(e) = sai3_bench::workload::finalize_operation_logger() {
                                        error!("Failed to finalize operation logger: {}", e);
                                    }
                                }
                                info!("Workload completed successfully for agent {}", agent_id_exec);
                                let _ = tx_done.send(Ok(summary)).await;
                            }
                            Err(e) => {
                                // v0.8.2: Finalize oplog on error
                                if final_op_log_path.is_some() {
                                    if let Err(finalize_err) = sai3_bench::workload::finalize_operation_logger() {
                                        error!("Failed to finalize operation logger: {}", finalize_err);
                                    }
                                }
                                error!("Workload execution failed for agent {}: {}", agent_id_exec, e);
                                let _ = tx_done.send(Err(format!("Workload execution failed: {}", e))).await;
                            }
                        }
                    } => result
                }
            });
            
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            // v0.7.12: Subscribe to abort signals in stream loop
            let mut abort_rx_stream = agent_state_stream.subscribe_abort();

            // v0.7.9: Store prepare metrics when received from workload task
            let mut stored_prepare_metrics: Option<sai3_bench::workload::PrepareMetrics> = None;

            loop {
                tokio::select! {
                    // v0.7.12: Handle abort signal - break out of stream loop immediately
                    _ = abort_rx_stream.recv() => {
                        warn!("Agent {} stream received abort signal - ending stream", agent_id_stream);
                        break;
                    }
                    
                    // v0.7.9: Receive prepare metrics from workload task
                    Some(prep_metrics) = rx_prepare.recv() => {
                        info!("Received prepare metrics: {} objects created", prep_metrics.objects_created);
                        stored_prepare_metrics = Some(prep_metrics);
                    }
                    
                    _ = interval.tick() => {
                        // v0.7.11: Sample CPU utilization
                        let cpu_util = cpu_monitor.sample()
                            .ok()
                            .flatten()
                            .unwrap_or(CpuUtilization {
                                user_percent: 0.0,
                                system_percent: 0.0,
                                iowait_percent: 0.0,
                                total_percent: 0.0,
                            });
                        
                        // Send live stats snapshot
                        let snapshot = tracker.snapshot();
                        debug!("Sending stats: PUT {} ops, {} bytes, elapsed {:.1}s, CPU {:.1}%", 
                               snapshot.put_ops, snapshot.put_bytes, snapshot.elapsed_secs(), cpu_util.total_percent);
                        let stats = LiveStats {
                            agent_id: agent_id_stream.clone(),
                            timestamp_s: snapshot.timestamp_secs() as f64,
                            get_ops: snapshot.get_ops,
                            get_bytes: snapshot.get_bytes,
                            get_mean_us: snapshot.get_mean_us as f64,
                            get_p50_us: snapshot.get_p50_us as f64,
                            get_p95_us: snapshot.get_p95_us as f64,
                            put_ops: snapshot.put_ops,
                            put_bytes: snapshot.put_bytes,
                            put_mean_us: snapshot.put_mean_us as f64,
                            put_p50_us: snapshot.put_p50_us as f64,
                            put_p95_us: snapshot.put_p95_us as f64,
                            meta_ops: snapshot.meta_ops,
                            meta_mean_us: snapshot.meta_mean_us as f64,
                            elapsed_s: snapshot.elapsed_secs(),
                            completed: false,
                            final_summary: None,  // v0.7.5: Only in final message
                            status: 2,  // RUNNING
                            error_message: String::new(),
                            in_prepare_phase: snapshot.in_prepare_phase,
                            prepare_objects_created: snapshot.prepare_objects_created,
                            prepare_objects_total: snapshot.prepare_objects_total,
                            prepare_summary: None,
                            cpu_user_percent: cpu_util.user_percent,
                            cpu_system_percent: cpu_util.system_percent,
                            cpu_iowait_percent: cpu_util.iowait_percent,
                            cpu_total_percent: cpu_util.total_percent,
                            agent_timestamp_ns: 0,
                            sequence: 0,
                        };
                        // v0.8.2: Yield can block if controller is slow - log if we're about to send
                        if snapshot.in_prepare_phase && snapshot.prepare_objects_created % 10000 == 0 {
                            debug!("Prepare progress: {}/{} objects ({:.1}%)",
                                snapshot.prepare_objects_created,
                                snapshot.prepare_objects_total,
                                (snapshot.prepare_objects_created as f64 / snapshot.prepare_objects_total as f64) * 100.0
                            );
                        }
                        yield Ok(stats);
                    }
                    
                    result = rx_done.recv() => {
                        // Workload completed (or failed)
                        match result {
                            Some(Ok(summary)) => {
                                // v0.7.5 / v0.8.2: Convert summary to proto for final message (with oplog path)
                                let proto_summary = match summary_to_proto(&agent_id_stream, &config_yaml_stream, &summary, final_op_log_path_for_summary.clone()).await {
                                    Ok(s) => Some(s),
                                    Err(e) => {
                                        error!("Failed to convert summary to proto: {:?}", e);
                                        None  // Continue without summary rather than failing entire stream
                                    }
                                };
                                
                                // v0.7.9: Convert prepare metrics to proto if available
                                let proto_prepare = if let Some(ref prep_metrics) = stored_prepare_metrics {
                                    match prepare_metrics_to_proto(&agent_id_stream, prep_metrics).await {
                                        Ok(s) => Some(s),
                                        Err(e) => {
                                            error!("Failed to convert prepare metrics to proto: {:?}", e);
                                            None
                                        }
                                    }
                                } else {
                                    None
                                };
                                
                                // Send final stats with completed=true and full summary
                                let snapshot = tracker.snapshot();
                                
                                // v0.7.11: Sample CPU one final time
                                let cpu_util = cpu_monitor.sample()
                                    .ok()
                                    .flatten()
                                    .unwrap_or(CpuUtilization {
                                        user_percent: 0.0,
                                        system_percent: 0.0,
                                        iowait_percent: 0.0,
                                        total_percent: 0.0,
                                    });
                                
                                let final_stats = LiveStats {
                                    agent_id: agent_id_stream.clone(),
                                    timestamp_s: snapshot.timestamp_secs() as f64,
                                    get_ops: snapshot.get_ops,
                                    get_bytes: snapshot.get_bytes,
                                    get_mean_us: snapshot.get_mean_us as f64,
                                    get_p50_us: snapshot.get_p50_us as f64,
                                    get_p95_us: snapshot.get_p95_us as f64,
                                    put_ops: snapshot.put_ops,
                                    put_bytes: snapshot.put_bytes,
                                    put_mean_us: snapshot.put_mean_us as f64,
                                    put_p50_us: snapshot.put_p50_us as f64,
                                    put_p95_us: snapshot.put_p95_us as f64,
                                    meta_ops: snapshot.meta_ops,
                                    meta_mean_us: snapshot.meta_mean_us as f64,
                                    elapsed_s: snapshot.elapsed_secs(),
                                    completed: true,
                                    final_summary: proto_summary,  // v0.7.5: Include complete results
                                    status: 4,  // COMPLETED
                                    error_message: String::new(),
                                    in_prepare_phase: false,
                                    prepare_objects_created: 0,
                                    prepare_objects_total: 0,
                                    prepare_summary: proto_prepare,  // v0.7.9: Include prepare metrics
                                    cpu_user_percent: cpu_util.user_percent,
                                    cpu_system_percent: cpu_util.system_percent,
                                    cpu_iowait_percent: cpu_util.iowait_percent,
                                    cpu_total_percent: cpu_util.total_percent,
                                    agent_timestamp_ns: 0,
                                    sequence: 0,
                                };
                                
                                // v0.7.13: Transition to Idle (workload completed successfully)
                                let _ = agent_state_stream.transition_to(WorkloadState::Idle, "workload completed").await;
                                
                                yield Ok(final_stats);
                                break;
                            }
                            Some(Err(e)) => {
                                // v0.7.13: Transition to Failed, then auto-reset to Idle
                                let _ = agent_state_stream.transition_to(WorkloadState::Failed, &e).await;
                                agent_state_stream.set_error(e.clone()).await;
                                
                                // Auto-reset to Idle BEFORE yielding error (yield breaks the stream)
                                let _ = agent_state_stream.transition_to(WorkloadState::Idle, "error sent, auto-reset").await;
                                agent_state_stream.clear_error().await;
                                
                                yield Err(Status::internal(e));
                                break;
                            }
                            None => {
                                // v0.7.13: Transition to Failed for unexpected termination
                                let _ = agent_state_stream.transition_to(WorkloadState::Failed, "task terminated unexpectedly").await;
                                
                                // Auto-reset to Idle BEFORE yielding error (yield breaks the stream)
                                let _ = agent_state_stream.transition_to(WorkloadState::Idle, "error sent, auto-reset").await;
                                
                                yield Err(Status::internal("Workload task terminated unexpectedly"));
                                break;
                            }
                        }
                    }
                }
            }
            
            // v0.7.8: Stream is ending (controller disconnected or normal completion)
            // Send cancellation signal to workload task
            info!("Agent {} stream ending - cleaning up", agent_id_stream);
            let _ = tx_cancel.send(());
            
            // v0.8.3: Signal completion to cleanup tasks (progress bars)
            agent_state_stream.send_completion();
            
            // v0.8.2: Reset agent state to Idle after stream completes
            // BUG FIX: Previously tried unconditional transition to Idle
            // 
            // Issue: Normal flow transitions Running→Idle when workload completes (line 992)
            //        Then stream cleanup tries Idle→Idle which fails validation
            //        Error: "Invalid state transition: Idle → Idle (reason: stream ended)"
            // 
            // Solution: Check current state before attempting transition
            //           Only transition if not already Idle (idempotent cleanup)
            let current_state = agent_state_stream.get_state().await;
            if current_state != WorkloadState::Idle {
                let _ = agent_state_stream.transition_to(WorkloadState::Idle, "stream ended").await;
                info!("Agent {} reset to Idle state from {:?}, ready for next workload", agent_id_stream, current_state);
            } else {
                debug!("Agent {} already in Idle state, no transition needed", agent_id_stream);
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }
    
    // v0.7.12: Abort ongoing workload (controller failure, user interrupt, etc.)
    async fn abort_workload(&self, _req: Request<Empty>) -> Result<Response<Empty>, Status> {
        let current_state = self.state.get_state().await;
        
        match current_state {
            WorkloadState::Running | WorkloadState::Ready => {
                warn!("⚠️  Abort workload requested - sending cancellation signal");
                
                // v0.7.13: Transition to Aborting state
                if let Err(e) = self.state.transition_to(WorkloadState::Aborting, "abort RPC received").await {
                    error!("Failed to transition to Aborting: {}", e);
                    return Err(Status::internal(format!("State transition failed: {}", e)));
                }
                
                self.state.send_abort();
                
                // v0.7.12: Reset to Idle after workload cleanup with retry logic
                // Try at 5s, retry at 15s (5s + 10s backoff)
                // This ensures agents reset even if stream cleanup fails
                let state_for_reset = self.state.clone();
                tokio::spawn(async move {
                    // First attempt: 5 second timeout
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    let current = state_for_reset.get_state().await;
                    if current == WorkloadState::Aborting {
                        let _ = state_for_reset.transition_to(WorkloadState::Idle, "abort timeout 5s").await;
                        info!("⏰ Abort timeout (5s) - reset agent to Idle state (workload cleanup complete)");
                    } else if current == WorkloadState::Idle {
                        debug!("Agent already reset to Idle via stream cleanup");
                    } else {
                        // State changed to something unexpected during abort
                        warn!("Unexpected state during abort: {:?}", current);
                    }
                    
                    // Second attempt: retry after 10 more seconds if still in wrong state
                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    let retry_state = state_for_reset.get_state().await;
                    if retry_state == WorkloadState::Aborting || retry_state == WorkloadState::Running {
                        warn!("⚠️  Agent stuck in {:?} state after 15s - forcing reset to Idle", retry_state);
                        // Force transition (may violate state machine, but recovery needed)
                        state_for_reset.set_state(WorkloadState::Idle).await;
                        info!("⏰ Abort retry (15s) - forced agent to Idle state");
                    }
                });
                
                Ok(Response::new(Empty {}))
            }
            WorkloadState::Aborting => {
                warn!("⚠️  Abort already in progress");
                Ok(Response::new(Empty {}))
            }
            WorkloadState::Idle | WorkloadState::Failed => {
                info!("Abort requested but agent is idle or failed");
                Ok(Response::new(Empty {}))
            }
        }
    }

    // v0.8.4: Bidirectional streaming RPC for robust control and stats
    // 
    // Two-channel architecture (one stream, two logical channels, two independent tasks):
    // 
    // 1. Control Reader Task (this function): Processes ControlMessage from controller
    //    - START: Validate config, spawn workload, signal stats writer
    //    - PING: Keepalive to prevent false timeout
    //    - ABORT: Cancel workload immediately
    //    - ACKNOWLEDGE: Controller confirms receipt of our message
    // 
    // 2. Stats Writer Task (spawned here): Sends LiveStats to controller
    //    - READY: Validation passed, agent waiting for START
    //    - RUNNING: Progress stats every 1s (with sequence numbers)
    //    - COMPLETED: Final summary with results
    // 
    // Key benefits over RunWorkloadWithLiveStats:
    // - No blocking during coordinated start (explicit START command)
    // - Independent buffers prevent gRPC backpressure from blocking control
    // - Control channel always available for PING/ABORT even during stats congestion
    // - Sequence numbers enable gap detection and acknowledgment
    async fn execute_workload(
        &self,
        req: Request<tonic::Streaming<ControlMessage>>,
    ) -> Result<Response<Self::ExecuteWorkloadStream>, Status> {
        info!("Received execute_workload request (bidirectional streaming mode)");
        
        let mut control_stream = req.into_inner();
        
        // Channel for stats writer to send LiveStats
        let (tx_stats, rx_stats) = tokio::sync::mpsc::channel::<LiveStats>(32);
        
        // Channel to signal workload completion (for stats writer)
        let (tx_done, mut rx_done) = tokio::sync::mpsc::channel::<Result<sai3_bench::workload::Summary, String>>(1);
        
        // Channel to send prepare metrics from workload task to stats writer
        let (tx_prepare, mut rx_prepare) = tokio::sync::mpsc::channel::<sai3_bench::workload::PrepareMetrics>(1);
        
        // Clone state for use in tasks
        let agent_state = self.state.clone();
        let agent_op_log_path = self.state.agent_op_log_path.clone();
        
        // Spawn stats writer task (sends LiveStats to controller)
        // Task runs independently and exits when rx_done receives completion or channel closes
        {
            let tx_stats = tx_stats.clone();
            let agent_state = agent_state.clone();
            
            tokio::spawn(async move {
                let mut sequence: i64 = 0;
                
                // Wait for START command validation to complete (signaled by transition to Ready)
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    let state = agent_state.get_state().await;
                    if state == WorkloadState::Ready {
                        break;
                    } else if state == WorkloadState::Failed || state == WorkloadState::Idle {
                        // Validation failed or agent reset - exit early
                        return;
                    }
                }
                
                // Get agent_id from state (stored during validation)
                let agent_id = agent_state.get_agent_id().await.unwrap_or_else(|| "unknown".to_string());
                
                // Send READY status with agent timestamp for clock synchronization
                let agent_timestamp_ns = get_agent_timestamp_ns();
                let ready_msg = LiveStats {
                    agent_id: agent_id.clone(),
                    timestamp_s: 0.0,
                    get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p95_us: 0.0,
                    put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p95_us: 0.0,
                    meta_ops: 0, meta_mean_us: 0.0,
                    elapsed_s: 0.0,
                    completed: false,
                    final_summary: None,
                    status: 1,  // READY
                    error_message: String::new(),
                    in_prepare_phase: false,
                    prepare_objects_created: 0,
                    prepare_objects_total: 0,
                    prepare_summary: None,
                    cpu_user_percent: 0.0, cpu_system_percent: 0.0, cpu_iowait_percent: 0.0, cpu_total_percent: 0.0,
                    agent_timestamp_ns,
                    sequence,
                };
                sequence += 1;
                
                if tx_stats.send(ready_msg).await.is_err() {
                    error!("Stats writer: Failed to send READY message (controller disconnected?)");
                    return;
                }
                info!("Stats writer: Sent READY message with timestamp {} ns", agent_timestamp_ns);
                
                // Wait for workload to start (transition to Running)
                // Block here - do NOT send any more messages until workload starts
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    let state = agent_state.get_state().await;
                    if state == WorkloadState::Running {
                        info!("Stats writer: Workload started, beginning stats transmission");
                        break;
                    } else if state == WorkloadState::Failed || state == WorkloadState::Idle || state == WorkloadState::Aborting {
                        // Workload cancelled or failed before starting
                        info!("Stats writer: Workload cancelled or failed, exiting (state: {:?})", state);
                        return;
                    }
                    // Still in READY state - keep waiting silently
                }
                
                // Get LiveStatsTracker from state (set during workload spawn)
                let tracker = match agent_state.get_tracker().await {
                    Some(t) => t,
                    None => {
                        error!("Stats writer: No LiveStatsTracker found in state");
                        return;
                    }
                };
                
                // Initialize CPU monitor
                let mut cpu_monitor = CpuMonitor::new();
                
                // Send RUNNING stats every 1 second until completion
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                
                // Track prepare phase metrics
                let mut prepare_proto: Option<PrepareSummary> = None;
                
                loop {
                    tokio::select! {
                        // Check for prepare metrics from workload task
                        Some(prepare_metrics) = rx_prepare.recv() => {
                            info!("Stats writer: Received prepare metrics");
                            // Convert to PrepareSummary proto (reuse existing conversion logic)
                            prepare_proto = prepare_metrics_to_proto(&agent_id, &prepare_metrics).await.ok();
                        }
                        
                        // Check for workload completion
                        result = rx_done.recv() => {
                            match result {
                                Some(Ok(summary)) => {
                                    info!("Stats writer: Workload completed successfully");
                                    
                                    // Get config_yaml and op_log_path from state
                                    let config_yaml = agent_state.get_config_yaml().await.unwrap_or_default();
                                    let op_log_path = agent_state.get_op_log_path().await;
                                    
                                    // Convert to proto
                                    let proto_summary = summary_to_proto(&agent_id, &config_yaml, &summary, op_log_path)
                                        .await
                                        .ok();
                                    
                                    // Get final snapshot and CPU stats
                                    let snapshot = tracker.snapshot();
                                    let cpu_util = cpu_monitor.sample()
                                        .ok()
                                        .flatten()
                                        .unwrap_or(CpuUtilization {
                                            user_percent: 0.0,
                                            system_percent: 0.0,
                                            iowait_percent: 0.0,
                                            total_percent: 0.0,
                                        });
                                    
                                    // v0.8.7: For fast-completing workloads (counted workloads like cleanup),
                                    // continue sending stats updates for minimum duration to give controller
                                    // time to process and display results properly.
                                    //
                                    // Protocol: Agents send cumulative stats every 1s. Controller calculates
                                    // deltas for display. When workload completes quickly, agent continues
                                    // sending same final values for up to 10 seconds, then sends COMPLETED.
                                    let elapsed_secs = snapshot.elapsed_secs();
                                    const MIN_STATS_DURATION_SECS: f64 = 3.0;  // Send stats for at least 3 seconds
                                    
                                    if elapsed_secs < MIN_STATS_DURATION_SECS {
                                        let remaining_secs = MIN_STATS_DURATION_SECS - elapsed_secs;
                                        let remaining_intervals = (remaining_secs.ceil() as u64).max(1);
                                        
                                        info!("Stats writer: Workload completed in {:.2}s, continuing stats for {} more intervals", 
                                              elapsed_secs, remaining_intervals);
                                        
                                        // Send RUNNING stats with final values for remaining intervals
                                        for i in 0..remaining_intervals {
                                            sequence += 1;
                                            let running_msg = LiveStats {
                                                agent_id: agent_id.clone(),
                                                timestamp_s: snapshot.timestamp_secs() as f64,
                                                get_ops: snapshot.get_ops,
                                                get_bytes: snapshot.get_bytes,
                                                get_mean_us: snapshot.get_mean_us as f64,
                                                get_p50_us: snapshot.get_p50_us as f64,
                                                get_p95_us: snapshot.get_p95_us as f64,
                                                put_ops: snapshot.put_ops,
                                                put_bytes: snapshot.put_bytes,
                                                put_mean_us: snapshot.put_mean_us as f64,
                                                put_p50_us: snapshot.put_p50_us as f64,
                                                put_p95_us: snapshot.put_p95_us as f64,
                                                meta_ops: snapshot.meta_ops,
                                                meta_mean_us: snapshot.meta_mean_us as f64,
                                                elapsed_s: elapsed_secs + (i as f64 + 1.0),  // Increment elapsed time
                                                completed: false,  // Still sending updates
                                                final_summary: None,
                                                status: 2,  // RUNNING
                                                error_message: String::new(),
                                                in_prepare_phase: false,
                                                prepare_objects_created: 0,
                                                prepare_objects_total: 0,
                                                prepare_summary: prepare_proto.clone(),
                                                cpu_user_percent: cpu_util.user_percent,
                                                cpu_system_percent: cpu_util.system_percent,
                                                cpu_iowait_percent: cpu_util.iowait_percent,
                                                cpu_total_percent: cpu_util.total_percent,
                                                agent_timestamp_ns: 0,
                                                sequence,
                                            };
                                            
                                            if tx_stats.send(running_msg).await.is_err() {
                                                error!("Stats writer: Failed to send running stats (interval {})", i);
                                                break;
                                            }
                                            
                                            // Wait 1 second before next update (except last iteration)
                                            if i < remaining_intervals - 1 {
                                                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                                            }
                                        }
                                    }
                                    
                                    // Now send COMPLETED message
                                    sequence += 1;
                                    let completed_msg = LiveStats {
                                        agent_id: agent_id.clone(),
                                        timestamp_s: snapshot.timestamp_secs() as f64,
                                        get_ops: snapshot.get_ops,
                                        get_bytes: snapshot.get_bytes,
                                        get_mean_us: snapshot.get_mean_us as f64,
                                        get_p50_us: snapshot.get_p50_us as f64,
                                        get_p95_us: snapshot.get_p95_us as f64,
                                        put_ops: snapshot.put_ops,
                                        put_bytes: snapshot.put_bytes,
                                        put_mean_us: snapshot.put_mean_us as f64,
                                        put_p50_us: snapshot.put_p50_us as f64,
                                        put_p95_us: snapshot.put_p95_us as f64,
                                        meta_ops: snapshot.meta_ops,
                                        meta_mean_us: snapshot.meta_mean_us as f64,
                                        elapsed_s: snapshot.elapsed_secs().max(MIN_STATS_DURATION_SECS),
                                        completed: true,
                                        final_summary: proto_summary,
                                        status: 4,  // COMPLETED
                                        error_message: String::new(),
                                        in_prepare_phase: false,
                                        prepare_objects_created: 0,
                                        prepare_objects_total: 0,
                                        prepare_summary: prepare_proto.clone(),
                                        cpu_user_percent: cpu_util.user_percent,
                                        cpu_system_percent: cpu_util.system_percent,
                                        cpu_iowait_percent: cpu_util.iowait_percent,
                                        cpu_total_percent: cpu_util.total_percent,
                                        agent_timestamp_ns: 0,
                                        sequence,
                                    };
                                    
                                    if tx_stats.send(completed_msg).await.is_err() {
                                        error!("Stats writer: Failed to send COMPLETED message");
                                    }
                                    
                                    // Transition to Idle
                                    let _ = agent_state.transition_to(WorkloadState::Idle, "workload completed").await;
                                    
                                    break;
                                }
                                Some(Err(e)) => {
                                    error!("Stats writer: Workload failed: {}", e);
                                    
                                    // Send ERROR message
                                    let error_msg = LiveStats {
                                        agent_id: agent_id.clone(),
                                        timestamp_s: 0.0,
                                        get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p95_us: 0.0,
                                        put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p95_us: 0.0,
                                        meta_ops: 0, meta_mean_us: 0.0,
                                        elapsed_s: 0.0,
                                        completed: false,
                                        final_summary: None,
                                        status: 3,  // ERROR
                                        error_message: e.clone(),
                                        in_prepare_phase: false,
                                        prepare_objects_created: 0,
                                        prepare_objects_total: 0,
                                        prepare_summary: None,
                                        cpu_user_percent: 0.0, cpu_system_percent: 0.0, cpu_iowait_percent: 0.0, cpu_total_percent: 0.0,
                                        agent_timestamp_ns: 0,
                                        sequence,
                                    };
                                    
                                    if tx_stats.send(error_msg).await.is_err() {
                                        error!("Stats writer: Failed to send ERROR message");
                                    }
                                    
                                    // Transition to Idle
                                    let _ = agent_state.transition_to(WorkloadState::Failed, &e).await;
                                    let _ = agent_state.transition_to(WorkloadState::Idle, "error sent").await;
                                    
                                    break;
                                }
                                None => {
                                    error!("Stats writer: Workload task terminated unexpectedly");
                                    break;
                                }
                            }
                        }
                        
                        // Send periodic RUNNING stats
                        _ = interval.tick() => {
                            let snapshot = tracker.snapshot();
                            let cpu_util = cpu_monitor.sample()
                                .ok()
                                .flatten()
                                .unwrap_or(CpuUtilization {
                                    user_percent: 0.0,
                                    system_percent: 0.0,
                                    iowait_percent: 0.0,
                                    total_percent: 0.0,
                                });
                            
                            let running_msg = LiveStats {
                                agent_id: agent_id.clone(),
                                timestamp_s: snapshot.timestamp_secs() as f64,
                                get_ops: snapshot.get_ops,
                                get_bytes: snapshot.get_bytes,
                                get_mean_us: snapshot.get_mean_us as f64,
                                get_p50_us: snapshot.get_p50_us as f64,
                                get_p95_us: snapshot.get_p95_us as f64,
                                put_ops: snapshot.put_ops,
                                put_bytes: snapshot.put_bytes,
                                put_mean_us: snapshot.put_mean_us as f64,
                                put_p50_us: snapshot.put_p50_us as f64,
                                put_p95_us: snapshot.put_p95_us as f64,
                                meta_ops: snapshot.meta_ops,
                                meta_mean_us: snapshot.meta_mean_us as f64,
                                elapsed_s: snapshot.elapsed_secs(),
                                completed: false,
                                final_summary: None,
                                status: 2,  // RUNNING
                                error_message: String::new(),
                                in_prepare_phase: snapshot.in_prepare_phase,
                                prepare_objects_created: snapshot.prepare_objects_created,
                                prepare_objects_total: snapshot.prepare_objects_total,
                                prepare_summary: None,
                                cpu_user_percent: cpu_util.user_percent,
                                cpu_system_percent: cpu_util.system_percent,
                                cpu_iowait_percent: cpu_util.iowait_percent,
                                cpu_total_percent: cpu_util.total_percent,
                                agent_timestamp_ns: 0,
                                sequence,
                            };
                            sequence += 1;
                            
                            if tx_stats.send(running_msg).await.is_err() {
                                error!("Stats writer: Failed to send RUNNING message (controller disconnected?)");
                                break;
                            }
                        }
                    }
                }
                
                info!("Stats writer task exiting");
            });
        }
        
        // Control reader task (this function continues)
        // Process control messages from controller
        let agent_state_reader = agent_state.clone();
        let agent_op_log_path_reader = agent_op_log_path.clone();
        let tx_stats_for_control = tx_stats.clone();
        
        // Spawn control message processor with timeout enforcement
        tokio::spawn(async move {
            // Import timeout configuration from constants module
            use sai3_bench::constants::{
                AGENT_IDLE_TIMEOUT_SECS,
                AGENT_READY_TIMEOUT_SECS,
                TIMEOUT_MONITOR_INTERVAL_SECS,
            };
            
            let mut last_message_time = tokio::time::Instant::now();
            let mut timeout_monitor = tokio::time::interval(
                tokio::time::Duration::from_secs(TIMEOUT_MONITOR_INTERVAL_SECS)
            );
            timeout_monitor.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            
            loop {
                tokio::select! {
                    // Receive control messages from controller
                    control_msg_result = control_stream.next() => {
                        match control_msg_result {
                            Some(Ok(control_msg)) => {
                                last_message_time = tokio::time::Instant::now();
                                
                                match Command::try_from(control_msg.command) {
                                    Ok(Command::Ping) => {
                                        debug!("Control reader: Received PING keepalive");
                                        
                                        // Respond with ACKNOWLEDGE message
                                        let agent_id = agent_state_reader.get_agent_id().await.unwrap_or_else(|| "unknown".to_string());
                                        let ack_msg = LiveStats {
                                            agent_id: agent_id.clone(),
                                            timestamp_s: 0.0,
                                            get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p95_us: 0.0,
                                            put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p95_us: 0.0,
                                            meta_ops: 0, meta_mean_us: 0.0,
                                            elapsed_s: 0.0,
                                            completed: false,
                                            final_summary: None,
                                            status: 6,  // ACKNOWLEDGE (new status code for PING response)
                                            error_message: String::new(),
                                            in_prepare_phase: false,
                                            prepare_objects_created: 0,
                                            prepare_objects_total: 0,
                                            prepare_summary: None,
                                            cpu_user_percent: 0.0,
                                            cpu_system_percent: 0.0,
                                            cpu_iowait_percent: 0.0,
                                            cpu_total_percent: 0.0,
                                            agent_timestamp_ns: 0,
                                            sequence: 0,
                                        };
                                        
                                        if let Err(e) = tx_stats_for_control.send(ack_msg).await {
                                            error!("Control reader: Failed to send ACKNOWLEDGE: {}", e);
                                            break;
                                        }
                                        
                                        debug!("Control reader: Sent ACKNOWLEDGE response to PING");
                                    }
                            Ok(Command::Start) => {
                                let start_timestamp_ns = control_msg.start_timestamp_ns;
                                
                                // Determine if this is initial START (validation) or coordinated START (execution)
                                let current_state = agent_state_reader.get_state().await;
                                
                                if current_state == WorkloadState::Idle {
                                    // First START: Validate config and send READY
                                    info!("Control reader: Received initial START command - validating config");
                                    
                                    // Extract config from START message
                                    let config_yaml = control_msg.config_yaml;
                                    let agent_id = control_msg.agent_id;
                                    let path_prefix = control_msg.path_prefix;
                                    let shared_storage = control_msg.shared_storage;
                                    let agent_index = control_msg.agent_index;
                                    let num_agents = control_msg.num_agents;
                                    
                                    // Store agent_id in state for stats writer
                                    agent_state_reader.set_agent_id(agent_id.clone()).await;
                                    
                                    // Store agent_index and num_agents for distributed cleanup
                                    agent_state_reader.set_agent_index(agent_index).await;
                                    agent_state_reader.set_num_agents(num_agents).await;
                                    
                                    // Store config_yaml for later use
                                    agent_state_reader.set_config_yaml(config_yaml.clone()).await;
                                    
                                    // Parse and validate config
                                    let mut config: sai3_bench::config::Config = match serde_yaml::from_str(&config_yaml) {
                                        Ok(c) => c,
                                        Err(e) => {
                                            error!("Control reader: Invalid YAML config: {}", e);
                                            let _ = agent_state_reader.transition_to(WorkloadState::Failed, "invalid config").await;
                                            agent_state_reader.set_error(format!("Invalid YAML: {}", e)).await;
                                            
                                            // Send ERROR via stats writer (it will pick up the error from state)
                                            let _ = tx_done.send(Err(format!("Invalid YAML config: {}", e))).await;
                                            return;
                                        }
                                    };
                                    
                                    if let Err(e) = config.apply_agent_prefix(&agent_id, &path_prefix, shared_storage) {
                                        error!("Control reader: Failed to apply agent prefix: {}", e);
                                        let _ = agent_state_reader.transition_to(WorkloadState::Failed, "prefix application failed").await;
                                        let _ = tx_done.send(Err(format!("Failed to apply path prefix: {}", e))).await;
                                        return;
                                    }
                                    
                                    // Validate config
                                    if let Err(e) = validate_workload_config(&config).await {
                                        error!("Control reader: Config validation failed: {}", e);
                                        let _ = agent_state_reader.transition_to(WorkloadState::Failed, "validation failed").await;
                                        let _ = tx_done.send(Err(format!("Config validation failed: {}", e))).await;
                                        return;
                                    }
                                    
                                    // Transition to Ready (validation passed)
                                    if let Err(e) = agent_state_reader.transition_to(WorkloadState::Ready, "validation passed").await {
                                        error!("Control reader: Failed to transition to Ready: {}", e);
                                        let _ = tx_done.send(Err(format!("State transition failed: {}", e))).await;
                                        return;
                                    }
                                    
                                    info!("Control reader: Config validated, agent ready - waiting for coordinated START");
                                    
                                    // Stats writer will send READY message with agent_timestamp_ns
                                    // Controller will calculate clock offset and send second START with start_timestamp_ns
                                    
                                } else if current_state == WorkloadState::Ready && start_timestamp_ns != 0 {
                                    // Second START: Coordinated start with timestamp
                                    info!("Control reader: Received coordinated START (timestamp: {} ns)", start_timestamp_ns);
                                    
                                    // Calculate wait duration until coordinated start time
                                    // Controller sends absolute Unix epoch timestamp (controller_now + delay)
                                    // Agent waits until local clock reaches that timestamp
                                    // NO clock offset adjustment needed - Unix timestamps are universal
                                    let now_ns = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap()
                                        .as_nanos() as i64;
                                    
                                    let wait_ns = start_timestamp_ns - now_ns;
                                    
                                    if wait_ns > 0 {
                                        let wait_duration = std::time::Duration::from_nanos(wait_ns as u64);
                                        let wait_ms = wait_duration.as_millis();
                                        info!("Control reader: Waiting {}ms until coordinated start", wait_ms);
                                        tokio::time::sleep(wait_duration).await;
                                        info!("Control reader: Coordinated start time reached - spawning workload");
                                    } else {
                                        warn!("Control reader: Start timestamp is in the past by {}ms - starting immediately", 
                                              (-wait_ns) / 1_000_000);
                                    }
                                    
                                    // Transition to Running
                                    if let Err(e) = agent_state_reader.transition_to(WorkloadState::Running, "coordinated start").await {
                                        error!("Control reader: Failed to transition to Running: {}", e);
                                        let _ = tx_done.send(Err(format!("State transition failed: {}", e))).await;
                                        return;
                                    }
                                    
                                    // Retrieve stored config from Phase 1 validation
                                    let config_yaml = match agent_state_reader.get_config_yaml().await {
                                        Some(yaml) => yaml,
                                        None => {
                                            error!("Control reader: No config_yaml found in state");
                                            let _ = tx_done.send(Err("Missing config_yaml".to_string())).await;
                                            return;
                                        }
                                    };
                                    
                                    // Parse config (already validated in Phase 1, should not fail)
                                    let config: sai3_bench::config::Config = match serde_yaml::from_str(&config_yaml) {
                                        Ok(c) => c,
                                        Err(e) => {
                                            error!("Control reader: Failed to parse stored config: {}", e);
                                            let _ = tx_done.send(Err(format!("Config parse error: {}", e))).await;
                                            return;
                                        }
                                    };
                                    
                                    // Get agent_id from state
                                    let agent_id = agent_state_reader.get_agent_id().await.unwrap_or_else(|| "unknown".to_string());
                                    
                                    // Create live stats tracker
                                    let tracker = Arc::new(sai3_bench::live_stats::LiveStatsTracker::new());
                                    
                                    // Store tracker in state for stats writer
                                    agent_state_reader.set_tracker(tracker.clone()).await;
                                    
                                    // Setup op_log_path
                                    let final_op_log_path = config.op_log_path.as_ref()
                                        .or(agent_op_log_path_reader.as_ref())
                                        .cloned();
                                    
                                    agent_state_reader.set_op_log_path(
                                        final_op_log_path.as_ref().map(|p| p.display().to_string())
                                    ).await;
                                    
                                    // Spawn workload execution task
                                    let mut config_exec = config.clone();
                                    let agent_id_exec = agent_id.clone();
                                    let tx_done_exec = tx_done.clone();
                                    let tx_prepare_exec = tx_prepare.clone();
                                    let agent_op_log_path_exec = agent_op_log_path_reader.clone();
                                    let tracker_for_prepare = tracker.clone();
                                    let agent_state_for_task = agent_state_reader.clone();
                                    
                                    tokio::spawn(async move {
                                        // Subscribe to abort signals for this workload
                                        let mut abort_rx_task = agent_state_for_task.subscribe_abort();
                                        // Wire tracker into config for live stats collection
                                        config_exec.live_stats_tracker = Some(tracker_for_prepare.clone());
                                        
                                        // Get agent_index and num_agents for distributed prepare/cleanup
                                        let agent_index = agent_state_for_task.get_agent_index().await.unwrap_or(0) as usize;
                                        let num_agents = agent_state_for_task.get_num_agents().await.unwrap_or(1) as usize;
                                        
                                        // v0.8.7: Detect cleanup-only mode (ONLY checks cleanup flag)
                                        let is_cleanup_only = config_exec.prepare.as_ref()
                                            .map(|p| p.cleanup_only.unwrap_or(false))
                                            .unwrap_or(false);
                                        
                                        // Execute prepare phase or generate cleanup list
                                        let (prepared_objects, tree_manifest) = if is_cleanup_only {
                                            // Cleanup-only mode
                                            if let Some(ref prepare_config) = config_exec.prepare {
                                                if prepare_config.skip_verification {
                                                    info!("Cleanup-only mode (skip_verification=true): Generating object list from config");
                                                    match sai3_bench::workload::generate_cleanup_objects(prepare_config, agent_index, num_agents) {
                                                        Ok(objects) => {
                                                            info!("Generated {} objects for cleanup", objects.len());
                                                            (objects, None)
                                                        }
                                                        Err(e) => {
                                                            let _ = tx_done_exec.send(Err(format!("Failed to generate cleanup objects: {}", e))).await;
                                                            return;
                                                        }
                                                    }
                                                } else {
                                                    info!("Cleanup-only mode (skip_verification=false): Listing existing objects");
                                                    match sai3_bench::workload::prepare_objects(prepare_config, Some(&config_exec.workload), Some(tracker_for_prepare.clone()), config_exec.concurrency, agent_index, num_agents).await {
                                                        Ok((prepared, manifest, _)) => (prepared, manifest),
                                                        Err(e) => {
                                                            let _ = tx_done_exec.send(Err(format!("Prepare phase failed: {}", e))).await;
                                                            return;
                                                        }
                                                    }
                                                }
                                            } else {
                                                (Vec::new(), None)
                                            }
                                        } else if let Some(ref prepare_config) = config_exec.prepare {
                                            match sai3_bench::workload::prepare_objects(prepare_config, Some(&config_exec.workload), Some(tracker_for_prepare.clone()), config_exec.concurrency, agent_index, num_agents).await {
                                                Ok((prepared, manifest, prepare_metrics)) => {
                                                    info!("Prepared {} objects for agent {}", prepared.len(), agent_id_exec);
                                                    
                                                    // Send prepare metrics to stats writer
                                                    let _ = tx_prepare_exec.send(prepare_metrics).await;
                                                    
                                                    // Reset stats counters before workload
                                                    tracker_for_prepare.reset_for_workload();
                                                    
                                                    if prepared.iter().any(|p| p.created) && prepare_config.post_prepare_delay > 0 {
                                                        tokio::time::sleep(tokio::time::Duration::from_secs(prepare_config.post_prepare_delay)).await;
                                                    }
                                                    (prepared, manifest)  // Store prepared objects for cleanup
                                                }
                                                Err(e) => {
                                                    let _ = tx_done_exec.send(Err(format!("Prepare phase failed: {}", e))).await;
                                                    return;
                                                }
                                            }
                                        } else {
                                            (Vec::new(), None)
                                        };
                                        
                                        // Setup operation logger if configured
                                        if let Some(op_log_base) = final_op_log_path.as_ref().or(agent_op_log_path_exec.as_ref()) {
                                            let op_log_path = if let Some(parent) = op_log_base.parent() {
                                                let filename = op_log_base.file_name()
                                                    .and_then(|f| f.to_str())
                                                    .unwrap_or("oplog.tsv.zst");
                                                let base_name = if filename.ends_with(".tsv.zst") {
                                                    filename.strip_suffix(".tsv.zst").unwrap()
                                                } else if filename.ends_with(".tsv") {
                                                    filename.strip_suffix(".tsv").unwrap()
                                                } else {
                                                    filename
                                                };
                                                let extension = if filename.ends_with(".tsv.zst") {
                                                    ".tsv.zst"
                                                } else if filename.ends_with(".tsv") {
                                                    ".tsv"
                                                } else {
                                                    ""
                                                };
                                                parent.join(format!("{}-{}{}", base_name, agent_id_exec, extension))
                                            } else {
                                                let filename = op_log_base.file_name()
                                                    .and_then(|f| f.to_str())
                                                    .unwrap_or("oplog.tsv.zst");
                                                std::path::PathBuf::from(format!("{}-{}", filename, agent_id_exec))
                                            };
                                            
                                            info!("Initializing s3dlio operation logger: {}", op_log_path.display());
                                            if let Err(e) = sai3_bench::workload::init_operation_logger(&op_log_path) {
                                                error!("Failed to initialize operation logger: {}", e);
                                                let _ = tx_done_exec.send(Err(format!("Failed to initialize oplog: {}", e))).await;
                                                return;
                                            }
                                            
                                            // v0.8.6: Set client_id for this agent
                                            if let Err(e) = s3dlio::set_client_id(&agent_id_exec) {
                                                warn!("Failed to set client_id for oplog: {} (continuing anyway)", e);
                                            } else {
                                                info!("Set operation logger client_id: {}", agent_id_exec);
                                            }
                                            
                                            // v0.8.6: Set clock offset for synchronized timestamps across agents
                                            // Controller sends start_timestamp_ns as absolute Unix epoch time
                                            // Calculate offset: agent_local_time - controller_reference_time
                                            // This aligns all agent timestamps to controller's reference clock
                                            let agent_local_time_ns = std::time::SystemTime::now()
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap()
                                                .as_nanos() as i64;
                                            
                                            // Clock offset = (agent_time - controller_time)
                                            // s3dlio will subtract this offset from all logged timestamps
                                            // Result: all agents log timestamps relative to controller's reference time
                                            let clock_offset_ns = agent_local_time_ns - start_timestamp_ns;
                                            
                                            if let Err(e) = s3dlio::set_clock_offset(clock_offset_ns) {
                                                warn!("Failed to set clock offset for oplog: {} (continuing anyway)", e);
                                            } else {
                                                info!("Set clock offset: {} ms (agent {} ms ahead of controller)", 
                                                      clock_offset_ns / 1_000_000, 
                                                      clock_offset_ns / 1_000_000);
                                            }
                                        }
                                        
                                        // Execute workload OR cleanup (cleanup-only mode runs cleanup as the workload)
                                        let result = if is_cleanup_only {
                                            info!("Cleanup-only mode: Running cleanup as workload");
                                            
                                            // Get tracker for stats reporting
                                            let tracker = agent_state_for_task.get_tracker().await;
                                            let start_time = std::time::Instant::now();
                                            
                                            // Run cleanup with stats tracking
                                            let agent_index = agent_state_for_task.get_agent_index().await.unwrap_or(0);
                                            let num_agents = agent_state_for_task.get_num_agents().await.unwrap_or(1);
                                            let cleanup_mode = config_exec.prepare.as_ref()
                                                .map(|p| p.cleanup_mode)
                                                .unwrap_or(sai3_bench::config::CleanupMode::Tolerant);
                                            
                                            match sai3_bench::workload::cleanup_prepared_objects(
                                                &prepared_objects,
                                                tree_manifest.as_ref(),
                                                agent_index as usize,
                                                num_agents as usize,
                                                cleanup_mode,
                                                tracker.clone(),
                                            ).await {
                                                Ok(_) => {
                                                    info!("Cleanup completed successfully");
                                                    
                                                    // Generate summary from tracker stats
                                                    if let Some(t) = tracker {
                                                        let snapshot = t.snapshot();
                                                        let elapsed = start_time.elapsed().as_secs_f64();
                                                        
                                                        Ok(sai3_bench::workload::Summary {
                                                            wall_seconds: elapsed,
                                                            total_ops: snapshot.meta_ops,
                                                            total_bytes: 0,  // DELETE operations don't transfer bytes
                                                            p50_us: 0,  // Only have mean for meta
                                                            p95_us: 0,
                                                            p99_us: 0,
                                                            get: Default::default(),
                                                            put: Default::default(),
                                                            meta: sai3_bench::workload::OpAgg {
                                                                ops: snapshot.meta_ops,
                                                                bytes: 0,
                                                                mean_us: snapshot.meta_mean_us,
                                                                p50_us: 0,
                                                                p95_us: 0,
                                                                p99_us: 0,
                                                            },
                                                            get_bins: Default::default(),
                                                            put_bins: Default::default(),
                                                            meta_bins: Default::default(),
                                                            get_hists: Default::default(),
                                                            put_hists: Default::default(),
                                                            meta_hists: Default::default(),  // Would need histogram export method
                                                            total_errors: 0,
                                                            error_rate: 0.0,
                                                        })
                                                    } else {
                                                        // No tracker - return empty summary
                                                        Ok(sai3_bench::workload::Summary {
                                                            wall_seconds: start_time.elapsed().as_secs_f64(),
                                                            total_ops: 0,
                                                            total_bytes: 0,
                                                            p50_us: 0,
                                                            p95_us: 0,
                                                            p99_us: 0,
                                                            get: Default::default(),
                                                            put: Default::default(),
                                                            meta: Default::default(),
                                                            get_bins: Default::default(),
                                                            put_bins: Default::default(),
                                                            meta_bins: Default::default(),
                                                            get_hists: Default::default(),
                                                            put_hists: Default::default(),
                                                            meta_hists: Default::default(),
                                                            total_errors: 0,
                                                            error_rate: 0.0,
                                                        })
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("Cleanup failed: {}", e);
                                                    Err(e)
                                                }
                                            }
                                        } else {
                                            sai3_bench::workload::run(&config_exec, tree_manifest.clone()).await
                                        };
                                        
                                        // Finalize oplog
                                        if final_op_log_path.is_some() || agent_op_log_path_exec.is_some() {
                                            info!("Finalizing s3dlio operation logger");
                                            if let Err(e) = sai3_bench::workload::finalize_operation_logger() {
                                                error!("Failed to finalize operation logger: {}", e);
                                            }
                                        }
                                        
                                        tokio::select! {
                                            _ = abort_rx_task.recv() => {
                                                warn!("Workload task aborted");
                                                // Finalize oplog
                                                if final_op_log_path.is_some() || agent_op_log_path_exec.is_some() {
                                                    if let Err(e) = sai3_bench::workload::finalize_operation_logger() {
                                                        error!("Failed to finalize operation logger: {}", e);
                                                    }
                                                }
                                                let _ = tx_done_exec.send(Err("Aborted".to_string())).await;
                                            }
                                            _ = async {
                                                match result {
                                                    Ok(summary) => {
                                                        info!("Workload completed successfully for agent {}", agent_id_exec);
                                                        
                                                        // v0.8.7: Execute cleanup phase if configured (skip in cleanup-only mode)
                                                        if !is_cleanup_only {
                                                            if let Some(ref prepare_config) = config_exec.prepare {
                                                                if prepare_config.cleanup {
                                                                    let agent_index = agent_state_for_task.get_agent_index().await.unwrap_or(0);
                                                                    let num_agents = agent_state_for_task.get_num_agents().await.unwrap_or(1);
                                                                    let cleanup_mode = prepare_config.cleanup_mode;
                                                                    
                                                                    info!("Starting cleanup phase (agent {}/{}, mode: {:?})", 
                                                                          agent_index, num_agents, cleanup_mode);
                                                                    
                                                                    // Get tracker for live stats reporting during cleanup
                                                                    let tracker = agent_state_for_task.get_tracker().await;
                                                                    
                                                                    if let Err(e) = sai3_bench::workload::cleanup_prepared_objects(
                                                                        &prepared_objects,
                                                                        tree_manifest.as_ref(),
                                                                        agent_index as usize,
                                                                        num_agents as usize,
                                                                        cleanup_mode,
                                                                        tracker,
                                                                    ).await {
                                                                        error!("Cleanup phase failed for agent {}: {}", agent_id_exec, e);
                                                                    } else {
                                                                        info!("Cleanup phase completed for agent {}", agent_id_exec);
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        
                                                        let _ = tx_done_exec.send(Ok(summary)).await;
                                                    }
                                                    Err(e) => {
                                                        error!("Workload execution failed for agent {}: {}", agent_id_exec, e);
                                                        let _ = tx_done_exec.send(Err(e.to_string())).await;
                                                    }
                                                }
                                            } => {}
                                        }
                                    });
                                    
                                    info!("Control reader: Workload task spawned after coordinated start");
                                    
                                } else {
                                    // Unexpected state or duplicate START
                                    error!("Control reader: Rejecting START - agent in {:?} state (expected Idle or Ready)", current_state);
                                    let _ = tx_done.send(Err(format!("Agent in unexpected state: {:?}", current_state))).await;
                                    return;
                                }
                            }
                            Ok(Command::Abort) => {
                                warn!("Control reader: Received ABORT command");
                                
                                let current_state = agent_state_reader.get_state().await;
                                info!("Control reader: Current state is {:?}, initiating abort", current_state);
                                
                                // Send abort signal (workload task listening via subscribe_abort())
                                agent_state_reader.send_abort();
                                
                                // Transition to Aborting
                                if let Err(e) = agent_state_reader.transition_to(WorkloadState::Aborting, "ABORT command").await {
                                    error!("Control reader: Failed to transition to Aborting: {}", e);
                                }
                                
                                // Send ABORTED status immediately
                                let agent_id = agent_state_reader.get_agent_id().await.unwrap_or_else(|| "unknown".to_string());
                                let aborted_msg = LiveStats {
                                    agent_id: agent_id.clone(),
                                    timestamp_s: 0.0,
                                    get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p95_us: 0.0,
                                    put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p95_us: 0.0,
                                    meta_ops: 0, meta_mean_us: 0.0,
                                    elapsed_s: 0.0,
                                    completed: false,
                                    final_summary: None,
                                    status: 5,  // ABORTED
                                    error_message: "Aborted by controller".to_string(),
                                    in_prepare_phase: false,
                                    prepare_objects_created: 0,
                                    prepare_objects_total: 0,
                                    prepare_summary: None,
                                    cpu_user_percent: 0.0,
                                    cpu_system_percent: 0.0,
                                    cpu_iowait_percent: 0.0,
                                    cpu_total_percent: 0.0,
                                    agent_timestamp_ns: 0,
                                    sequence: 0,
                                };
                                
                                if let Err(e) = tx_stats_for_control.send(aborted_msg).await {
                                    error!("Control reader: Failed to send ABORTED status: {}", e);
                                }
                                
                                // Wait briefly for workload cleanup
                                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                                
                                // Reset to Idle
                                if let Err(e) = agent_state_reader.transition_to(WorkloadState::Idle, "abort complete").await {
                                    error!("Control reader: Failed to transition to Idle after abort: {}", e);
                                }
                                
                                info!("Control reader: Abort complete, agent returned to Idle");
                            }
                            Ok(Command::Acknowledge) => {
                                debug!("Control reader: Received ACK for sequence {}", control_msg.ack_sequence);
                                // Controller acknowledged our message - could use this for reliability
                            }
                            Err(e) => {
                                warn!("Control reader: Unknown command: {}", e);
                            }
                        }
                    }
                    Some(Err(e)) => {
                        // Check if workload is already complete - if so, this is normal disconnect
                        let current_state = agent_state_reader.get_state().await;
                        if current_state == WorkloadState::Idle {
                            info!("Control reader: Stream error after workload completion (normal): {}", e);
                        } else {
                            error!("Control reader: Error receiving control message: {}", e);
                            
                            // Connection lost during active workload - send error and exit
                            let agent_id = agent_state_reader.get_agent_id().await.unwrap_or_else(|| "unknown".to_string());
                            let error_msg = LiveStats {
                                agent_id,
                                timestamp_s: 0.0,
                                get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p95_us: 0.0,
                                put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p95_us: 0.0,
                                meta_ops: 0, meta_mean_us: 0.0,
                                elapsed_s: 0.0,
                                completed: false,
                                final_summary: None,
                                status: 3,  // ERROR
                                error_message: format!("Connection error: {}", e),
                                in_prepare_phase: false,
                                prepare_objects_created: 0,
                                prepare_objects_total: 0,
                                prepare_summary: None,
                                cpu_user_percent: 0.0,
                                cpu_system_percent: 0.0,
                                cpu_iowait_percent: 0.0,
                                cpu_total_percent: 0.0,
                                agent_timestamp_ns: 0,
                                sequence: 0,
                            };
                            
                            let _ = tx_stats_for_control.send(error_msg).await;
                        }
                        break;
                    }
                    None => {
                        info!("Control reader: Stream ended (controller disconnected gracefully)");
                        break;
                    }
                }
            }
            
            // Timeout monitoring - check for hung states
            _ = timeout_monitor.tick() => {
                let current_state = agent_state_reader.get_state().await;
                let elapsed = last_message_time.elapsed();
                
                let timeout_exceeded = match current_state {
                    WorkloadState::Idle => elapsed.as_secs() > AGENT_IDLE_TIMEOUT_SECS,
                    WorkloadState::Ready => elapsed.as_secs() > AGENT_READY_TIMEOUT_SECS,
                    _ => false,  // Only monitor IDLE and READY states
                };
                
                if timeout_exceeded {
                    error!(
                        "Control reader: Timeout in {:?} state ({}s since last message, limit {}s)",
                        current_state,
                        elapsed.as_secs(),
                        match current_state {
                            WorkloadState::Idle => AGENT_IDLE_TIMEOUT_SECS,
                            WorkloadState::Ready => AGENT_READY_TIMEOUT_SECS,
                            _ => 0,
                        }
                    );
                    
                    // Send ERROR status
                    let agent_id = agent_state_reader.get_agent_id().await.unwrap_or_else(|| "unknown".to_string());
                    let timeout_msg = LiveStats {
                        agent_id,
                        timestamp_s: 0.0,
                        get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p95_us: 0.0,
                        put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p95_us: 0.0,
                        meta_ops: 0, meta_mean_us: 0.0,
                        elapsed_s: 0.0,
                        completed: false,
                        final_summary: None,
                        status: 3,  // ERROR
                        error_message: format!("Timeout in {:?} state after {}s", current_state, elapsed.as_secs()),
                        in_prepare_phase: false,
                        prepare_objects_created: 0,
                        prepare_objects_total: 0,
                        prepare_summary: None,
                        cpu_user_percent: 0.0,
                        cpu_system_percent: 0.0,
                        cpu_iowait_percent: 0.0,
                        cpu_total_percent: 0.0,
                        agent_timestamp_ns: 0,
                        sequence: 0,
                    };
                    
                    let _ = tx_stats_for_control.send(timeout_msg).await;
                    
                    // Transition to Failed
                    let _ = agent_state_reader.transition_to(WorkloadState::Failed, "timeout").await;
                    
                    break;
                }
            }
        }
    }
            
            info!("Control reader: Stream ended (controller disconnected)");
            
            // Check if workload completed successfully vs abnormal disconnect
            let current_state = agent_state_reader.get_state().await;
            match current_state {
                WorkloadState::Idle => {
                    // Workload already completed - this is normal controller disconnect after receiving results
                    info!("Control reader: Normal disconnect (workload completed, agent idle)");
                }
                WorkloadState::Ready | WorkloadState::Running | WorkloadState::Aborting => {
                    // Abnormal disconnect during workload - send abort and transition to Failed
                    warn!("Control reader: Abnormal disconnect during {:?} state", current_state);
                    agent_state_reader.send_abort();
                    let _ = agent_state_reader.transition_to(WorkloadState::Failed, "controller disconnected during workload").await;
                    let _ = agent_state_reader.transition_to(WorkloadState::Idle, "cleanup after abnormal disconnect").await;
                }
                WorkloadState::Failed => {
                    // Already failed - just log
                    info!("Control reader: Disconnect after failure (expected)");
                }
            }
        });
        
        // Convert stats channel to stream, wrapping each LiveStats in Ok()
        let output_stream = tokio_stream::wrappers::ReceiverStream::new(rx_stats)
            .map(|stats| Ok(stats));
        
        Ok(Response::new(Box::pin(output_stream)))
    }
}

/// Convert Summary to WorkloadSummary protobuf (v0.7.5)
/// Helper to avoid duplication between blocking and streaming RPCs
async fn summary_to_proto(
    agent_id: &str,
    config_yaml: &str,
    summary: &sai3_bench::workload::Summary,
    op_log_path: Option<String>,
) -> Result<WorkloadSummary, Status> {
    // Create results directory and capture output
    let (metadata_json, tsv_content, results_path, _op_log_path, 
         histogram_get, histogram_put, histogram_meta) = 
        create_agent_results(agent_id, config_yaml, summary, op_log_path.clone())
            .await
            .map_err(|e| {
                error!("Failed to create agent results: {}", e);
                Status::internal(format!("Failed to create agent results: {}", e))
            })?;

    Ok(WorkloadSummary {
        agent_id: agent_id.to_string(),
        wall_seconds: summary.wall_seconds,
        total_ops: summary.total_ops,
        total_bytes: summary.total_bytes,
        console_log: String::new(),
        metadata_json,
        tsv_content,
        results_path,
        op_log_path: op_log_path.unwrap_or_default(),
        histogram_get,
        histogram_put,
        histogram_meta,
        get: Some(OpAggregateMetrics {
            bytes: summary.get.bytes,
            ops: summary.get.ops,
            mean_us: summary.get.mean_us,
            p50_us: summary.get.p50_us,
            p95_us: summary.get.p95_us,
            p99_us: summary.get.p99_us,
        }),
        put: Some(OpAggregateMetrics {
            bytes: summary.put.bytes,
            ops: summary.put.ops,
            mean_us: summary.put.mean_us,
            p50_us: summary.put.p50_us,
            p95_us: summary.put.p95_us,
            p99_us: summary.put.p99_us,
        }),
        meta: Some(OpAggregateMetrics {
            bytes: summary.meta.bytes,
            ops: summary.meta.ops,
            mean_us: summary.meta.mean_us,
            p50_us: summary.meta.p50_us,
            p95_us: summary.meta.p95_us,
            p99_us: summary.meta.p99_us,
        }),
        p50_us: summary.p50_us,
        p95_us: summary.p95_us,
        p99_us: summary.p99_us,
    })
}

/// Convert PrepareMetrics to PrepareSummary protobuf (v0.7.9)
/// Follows same pattern as summary_to_proto for consistency
async fn prepare_metrics_to_proto(
    agent_id: &str,
    metrics: &sai3_bench::workload::PrepareMetrics,
) -> Result<PrepareSummary, Status> {
    use sai3_bench::tsv_export::TsvExporter;
    use hdrhistogram::serialization::{Serializer, V2Serializer};
    use std::fs;
    
    // Get PID for unique naming (prevents collisions when multiple agents run on same host)
    let pid = std::process::id();
    
    // Create prepare results directory structure similar to workload results
    let agent_name = format!("{}-pid{}-prepare", agent_id, pid);
    let results_base = std::env::temp_dir();
    let results_path = results_base.join(&agent_name);
    
    // Create directory
    fs::create_dir_all(&results_path)
        .map_err(|e| Status::internal(format!("Failed to create prepare results directory: {}", e)))?;
    
    // Write TSV export to prepare_results.tsv
    let tsv_path = results_path.join("prepare_results.tsv");
    let exporter = TsvExporter::with_path(&tsv_path)
        .map_err(|e| Status::internal(format!("Failed to create TSV exporter: {}", e)))?;
    exporter.export_prepare_metrics(metrics)
        .map_err(|e| Status::internal(format!("Failed to export prepare metrics: {}", e)))?;
    
    // Read back TSV content for transmission
    let tsv_content = fs::read_to_string(&tsv_path)
        .map_err(|e| Status::internal(format!("Failed to read prepare TSV: {}", e)))?;
    
    // Serialize histograms for accurate aggregation (same pattern as workload)
    let mut serializer = V2Serializer::new();
    let mut put_hist_bytes = Vec::new();
    
    for bucket_hist in metrics.put_hists.buckets.iter() {
        let hist = bucket_hist.lock().unwrap();
        serializer.serialize(&*hist, &mut put_hist_bytes)
            .map_err(|e| Status::internal(format!("Failed to serialize PUT histogram: {}", e)))?;
    }
    
    Ok(PrepareSummary {
        agent_id: agent_id.to_string(),
        wall_seconds: metrics.wall_seconds,
        objects_created: metrics.objects_created,
        objects_existed: metrics.objects_existed,
        put: Some(OpAggregateMetrics {
            bytes: metrics.put.bytes,
            ops: metrics.put.ops,
            mean_us: metrics.put.mean_us,
            p50_us: metrics.put.p50_us,
            p95_us: metrics.put.p95_us,
            p99_us: metrics.put.p99_us,
        }),
        mkdir: Some(OpAggregateMetrics {
            bytes: metrics.mkdir.bytes,
            ops: metrics.mkdir.ops,
            mean_us: metrics.mkdir.mean_us,
            p50_us: metrics.mkdir.p50_us,
            p95_us: metrics.mkdir.p95_us,
            p99_us: metrics.mkdir.p99_us,
        }),
        mkdir_count: metrics.mkdir_count,
        histogram_put: put_hist_bytes,
        tsv_content,
        results_path: results_path.display().to_string(),
    })
}

/// Create agent results directory and capture output files (v0.6.4)
/// Returns: (metadata_json, tsv_content, results_path, op_log_path, histogram_bytes)
async fn create_agent_results(
    agent_id: &str,
    config_yaml: &str,
    summary: &sai3_bench::workload::Summary,
    op_log_path: Option<String>,
) -> anyhow::Result<(String, String, String, Option<String>, Vec<u8>, Vec<u8>, Vec<u8>)> {
    use sai3_bench::results_dir::ResultsDir;
    use sai3_bench::tsv_export::TsvExporter;
    use std::fs;
    use std::io::Write;
    use hdrhistogram::serialization::{Serializer, V2Serializer};
    
    // Get PID for unique naming (prevents collisions when multiple agents run on same host)
    let pid = std::process::id();
    
    // Write config to temp file so ResultsDir can copy it
    let temp_config_path = std::env::temp_dir().join(format!("agent-{}-{}-config.yaml", agent_id, pid));
    {
        let mut temp_file = fs::File::create(&temp_config_path)?;
        temp_file.write_all(config_yaml.as_bytes())?;
    }
    
    // Create results directory in /tmp for agent (includes PID for uniqueness)
    let agent_name = format!("{}-pid{}", agent_id, pid);
    let results_base = std::env::temp_dir();
    let mut results_dir = ResultsDir::create(
        &temp_config_path,
        Some(&agent_name),
        Some(&results_base),
    )?;
    
    // Clean up temp config file
    let _ = fs::remove_file(&temp_config_path);
    
    // Write TSV export
    let tsv_path = results_dir.tsv_path();
    let exporter = TsvExporter::with_path(&tsv_path)?;
    exporter.export_results(
        &summary.get_hists,
        &summary.put_hists,
        &summary.meta_hists,
        &summary.get_bins,
        &summary.put_bins,
        &summary.meta_bins,
        summary.wall_seconds,
    )?;
    
    // v0.8.2: Operation log path (if oplog was initialized, passed from caller)
    // Oplog files remain on agent's local filesystem (not transferred via gRPC)
    
    // Finalize results directory
    results_dir.finalize(summary.wall_seconds)?;
    
    // Read back the files we need to transfer
    let metadata_json = fs::read_to_string(results_dir.path().join("metadata.json"))?;
    let tsv_content = fs::read_to_string(&tsv_path)?;
    let results_path = results_dir.path().display().to_string();
    
    // Serialize histograms for accurate aggregation (v0.6.4)
    let mut serializer = V2Serializer::new();
    let mut get_hist_bytes = Vec::new();
    let mut put_hist_bytes = Vec::new();
    let mut meta_hist_bytes = Vec::new();
    
    // Serialize each histogram type
    for bucket_hist in summary.get_hists.buckets.iter() {
        let hist = bucket_hist.lock().unwrap();
        serializer.serialize(&*hist, &mut get_hist_bytes)
            .context("Failed to serialize GET histogram")?;
    }
    for bucket_hist in summary.put_hists.buckets.iter() {
        let hist = bucket_hist.lock().unwrap();
        serializer.serialize(&*hist, &mut put_hist_bytes)
            .context("Failed to serialize PUT histogram")?;
    }
    for bucket_hist in summary.meta_hists.buckets.iter() {
        let hist = bucket_hist.lock().unwrap();
        serializer.serialize(&*hist, &mut meta_hist_bytes)
            .context("Failed to serialize META histogram")?;
    }
    
    Ok((metadata_json, tsv_content, results_path, op_log_path, 
         get_hist_bytes, put_hist_bytes, meta_hist_bytes))
}

fn to_status<E: std::fmt::Display>(e: E) -> Status {
    Status::internal(e.to_string())
}

fn glob_match(pattern: &str, key: &str) -> bool {
    let escaped = regex::escape(pattern).replace(r"\*", ".*");
    let re = regex::Regex::new(&format!("^{}$", escaped)).unwrap();
    re.is_match(key)
}

/// Universal helper for listing keys from any URI backend
/// Works with s3://, file://, az://, gs://, etc.
async fn list_keys_for_uri(uri: &str) -> Result<Vec<String>> {
    // Try ObjectStore first (works for most backends)
    match store_for_uri(uri) {
        Ok(store) => {
            let keys = store.list(uri, false).await
                .context("Failed to list objects")?;
            // Extract just the key portion (filename) from full URIs
            let base_uri = if let Some(pos) = uri.rfind('/') {
                &uri[..=pos]
            } else {
                uri
            };
            Ok(keys.into_iter()
                .filter_map(|k| {
                    if let Some(stripped) = k.strip_prefix(base_uri) {
                        Some(stripped.to_string())
                    } else {
                        Some(k)
                    }
                })
                .collect())
        }
        Err(_) => {
            // Fallback to AWS SDK for S3-specific URIs (for compatibility)
            if uri.starts_with("s3://") {
                let parts: Vec<&str> = uri.strip_prefix("s3://").unwrap().split('/').collect();
                if parts.is_empty() {
                    return Ok(Vec::new());
                }
                let bucket = parts[0];
                let prefix = if parts.len() > 1 {
                    parts[1..].join("/")
                } else {
                    String::new()
                };
                list_keys_async(bucket, &prefix).await
            } else {
                Err(anyhow::anyhow!("Unsupported URI scheme: {}", uri))
            }
        }
    }
}

/// Async helper that lists object keys under `prefix` for `bucket` using the AWS Rust SDK.
///
/// We do this here (instead of using `s3dlio::s3_utils::list_objects`) to avoid
/// calling a blocking `block_on` inside a Tokio runtime, which can panic.
async fn list_keys_async(bucket: &str, prefix: &str) -> Result<Vec<String>> {
    // Use the modern, non-deprecated loader
    let cfg = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let client = s3::Client::new(&cfg);

    let mut out = Vec::new();
    let mut cont: Option<String> = None;
    loop {
        let mut req = client.list_objects_v2().bucket(bucket).prefix(prefix);
        if let Some(c) = cont.as_deref() {
            req = req.continuation_token(c);
        }
        let resp = req.send().await.map_err(|e| anyhow::anyhow!(e))?;

        // `resp.contents()` is a slice &[Object]
        for obj in resp.contents() {
            if let Some(k) = obj.key() {
                let key = k.strip_prefix(prefix).unwrap_or(k);
                out.push(key.to_string());
            }
        }
        match resp.next_continuation_token() {
            Some(tok) if !tok.is_empty() => cont = Some(tok.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

/// v0.7.6: Validate workload configuration before execution
/// Checks for common errors that would cause workload startup to fail
async fn validate_workload_config(config: &sai3_bench::config::Config) -> Result<()> {
    use sai3_bench::config::OpSpec;
    
    // Check that workload has operations configured (unless cleanup-only mode)
    let is_cleanup_only = config.prepare.as_ref()
        .map(|p| p.cleanup_only.unwrap_or(false))
        .unwrap_or(false);
    
    if config.workload.is_empty() && !is_cleanup_only {
        anyhow::bail!("No operations configured in workload");
    }
    
    // Validate each operation
    for weighted_op in &config.workload {
        match &weighted_op.spec {
            OpSpec::Get { path } => {
                // For file:// backends, verify files exist
                if path.starts_with("file://") {
                    let file_path = path.replace("file://", "");
                    // Check if it's a glob pattern
                    if file_path.contains('*') {
                        let paths: Vec<_> = glob::glob(&file_path)
                            .map_err(|e| anyhow::anyhow!("Invalid glob pattern '{}': {}", path, e))?
                            .collect();
                        if paths.is_empty() {
                            anyhow::bail!("No files found matching GET pattern: {}", path);
                        }
                    }
                }
            }
            OpSpec::Put { path, object_size, size_spec, .. } => {
                // PUT operations need size configuration
                if object_size.is_none() && size_spec.is_none() {
                    anyhow::bail!("PUT operation at '{}' requires either 'object_size' or 'size_spec'", path);
                }
            }
            OpSpec::Delete { path } => {
                // Delete needs a valid path
                if path.is_empty() {
                    anyhow::bail!("DELETE operation requires non-empty path");
                }
            }
            OpSpec::List { path } => {
                // List needs a valid path
                if path.is_empty() {
                    anyhow::bail!("LIST operation requires non-empty path");
                }
            }
            OpSpec::Stat { path } => {
                // Stat needs a valid path
                if path.is_empty() {
                    anyhow::bail!("STAT operation requires non-empty path");
                }
            }
            _ => {
                // Other operations (if any) are assumed valid
            }
        }
    }
    
    Ok(())
}

/// v0.7.13: Wait for shutdown signals (SIGINT or SIGTERM)
/// 
/// Returns signal name for logging. Provides graceful shutdown on Ctrl-C or systemd/Docker stop.
async fn wait_for_shutdown_signal() -> &'static str {
    use tokio::signal::unix::{signal, SignalKind};
    
    let mut sigint = signal(SignalKind::interrupt())
        .expect("failed to install SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate())
        .expect("failed to install SIGTERM handler");
    
    tokio::select! {
        _ = sigint.recv() => "SIGINT",
        _ = sigterm.recv() => "SIGTERM",
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    dotenv().ok();
    let args = Cli::parse();
    
    // Initialize logging based on verbosity level
    let level = match args.verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::new(format!("sai3bench_agent={},sai3_bench={}", level, level));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    debug!("Logging initialized at level: {}", level);
    
    let addr: SocketAddr = args.listen.parse().context("invalid listen addr")?;

    // Decide between plaintext and TLS
    if !args.tls {
        info!("Agent starting in PLAINTEXT mode on {}", addr);
        println!("sai3bench-agent listening (PLAINTEXT) on {}", addr);
        let agent_state = AgentState::new(args.op_log.clone());
        Server::builder()
            .add_service(AgentServer::new(AgentSvc::new(agent_state)))
            .serve_with_shutdown(addr, async {
                let sig = wait_for_shutdown_signal().await;
                info!("Received {} - initiating graceful shutdown", sig);
            })
            .await
            .context("tonic server failed")?;
        info!("Agent shutdown complete");
        return Ok(());
    }

    // --- TLS path with self-signed cert generated at startup ---
    use rcgen::generate_simple_self_signed;
    use tonic::transport::{Identity, ServerTlsConfig};

    // Build SANs: use --tls-sans if provided (comma-separated), otherwise fallback to --tls-domain
    let sans: Vec<String> = if let Some(list) = &args.tls_sans {
        list.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![args.tls_domain.clone()]
    };

    let cert = generate_simple_self_signed(sans).context("generate self-signed cert")?;

    // rcgen 0.14: pull the PEM from the inner cert and the signing key
    let cert_pem = cert.cert.pem();                  // certificate as PEM string
    let key_pem  = cert.signing_key.serialize_pem(); // private key as PEM string

    // Optionally write them so the controller can trust with --agent-ca
    if let Some(dir) = args.tls_write_ca.as_ref() {
        fs::create_dir_all(dir).ok();
        fs::write(dir.join("agent_cert.pem"), &cert_pem).ok();
        fs::write(dir.join("agent_key.pem"), &key_pem).ok();
        println!(
            "wrote self-signed cert & key to {}",
            dir.to_string_lossy()
        );
    }

    let identity = Identity::from_pem(cert_pem.as_bytes(), key_pem.as_bytes());
    let tls = ServerTlsConfig::new().identity(identity);

    println!(
        "sai3bench-agent listening (TLS) on {} — SANs: {}",
        addr,
        if let Some(list) = &args.tls_sans {
            list
        } else {
            &args.tls_domain
        }
    );

    let agent_state = AgentState::new(args.op_log.clone());
    Server::builder()
        .tls_config(tls)?
        .add_service(AgentServer::new(AgentSvc::new(agent_state)))
        .serve_with_shutdown(addr, async {
            let sig = wait_for_shutdown_signal().await;
            info!("Received {} - initiating graceful shutdown", sig);
        })
        .await
        .context("tonic server (TLS) failed")?;
    
    info!("Agent shutdown complete");
    Ok(())
}

