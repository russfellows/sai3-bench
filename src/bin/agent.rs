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
use pb::iobench::{Empty, LiveStats, OpSummary, PingReply, PrepareSummary, RunGetRequest, RunPutRequest, RunWorkloadRequest, WorkloadSummary, OpAggregateMetrics, ControlMessage, control_message::Command, PreFlightRequest, PreFlightResponse, ValidationResult, ResultLevel, ErrorType, BarrierRequest, BarrierResponse, AgentQueryRequest, AgentQueryResponse, PhaseProgress, WorkloadPhase, StageSummary};

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
    /// v0.8.14: Flag to track when COMPLETED message has been sent (fixes race with control reader)
    completion_sent: Arc<Mutex<bool>>,
    /// v0.8.24: Shared storage flag for distributed prepare (true=shared, false=isolated)
    shared_storage: Arc<Mutex<Option<bool>>>,
    /// v0.8.26: YAML-driven stage sequence (optional - uses hardcoded states if None)
    stages: Arc<Mutex<Option<Vec<sai3_bench::config::StageConfig>>>>,
    /// v0.8.26: Barrier release channel - sends (barrier_name, barrier_sequence) when RELEASE_BARRIER received
    barrier_release_tx: broadcast::Sender<(String, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
enum WorkloadState {
    Idle,              // Ready to accept new workload
    
    // YAML-driven stage execution (v0.8.26+)
    // This is now the ONLY execution model - hardcoded states have been removed
    AtStage {
        stage_index: usize,       // Index in stages array (0-based)
        stage_name: String,       // Stage name from config (e.g., "preflight", "epoch-1")
        ready_for_next: bool,     // true = at barrier, waiting to proceed; false = executing stage
    },
    
    /* v0.8.29: LEGACY STATES REMOVED - All code must use YAML-driven AtStage
    // Legacy hardcoded states (DEPRECATED - use AtStage for new code)
    // These remain for backward compatibility when config.distributed.stages is None
    #[deprecated(note = "Use AtStage with YAML-driven stages instead")]
    #[allow(dead_code)]  // Reserved for backward compatibility
    Validating,        // Pre-flight checks running
    #[deprecated(note = "Use AtStage with YAML-driven stages instead")]
    #[allow(dead_code)]  // Reserved for backward compatibility
    PrepareReady,      // Validation passed, waiting for prepare barrier
    #[deprecated(note = "Use AtStage with YAML-driven stages instead")]
    #[allow(dead_code)]  // Reserved for backward compatibility
    Preparing,         // Creating/cleaning objects
    #[deprecated(note = "Use AtStage with YAML-driven stages instead")]
    #[allow(dead_code)]  // Reserved for backward compatibility
    ExecuteReady,      // Prepare complete, waiting for execute barrier
    #[deprecated(note = "Use AtStage with YAML-driven stages instead")]
    #[allow(dead_code)]  // Reserved for backward compatibility
    Executing,         // Running I/O workload
    #[deprecated(note = "Use AtStage with YAML-driven stages instead")]
    #[allow(dead_code)]  // Reserved for backward compatibility
    CleanupReady,      // Execution complete, waiting for cleanup barrier
    #[deprecated(note = "Use AtStage with YAML-driven stages instead")]
    #[allow(dead_code)]  // Reserved for backward compatibility
    Cleaning,          // Removing objects
    */
    
    Completed,         // All phases done successfully
    Failed,            // Error occurred (backward compat - no error message)
    Aborting,          // Emergency shutdown in progress
    
    /* v0.8.29: EVEN OLDER LEGACY STATES REMOVED
    // Even older legacy states for backward compatibility (will be phased out)
    // Note: Ready is pattern-matched but not constructed in production code (only in tests)
    #[allow(dead_code)]
    Ready,             // Old: Validated, waiting for coordinated start
    Running,           // Old: Workload executing (maps to Executing)
    */
}


impl AgentState {
    fn new(agent_op_log_path: Option<std::path::PathBuf>) -> Self {
        let (abort_tx, _) = broadcast::channel(16);
        let (barrier_release_tx, _) = broadcast::channel(16);  // v0.8.26: Barrier release notifications
        Self {
            abort_tx,
            state: Arc::new(Mutex::new(WorkloadState::Idle)),
            error_message: Arc::new(Mutex::new(None)),
            agent_op_log_path,
            agent_id: Arc::new(Mutex::new(None)),
            config_yaml: Arc::new(Mutex::new(None)),
            tracker: Arc::new(Mutex::new(None)),
            op_log_path: Arc::new(Mutex::new(None)),
            agent_index: Arc::new(Mutex::new(None)),
            num_agents: Arc::new(Mutex::new(None)),
            completion_sent: Arc::new(Mutex::new(false)),
            shared_storage: Arc::new(Mutex::new(None)),
            stages: Arc::new(Mutex::new(None)),
            barrier_release_tx,
        }
    }
    
    /// Validate state transition before applying
    #[allow(deprecated)]  // We still support deprecated states for backward compat
    fn can_transition(from: &WorkloadState, to: &WorkloadState) -> bool {
        use WorkloadState::*;
        
        // AtStage transitions (v0.8.26+ YAML-driven stages)
        match (from, to) {
            // Stage execution: not ready -> ready (at barrier)
            (AtStage { stage_index: i1, ready_for_next: false, .. }, 
             AtStage { stage_index: i2, ready_for_next: true, .. }) if i1 == i2 => true,
            
            // Stage transition: ready at stage N -> executing stage N+1
            (AtStage { stage_index: i1, ready_for_next: true, .. }, 
             AtStage { stage_index: i2, ready_for_next: false, .. }) if *i2 == i1 + 1 => true,
            
            // v0.8.28: Validated state (pseudo-stage 0) -> execute first real stage (also index 0)
            // This handles: "validated" ready -> "preflight" executing (both at index 0)
            (AtStage { stage_index: 0, ready_for_next: true, stage_name }, 
             AtStage { stage_index: 0, ready_for_next: false, .. }) if stage_name == "validated" => true,
            
            // Start first stage from Idle (executing mode)
            (Idle, AtStage { stage_index: 0, ready_for_next: false, .. }) => true,
            
            // v0.8.28: Validation complete - ready for first stage barrier
            // This is used when START handler validates config and sets ready state
            (Idle, AtStage { stage_index: 0, ready_for_next: true, .. }) => true,
            
            // =========================================================================
            // LEGACY TRANSITION COMMENTED OUT - v0.8.29 Preparing state removed
            // =========================================================================
            // Bridge from old barrier flow to new YAML-driven stages (v0.8.26)
            // When using stages with barrier_sync, we go Idle→Validating→PrepareReady→Preparing→AtStage
            // (Preparing, AtStage { stage_index: 0, ready_for_next: false, .. }) => return true,
            
            // Complete final stage -> Completed
            (AtStage { ready_for_next: true, .. }, Completed) => true,
            
            // v0.8.28: Cleanup after disconnect at barrier -> Idle
            (AtStage { ready_for_next: true, .. }, Idle) => true,
            
            // Stage error -> Failed
            (AtStage { .. }, Failed) => true,
            
            // Abort from any stage
            (AtStage { .. }, Aborting) => true,
            
            // Terminal state transitions
            (Completed, Idle) => true,  // Reset for next workload
            (Failed, Idle) => true,     // Error recovery
            
            // Abort from any state
            (_, Aborting) => true,
            (Aborting, Idle) => true,   // Cleanup complete after abort
            
            // No-op: Idle → Idle for safety
            (Idle, Idle) => true,
            (Failed, Failed) => true,
            
            _ => false,  // All other transitions invalid
        }
        
        /* v0.8.29: ALL LEGACY TRANSITIONS REMOVED - Use YAML-driven stages only
        matches!(
            (from, to),
            // Deprecated v0.8.25 barrier sync flow (still supported for backward compat)
            (Idle, Validating)                  // Pre-flight validation starts
            | (Validating, PrepareReady)        // Validation passed, ready for prepare
            | (Validating, Failed)              // Validation failed
            | (PrepareReady, Preparing)         // Barrier cleared, start prepare
            | (Preparing, ExecuteReady)         // Prepare complete, ready for execute
            | (Preparing, Failed)               // Prepare error
            | (ExecuteReady, Executing)         // Barrier cleared, start execution
            | (Executing, CleanupReady)         // Execution complete, ready for cleanup
            | (Executing, Failed)               // Execution error
            | (CleanupReady, Cleaning)          // Barrier cleared, start cleanup
            | (Cleaning, Completed)             // Cleanup complete
            | (Cleaning, Failed)                // Cleanup error
            | (Completed, Idle)                 // Reset for next workload
            | (Failed, Idle)                    // Error recovery
            
            // Abort from any state
            | (_, Aborting)
            | (Aborting, Idle)                  // Cleanup complete
            
            // Even older legacy backward-compatible flow (when barrier_sync.enabled=false)
            | (Idle, Ready)                     // Old: RPC arrives, validation passes
            | (Idle, Failed)                    // Old: RPC arrives, validation fails
            | (Ready, Running)                  // Old: Start time reached, spawn workload
            | (Ready, Idle)                     // Old: Abort during coordinated start
            | (Running, Idle)                   // Old: Workload completed successfully
            | (Running, Failed)                 // Old: Workload error
            // Note: (Running, Aborting) removed - already covered by (_, Aborting) above
            
            // No-op transitions for race condition safety
            | (Idle, Idle)
            | (Failed, Failed)
        )
        */
    }
    
    /// Transition to new state with validation and logging
    async fn transition_to(&self, new_state: WorkloadState, reason: &str) -> Result<(), String> {
        let mut state = self.state.lock().await;
        
        // v0.8.14: Handle same-state transitions as no-op (race condition between stats writer and control reader)
        if *state == new_state {
            debug!("Agent state already {:?}, ignoring redundant transition ({})", *state, reason);
            return Ok(());
        }
        
        if !Self::can_transition(&state, &new_state) {
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
    
    /// Send abort signal to running workload
    fn send_abort(&self) {
        let _ = self.abort_tx.send(());
        info!("Abort signal broadcast to workload");
    }
    
    /// Subscribe to abort signals
    /// TODO: Wire into execute_stages_workflow for proper abort handling
    #[allow(dead_code)]  // Reserved for abort handling in YAML stage workflow
    fn subscribe_abort(&self) -> broadcast::Receiver<()> {
        self.abort_tx.subscribe()
    }
    
    /// v0.8.14: Mark completion message as sent (before flush delay)
    /// This allows control reader to distinguish normal disconnect from abnormal
    async fn mark_completion_sent(&self) {
        let mut sent = self.completion_sent.lock().await;
        *sent = true;
        debug!("Marked completion as sent");
    }
    
    /// v0.8.14: Check if completion message was sent
    async fn is_completion_sent(&self) -> bool {
        *self.completion_sent.lock().await
    }
    
    /// v0.8.14: Reset completion flag for new workload
    async fn reset_completion_sent(&self) {
        let mut sent = self.completion_sent.lock().await;
        *sent = false;
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
    
    async fn set_shared_storage(&self, shared: bool) {
        let mut shared_storage = self.shared_storage.lock().await;
        *shared_storage = Some(shared);
    }
    
    async fn get_shared_storage(&self) -> Option<bool> {
        *self.shared_storage.lock().await
    }
    
    async fn set_stages(&self, stages: Option<Vec<sai3_bench::config::StageConfig>>) {
        let mut stages_lock = self.stages.lock().await;
        *stages_lock = stages;
    }
    
    async fn get_stages(&self) -> Option<Vec<sai3_bench::config::StageConfig>> {
        self.stages.lock().await.clone()
    }
    
    /// v0.8.26: Subscribe to barrier release notifications
    fn subscribe_barrier_release(&self) -> broadcast::Receiver<(String, u32)> {
        self.barrier_release_tx.subscribe()
    }
    
    /// v0.8.26: Notify that a barrier has been released by controller
    fn notify_barrier_release(&self, barrier_name: String, barrier_sequence: u32) {
        // Note: send() can fail if no receivers, but that's OK (agent may not be at barrier)
        let _ = self.barrier_release_tx.send((barrier_name, barrier_sequence));
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

/// Extract filesystem path from Config.target or multi_endpoint, return None if not a file:// or direct:// URI
fn extract_filesystem_path_from_config(config: &sai3_bench::config::Config) -> Option<std::path::PathBuf> {
    // First try config.target (standalone mode)
    if let Some(target) = config.target.as_ref() {
        if let Some(path) = target.strip_prefix("file://")
            .or_else(|| target.strip_prefix("direct://"))
            .map(std::path::PathBuf::from) {
            return Some(path);
        }
    }
    
    // If target not set or not filesystem, check multi_endpoint (distributed mode)
    if let Some(ref multi) = config.multi_endpoint {
        for endpoint in &multi.endpoints {
            if let Some(path) = endpoint.strip_prefix("file://")
                .or_else(|| endpoint.strip_prefix("direct://"))
                .map(std::path::PathBuf::from) {
                // Return first filesystem endpoint found
                return Some(path);
            }
        }
    }
    
    // Check prepare.ensure_objects[].base_uri (v0.8.26+)
    if let Some(ref prepare) = config.prepare {
        for ensure in &prepare.ensure_objects {
            if let Some(ref base_uri) = ensure.base_uri {
                if let Some(path) = base_uri.strip_prefix("file://")
                    .or_else(|| base_uri.strip_prefix("direct://"))
                    .map(std::path::PathBuf::from) {
                    return Some(path);
                }
            }
        }
    }
    
    // Check distributed.agents[].target_override (v0.8.26+)
    if let Some(ref distributed) = config.distributed {
        for agent in &distributed.agents {
            if let Some(ref target) = agent.target_override {
                if let Some(fs_path) = target.strip_prefix("file://")
                    .or_else(|| target.strip_prefix("direct://"))
                    .map(std::path::PathBuf::from) {
                    return Some(fs_path);
                }
            }
        }
    }
    
    None
}

#[tonic::async_trait]
impl Agent for AgentSvc {
    async fn ping(&self, _req: Request<Empty>) -> Result<Response<PingReply>, Status> {
        Ok(Response::new(PingReply {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }))
    }

    async fn pre_flight_validation(
        &self,
        req: Request<PreFlightRequest>,
    ) -> Result<Response<PreFlightResponse>, Status> {
        let PreFlightRequest { config_yaml, agent_id } = req.into_inner();
        
        info!("Running pre-flight validation for agent {}", agent_id);
        
        // Parse the YAML configuration
        let config: sai3_bench::config::Config = serde_yaml::from_str(&config_yaml)
            .map_err(|e| {
                error!("Failed to parse YAML config: {}", e);
                Status::invalid_argument(format!("Invalid YAML config: {}", e))
            })?;
        
        // Run filesystem validation only if target is file:// or direct://
        let fs_path = extract_filesystem_path_from_config(&config);
        let fs_summary = if let Some(path) = fs_path {
            sai3_bench::preflight::filesystem::validate_filesystem(
                &path,
                true,  // check_write = true
                Some(1024 * 1024),  // require at least 1 MB free space
            ).await.map_err(|e| {
                error!("Filesystem validation failed: {}", e);
                Status::internal(format!("Filesystem validation error: {}", e))
            })?
        } else {
            // Not a filesystem target - skip filesystem validation
            sai3_bench::preflight::ValidationSummary::new(vec![
                sai3_bench::preflight::ValidationResult {
                    level: sai3_bench::preflight::ResultLevel::Info,
                    error_type: None,
                    message: "Skipping filesystem validation (target is object storage)".to_string(),
                    suggestion: String::new(),
                    details: None,
                    test_phase: "filesystem".to_string(),
                }
            ])
        };
        
        // TODO Phase 2: Add object storage validation
        // let obj_summary = sai3_bench::preflight::object_storage::validate_object_storage(&config).await;
        
        // Convert ValidationSummary to proto response
        let results: Vec<ValidationResult> = fs_summary.results.iter().map(|r| {
            ValidationResult {
                level: match r.level {
                    sai3_bench::preflight::ResultLevel::Success => ResultLevel::Success as i32,
                    sai3_bench::preflight::ResultLevel::Info => ResultLevel::Info as i32,
                    sai3_bench::preflight::ResultLevel::Warning => ResultLevel::Warning as i32,
                    sai3_bench::preflight::ResultLevel::Error => ResultLevel::Error as i32,
                },
                error_type: match r.error_type {
                    Some(sai3_bench::preflight::ErrorType::Authentication) => ErrorType::Authentication as i32,
                    Some(sai3_bench::preflight::ErrorType::Permission) => ErrorType::Permission as i32,
                    Some(sai3_bench::preflight::ErrorType::Network) => ErrorType::Network as i32,
                    Some(sai3_bench::preflight::ErrorType::Configuration) => ErrorType::Configuration as i32,
                    Some(sai3_bench::preflight::ErrorType::Resource) => ErrorType::Resource as i32,
                    Some(sai3_bench::preflight::ErrorType::System) => ErrorType::System as i32,
                    None => ErrorType::Unknown as i32,
                },
                message: r.message.clone(),
                suggestion: r.suggestion.clone(),
                details: r.details.clone().unwrap_or_default(),
                test_phase: r.test_phase.clone(),
            }
        }).collect();
        
        let passed = !fs_summary.has_errors();
        
        info!(
            "Pre-flight validation completed: {} errors, {} warnings, {} info",
            fs_summary.error_count(),
            fs_summary.warning_count(),
            fs_summary.info_count()
        );
        
        Ok(Response::new(PreFlightResponse {
            passed,
            results,
            error_count: fs_summary.error_count() as i32,
            warning_count: fs_summary.warning_count() as i32,
            info_count: fs_summary.info_count() as i32,
        }))
    }

    // v0.8.25: Barrier synchronization RPCs
    async fn report_barrier_ready(
        &self,
        req: Request<BarrierRequest>,
    ) -> Result<Response<BarrierResponse>, Status> {
        let request = req.into_inner();
        
        info!(
            "Agent {} reporting barrier '{}' readiness",
            request.agent_id,
            request.barrier_id
        );
        
        // Process the progress report from the request
        if let Some(progress) = request.progress {
            debug!(
                "Progress: phase={:?}, ops={}, at_barrier={}",
                progress.current_phase,
                progress.operations_completed,
                progress.at_barrier
            );
        }
        
        // v0.8.25: Actual barrier coordination will be implemented when integrated with
        // BarrierManager in controller. For now, acknowledge receipt and allow agent to proceed.
        // The controller's BarrierManager will track all agents and only set proceed=true
        // when the barrier is actually satisfied.
        Ok(Response::new(BarrierResponse {
            proceed: true,
            waiting_agents: vec![],
            failed_agents: vec![],
            next_heartbeat_ms: 30000, // 30s default
        }))
    }

    async fn query_agent_status(
        &self,
        req: Request<AgentQueryRequest>,
    ) -> Result<Response<AgentQueryResponse>, Status> {
        let request = req.into_inner();
        
        info!(
            "Agent {} status query (reason: {})",
            request.agent_id,
            request.reason
        );
        
        // Get current agent state
        let current_state = self.state.get_state().await;
        let agent_id = self.state.get_agent_id().await.unwrap_or_else(|| request.agent_id.clone());
        
        // Map WorkloadState to WorkloadPhase for reporting
        #[allow(deprecated)]  // Still support deprecated states
        let (current_phase, status_message) = match current_state {
            WorkloadState::Idle => (
                WorkloadPhase::PhaseIdle,
                "Agent is idle and ready to accept workload".to_string()
            ),
            // AtStage: Map based on ready_for_next flag and stage name
            WorkloadState::AtStage { ref stage_name, ready_for_next, .. } => {
                if ready_for_next {
                    (
                        WorkloadPhase::PhasePrepareReady,  // Generic "ready" phase (waiting at barrier)
                        format!("Stage '{}' complete, waiting at barrier", stage_name)
                    )
                } else {
                    (
                        WorkloadPhase::PhaseExecuting,     // Generic "executing" phase
                        format!("Executing stage '{}'", stage_name)
                    )
                }
            },
            /* v0.8.29: LEGACY STATES REMOVED - These match arms are no longer valid
            WorkloadState::Validating => (
                WorkloadPhase::PhaseValidating,
                "Running pre-flight validation".to_string()
            ),
            WorkloadState::PrepareReady => (
                WorkloadPhase::PhasePrepareReady,
                "Validation passed, waiting at prepare barrier".to_string()
            ),
            WorkloadState::Preparing => (
                WorkloadPhase::PhasePreparing,
                "Preparing objects for workload".to_string()
            ),
            WorkloadState::ExecuteReady => (
                WorkloadPhase::PhaseExecuteReady,
                "Prepare complete, waiting at execute barrier".to_string()
            ),
            WorkloadState::Executing => (
                WorkloadPhase::PhaseExecuting,
                "Executing workload".to_string()
            ),
            WorkloadState::CleanupReady => (
                WorkloadPhase::PhaseCleanupReady,
                "Execution complete, waiting at cleanup barrier".to_string()
            ),
            WorkloadState::Cleaning => (
                WorkloadPhase::PhaseCleaning,
                "Cleaning up objects".to_string()
            ),
            */
            WorkloadState::Completed => (
                WorkloadPhase::PhaseCompleted,
                "All phases completed successfully".to_string()
            ),
            WorkloadState::Failed => (
                WorkloadPhase::PhaseFailed,
                self.state.error_message.lock().await.clone()
                    .unwrap_or_else(|| "Agent in failed state".to_string())
            ),
            WorkloadState::Aborting => (
                WorkloadPhase::PhaseAborting,
                "Aborting workload".to_string()
            ),
            /* v0.8.29: LEGACY STATES REMOVED
            // Legacy states
            WorkloadState::Ready => (
                WorkloadPhase::PhasePrepareReady,
                "Ready to start (legacy state)".to_string()
            ),
            WorkloadState::Running => (
                WorkloadPhase::PhaseExecuting,
                "Running workload (legacy state)".to_string()
            ),
            */
        };
        
        // Get progress from LiveStatsTracker if available
        let tracker = self.state.tracker.lock().await;
        let (operations_completed, objects_created, bytes_written) = if let Some(t) = tracker.as_ref() {
            let stats = t.snapshot();
            (
                stats.get_ops + stats.put_ops + stats.meta_ops,
                stats.prepare_objects_created,
                stats.get_bytes + stats.put_bytes,
            )
        } else {
            (0, 0, 0)
        };
        drop(tracker); // Release lock
        
        Ok(Response::new(AgentQueryResponse {
            current_progress: Some(PhaseProgress {
                agent_id: agent_id.clone(),
                current_phase: current_phase as i32,
                phase_start_time_ms: 0, // TODO: Track phase start time
                heartbeat_time_ms: chrono::Utc::now().timestamp_millis() as u64,
                objects_created,
                objects_total: 0, // TODO: Get from config
                current_operation: status_message.clone(),
                bytes_written,
                errors_encountered: 0, // TODO: Track errors
                operations_completed,
                current_throughput: 0.0, // TODO: Calculate throughput
                phase_elapsed_ms: 0, // TODO: Calculate from phase_start_time_ms
                objects_deleted: 0, // TODO: Track in cleanup phase
                objects_remaining: 0,
                is_stuck: false, // TODO: Detect stuck state
                stuck_reason: String::new(),
                progress_rate: 0.0, // TODO: Calculate from recent progress
                phase_completed: matches!(current_state, WorkloadState::Completed),
                // v0.8.29: Detect barrier state from AtStage { ready_for_next: true }
                at_barrier: matches!(
                    current_state,
                    WorkloadState::AtStage { ready_for_next: true, .. }
                ),
            }),
            is_alive: true,
            status_message,
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
        // Generate zero-filled data for all objects
        // Convert to Bytes for zero-copy semantics
        let data = bytes::Bytes::from(vec![0u8; object_size as usize]);

        let started = Instant::now();
        let sem = Arc::new(Semaphore::new(concurrency as usize));
        let mut futs = FuturesUnordered::new();
        
        for full_uri in keys {
            let d = data.clone();  // Clone is cheap: Bytes is Arc-like
            let sem2 = sem.clone();
            futs.push(tokio::spawn(async move {
                let _p = sem2.acquire_owned().await.unwrap();
                // Use ObjectStore pattern with full URI
                let store = store_for_uri(&full_uri).map_err(|e| anyhow::anyhow!(e))?;
                store.put(&full_uri, d).await?;  // Zero-copy: Bytes passed directly
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

        // CRITICAL: Initialize RNG seed for THIS RUN to ensure unique data generation
        // This combines agent_id + PID + current nanosecond timestamp
        // Each successive run will get a different timestamp = different data
        sai3_bench::data_gen_pool::set_global_rng_seed(Some(&agent_id));
        
        // Print hardware detection info (helps diagnose performance issues)
        sai3_bench::data_gen_pool::print_hardware_info();

        // Parse the YAML configuration
        let mut config: sai3_bench::config::Config = serde_yaml::from_str(&config_yaml)
            .map_err(|e| {
                error!("Failed to parse YAML config: {}", e);
                Status::invalid_argument(format!("Invalid YAML config: {}", e))
            })?;

        debug!("YAML config parsed successfully");
        debug!("Config has multi_endpoint: {}", config.multi_endpoint.is_some());
        if let Some(ref me) = config.multi_endpoint {
            debug!("Multi-endpoint config: {} endpoints, strategy: {}", me.endpoints.len(), me.strategy);
        }

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
            // Cleanup-only mode: Generate list of objects to clean up
            if let Some(ref prepare_config) = config.prepare {
                if prepare_config.skip_verification {
                    // Generate from config without listing
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
                    // List existing objects WITHOUT creating any (use new list_existing_objects function)
                    info!("Cleanup-only mode (skip_verification=false): Listing existing objects only");
                    if num_agents > 1 {
                        warn!("Note: In shared storage mode with {} agents, each will list ALL objects then filter to their subset.", num_agents);
                        warn!("For large datasets (>10k files), strongly recommend skip_verification=true to avoid listing overhead.");
                    }
                    
                    sai3_bench::cleanup::list_existing_objects(
                        prepare_config,
                        agent_index as usize,
                        num_agents as usize,
                    ).await.map_err(|e| {
                        error!("Failed to list objects: {}", e);
                        Status::internal(format!("Failed to list objects: {}", e))
                    })?
                }
            } else {
                (Vec::new(), None)
            }
        } else if let Some(ref prepare_config) = config.prepare {
            // Normal mode: Execute prepare phase
            debug!("Executing prepare phase");
            
            // v0.8.22: Create multi-endpoint cache for prepare phase statistics
            use std::sync::{Arc, Mutex};
            use std::collections::HashMap;
            let prepare_multi_ep_cache: sai3_bench::workload::MultiEndpointCache = Arc::new(Mutex::new(HashMap::new()));
            
            debug!("Prepare phase: config.multi_endpoint.is_some() = {}", config.multi_endpoint.is_some());
            if let Some(ref me) = config.multi_endpoint {
                debug!("Prepare phase: multi_endpoint has {} endpoints", me.endpoints.len());
            }
            
            let (prepared, manifest, prepare_metrics) = sai3_bench::workload::prepare_objects(
                prepare_config,
                Some(&config.workload),
                None,  // live_stats_tracker - not available in run_workload RPC (deprecated)
                config.multi_endpoint.as_ref(),  // v0.8.22: pass multi-endpoint config
                &prepare_multi_ep_cache,  // v0.8.22: pass multi-endpoint cache for stats
                config.concurrency,
                agent_index as usize,
                num_agents as usize,
                shared_storage,  // v0.8.24: Only filter by agent_id in shared storage mode
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
            
            // v0.8.9: Set stage to CLEANUP for proper progress display
            if let Some(ref t) = tracker {
                use sai3_bench::live_stats::WorkloadStage;
                t.set_stage(WorkloadStage::Cleanup, prepared_objects.len() as u64);
            }
            
            // Run cleanup with stats tracking
            sai3_bench::cleanup::cleanup_prepared_objects(
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
                    endpoint_stats: None,
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
                    endpoint_stats: None,
                }
            }
        } else {
            // v0.8.9: Set stage to WORKLOAD for proper progress display
            if let Some(ref t) = config.live_stats_tracker {
                use sai3_bench::live_stats::WorkloadStage;
                t.set_stage(WorkloadStage::Workload, 0);  // 0 = time-based, not count-based
            }
            
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
                    
                    // v0.8.9: Set stage to CLEANUP for proper progress display
                    if let Some(ref t) = tracker {
                        use sai3_bench::live_stats::WorkloadStage;
                        t.set_stage(WorkloadStage::Cleanup, prepared_objects.len() as u64);
                    }
                    
                    if let Err(e) = sai3_bench::cleanup::cleanup_prepared_objects(
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

    // v0.8.4: Bidirectional streaming type (used by execute_workload)
    // Note: RunWorkloadWithLiveStatsStream type kept for proto compatibility but implementation removed
    type RunWorkloadWithLiveStatsStream = 
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<LiveStats, Status>> + Send>>;
    type ExecuteWorkloadStream = 
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<LiveStats, Status>> + Send>>;

    // DEPRECATED: Legacy unidirectional streaming RPC removed in v0.8.11
    // Use execute_workload() bidirectional streaming instead for proper abort handling
    async fn run_workload_with_live_stats(
        &self,
        _req: Request<RunWorkloadRequest>,
    ) -> Result<Response<Self::RunWorkloadWithLiveStatsStream>, Status> {
        Err(Status::unimplemented(
            "run_workload_with_live_stats is deprecated. Use execute_workload() bidirectional streaming instead."
        ))
    }
    
    // v0.7.12: Abort ongoing workload (controller failure, user interrupt, etc.)
    async fn abort_workload(&self, _req: Request<Empty>) -> Result<Response<Empty>, Status> {
        let current_state = self.state.get_state().await;
        
        // v0.8.29: Only AtStage is a valid running state now (legacy states removed)
        match current_state {
            WorkloadState::AtStage { .. } => {  // Can abort from any stage
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
                    // v0.8.29: Check for Aborting or any AtStage (legacy Running removed)
                    if retry_state == WorkloadState::Aborting || matches!(retry_state, WorkloadState::AtStage { .. }) {
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
            
            // ============================================================================
            // CATCH-ALL ARM FOR v0.8.25 BARRIER SYNC STATES (TEMPORARY FAILSAFE)
            // ============================================================================
            // TODO v0.8.25: Replace this catch-all with proper abort handling for each new state:
            //
            // WorkloadState::Validating => {
            //     // Pre-flight validation in progress - abort immediately
            //     // Transition: Validating → Aborting → Idle
            //     warn!("⚠️  Abort during validation - cancelling pre-flight checks");
            //     self.state.transition_to(WorkloadState::Aborting, "abort during validation").await?;
            //     self.state.send_abort();
            //     // Spawn cleanup task...
            // }
            //
            // WorkloadState::PrepareReady | WorkloadState::ExecuteReady | WorkloadState::CleanupReady => {
            //     // Agent at barrier, waiting for others - abort is trivial (no work in progress)
            //     // Transition: *Ready → Aborting → Idle
            //     info!("Abort at barrier (no work in progress) - resetting to Idle");
            //     self.state.transition_to(WorkloadState::Idle, "abort at barrier").await?;
            //     Ok(Response::new(Empty {}))
            // }
            //
            // WorkloadState::Completed => {
            //     // Workload already completed successfully - nothing to abort
            //     info!("Abort requested but workload already completed");
            //     Ok(Response::new(Empty {}))
            // }
            //
            // For now, treat all new states conservatively as if they're Running/Preparing/Cleaning
            // (which are already handled above). This ensures abort works even if state machine
            // transitions to a new state we haven't explicitly coded for yet.
            // ============================================================================
            _ => {
                warn!("⚠️  Abort requested in unhandled state {:?} - treating as active workload", current_state);
                
                // Conservative approach: assume state is active and needs abort signal
                if let Err(e) = self.state.transition_to(WorkloadState::Aborting, "abort in unknown state").await {
                    error!("Failed to transition to Aborting from {:?}: {}", current_state, e);
                    return Err(Status::internal(format!("State transition failed: {}", e)));
                }
                
                self.state.send_abort();
                
                // Same cleanup logic as Running/Ready above
                let state_for_reset = self.state.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    let current = state_for_reset.get_state().await;
                    if current == WorkloadState::Aborting {
                        let _ = state_for_reset.transition_to(WorkloadState::Idle, "abort cleanup (5s)").await;
                        info!("⏰ Abort cleanup (5s) - reset agent to Idle");
                    } else if current == WorkloadState::Idle {
                        debug!("Agent already reset to Idle via stream cleanup");
                    } else {
                        warn!("Unexpected state during abort: {:?}", current);
                    }
                    
                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    let retry_state = state_for_reset.get_state().await;
                    if retry_state == WorkloadState::Aborting {
                        warn!("⚠️  Agent stuck in Aborting state after 15s - forcing reset to Idle");
                        state_for_reset.set_state(WorkloadState::Idle).await;
                        info!("⏰ Abort retry (15s) - forced agent to Idle state");
                    }
                });
                
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
                
                // Wait for START command validation to complete
                // v0.8.29: Accept AtStage with ready_for_next: true as READY signal (replaces deprecated PrepareReady)
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    let state = agent_state.get_state().await;
                    let is_validated = match &state {
                        WorkloadState::AtStage { ready_for_next, .. } => *ready_for_next,
                        _ => false,
                    };
                    if is_validated {
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
                    get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p90_us: 0.0, get_p95_us: 0.0, get_p99_us: 0.0,
                    put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p90_us: 0.0, put_p95_us: 0.0, put_p99_us: 0.0,
                    meta_ops: 0, meta_mean_us: 0.0, meta_p50_us: 0.0, meta_p90_us: 0.0, meta_p99_us: 0.0,
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
                    // v0.8.9: Stage tracking (not in any stage yet)
                    current_stage: 0, stage_name: String::new(), stage_progress_current: 0, stage_progress_total: 0, stage_elapsed_s: 0.0,
                    // v0.8.14: Concurrency (not known yet - will be sent with RUNNING messages)
                    concurrency: 0,
                    // v0.8.26: Barrier coordination
                    at_barrier: false,
                    barrier_name: String::new(),
                    barrier_sequence: 0,
                    stage_summary: None,
                };
                sequence += 1;
                
                if tx_stats.send(ready_msg).await.is_err() {
                    error!("Stats writer: Failed to send READY message (controller disconnected?)");
                    return;
                }
                info!("Stats writer: Sent READY message with timestamp {} ns", agent_timestamp_ns);
                
                // Wait for workload to start (transition to Running or AtStage with ready_for_next: false)
                // Block here - do NOT send any more messages until workload starts
                // v0.8.28: Add timeout to prevent indefinite waits
                // v0.8.29: Distinguish AtStage { ready_for_next: true } (waiting) from { ready_for_next: false } (running)
                const MAX_WAIT_SECS: u32 = 60;  // Timeout after 60 seconds
                let mut wait_count: u32 = 0;
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    wait_count += 1;
                    
                    let state = agent_state.get_state().await;
                    // v0.8.29: AtStage with ready_for_next: false means actively executing
                    // AtStage { ready_for_next: true } means waiting at barrier, not yet running
                    let is_running = matches!(&state, WorkloadState::AtStage { ready_for_next: false, .. });
                    if is_running {
                        info!("Stats writer: Workload started (state: {:?}), beginning stats transmission", state);
                        break;
                    } else if state == WorkloadState::Failed || state == WorkloadState::Idle || state == WorkloadState::Aborting {
                        // Workload cancelled or failed before starting
                        info!("Stats writer: Workload cancelled or failed, exiting (state: {:?})", state);
                        return;
                    } else if wait_count >= MAX_WAIT_SECS {
                        error!("Stats writer: Timeout waiting for workload to start ({} seconds). Current state: {:?}",
                               MAX_WAIT_SECS, state);
                        return;
                    } else if wait_count.is_multiple_of(10) {
                        // Log every 10 seconds while waiting
                        warn!("Stats writer: Still waiting for workload to start ({}s, state: {:?})",
                              wait_count, state);
                    }
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
                
                // v0.8.10: Progress bar display on agent
                use indicatif::{ProgressBar, ProgressStyle};
                
                // Create progress bars (recreated when stage changes)
                let mut prepare_pb: Option<ProgressBar> = None;
                let mut workload_pb: Option<ProgressBar> = None;
                let mut last_prepare_total: u64 = 0;
                
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
                                    
                                    // v0.8.10: Clean up progress bars
                                    if let Some(pb) = prepare_pb.take() {
                                        pb.finish_and_clear();
                                    }
                                    if let Some(pb) = workload_pb.take() {
                                        pb.finish_with_message(format!("[{}] workload complete", agent_id));
                                    }
                                    
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
                                                get_p90_us: snapshot.get_p90_us as f64,
                                                get_p95_us: snapshot.get_p95_us as f64,
                                                get_p99_us: snapshot.get_p99_us as f64,
                                                put_ops: snapshot.put_ops,
                                                put_bytes: snapshot.put_bytes,
                                                put_mean_us: snapshot.put_mean_us as f64,
                                                put_p50_us: snapshot.put_p50_us as f64,
                                                put_p90_us: snapshot.put_p90_us as f64,
                                                put_p95_us: snapshot.put_p95_us as f64,
                                                put_p99_us: snapshot.put_p99_us as f64,
                                                meta_ops: snapshot.meta_ops,
                                                meta_mean_us: snapshot.meta_mean_us as f64,
                                                meta_p50_us: snapshot.meta_p50_us as f64,
                                                meta_p90_us: snapshot.meta_p90_us as f64,
                                                meta_p99_us: snapshot.meta_p99_us as f64,
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
                                                // v0.8.9: Stage tracking from snapshot
                                                current_stage: snapshot.current_stage.to_proto_i32(),
                                                stage_name: snapshot.stage_name.clone(),
                                                stage_progress_current: snapshot.stage_progress_current,
                                                stage_progress_total: snapshot.stage_progress_total,
                                                stage_elapsed_s: snapshot.stage_elapsed_s,
                                                // v0.8.14: Concurrency from snapshot
                                                concurrency: snapshot.concurrency,
                                                // v0.8.26: Barrier coordination
                                                at_barrier: false,
                                                barrier_name: String::new(),
                                                barrier_sequence: 0,
                                                stage_summary: None,
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
                                    
                                    // v0.8.14: Mark completion sent BEFORE sending message
                                    // This allows control reader to recognize normal disconnect
                                    agent_state.mark_completion_sent().await;
                                    
                                    let completed_msg = LiveStats {
                                        agent_id: agent_id.clone(),
                                        timestamp_s: snapshot.timestamp_secs() as f64,
                                        get_ops: snapshot.get_ops,
                                        get_bytes: snapshot.get_bytes,
                                        get_mean_us: snapshot.get_mean_us as f64,
                                        get_p50_us: snapshot.get_p50_us as f64,
                                        get_p90_us: snapshot.get_p90_us as f64,
                                        get_p95_us: snapshot.get_p95_us as f64,
                                        get_p99_us: snapshot.get_p99_us as f64,
                                        put_ops: snapshot.put_ops,
                                        put_bytes: snapshot.put_bytes,
                                        put_mean_us: snapshot.put_mean_us as f64,
                                        put_p50_us: snapshot.put_p50_us as f64,
                                        put_p90_us: snapshot.put_p90_us as f64,
                                        put_p95_us: snapshot.put_p95_us as f64,
                                        put_p99_us: snapshot.put_p99_us as f64,
                                        meta_ops: snapshot.meta_ops,
                                        meta_mean_us: snapshot.meta_mean_us as f64,
                                        meta_p50_us: snapshot.meta_p50_us as f64,
                                        meta_p90_us: snapshot.meta_p90_us as f64,
                                        meta_p99_us: snapshot.meta_p99_us as f64,
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
                                        // v0.8.9: Stage tracking from snapshot
                                        current_stage: snapshot.current_stage.to_proto_i32(),
                                        stage_name: snapshot.stage_name.clone(),
                                        stage_progress_current: snapshot.stage_progress_current,
                                        stage_progress_total: snapshot.stage_progress_total,
                                        stage_elapsed_s: snapshot.stage_elapsed_s,
                                        // v0.8.14: Concurrency from snapshot
                                        concurrency: snapshot.concurrency,
                                        // v0.8.26: Barrier coordination
                                        at_barrier: false,
                                        barrier_name: String::new(),
                                        barrier_sequence: 0,
                                        stage_summary: None,
                                    };
                                    
                                    if tx_stats.send(completed_msg).await.is_err() {
                                        error!("Stats writer: Failed to send COMPLETED message");
                                    }
                                    
                                    // v0.8.12: Give gRPC stream time to flush completed message to controller
                                    // This prevents race condition where stream closes before controller receives final status
                                    debug!("Stats writer: Waiting {}s for COMPLETED message to flush", sai3_bench::constants::AGENT_ERROR_FLUSH_DELAY_SECS);
                                    tokio::time::sleep(tokio::time::Duration::from_secs(
                                        sai3_bench::constants::AGENT_ERROR_FLUSH_DELAY_SECS
                                    )).await;
                                    
                                    // Transition to Idle
                                    let _ = agent_state.transition_to(WorkloadState::Idle, "workload completed").await;
                                    
                                    break;
                                }
                                Some(Err(e)) => {
                                    error!("Stats writer: Workload failed: {}", e);
                                    
                                    // v0.8.10: Clean up progress bars with error message
                                    if let Some(pb) = prepare_pb.take() {
                                        pb.abandon_with_message(format!("[{}] aborted", agent_id));
                                    }
                                    if let Some(pb) = workload_pb.take() {
                                        pb.abandon_with_message(format!("[{}] error: {}", agent_id, e));
                                    }
                                    
                                    // Send ERROR message
                                    let error_msg = LiveStats {
                                        agent_id: agent_id.clone(),
                                        timestamp_s: 0.0,
                                        get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p90_us: 0.0, get_p95_us: 0.0, get_p99_us: 0.0,
                                        put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p90_us: 0.0, put_p95_us: 0.0, put_p99_us: 0.0,
                                        meta_ops: 0, meta_mean_us: 0.0, meta_p50_us: 0.0, meta_p90_us: 0.0, meta_p99_us: 0.0,
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
                                        // v0.8.9: Stage tracking (error state)
                                        current_stage: 0, stage_name: String::new(), stage_progress_current: 0, stage_progress_total: 0, stage_elapsed_s: 0.0,
                                        // v0.8.14: Concurrency (0 for error messages)
                                        concurrency: 0,
                                        // v0.8.26: Barrier coordination
                                        at_barrier: false,
                                        barrier_name: String::new(),
                                        barrier_sequence: 0,
                                        stage_summary: None,
                                    };
                                    
                                    if tx_stats.send(error_msg).await.is_err() {
                                        error!("Stats writer: Failed to send ERROR message");
                                    }
                                    
                                    // v0.8.12: Give gRPC stream time to flush error message to controller
                                    // This prevents race condition where stream closes before controller receives error
                                    info!("Stats writer: Waiting {}s for error message to flush", sai3_bench::constants::AGENT_ERROR_FLUSH_DELAY_SECS);
                                    tokio::time::sleep(tokio::time::Duration::from_secs(
                                        sai3_bench::constants::AGENT_ERROR_FLUSH_DELAY_SECS
                                    )).await;
                                    
                                    // Transition to Failed then Idle
                                    let _ = agent_state.set_error(e.to_string()).await;
                                    let _ = agent_state.transition_to(WorkloadState::Failed, &e).await;
                                    let _ = agent_state.transition_to(WorkloadState::Idle, "error sent").await;
                                    
                                    break;
                                }
                                None => {
                                    error!("Stats writer: Workload task terminated unexpectedly");
                                    
                                    // v0.8.10: Clean up progress bars
                                    if let Some(pb) = prepare_pb.take() {
                                        pb.abandon_with_message(format!("[{}] terminated", agent_id));
                                    }
                                    if let Some(pb) = workload_pb.take() {
                                        pb.abandon_with_message(format!("[{}] terminated", agent_id));
                                    }
                                    
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
                            
                            // v0.8.10: Update progress bars based on current stage
                            let current_stage = snapshot.current_stage.to_proto_i32();
                            
                            // Handle prepare phase (stage 1 = PREPARE)
                            if snapshot.in_prepare_phase {
                                // Create or update prepare progress bar
                                if prepare_pb.is_none() || last_prepare_total != snapshot.prepare_objects_total {
                                    // Finish old bar if switching totals
                                    if let Some(old_pb) = prepare_pb.take() {
                                        old_pb.finish_and_clear();
                                    }
                                    let pb = ProgressBar::new(snapshot.prepare_objects_total);
                                    pb.set_style(
                                        ProgressStyle::default_bar()
                                            .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({per_sec}) {msg}")
                                            .unwrap_or_else(|_| ProgressStyle::default_bar())
                                    );
                                    pb.set_message(format!("[{}] preparing...", agent_id));
                                    last_prepare_total = snapshot.prepare_objects_total;
                                    prepare_pb = Some(pb);
                                }
                                
                                if let Some(pb) = prepare_pb.as_ref() {
                                    pb.set_position(snapshot.prepare_objects_created);
                                    // Show throughput in message
                                    let elapsed = snapshot.elapsed_secs();
                                    if elapsed > 0.0 {
                                        let rate = snapshot.prepare_objects_created as f64 / elapsed;
                                        pb.set_message(format!(
                                            "[{}] preparing ({:.1} obj/s)", 
                                            agent_id, rate
                                        ));
                                    }
                                }
                            } else if prepare_pb.is_some() {
                                // Prepare phase finished - close the bar
                                if let Some(pb) = prepare_pb.take() {
                                    pb.finish_with_message(format!("[{}] prepare complete", agent_id));
                                }
                            }
                            
                            // Handle workload phase (stage 2 = WORKLOAD)
                            if !snapshot.in_prepare_phase && (snapshot.get_ops > 0 || snapshot.put_ops > 0 || current_stage == 2) {
                                if workload_pb.is_none() {
                                    // Create workload spinner
                                    let pb = ProgressBar::new_spinner();
                                    pb.set_style(
                                        ProgressStyle::default_spinner()
                                            .template("{spinner:.green} [{elapsed_precise}] {msg}")
                                            .unwrap_or_else(|_| ProgressStyle::default_spinner())
                                    );
                                    pb.set_message(format!("[{}] workload running...", agent_id));
                                    workload_pb = Some(pb);
                                }
                                
                                if let Some(pb) = workload_pb.as_ref() {
                                    pb.tick();
                                    // Show throughput stats
                                    let elapsed = snapshot.elapsed_secs();
                                    let total_ops = snapshot.get_ops + snapshot.put_ops;
                                    let total_bytes = snapshot.get_bytes + snapshot.put_bytes;
                                    if elapsed > 0.0 {
                                        let ops_rate = total_ops as f64 / elapsed;
                                        let mb_rate = total_bytes as f64 / (1024.0 * 1024.0) / elapsed;
                                        pb.set_message(format!(
                                            "[{}] GET:{} PUT:{} | {:.1} op/s | {:.1} MiB/s",
                                            agent_id, snapshot.get_ops, snapshot.put_ops,
                                            ops_rate, mb_rate
                                        ));
                                    }
                                }
                            }
                            
                            let running_msg = LiveStats {
                                agent_id: agent_id.clone(),
                                timestamp_s: snapshot.timestamp_secs() as f64,
                                get_ops: snapshot.get_ops,
                                get_bytes: snapshot.get_bytes,
                                get_mean_us: snapshot.get_mean_us as f64,
                                get_p50_us: snapshot.get_p50_us as f64,
                                get_p90_us: snapshot.get_p90_us as f64,
                                get_p95_us: snapshot.get_p95_us as f64,
                                get_p99_us: snapshot.get_p99_us as f64,
                                put_ops: snapshot.put_ops,
                                put_bytes: snapshot.put_bytes,
                                put_mean_us: snapshot.put_mean_us as f64,
                                put_p50_us: snapshot.put_p50_us as f64,
                                put_p90_us: snapshot.put_p90_us as f64,
                                put_p95_us: snapshot.put_p95_us as f64,
                                put_p99_us: snapshot.put_p99_us as f64,
                                meta_ops: snapshot.meta_ops,
                                meta_mean_us: snapshot.meta_mean_us as f64,
                                meta_p50_us: snapshot.meta_p50_us as f64,
                                meta_p90_us: snapshot.meta_p90_us as f64,
                                meta_p99_us: snapshot.meta_p99_us as f64,
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
                                // v0.8.9: Stage tracking from snapshot
                                current_stage: snapshot.current_stage.to_proto_i32(),
                                stage_name: snapshot.stage_name.clone(),
                                stage_progress_current: snapshot.stage_progress_current,
                                stage_progress_total: snapshot.stage_progress_total,
                                stage_elapsed_s: snapshot.stage_elapsed_s,
                                // v0.8.14: Concurrency from snapshot
                                concurrency: snapshot.concurrency,
                                // v0.8.26: Barrier coordination
                                at_barrier: false,
                                barrier_name: String::new(),
                                barrier_sequence: 0,
                                stage_summary: None,
                            };
                            sequence += 1;
                            
                            if tx_stats.send(running_msg).await.is_err() {
                                error!("Stats writer: Failed to send RUNNING message (controller disconnected?)");
                                
                                // v0.8.10: Clean up progress bars on disconnect
                                if let Some(pb) = prepare_pb.take() {
                                    pb.abandon_with_message(format!("[{}] disconnected", agent_id));
                                }
                                if let Some(pb) = workload_pb.take() {
                                    pb.abandon_with_message(format!("[{}] disconnected", agent_id));
                                }
                                
                                break;
                            }
                        }
                    }
                }
                
                // v0.8.10: Final cleanup of any remaining progress bars
                if let Some(pb) = prepare_pb {
                    pb.finish_and_clear();
                }
                if let Some(pb) = workload_pb {
                    pb.finish_and_clear();
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
                AGENT_READY_TIMEOUT_SECS,
                TIMEOUT_MONITOR_INTERVAL_SECS,
            };
            
            let mut last_message_time = tokio::time::Instant::now();
            let mut timeout_monitor = tokio::time::interval(
                tokio::time::Duration::from_secs(TIMEOUT_MONITOR_INTERVAL_SECS)
            );
            timeout_monitor.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            
            let mut loop_iteration = 0u64;
            loop {
                loop_iteration += 1;
                if loop_iteration.is_multiple_of(100) {
                    debug!("Control reader: loop iteration {} - waiting for messages", loop_iteration);
                }
                tokio::select! {
                    // Receive control messages from controller
                    control_msg_result = control_stream.next() => {
                        debug!("Control reader: Received message from control_stream (iteration {})", loop_iteration);
                        match control_msg_result {
                            Some(Ok(control_msg)) => {
                                debug!("Control reader: Got command={} barrier_name='{}' barrier_seq={}",
                                       control_msg.command, control_msg.barrier_name, control_msg.barrier_sequence);
                                last_message_time = tokio::time::Instant::now();
                                
                                match Command::try_from(control_msg.command) {
                                    Ok(Command::Ping) => {
                                        debug!("Control reader: Received PING keepalive");
                                        
                                        // Respond with ACKNOWLEDGE message
                                        let agent_id = agent_state_reader.get_agent_id().await.unwrap_or_else(|| "unknown".to_string());
                                        let ack_msg = LiveStats {
                                            agent_id: agent_id.clone(),
                                            timestamp_s: 0.0,
                                            get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p90_us: 0.0, get_p95_us: 0.0, get_p99_us: 0.0,
                                            put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p90_us: 0.0, put_p95_us: 0.0, put_p99_us: 0.0,
                                            meta_ops: 0, meta_mean_us: 0.0, meta_p50_us: 0.0, meta_p90_us: 0.0, meta_p99_us: 0.0,
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
                                            // v0.8.9: Stage tracking (ack has no stage)
                                            current_stage: 0, stage_name: String::new(), stage_progress_current: 0, stage_progress_total: 0, stage_elapsed_s: 0.0,
                                            // v0.8.14: Concurrency (0 for ack messages)
                                            concurrency: 0,
                                            // v0.8.26: Barrier coordination
                                            at_barrier: false,
                                            barrier_name: String::new(),
                                            barrier_sequence: 0,
                                            stage_summary: None,
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
                                    // v0.8.29: No longer use deprecated Validating state - validation is synchronous
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
                                    
                                    // v0.8.14: Reset completion_sent flag for new workload
                                    agent_state_reader.reset_completion_sent().await;
                                    
                                    // Store agent_index and num_agents for distributed cleanup
                                    agent_state_reader.set_agent_index(agent_index).await;
                                    agent_state_reader.set_num_agents(num_agents).await;
                                    agent_state_reader.set_shared_storage(shared_storage).await;  // v0.8.24
                                    
                                    // Store config_yaml for later use
                                    agent_state_reader.set_config_yaml(config_yaml.clone()).await;
                                    
                                    // Parse and validate config
                                    let mut config: sai3_bench::config::Config = match serde_yaml::from_str(&config_yaml) {
                                        Ok(c) => c,
                                        Err(e) => {
                                            error!("Control reader: Invalid YAML config: {}", e);
                                            agent_state_reader.set_error(format!("Invalid YAML: {}", e)).await;
                                            let _ = agent_state_reader.transition_to(WorkloadState::Failed, "invalid config").await;
                                            
                                            // Send ERROR via stats writer (it will pick up the error from state)
                                            let _ = tx_done.send(Err(format!("Invalid YAML config: {}", e))).await;
                                            return;
                                        }
                                    };
                                    
                                    if let Err(e) = config.apply_agent_prefix(&agent_id, &path_prefix, shared_storage) {
                                        error!("Control reader: Failed to apply agent prefix: {}", e);
                                        agent_state_reader.set_error(format!("Failed to apply path prefix: {}", e)).await;
                                        let _ = agent_state_reader.transition_to(WorkloadState::Failed, "prefix application failed").await;
                                        let _ = tx_done.send(Err(format!("Failed to apply path prefix: {}", e))).await;
                                        return;
                                    }
                                    
                                    // Validate config
                                    if let Err(e) = validate_workload_config(&config).await {
                                        error!("Control reader: Config validation failed: {}", e);
                                        agent_state_reader.set_error(format!("Config validation failed: {}", e)).await;
                                        let _ = agent_state_reader.transition_to(WorkloadState::Failed, "validation failed").await;
                                        let _ = tx_done.send(Err(format!("Config validation failed: {}", e))).await;
                                        return;
                                    }
                                    
                                    // Phase 2b: Parse YAML-driven stages if configured
                                    // If stages exist, agent will use AtStage state machine
                                    // If None, agent will use deprecated hardcoded states (backward compat)
                                    let stages = if let Some(ref distributed_config) = config.distributed {
                                        // Use config.duration as default_duration for stages (backward compat)
                                        let default_duration = config.duration;
                                        
                                        match distributed_config.get_sorted_stages(default_duration) {
                                            Ok(stage_vec) => {
                                                if !stage_vec.is_empty() {
                                                    info!("Control reader: Parsed {} YAML-driven stages", stage_vec.len());
                                                    for (i, stage) in stage_vec.iter().enumerate() {
                                                        info!("  Stage {}: {} ({})", i, stage.name, 
                                                            match &stage.config {
                                                                sai3_bench::config::StageSpecificConfig::Execute { .. } => "execute",
                                                                sai3_bench::config::StageSpecificConfig::Prepare { .. } => "prepare",
                                                                sai3_bench::config::StageSpecificConfig::Cleanup { .. } => "cleanup",
                                                                sai3_bench::config::StageSpecificConfig::Custom { .. } => "custom",
                                                                sai3_bench::config::StageSpecificConfig::Hybrid { .. } => "hybrid",
                                                                sai3_bench::config::StageSpecificConfig::Validation { .. } => "validation",
                                                            }
                                                        );
                                                    }
                                                    Some(stage_vec)
                                                } else {
                                                    info!("Control reader: No stages configured, using hardcoded state flow");
                                                    None
                                                }
                                            }
                                            Err(e) => {
                                                error!("Control reader: Failed to parse stages: {}", e);
                                                agent_state_reader.set_error(format!("Stage parsing failed: {}", e)).await;
                                                let _ = agent_state_reader.transition_to(WorkloadState::Failed, "stage parsing failed").await;
                                                let _ = tx_done.send(Err(format!("Stage parsing failed: {}", e))).await;
                                                return;
                                            }
                                        }
                                    } else {
                                        info!("Control reader: No distributed config, using hardcoded state flow");
                                        None
                                    };
                                    
                                    // Store stages in AgentState for use during execution
                                    agent_state_reader.set_stages(stages).await;
                                    
                                    // v0.8.29: Transition to AtStage { ready_for_next: true } to signal READY
                                    // This replaces deprecated PrepareReady state
                                    let ready_state = WorkloadState::AtStage {
                                        stage_index: 0,
                                        stage_name: "validated".to_string(),
                                        ready_for_next: true,  // Signals "READY" to stats writer
                                    };
                                    if let Err(e) = agent_state_reader.transition_to(ready_state, "validation passed").await {
                                        error!("Control reader: Failed to transition to validated state: {}", e);
                                        let _ = tx_done.send(Err(format!("State transition failed: {}", e))).await;
                                        return;
                                    }
                                    
                                    info!("Control reader: Config validated - waiting for coordinated START");
                                    
                                    // Stats writer will send READY message with agent_timestamp_ns
                                    // Controller will calculate clock offset and send second START with start_timestamp_ns
                                    
                                // v0.8.29: Check for AtStage { ready_for_next: true } instead of deprecated PrepareReady
                                } else if matches!(current_state, WorkloadState::AtStage { ready_for_next: true, .. }) && start_timestamp_ns != 0 {
                                    // Second START: Coordinated start with timestamp (controller released prepare barrier)
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
                                    
                                    // v0.8.29: No transition to deprecated Preparing state
                                    // The spawned workflow will immediately set its own AtStage state
                                    
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
                                    
                                    // Create live stats tracker with concurrency for total thread count display (v0.8.14)
                                    let tracker = Arc::new(sai3_bench::live_stats::LiveStatsTracker::new_with_concurrency(
                                        config.concurrency as u32
                                    ));
                                    
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
                                    let tx_stats_exec = tx_stats_for_control.clone();  // v0.8.26: For barrier coordination
                                    let agent_op_log_path_exec = agent_op_log_path_reader.clone();
                                    let tracker_for_prepare = tracker.clone();
                                    let agent_state_for_task = agent_state_reader.clone();
                                    
                                    tokio::spawn(async move {
                                        // Wire tracker into config for live stats collection
                                        config_exec.live_stats_tracker = Some(tracker_for_prepare.clone());
                                        
                                        // Get agent_index and num_agents for distributed prepare/cleanup
                                        let agent_index = agent_state_for_task.get_agent_index().await.unwrap_or(0) as usize;
                                        let num_agents = agent_state_for_task.get_num_agents().await.unwrap_or(1) as usize;
                                        let shared_storage = agent_state_for_task.get_shared_storage().await.unwrap_or(true);  // v0.8.24: Default to shared for backward compat
                                        
                                        // Phase 2b: Check if YAML-driven stages are configured
                                        let stages = agent_state_for_task.get_stages().await;
                                        
                                        // If stages exist, use stage-driven execution
                                        // Otherwise fall back to deprecated hardcoded prepare->execute->cleanup flow
                                        if let Some(stages_vec) = stages {
                                            info!("Using YAML-driven stage execution ({} stages)", stages_vec.len());
                                            
                                            // Execute stage-driven workflow (NEW)
                                            let stage_result = execute_stages_workflow(
                                                stages_vec,
                                                config_exec.clone(),
                                                agent_state_for_task.clone(),
                                                tracker_for_prepare.clone(),
                                                tx_done_exec.clone(),
                                                tx_prepare_exec.clone(),
                                                tx_stats_exec.clone(),  // v0.8.26: For barrier coordination
                                                final_op_log_path.clone(),
                                                agent_op_log_path_exec.clone(),
                                                agent_id_exec.clone(),
                                                agent_index,
                                                num_agents,
                                                shared_storage,
                                            ).await;
                                            
                                            // Send result to stats writer
                                            let _ = tx_done_exec.send(stage_result).await;
                                            return;  // Exit workload task
                                        }
                                        
                                        // YAML stages MUST be configured - error if we reach here
                                        error!("No stages configured - check yaml config");
                                        let _ = tx_done_exec.send(Err("No stages configured. Enable barrier_sync to use YAML stages.".to_string())).await;
                                        
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
                                    get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p90_us: 0.0, get_p95_us: 0.0, get_p99_us: 0.0,
                                    put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p90_us: 0.0, put_p95_us: 0.0, put_p99_us: 0.0,
                                    meta_ops: 0, meta_mean_us: 0.0, meta_p50_us: 0.0, meta_p90_us: 0.0, meta_p99_us: 0.0,
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
                                    // v0.8.9: Stage tracking (aborted)
                                    current_stage: 0, stage_name: String::new(), stage_progress_current: 0, stage_progress_total: 0, stage_elapsed_s: 0.0,
                                    // v0.8.14: Concurrency (0 for aborted messages)
                                    concurrency: 0,
                                    // v0.8.26: Barrier coordination
                                    at_barrier: false,
                                    barrier_name: String::new(),
                                    barrier_sequence: 0,
                                    stage_summary: None,
                                };
                                
                                if let Err(e) = tx_stats_for_control.send(aborted_msg).await {
                                    error!("Control reader: Failed to send ABORTED status: {}", e);
                                }
                                
                                // v0.8.12: Give gRPC stream time to flush abort message to controller
                                // before transitioning to Idle and closing the stream
                                info!("Control reader: Waiting {}s for ABORTED message to flush", sai3_bench::constants::AGENT_ERROR_FLUSH_DELAY_SECS);
                                tokio::time::sleep(tokio::time::Duration::from_secs(
                                    sai3_bench::constants::AGENT_ERROR_FLUSH_DELAY_SECS
                                )).await;
                                
                                // Reset to Idle
                                if let Err(e) = agent_state_reader.transition_to(WorkloadState::Idle, "abort complete").await {
                                    error!("Control reader: Failed to transition to Idle after abort: {}", e);
                                }
                                
                                info!("Control reader: Abort complete, agent returned to Idle");
                            }
                            Ok(Command::Preflight) => {
                                info!("Control reader: Received PREFLIGHT command - running validation");
                                
                                // Extract config from PREFLIGHT message (same fields as START)
                                let config_yaml = control_msg.config_yaml;
                                let agent_id = control_msg.agent_id;
                                
                                // Run filesystem validation
                                let fs_summary = match serde_yaml::from_str::<sai3_bench::config::Config>(&config_yaml) {
                                    Ok(config) => {
                                        // Extract filesystem path from target URI
                                        if let Some(fs_path) = extract_filesystem_path_from_config(&config) {
                                            match sai3_bench::preflight::filesystem::validate_filesystem(
                                                &fs_path,
                                                true,  // check_write = true
                                                Some(1024 * 1024),  // require at least 1 MB free space
                                            ).await {
                                                Ok(summary) => summary,
                                                Err(e) => {
                                                    error!("Control reader: Filesystem validation error: {}", e);
                                                    sai3_bench::preflight::ValidationSummary::new(vec![
                                                        sai3_bench::preflight::ValidationResult {
                                                            level: sai3_bench::preflight::ResultLevel::Error,
                                                            error_type: Some(sai3_bench::preflight::ErrorType::System),
                                                            message: format!("Validation error: {}", e),
                                                            suggestion: "Check filesystem permissions and disk space".to_string(),
                                                            details: None,
                                                            test_phase: "validation".to_string(),
                                                        }
                                                    ])
                                                }
                                            }
                                        } else {
                                            // Not a filesystem target - skip validation
                                            sai3_bench::preflight::ValidationSummary::new(vec![
                                                sai3_bench::preflight::ValidationResult {
                                                    level: sai3_bench::preflight::ResultLevel::Info,
                                                    error_type: None,
                                                    message: "Skipping filesystem validation (target is object storage)".to_string(),
                                                    suggestion: String::new(),
                                                    details: None,
                                                    test_phase: "filesystem".to_string(),
                                                }
                                            ])
                                        }
                                    }
                                    Err(e) => {
                                        // Config parse error becomes a validation error
                                        error!("Control reader: Failed to parse config for pre-flight: {}", e);
                                        sai3_bench::preflight::ValidationSummary::new(vec![
                                            sai3_bench::preflight::ValidationResult {
                                                level: sai3_bench::preflight::ResultLevel::Error,
                                                error_type: Some(sai3_bench::preflight::ErrorType::Configuration),
                                                message: format!("YAML parse error: {}", e),
                                                suggestion: "Check YAML syntax and field names".to_string(),
                                                details: None,
                                                test_phase: "config_parse".to_string(),
                                            }
                                        ])
                                    }
                                };
                                
                                // Convert to proto and send via LiveStats status=7 (PREFLIGHT_RESULT)
                                // We encode validation results in error_message as JSON for now
                                // TODO Phase 2: Add proper PreFlightResult field to LiveStats message
                                let passed = !fs_summary.has_errors();
                                let results_json = serde_json::json!({
                                    "passed": passed,
                                    "error_count": fs_summary.error_count(),
                                    "warning_count": fs_summary.warning_count(),
                                    "info_count": fs_summary.info_count(),
                                    "results": fs_summary.results.iter().map(|r| {
                                        serde_json::json!({
                                            "level": format!("{:?}", r.level),
                                            "error_type": format!("{:?}", r.error_type),
                                            "message": r.message,
                                            "suggestion": r.suggestion,
                                            "details": r.details,
                                            "test_phase": r.test_phase,
                                        })
                                    }).collect::<Vec<_>>()
                                }).to_string();
                                
                                let preflight_msg = LiveStats {
                                    agent_id,
                                    timestamp_s: 0.0,
                                    get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p90_us: 0.0, get_p95_us: 0.0, get_p99_us: 0.0,
                                    put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p90_us: 0.0, put_p95_us: 0.0, put_p99_us: 0.0,
                                    meta_ops: 0, meta_mean_us: 0.0, meta_p50_us: 0.0, meta_p90_us: 0.0, meta_p99_us: 0.0,
                                    elapsed_s: 0.0,
                                    completed: false,
                                    final_summary: None,
                                    status: 7,  // PREFLIGHT_RESULT (new status code)
                                    error_message: results_json,
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
                                    current_stage: 0, stage_name: String::new(), stage_progress_current: 0, stage_progress_total: 0, stage_elapsed_s: 0.0,
                                    concurrency: 0,
                                    // v0.8.26: Barrier coordination
                                    at_barrier: false,
                                    barrier_name: String::new(),
                                    barrier_sequence: 0,
                                    stage_summary: None,
                                };
                                
                                if let Err(e) = tx_stats_for_control.send(preflight_msg).await {
                                    error!("Control reader: Failed to send PREFLIGHT_RESULT: {}", e);
                                    break;
                                }
                                
                                info!("Control reader: Pre-flight validation completed (passed={}, errors={}, warnings={})",
                                      passed, fs_summary.error_count(), fs_summary.warning_count());
                            }
                            Ok(Command::Acknowledge) => {
                                debug!("Control reader: Received ACK for sequence {}", control_msg.ack_sequence);
                                // Controller acknowledged our message - could use this for reliability
                            }
                            Ok(Command::ReleaseBarrier) => {
                                // v0.8.26: Controller releases barrier - agent may proceed to next stage
                                info!(
                                    "Control reader: Received RELEASE_BARRIER for '{}' (sequence {})",
                                    control_msg.barrier_name,
                                    control_msg.barrier_sequence
                                );
                                // Notify the stage execution loop that this barrier is released
                                agent_state_reader.notify_barrier_release(
                                    control_msg.barrier_name.clone(),
                                    control_msg.barrier_sequence,
                                );
                            }
                            Ok(Command::Goodbye) => {
                                // v0.8.29: Graceful disconnect - controller is leaving, agent should return to Idle
                                info!("Control reader: Received GOODBYE - controller is disconnecting gracefully");
                                
                                // Transition to Idle state (ready for new connection)
                                if let Err(e) = agent_state_reader.transition_to(WorkloadState::Idle, "graceful disconnect from controller").await {
                                    debug!("Control reader: Already in Idle state or transition failed: {}", e);
                                }
                                
                                // Break out of control reader loop - stream will close cleanly
                                break;
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
                            // v0.8.13: Downgrade to debug - h2 errors after completion are normal
                            // Controller disconnects while agent still has buffered data
                            debug!("Control reader: Stream closed after workload completion (normal h2 shutdown)");
                        } else {
                            error!("Control reader: Error receiving control message: {}", e);
                            
                            // Connection lost during active workload - send error and exit
                            let agent_id = agent_state_reader.get_agent_id().await.unwrap_or_else(|| "unknown".to_string());
                            let error_msg = LiveStats {
                                agent_id,
                                timestamp_s: 0.0,
                                get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p90_us: 0.0, get_p95_us: 0.0, get_p99_us: 0.0,
                                put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p90_us: 0.0, put_p95_us: 0.0, put_p99_us: 0.0,
                                meta_ops: 0, meta_mean_us: 0.0, meta_p50_us: 0.0, meta_p90_us: 0.0, meta_p99_us: 0.0,
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
                                // v0.8.9: Stage tracking (connection error)
                                current_stage: 0, stage_name: String::new(), stage_progress_current: 0, stage_progress_total: 0, stage_elapsed_s: 0.0,
                                // v0.8.14: Concurrency (0 for error messages)
                                concurrency: 0,
                                // v0.8.26: Barrier coordination
                                at_barrier: false,
                                barrier_name: String::new(),
                                barrier_sequence: 0,
                                stage_summary: None,
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
            
            // Timeout monitoring - only check for hung READY state (awaiting coordinated start)
            // IMPORTANT: IDLE state has NO timeout - agents wait indefinitely for controllers
            // This allows agents to run continuously as long-lived services
            _ = timeout_monitor.tick() => {
                let current_state = agent_state_reader.get_state().await;
                let elapsed = last_message_time.elapsed();
                
                // v0.8.29: READY state is now AtStage { stage_name: "validated", ready_for_next: true }
                // Only this state has a timeout - waiting too long for coordinated START
                // IDLE state: No timeout (agent waits forever for controller)
                // AtStage { ready_for_next: false }: No timeout (actively executing stage)
                let timeout_exceeded = match &current_state {
                    WorkloadState::AtStage { stage_name, ready_for_next: true, .. } 
                        if stage_name == "validated" => elapsed.as_secs() > AGENT_READY_TIMEOUT_SECS,
                    _ => false,  // All other states have no timeout
                };
                
                if timeout_exceeded {
                    error!(
                        "Control reader: Timeout in READY state ({}s since last message, limit {}s) - controller failed to send coordinated START",
                        elapsed.as_secs(),
                        AGENT_READY_TIMEOUT_SECS,
                    );
                    
                    // Send ERROR status
                    let agent_id = agent_state_reader.get_agent_id().await.unwrap_or_else(|| "unknown".to_string());
                    let timeout_msg = LiveStats {
                        agent_id,
                        timestamp_s: 0.0,
                        get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p90_us: 0.0, get_p95_us: 0.0, get_p99_us: 0.0,
                        put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p90_us: 0.0, put_p95_us: 0.0, put_p99_us: 0.0,
                        meta_ops: 0, meta_mean_us: 0.0, meta_p50_us: 0.0, meta_p90_us: 0.0, meta_p99_us: 0.0,
                        elapsed_s: 0.0,
                        completed: false,
                        final_summary: None,
                        status: 3,  // ERROR
                        error_message: format!("Timeout in READY state after {}s - controller did not send coordinated START", elapsed.as_secs()),
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
                        // v0.8.9: Stage tracking (timeout)
                        current_stage: 0, stage_name: String::new(), stage_progress_current: 0, stage_progress_total: 0, stage_elapsed_s: 0.0,
                        // v0.8.14: Concurrency (0 for timeout messages)
                        concurrency: 0,
                        // v0.8.26: Barrier coordination
                        at_barrier: false,
                        barrier_name: String::new(),
                        barrier_sequence: 0,
                        stage_summary: None,
                    };
                    
                    let _ = tx_stats_for_control.send(timeout_msg).await;
                    
                    // Transition to Failed, then immediately back to Idle so agent can accept new connections
                    agent_state_reader.set_error("READY state timeout".to_string()).await;
                    let _ = agent_state_reader.transition_to(WorkloadState::Failed, "READY state timeout").await;
                    let _ = agent_state_reader.transition_to(WorkloadState::Idle, "auto-reset after READY timeout").await;
                    
                    break;
                }
            }
        }
    }
            
            info!("Control reader: Stream ended (controller disconnected)");
            
            // Check if workload completed successfully vs abnormal disconnect
            let current_state = agent_state_reader.get_state().await;
            // v0.8.29: completion_sent was used for legacy Running state race condition check
            // Kept for future reference if needed for AtStage races
            let _completion_sent = agent_state_reader.is_completion_sent().await;
            
            match current_state {
                WorkloadState::Idle => {
                    // Workload already completed - this is normal controller disconnect after receiving results
                    info!("Control reader: Normal disconnect (workload completed, agent idle)");
                }
                // ==========================================================================
                // LEGACY STATES COMMENTED OUT - v0.8.29 YAML-driven stages only
                // ==========================================================================
                // WorkloadState::Running if completion_sent => {
                //     // v0.8.14: COMPLETED message was sent but state hasn't transitioned yet
                //     // This is a race condition between stats writer and control reader - NOT abnormal
                //     info!("Control reader: Normal disconnect (completion sent, state transition pending)");
                //     // Don't send abort - let stats writer complete its transition
                // }
                // WorkloadState::Ready | WorkloadState::Running | WorkloadState::Aborting => {
                //     // Abnormal disconnect during workload - send abort and transition to Failed
                //     warn!("Control reader: Abnormal disconnect during {:?} state", current_state);
                //     agent_state_reader.send_abort();
                //     let _ = agent_state_reader.transition_to(WorkloadState::Failed, "controller disconnected during workload").await;
                //     let _ = agent_state_reader.transition_to(WorkloadState::Idle, "cleanup after abnormal disconnect").await;
                // }
                WorkloadState::Aborting => {
                    // Aborting is still valid - disconnect during abort
                    warn!("Control reader: Disconnect during abort");
                    let _ = agent_state_reader.transition_to(WorkloadState::Failed, "disconnect during abort").await;
                    let _ = agent_state_reader.transition_to(WorkloadState::Idle, "cleanup after abort disconnect").await;
                }
                WorkloadState::Failed => {
                    // Already failed - just log
                    info!("Control reader: Disconnect after failure (expected)");
                }
                
                WorkloadState::Completed => {
                    // v0.8.28: Normal disconnect - workload completed successfully
                    info!("Control reader: Normal disconnect (workload completed)");
                }
                
                // v0.8.28: AtStage with ready_for_next=true means we're waiting at a barrier
                // If final stage completed (cleanup), this is a normal disconnect
                WorkloadState::AtStage { ready_for_next: true, ref stage_name, .. } => {
                    info!("Control reader: Disconnect while at barrier '{}' (normal if final stage)", stage_name);
                    // Transition to Idle for cleanup (no abort needed - we're at a barrier, not executing)
                    let _ = agent_state_reader.transition_to(WorkloadState::Idle, "disconnect at barrier").await;
                }
                
                // v0.8.28: AtStage with ready_for_next=false means actively executing a stage
                WorkloadState::AtStage { ready_for_next: false, ref stage_name, .. } => {
                    warn!("Control reader: Abnormal disconnect during stage '{}'", stage_name);
                    agent_state_reader.send_abort();
                    let _ = agent_state_reader.transition_to(WorkloadState::Failed, "controller disconnected during stage").await;
                    let _ = agent_state_reader.transition_to(WorkloadState::Idle, "cleanup after stage disconnect").await;
                }
                
                // ============================================================================
                // CATCH-ALL ARM FOR v0.8.25 BARRIER SYNC STATES (TEMPORARY FAILSAFE)
                // ============================================================================
                // TODO v0.8.25: Replace this catch-all with proper disconnect handling for each new state:
                //
                // WorkloadState::Validating => {
                //     // Disconnect during pre-flight validation - treat as abnormal
                //     warn!("Control reader: Disconnect during pre-flight validation");
                //     agent_state_reader.send_abort();
                //     let _ = agent_state_reader.transition_to(WorkloadState::Failed, "controller disconnected during validation").await;
                //     let _ = agent_state_reader.transition_to(WorkloadState::Idle, "cleanup after disconnect").await;
                // }
                //
                // WorkloadState::PrepareReady | WorkloadState::ExecuteReady | WorkloadState::CleanupReady => {
                //     // Disconnect while waiting at barrier - clean transition to Idle (no work in progress)
                //     info!("Control reader: Disconnect while at barrier - resetting to Idle");
                //     let _ = agent_state_reader.transition_to(WorkloadState::Idle, "barrier wait cancelled by disconnect").await;
                // }
                //
                // WorkloadState::Preparing | WorkloadState::Executing | WorkloadState::Cleaning => {
                //     // Disconnect during active phase - treat as abnormal, send abort
                //     warn!("Control reader: Abnormal disconnect during {:?} phase", current_state);
                //     agent_state_reader.send_abort();
                //     let _ = agent_state_reader.transition_to(WorkloadState::Failed, "controller disconnected during phase").await;
                //     let _ = agent_state_reader.transition_to(WorkloadState::Idle, "cleanup after abnormal disconnect").await;
                // }
                //
                // WorkloadState::Completed => {
                //     // Workload completed successfully - normal disconnect
                //     info!("Control reader: Normal disconnect (workload completed)");
                // }
                //
                // For now, treat all unknown states as potentially active workload states that need abort.
                // This ensures we don't leave agents in bad states if the state machine evolves.
                // ============================================================================
                // v0.8.29: Catch-all now unreachable - all WorkloadState variants are exhaustively handled
                // _ => {
                //     warn!("Control reader: Disconnect in unhandled state {:?} - treating as abnormal", current_state);
                //     
                //     // Conservative approach: assume state is active and needs abort/cleanup
                //     agent_state_reader.send_abort();
                //     let _ = agent_state_reader.transition_to(WorkloadState::Failed, "controller disconnected in unknown state").await;
                //     let _ = agent_state_reader.transition_to(WorkloadState::Idle, "cleanup after disconnect").await;
                // }
            }
        });
        
        // Convert stats channel to stream, wrapping each LiveStats in Ok()
        let output_stream = tokio_stream::wrappers::ReceiverStream::new(rx_stats)
            .map(Ok);
        
        Ok(Response::new(Box::pin(output_stream)))
    }
}

// Phase 2b: YAML-driven stage execution workflow
// Executes stages in order, checking completion criteria and transitioning state
#[allow(dead_code)]  // Will be used when stages are configured
async fn execute_stages_workflow(
    stages: Vec<sai3_bench::config::StageConfig>,
    mut config: sai3_bench::config::Config,
    agent_state: AgentState,
    tracker: Arc<sai3_bench::live_stats::LiveStatsTracker>,
    _tx_done: tokio::sync::mpsc::Sender<Result<sai3_bench::workload::Summary, String>>,
    tx_prepare: tokio::sync::mpsc::Sender<sai3_bench::workload::PrepareMetrics>,
    tx_stats: tokio::sync::mpsc::Sender<LiveStats>,  // v0.8.26: For barrier coordination
    final_op_log_path: Option<std::path::PathBuf>,
    agent_op_log_path: Option<std::path::PathBuf>,
    agent_id: String,
    agent_index: usize,
    num_agents: usize,
    shared_storage: bool,
) -> Result<sai3_bench::workload::Summary, String> {
    info!("Starting YAML-driven stage workflow with {} stages", stages.len());
    
    // Wire tracker into config
    config.live_stats_tracker = Some(tracker.clone());
    
    // Storage for prepared objects (needed for cleanup stages)
    let mut prepared_objects: Vec<sai3_bench::prepare::PreparedObject> = Vec::new();
    let mut tree_manifest = None;  // Type inferred from prepare_objects return
    
    // Iterate through stages in order
    for (stage_index, stage) in stages.iter().enumerate() {
        info!("=== Stage {}/{}: {} ===", stage_index + 1, stages.len(), stage.name);
        
        // Transition to AtStage (ready_for_next=false, executing)
        let at_stage = WorkloadState::AtStage {
            stage_index,
            stage_name: stage.name.clone(),
            ready_for_next: false,
        };
        
        if let Err(e) = agent_state.transition_to(at_stage, &format!("starting stage {}", stage.name)).await {
            error!("Failed to transition to AtStage: {}", e);
            return Err(format!("State transition failed: {}", e));
        }
        
        // v0.8.27: Reset stats for this stage to get accurate per-stage metrics
        let live_stage = match &stage.config {
            sai3_bench::config::StageSpecificConfig::Prepare { .. } => sai3_bench::live_stats::WorkloadStage::Prepare,
            sai3_bench::config::StageSpecificConfig::Execute { .. } => sai3_bench::live_stats::WorkloadStage::Workload,
            sai3_bench::config::StageSpecificConfig::Cleanup { .. } => sai3_bench::live_stats::WorkloadStage::Cleanup,
            sai3_bench::config::StageSpecificConfig::Custom { .. } => sai3_bench::live_stats::WorkloadStage::Custom,
            sai3_bench::config::StageSpecificConfig::Hybrid { .. } => sai3_bench::live_stats::WorkloadStage::Custom,
            sai3_bench::config::StageSpecificConfig::Validation { .. } => sai3_bench::live_stats::WorkloadStage::Custom,
        };
        tracker.reset_for_stage(&stage.name, live_stage);
        
        // v0.8.28: Capture per-bucket histogram data from stage execution
        // This is used to create per-size-bucket rows in the TSV output (matching legacy format)
        let mut stage_get_hists: Option<sai3_bench::metrics::OpHists> = None;
        let mut stage_put_hists: Option<sai3_bench::metrics::OpHists> = None;
        let mut stage_meta_hists: Option<sai3_bench::metrics::OpHists> = None;
        let mut stage_get_bins: Option<sai3_bench::workload::SizeBins> = None;
        let mut stage_put_bins: Option<sai3_bench::workload::SizeBins> = None;
        let mut stage_meta_bins: Option<sai3_bench::workload::SizeBins> = None;
        
        // Execute stage based on type
        match &stage.config {
            sai3_bench::config::StageSpecificConfig::Prepare { .. } => {
                info!("Executing PREPARE stage: {}", stage.name);
                
                // Execute prepare logic (similar to existing prepare phase)
                if let Some(ref prepare_config) = config.prepare {
                    use sai3_bench::live_stats::WorkloadStage;
                    let expected_objects: u64 = prepare_config.ensure_objects.iter().map(|e| e.count).sum();
                    tracker.set_stage(WorkloadStage::Prepare, expected_objects);
                    
                    use std::sync::{Arc as StdArc, Mutex as StdMutex};
                    use std::collections::HashMap;
                    let prepare_multi_ep_cache: sai3_bench::workload::MultiEndpointCache = 
                        StdArc::new(StdMutex::new(HashMap::new()));
                    
                    match sai3_bench::workload::prepare_objects(
                        prepare_config,
                        Some(&config.workload),
                        Some(tracker.clone()),
                        config.multi_endpoint.as_ref(),
                        &prepare_multi_ep_cache,
                        config.concurrency,
                        agent_index,
                        num_agents,
                        shared_storage,
                    ).await {
                        Ok((prepared, manifest, prepare_metrics)) => {
                            info!("Prepare stage complete: {} objects", prepared.len());
                            prepared_objects = prepared;
                            tree_manifest = manifest;
                            
                            // v0.8.28: Capture per-bucket histogram data for stage summary
                            // PREPARE only has PUT ops (object creation)
                            stage_put_hists = Some(prepare_metrics.put_hists.clone());
                            stage_put_bins = Some(prepare_metrics.put_bins.clone());
                            
                            // Send prepare metrics via channel (for legacy flow compatibility)
                            let _ = tx_prepare.send(prepare_metrics).await;
                            // NOTE: Do NOT reset tracker here - stats must persist until
                            // stage summary is computed at barrier. reset_for_stage() at
                            // the start of the NEXT stage will clear stats.
                        }
                        Err(e) => {
                            return Err(format!("Prepare stage '{}' failed: {}", stage.name, e));
                        }
                    }
                } else {
                    warn!("PREPARE stage '{}' configured but config.prepare is None", stage.name);
                }
            }
            
            sai3_bench::config::StageSpecificConfig::Execute { duration } => {
                info!("Executing EXECUTE stage: {} (duration: {:?})", stage.name, duration);
                
                // Set stage for progress tracking
                use sai3_bench::live_stats::WorkloadStage;
                tracker.set_stage(WorkloadStage::Workload, 0);  // time-based
                
                // Setup operation logger if configured
                if let Some(op_log_base) = final_op_log_path.as_ref().or(agent_op_log_path.as_ref()) {
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
                        parent.join(format!("{}_{}.tsv.zst", base_name, agent_id))
                    } else {
                        op_log_base.join(format!("oplog_{}.tsv.zst", agent_id))
                    };
                    
                    info!("Initializing operation logger: {}", op_log_path.display());
                    if let Err(e) = sai3_bench::workload::init_operation_logger(&op_log_path) {
                        error!("Failed to initialize operation logger: {}", e);
                    }
                }
                
                // Override config.duration with stage duration
                config.duration = *duration;
                
                // Run workload
                match sai3_bench::workload::run(&config, tree_manifest.clone()).await {
                    Ok(summary) => {
                        info!("Execute stage complete");
                        
                        // v0.8.28: Capture per-bucket histogram data for stage summary
                        // EXECUTE has GET, PUT, and META ops
                        stage_get_hists = Some(summary.get_hists.clone());
                        stage_put_hists = Some(summary.put_hists.clone());
                        stage_meta_hists = Some(summary.meta_hists.clone());
                        stage_get_bins = Some(summary.get_bins.clone());
                        stage_put_bins = Some(summary.put_bins.clone());
                        stage_meta_bins = Some(summary.meta_bins.clone());
                        
                        // Finalize oplog
                        if final_op_log_path.is_some() || agent_op_log_path.is_some() {
                            if let Err(e) = sai3_bench::workload::finalize_operation_logger() {
                                error!("Failed to finalize operation logger: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        return Err(format!("Execute stage '{}' failed: {}", stage.name, e));
                    }
                }
            }
            
            sai3_bench::config::StageSpecificConfig::Cleanup { .. } => {
                info!("Executing CLEANUP stage: {}", stage.name);
                
                use sai3_bench::live_stats::WorkloadStage;
                tracker.set_stage(WorkloadStage::Cleanup, prepared_objects.len() as u64);
                
                if let Some(ref prepare_config) = config.prepare {
                    let cleanup_mode = prepare_config.cleanup_mode;
                    
                    if let Err(e) = sai3_bench::cleanup::cleanup_prepared_objects(
                        &prepared_objects,
                        tree_manifest.as_ref(),
                        agent_index,
                        num_agents,
                        cleanup_mode,
                        Some(tracker.clone()),
                    ).await {
                        return Err(format!("Cleanup stage '{}' failed: {}", stage.name, e));
                    }
                    
                    info!("Cleanup stage complete");
                } else {
                    warn!("CLEANUP stage '{}' configured but config.prepare is None", stage.name);
                }
            }
            
            sai3_bench::config::StageSpecificConfig::Custom { command, args } => {
                info!("Executing CUSTOM stage: {} (command: {} {:?})", stage.name, command, args);
                
                use sai3_bench::live_stats::WorkloadStage;
                tracker.set_stage(WorkloadStage::Custom, 0);
                
                // Execute custom command
                let output = tokio::process::Command::new(command)
                    .args(args)
                    .output()
                    .await
                    .map_err(|e| format!("Custom command '{}' failed to execute: {}", command, e))?;
                
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!(
                        "Custom command '{}' exited with status {}: {}",
                        command, output.status, stderr
                    ));
                }
                
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.is_empty() {
                    info!("Custom command output: {}", stdout.trim());
                }
                
                info!("Custom stage complete");
            }
            
            sai3_bench::config::StageSpecificConfig::Hybrid { max_duration, expected_tasks } => {
                info!("Executing HYBRID stage: {} (max_duration: {:?}, expected_tasks: {:?})", 
                      stage.name, max_duration, expected_tasks);
                
                // Hybrid stage: prepare (if needed) + execute + cleanup (if needed)
                // For now, just run execute portion with optional duration override
                use sai3_bench::live_stats::WorkloadStage;
                tracker.set_stage(WorkloadStage::Workload, expected_tasks.unwrap_or(0) as u64);
                
                // Override duration if specified
                if let Some(duration) = max_duration {
                    config.duration = *duration;
                }
                
                // Run workload
                match sai3_bench::workload::run(&config, tree_manifest.clone()).await {
                    Ok(_summary) => {
                        info!("Hybrid stage complete");
                    }
                    Err(e) => {
                        return Err(format!("Hybrid stage '{}' failed: {}", stage.name, e));
                    }
                }
            }
            
            sai3_bench::config::StageSpecificConfig::Validation => {
                info!("Executing VALIDATION stage: {} (timeout: {:?}s)", stage.name, stage.timeout_secs);
                
                // Validation stage completes immediately - actual validation happened
                // at RPC level (pre-flight checks) before YAML stages execute
                // This stage exists for barrier synchronization purposes
                info!("Validation stage complete (pre-flight checks already passed)");
            }
        }
        
        // Check completion criteria
        match stage.completion {
            sai3_bench::config::CompletionCriteria::Duration => {
                // Duration-based stages complete naturally via workload::run() timeout
                info!("Stage '{}' completed via Duration criteria", stage.name);
            }
            sai3_bench::config::CompletionCriteria::TasksDone => {
                // Task-based stages (prepare/cleanup) complete when all tasks finish
                let snapshot = tracker.snapshot();
                let total_ops = snapshot.get_ops + snapshot.put_ops + snapshot.meta_ops;
                info!("Stage '{}' completed via TasksDone criteria ({} ops)", stage.name, total_ops);
            }
            sai3_bench::config::CompletionCriteria::ScriptExit => {
                // Custom stages complete when script exits (already handled above)
                info!("Stage '{}' completed via ScriptExit criteria", stage.name);
            }
            sai3_bench::config::CompletionCriteria::ValidationPassed => {
                // Validation stages complete when checks pass
                // For now, assume validation passed if we got here without error
                info!("Stage '{}' completed via ValidationPassed criteria", stage.name);
            }
            sai3_bench::config::CompletionCriteria::DurationOrTasks => {
                // Hybrid criterion - whichever completes first
                let snapshot = tracker.snapshot();
                let total_ops = snapshot.get_ops + snapshot.put_ops + snapshot.meta_ops;
                info!("Stage '{}' completed via DurationOrTasks criteria ({} ops, {:?} elapsed)", 
                      stage.name, total_ops, snapshot.elapsed);
            }
        }
        
        // Transition to ready_for_next=true (at barrier)
        let ready_state = WorkloadState::AtStage {
            stage_index,
            stage_name: stage.name.clone(),
            ready_for_next: true,
        };
        
        if let Err(e) = agent_state.transition_to(ready_state, &format!("stage {} complete, at barrier", stage.name)).await {
            error!("Failed to transition to ready state: {}", e);
            return Err(format!("State transition failed: {}", e));
        }
        
        info!("Stage {}/{} complete, waiting at barrier", stage_index + 1, stages.len());
        
        // v0.8.27: Compute stage summary before barrier wait
        // This ensures stats are captured and sent immediately, preventing data loss
        let stage_snapshot = tracker.snapshot();
        let stage_type = match &stage.config {
            sai3_bench::config::StageSpecificConfig::Prepare { .. } => "prepare",
            sai3_bench::config::StageSpecificConfig::Execute { .. } => "execute",
            sai3_bench::config::StageSpecificConfig::Cleanup { .. } => "cleanup",
            sai3_bench::config::StageSpecificConfig::Custom { .. } => "custom",
            sai3_bench::config::StageSpecificConfig::Hybrid { .. } => "hybrid",
            sai3_bench::config::StageSpecificConfig::Validation { .. } => "validation",
        };
        
        // v0.8.28: Serialize per-bucket histograms for accurate aggregation
        // This matches the legacy format with 9 size buckets per operation type
        let serialize_ophists = |ophists: &sai3_bench::metrics::OpHists| -> Vec<u8> {
            use hdrhistogram::serialization::{Serializer, V2Serializer};
            let mut serializer = V2Serializer::new();
            let mut bytes = Vec::new();
            for bucket_hist in ophists.buckets.iter() {
                let hist = bucket_hist.lock().unwrap();
                if let Err(e) = serializer.serialize(&*hist, &mut bytes) {
                    warn!("Failed to serialize histogram bucket: {}", e);
                }
            }
            bytes
        };
        
        // Convert SizeBins to proto SizeBins
        let convert_size_bins = |bins: &sai3_bench::workload::SizeBins| -> crate::pb::iobench::SizeBins {
            use crate::pb::iobench::{SizeBins as ProtoSizeBins, SizeBucketData};
            let mut buckets = Vec::new();
            for (idx, (ops, bytes)) in &bins.by_bucket {
                buckets.push(SizeBucketData {
                    bucket_idx: *idx as u32,
                    ops: *ops,
                    bytes: *bytes,
                });
            }
            ProtoSizeBins { buckets }
        };
        
        // Serialize histograms - prefer captured per-bucket data, fall back to tracker
        let (hist_get, hist_put, hist_meta) = if stage_get_hists.is_some() || stage_put_hists.is_some() {
            // Use captured per-bucket histograms from Summary/PrepareMetrics
            let get = stage_get_hists.as_ref().map(serialize_ophists).unwrap_or_default();
            let put = stage_put_hists.as_ref().map(serialize_ophists).unwrap_or_default();
            let meta = stage_meta_hists.as_ref().map(serialize_ophists).unwrap_or_default();
            (get, put, meta)
        } else {
            // Fall back to tracker's single histograms (for stages without Summary)
            tracker.serialize_histograms()
                .unwrap_or_else(|e| {
                    warn!("Failed to serialize stage histograms: {}", e);
                    (Vec::new(), Vec::new(), Vec::new())
                })
        };
        
        // Convert captured size bins to proto format
        let proto_get_bins = stage_get_bins.as_ref().map(convert_size_bins);
        let proto_put_bins = stage_put_bins.as_ref().map(convert_size_bins);
        let proto_meta_bins = stage_meta_bins.as_ref().map(convert_size_bins);
        
        let stage_summary = StageSummary {
            stage_index: stage_index as u32,
            stage_name: stage.name.clone(),
            stage_type: stage_type.to_string(),
            wall_seconds: stage_snapshot.stage_elapsed_s,
            get_ops: stage_snapshot.get_ops,
            get_bytes: stage_snapshot.get_bytes,
            put_ops: stage_snapshot.put_ops,
            put_bytes: stage_snapshot.put_bytes,
            meta_ops: stage_snapshot.meta_ops,
            errors: 0,  // TODO: Track errors per stage
            get: Some(OpAggregateMetrics {
                bytes: stage_snapshot.get_bytes,
                ops: stage_snapshot.get_ops,
                mean_us: stage_snapshot.get_mean_us,
                p50_us: stage_snapshot.get_p50_us,
                p95_us: stage_snapshot.get_p95_us,
                p99_us: stage_snapshot.get_p99_us,
            }),
            put: Some(OpAggregateMetrics {
                bytes: stage_snapshot.put_bytes,
                ops: stage_snapshot.put_ops,
                mean_us: stage_snapshot.put_mean_us,
                p50_us: stage_snapshot.put_p50_us,
                p95_us: stage_snapshot.put_p95_us,
                p99_us: stage_snapshot.put_p99_us,
            }),
            meta: Some(OpAggregateMetrics {
                bytes: 0,  // META has no bytes
                ops: stage_snapshot.meta_ops,
                mean_us: stage_snapshot.meta_mean_us,
                p50_us: stage_snapshot.meta_p50_us,
                p95_us: stage_snapshot.meta_p95_us,
                p99_us: stage_snapshot.meta_p99_us,
            }),
            histogram_get: hist_get,
            histogram_put: hist_put,
            histogram_meta: hist_meta,
            // v0.8.28: Per-bucket size tracking (ops and bytes per size bucket)
            get_bins: proto_get_bins,
            put_bins: proto_put_bins,
            meta_bins: proto_meta_bins,
            endpoint_stats: Vec::new(),  // TODO: Capture per-stage endpoint stats
        };
        
        info!("Stage '{}' summary: {} GET ops, {} PUT ops, {} META ops in {:.2}s",
              stage.name, stage_snapshot.get_ops, stage_snapshot.put_ops, 
              stage_snapshot.meta_ops, stage_snapshot.stage_elapsed_s);
        
        // v0.8.26: Barrier coordination implementation
        // Phase 3: Wait for controller to release barrier before proceeding to next stage
        let barrier_name = format!("stage_{}", stage.name);
        let mut barrier_sequence = 0u32;
        
        // Get configurable barrier timeout from stage config or use default
        let barrier_timeout_secs = stage.barrier.as_ref()
            .map(|b| b.agent_barrier_timeout)
            .unwrap_or(120);  // Default 2 minutes if no barrier config
        let barrier_timeout = std::time::Duration::from_secs(barrier_timeout_secs);
        let max_barrier_retries = 5;
        
        // Subscribe to barrier release notifications
        let mut barrier_rx = agent_state.subscribe_barrier_release();
        
        // Barrier waiting loop with timeout and retry
        let mut barrier_released = false;
        for retry in 0..=max_barrier_retries {
            if retry > 0 {
                warn!("Barrier retry {}/{} for '{}' (sequence {})", 
                      retry, max_barrier_retries, barrier_name, barrier_sequence);
            }
            
            // Send barrier-ready notification to controller via LiveStats
            // Include stage_summary only on first send (not retries) to avoid duplicate processing
            let snapshot = tracker.snapshot();
            let barrier_stats = LiveStats {
                agent_id: agent_id.clone(),
                timestamp_s: snapshot.elapsed.as_secs_f64(),
                elapsed_s: snapshot.elapsed.as_secs_f64(),
                status: 2,  // RUNNING
                at_barrier: true,
                barrier_name: barrier_name.clone(),
                barrier_sequence,
                // Include current stats
                get_ops: snapshot.get_ops,
                get_bytes: snapshot.get_bytes,
                put_ops: snapshot.put_ops,
                put_bytes: snapshot.put_bytes,
                meta_ops: snapshot.meta_ops,
                // v0.8.27: Include stage summary on first barrier notification
                stage_summary: if retry == 0 { Some(stage_summary.clone()) } else { None },
                // Fill remaining fields from tracker
                ..Default::default()
            };
            
            if let Err(e) = tx_stats.send(barrier_stats).await {
                warn!("Failed to send barrier notification: {} - controller may have disconnected", e);
                // Don't fail immediately - controller might reconnect
            }
            
            debug!("Sent barrier notification for '{}' (sequence {}), waiting for release...",
                   barrier_name, barrier_sequence);
            
            // Wait for barrier release with timeout
            let wait_result = tokio::time::timeout(barrier_timeout, async {
                loop {
                    match barrier_rx.recv().await {
                        Ok((name, seq)) => {
                            // Check if this release matches our barrier
                            if name == barrier_name && seq >= barrier_sequence {
                                return Ok(());
                            }
                            // Received release for different barrier - keep waiting
                            debug!("Received barrier release for '{}' seq {} (waiting for '{}' seq {})",
                                   name, seq, barrier_name, barrier_sequence);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Barrier receiver lagged by {} messages", n);
                            // Continue waiting
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            return Err("Barrier channel closed - agent shutting down");
                        }
                    }
                }
            }).await;
            
            match wait_result {
                Ok(Ok(())) => {
                    info!("Barrier '{}' released (sequence {}), proceeding to next stage",
                          barrier_name, barrier_sequence);
                    barrier_released = true;
                    break;
                }
                Ok(Err(e)) => {
                    warn!("Barrier wait error: {}", e);
                    // Channel closed - likely shutting down
                    break;
                }
                Err(_) => {
                    // Timeout - increment sequence and retry
                    barrier_sequence += 1;
                    warn!("Barrier '{}' timeout ({}s), incrementing to sequence {}",
                          barrier_name, barrier_timeout.as_secs(), barrier_sequence);
                }
            }
        }
        
        if !barrier_released {
            // All retries exhausted - this is a critical failure
            error!("Barrier '{}' not released after {} retries - aborting workload",
                   barrier_name, max_barrier_retries);
            return Err(format!(
                "Barrier coordination failed: '{}' not released after {}s x {} retries. Controller may be unresponsive.",
                barrier_name, barrier_timeout.as_secs(), max_barrier_retries
            ));
        }
    }
    
    // All stages complete - transition to Completed
    if let Err(e) = agent_state.transition_to(WorkloadState::Completed, "all stages complete").await {
        error!("Failed to transition to Completed: {}", e);
    }
    
    info!("All {} stages completed successfully", stages.len());
    
    // Generate final summary from tracker
    let snapshot = tracker.snapshot();
    Ok(sai3_bench::workload::Summary {
        wall_seconds: snapshot.elapsed.as_secs_f64(),
        total_ops: snapshot.get_ops + snapshot.put_ops + snapshot.meta_ops,
        total_bytes: snapshot.get_bytes + snapshot.put_bytes,
        p50_us: snapshot.get_p50_us,
        p95_us: snapshot.get_p95_us,
        p99_us: snapshot.get_p99_us,
        get: sai3_bench::workload::OpAgg {
            ops: snapshot.get_ops,
            bytes: snapshot.get_bytes,
            mean_us: snapshot.get_mean_us,
            p50_us: snapshot.get_p50_us,
            p95_us: snapshot.get_p95_us,
            p99_us: snapshot.get_p99_us,
        },
        put: sai3_bench::workload::OpAgg {
            ops: snapshot.put_ops,
            bytes: snapshot.put_bytes,
            mean_us: snapshot.put_mean_us,
            p50_us: snapshot.put_p50_us,
            p95_us: snapshot.put_p95_us,
            p99_us: snapshot.put_p99_us,
        },
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
        endpoint_stats: None,
    })
}

/// Convert endpoint stats to proto format (v0.8.22)
fn endpoint_stats_to_proto(
    endpoint_stats: Option<&Vec<sai3_bench::workload::EndpointStatsSnapshot>>
) -> Vec<pb::iobench::EndpointStatsSnapshot> {
    endpoint_stats
        .map(|stats| {
            stats.iter().map(|s| pb::iobench::EndpointStatsSnapshot {
                uri: s.uri.clone(),
                total_requests: s.total_requests,
                bytes_read: s.bytes_read,
                bytes_written: s.bytes_written,
                error_count: s.error_count,
                active_requests: s.active_requests as u64,
            }).collect()
        })
        .unwrap_or_default()
}

/// Convert SizeBins to protobuf SizeBins message (v0.8.18)
fn size_bins_to_proto(bins: &sai3_bench::workload::SizeBins) -> pb::iobench::SizeBins {
    let buckets = bins.by_bucket
        .iter()
        .map(|(bucket_idx, (ops, bytes))| {
            pb::iobench::SizeBucketData {
                bucket_idx: *bucket_idx as u32,
                ops: *ops,
                bytes: *bytes,
            }
        })
        .collect();
    
    pb::iobench::SizeBins { buckets }
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
        get_bins: Some(size_bins_to_proto(&summary.get_bins)),
        put_bins: Some(size_bins_to_proto(&summary.put_bins)),
        meta_bins: Some(size_bins_to_proto(&summary.meta_bins)),
        endpoint_stats: endpoint_stats_to_proto(summary.endpoint_stats.as_ref()),
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
        put_bins: Some(size_bins_to_proto(&metrics.put_bins)),
        endpoint_stats: endpoint_stats_to_proto(metrics.endpoint_stats.as_ref()),
    })
}

/// Convert endpoint stats to proto format (v0.8.22)
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
                .map(|k| {
                    if let Some(stripped) = k.strip_prefix(base_uri) {
                        stripped.to_string()
                    } else {
                        k
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
            OpSpec::Get { path, .. } => {
                // For file:// backends, verify files exist
                if path.starts_with("file://") {
                    let file_path = path.replace("file://", "");
                    // Check if it's a glob pattern
                    if file_path.contains('*') {
                        // v0.8.51: Use spawn_blocking to prevent executor starvation on large globs
                        let file_path_clone = file_path.clone();
                        let path_clone = path.clone();
                        let paths = tokio::task::spawn_blocking(move || -> Result<Vec<std::path::PathBuf>> {
                            let paths: Vec<_> = glob::glob(&file_path_clone)
                                .map_err(|e| anyhow::anyhow!("Invalid glob pattern '{}': {}", path_clone, e))?
                                .collect::<Result<Vec<_>, _>>()
                                .map_err(|e| anyhow::anyhow!("Glob error: {}", e))?;
                            Ok(paths)
                        })
                        .await
                        .map_err(|e| anyhow::anyhow!("Blocking task failed: {}", e))??;
                        
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
            OpSpec::Delete { path, .. } => {
                // Delete needs a valid path
                if path.is_empty() {
                    anyhow::bail!("DELETE operation requires non-empty path");
                }
            }
            OpSpec::List { path, .. } => {
                // List needs a valid path
                if path.is_empty() {
                    anyhow::bail!("LIST operation requires non-empty path");
                }
            }
            OpSpec::Stat { path, .. } => {
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
    // Map verbosity to appropriate levels for both agent/sai3_bench and s3dlio:
    // -v (1): agent=info, s3dlio=warn (default passthrough)
    // -vv (2): agent=debug, s3dlio=info (detailed agent, operational s3dlio)
    // -vvv (3+): agent=trace, s3dlio=debug (full debugging both crates)
    let (agent_level, s3dlio_level) = match args.verbose {
        0 => ("warn", "warn"),   // Default: only warnings and errors
        1 => ("info", "warn"),   // -v: info level for agent, minimal s3dlio
        2 => ("debug", "info"),  // -vv: debug agent, info s3dlio
        _ => ("trace", "debug"), // -vvv+: trace agent, debug s3dlio
    };
    
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::new(format!("sai3bench_agent={},sai3_bench={},s3dlio={}", agent_level, agent_level, s3dlio_level));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    debug!("Logging initialized at level: {}", agent_level);
    
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

#[cfg(test)]
mod tests {
    use super::*;
    
    // ============================================================================
    // State Transition Validation Tests (v0.8.14) - DEPRECATED
    // ============================================================================
    // NOTE: These tests use WorkloadState::Ready and WorkloadState::Running states
    // which were removed in v0.8.29 as part of the YAML-driven stage migration.
    // They are kept for historical reference but marked as #[ignore].
    // New state machine tests should use WorkloadState::AtStage instead.
    // ============================================================================
    
    #[test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    fn test_can_transition_valid_normal_flow() {
        // Test body removed - uses WorkloadState::Ready and WorkloadState::Running
        // which were replaced by WorkloadState::AtStage in v0.8.29
    }
    
    #[test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    fn test_can_transition_valid_error_flow() {
        // Test body removed - uses deprecated states
    }
    
    #[test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    fn test_can_transition_valid_abort_flow() {
        // Test body removed - uses deprecated states
    }
    
    #[test]
    fn test_can_transition_same_state_noop() {
        use WorkloadState::*;
        
        // v0.8.14: Same-state transitions are valid no-ops for race condition safety
        assert!(AgentState::can_transition(&Idle, &Idle));
        assert!(AgentState::can_transition(&Failed, &Failed));
    }
    
    #[test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    fn test_can_transition_invalid_transitions() {
        // Test body removed - uses deprecated states
    }
    
    #[test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    fn test_can_transition_all_same_state_except_running_ready() {
        // Test body removed - uses deprecated states
    }
    
    // ============================================================================
    // AgentState transition_to() Tests (async) - DEPRECATED
    // ============================================================================
    
    #[tokio::test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    async fn test_transition_to_valid_transition() {
        // Test body removed - uses deprecated states
    }
    
    #[tokio::test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    async fn test_transition_to_invalid_transition() {
        // Test body removed - uses deprecated states
    }
    
    #[tokio::test]
    async fn test_transition_to_same_state_noop() {
        let state = AgentState::new(None);
        
        // Same-state transition should succeed as no-op
        let result = state.transition_to(WorkloadState::Idle, "test").await;
        assert!(result.is_ok());
        assert_eq!(state.get_state().await, WorkloadState::Idle);
    }
    
    #[tokio::test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    async fn test_transition_to_failed_same_state_noop() {
        // Test body removed - uses deprecated states
    }
    
    // ============================================================================
    // Completion Sent Flag Tests (v0.8.14)
    // ============================================================================
    
    #[tokio::test]
    async fn test_completion_sent_initial_state() {
        let state = AgentState::new(None);
        assert!(!state.is_completion_sent().await);
    }
    
    #[tokio::test]
    async fn test_completion_sent_mark_and_check() {
        let state = AgentState::new(None);
        
        state.mark_completion_sent().await;
        assert!(state.is_completion_sent().await);
    }
    
    #[tokio::test]
    async fn test_completion_sent_reset() {
        let state = AgentState::new(None);
        
        state.mark_completion_sent().await;
        assert!(state.is_completion_sent().await);
        
        state.reset_completion_sent().await;
        assert!(!state.is_completion_sent().await);
    }
    
    #[tokio::test]
    async fn test_completion_sent_shared_across_clones() {
        let state1 = AgentState::new(None);
        let state2 = state1.clone();
        
        // Set flag on state1
        state1.mark_completion_sent().await;
        
        // Should be visible on state2 (they share the same Arc)
        assert!(state2.is_completion_sent().await);
        
        // Reset on state2
        state2.reset_completion_sent().await;
        
        // Should be reflected in state1
        assert!(!state1.is_completion_sent().await);
    }
    
    // ============================================================================
    // Race Condition Scenario Tests (v0.8.14) - DEPRECATED
    // ============================================================================
    
    #[tokio::test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    async fn test_race_condition_scenario_success_completion() {
        // Test body removed - uses deprecated states
    }
    
    #[tokio::test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    async fn test_race_condition_scenario_control_reader_wins() {
        // Test body removed - uses deprecated states
    }
    
    #[tokio::test]
    #[ignore = "Uses deprecated Ready/Running states removed in v0.8.29"]
    async fn test_race_condition_scenario_error_path() {
        // Test body removed - uses deprecated states
    }
}
