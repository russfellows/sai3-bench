// src/bin/controller.rs

use anyhow::{anyhow, Context, Result, bail};
use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use tracing::{debug, error, info, warn};

// Results directory for v0.6.4+
use sai3_bench::results_dir::ResultsDir;
// Import BUCKET_LABELS from constants module
use sai3_bench::constants::BUCKET_LABELS;

pub mod pb {
    pub mod iobench {
        include!("../pb/iobench.rs");
    }
}

use pb::iobench::agent_client::AgentClient;
use pb::iobench::{Empty, LiveStats, PrepareSummary, RunGetRequest, RunPutRequest, RunWorkloadRequest, WorkloadSummary};

/// v0.7.13: Controller's view of agent states
/// 
/// Controller tracks more states than agents report because it sees the full lifecycle:
/// - Connecting: Stream opened, waiting for first message
/// - Validating: First message received, validation in progress
/// - Ready: Agent sent READY(1) status
/// - Preparing: In prepare phase (in_prepare_phase=true)
/// - Running: Executing workload (RUNNING(2) status)
/// - Completed: Workload finished successfully
/// - Failed: Agent sent ERROR(3) status or validation failed
/// - Disconnected: Stream closed unexpectedly or timeout
/// - Aborting: Abort RPC sent, waiting for cleanup
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControllerAgentState {
    Connecting,   // Stream opened
    Validating,   // First message, validation in progress
    Ready,        // READY(1) received
    Preparing,    // in_prepare_phase=true
    Running,      // RUNNING(2) received
    Completed,    // Workload done
    Failed,       // ERROR(3) received
    Disconnected, // Stream closed or timeout
    Aborting,     // Abort sent
}

impl ControllerAgentState {
    /// Check if transition from current state to new state is valid
    fn can_transition(from: &Self, to: &Self) -> bool {
        use ControllerAgentState::*;
        matches!(
            (from, to),
            // Startup sequence
            (Connecting, Validating) // First message
            | (Validating, Ready)      // Validation passes
            | (Validating, Failed)     // Validation fails
            // Normal execution
            | (Ready, Preparing)       // Prepare phase starts
            | (Ready, Running)         // No prepare, direct to running
            | (Preparing, Running)     // Prepare → workload
            | (Running, Completed)     // Success
            | (Running, Failed)        // Error
            // Disconnect/timeout
            | (Connecting, Disconnected)
            | (Validating, Disconnected)
            | (Ready, Disconnected)
            | (Preparing, Disconnected)
            | (Running, Disconnected)
            // Abort paths
            | (Ready, Aborting)
            | (Preparing, Aborting)
            | (Running, Aborting)
            | (Aborting, Completed)    // Cleanup done
            | (Aborting, Failed)       // Abort failed
            | (Aborting, Disconnected) // Lost during abort
            // v0.8.2: Recovery from Disconnected (gRPC stream backpressure, network issues)
            // BUG FIX: Previously Disconnected was terminal - agents couldn't recover
            // Issue: Large prepare phases (400K+ objects) cause gRPC backpressure
            //        Agent's yield blocks → no updates → timeout → marked Disconnected
            //        Then agent recovers but controller couldn't process recovery
            // Solution: Allow transitions FROM Disconnected based on message content
            | (Disconnected, Preparing)  // Reconnect during prepare phase
            | (Disconnected, Running)    // Reconnect during workload phase
            | (Disconnected, Completed)  // Reconnect with completion message
            // Stay in terminal states
            | (Completed, Completed)
            | (Failed, Failed)
            | (Disconnected, Disconnected)
        )
    }
}

/// v0.7.13: Tracks state and metadata for a single agent
/// 
/// Replaces ad-hoc HashSets (ready_agents, completed_agents, dead_agents)
/// with structured state tracking and validation.
struct AgentTracker {
    agent_id: String,
    state: ControllerAgentState,
    last_seen: std::time::Instant,
    error_message: Option<String>,
    latest_stats: Option<LiveStats>,
    reconnect_count: usize,  // v0.8.2: Count of disconnections followed by reconnection
    clock_offset_ns: Option<i64>,  // v0.8.4: Agent clock offset (agent_time - controller_time)
}

impl AgentTracker {
    fn new(agent_id: String) -> Self {
        Self {
            agent_id,
            state: ControllerAgentState::Connecting,  // v0.7.13: Stream opened, waiting for first message
            last_seen: std::time::Instant::now(),
            error_message: None,
            latest_stats: None,
            reconnect_count: 0,  // v0.8.2: Track disconnect/reconnect events
            clock_offset_ns: None,  // v0.8.4: Calculated when READY message received
        }
    }

    /// Attempt to transition to new state with validation and logging
    fn transition_to(&mut self, new_state: ControllerAgentState, reason: &str) -> Result<()> {
        if !ControllerAgentState::can_transition(&self.state, &new_state) {
            let msg = format!(
                "Invalid agent state transition: {:?} → {:?} ({})",
                self.state, new_state, reason
            );
            error!("❌ Agent {}: {}", self.agent_id, msg);
            return Err(anyhow::anyhow!(msg));
        }

        debug!(
            "Agent {} state: {:?} → {:?} ({})",
            self.agent_id, self.state, new_state, reason
        );
        self.state = new_state;
        self.last_seen = std::time::Instant::now();
        Ok(())
    }

    /// Update last seen timestamp (for timeout detection)
    fn touch(&mut self) {
        self.last_seen = std::time::Instant::now();
    }

    /// Check if agent is in a terminal state (won't send more updates)
    /// v0.8.2: Disconnected is NOT terminal - agents can recover
    /// 
    /// BUG FIX: Previously included Disconnected as terminal state
    /// This prevented processing recovery messages from agents that timed out
    /// but later resumed (e.g., after gRPC backpressure resolved)
    /// 
    /// Only Completed and Failed are truly terminal - no further messages expected
    fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            ControllerAgentState::Completed | ControllerAgentState::Failed
        )
    }

    /// Check if agent is active (should be sending updates)
    fn is_active(&self) -> bool {
        matches!(
            self.state,
            ControllerAgentState::Preparing | ControllerAgentState::Running
        )
    }

    /// Get time since last message (for timeout detection)
    fn time_since_last_seen(&self) -> std::time::Duration {
        self.last_seen.elapsed()
    }
}

/// v0.7.5: Aggregator for live stats from multiple agents
/// 
/// Collects LiveStats messages from all agent streams and computes weighted aggregate metrics.
/// Uses weighted averaging for latencies (weighted by operation count).
/// v0.7.12: Tracks previous snapshot for windowed throughput calculation
/// v0.8.2: Tracks expected_agents for "X of Y Agents" display
struct LiveStatsAggregator {
    agent_stats: std::collections::HashMap<String, LiveStats>,
    // v0.7.12: Previous aggregate for computing windowed (current) throughput
    previous_aggregate: Option<AggregateStats>,
    // v0.8.2: Expected number of agents (from agent_addrs.len())
    expected_agents: usize,
}

impl LiveStatsAggregator {
    fn new(expected_agents: usize) -> Self {
        Self {
            agent_stats: std::collections::HashMap::new(),
            previous_aggregate: None,
            expected_agents,
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
        self.previous_aggregate = None;  // v0.7.12: Reset windowed tracking
    }

    /// Check if all agents have completed
    fn all_completed(&self) -> bool {
        !self.agent_stats.is_empty() && self.agent_stats.values().all(|s| s.completed)
    }

    /// Get aggregate stats across all agents (weighted latency averaging)
    /// v0.7.12: Also computes windowed throughput for current rate display
    fn aggregate(&mut self) -> AggregateStats {
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

        // v0.7.11: CPU utilization sums (simple average across agents)
        let mut cpu_user_sum = 0.0f64;
        let mut cpu_system_sum = 0.0f64;
        let mut cpu_iowait_sum = 0.0f64;
        let mut cpu_total_sum = 0.0f64;

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
            
            // v0.7.11: Accumulate CPU metrics
            cpu_user_sum += stats.cpu_user_percent;
            cpu_system_sum += stats.cpu_system_percent;
            cpu_iowait_sum += stats.cpu_iowait_percent;
            cpu_total_sum += stats.cpu_total_percent;
        }

        // Calculate weighted averages
        let num_agents = self.agent_stats.len() as f64;
        let cpu_user_percent = if num_agents > 0.0 { cpu_user_sum / num_agents } else { 0.0 };
        let cpu_system_percent = if num_agents > 0.0 { cpu_system_sum / num_agents } else { 0.0 };
        let cpu_iowait_percent = if num_agents > 0.0 { cpu_iowait_sum / num_agents } else { 0.0 };
        let cpu_total_percent = if num_agents > 0.0 { cpu_total_sum / num_agents } else { 0.0 };

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

        // v0.7.12: Compute windowed (current) throughput from delta since last update
        let (windowed_get_ops_s, windowed_get_bytes, windowed_put_ops_s, windowed_put_bytes) = 
            if let Some(ref prev) = self.previous_aggregate {
                let delta_time = max_elapsed - prev.elapsed_s;
                if delta_time > 0.1 {  // Only compute if >= 100ms elapsed
                    let delta_get_ops = total_get_ops.saturating_sub(prev.total_get_ops);
                    let delta_get_bytes = total_get_bytes.saturating_sub(prev.total_get_bytes);
                    let delta_put_ops = total_put_ops.saturating_sub(prev.total_put_ops);
                    let delta_put_bytes = total_put_bytes.saturating_sub(prev.total_put_bytes);
                    (
                        delta_get_ops as f64 / delta_time,
                        delta_get_bytes,
                        delta_put_ops as f64 / delta_time,
                        delta_put_bytes
                    )
                } else {
                    (0.0, 0, 0.0, 0)  // Not enough time elapsed
                }
            } else {
                // First update: use cumulative
                (0.0, 0, 0.0, 0)  // No previous data yet
            };

        let aggregate = AggregateStats {
            num_agents: self.agent_stats.len(),
            expected_agents: self.expected_agents,  // v0.8.2: Track expected count
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
            cpu_user_percent,
            cpu_system_percent,
            cpu_iowait_percent,
            cpu_total_percent,
            windowed_get_ops_s,
            windowed_get_bytes,
            windowed_put_ops_s,
            windowed_put_bytes,
            windowed_delta_time: if self.previous_aggregate.is_some() {
                max_elapsed - self.previous_aggregate.as_ref().unwrap().elapsed_s
            } else {
                0.0
            },
        };

        // v0.7.12: Store current aggregate for next windowed calculation
        self.previous_aggregate = Some(aggregate.clone());

        aggregate
    }
}

/// Aggregate statistics across all agents
#[derive(Debug, Clone)]
struct AggregateStats {
    num_agents: usize,
    expected_agents: usize,  // v0.8.2: For "X of Y Agents" display
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
    // v0.7.11: CPU utilization metrics (averaged across agents)
    cpu_user_percent: f64,
    cpu_system_percent: f64,
    cpu_iowait_percent: f64,
    cpu_total_percent: f64,
    // v0.7.12: Windowed (current) throughput for responsive display
    windowed_get_ops_s: f64,
    windowed_get_bytes: u64,
    windowed_put_ops_s: f64,
    windowed_put_bytes: u64,
    windowed_delta_time: f64,
}

impl AggregateStats {
    /// Format multi-line progress display (GET on line 1, PUT on line 2, META on line 3 if present)
    /// v0.7.12: Shows windowed (current) throughput for responsive updates
    fn format_progress(&self) -> String {
        // v0.7.12: Use windowed rates if available (more responsive to current performance)
        // Fall back to cumulative if windowed not yet available
        let (get_ops_s, get_bandwidth) = if self.windowed_get_ops_s > 0.0 {
            (self.windowed_get_ops_s, format_bandwidth(self.windowed_get_bytes, self.windowed_delta_time))
        } else {
            let ops_s = if self.elapsed_s > 0.0 { self.total_get_ops as f64 / self.elapsed_s } else { 0.0 };
            (ops_s, format_bandwidth(self.total_get_bytes, self.elapsed_s))
        };
        
        let (put_ops_s, put_bandwidth) = if self.windowed_put_ops_s > 0.0 {
            (self.windowed_put_ops_s, format_bandwidth(self.windowed_put_bytes, self.windowed_delta_time))
        } else {
            let ops_s = if self.elapsed_s > 0.0 { self.total_put_ops as f64 / self.elapsed_s } else { 0.0 };
            (ops_s, format_bandwidth(self.total_put_bytes, self.elapsed_s))
        };
        
        let meta_ops_s = if self.elapsed_s > 0.0 {
            self.total_meta_ops as f64 / self.elapsed_s
        } else {
            0.0
        };

        // v0.7.11: Build CPU line (only if we have CPU data)
        let cpu_line = if self.cpu_total_percent > 0.0 {
            format!("\n  CPU: {:.1}% total (user: {:.1}%, system: {:.1}%, iowait: {:.1}%)",
                    self.cpu_total_percent, self.cpu_user_percent, self.cpu_system_percent, self.cpu_iowait_percent)
        } else {
            String::new()
        };

        // v0.7.12: Format latencies with auto-switching µs/ms
        let get_mean_str = format_latency(self.get_mean_us);
        let get_p50_str = format_latency(self.get_p50_us);
        let get_p95_str = format_latency(self.get_p95_us);
        let put_mean_str = format_latency(self.put_mean_us);
        let put_p50_str = format_latency(self.put_p50_us);
        let put_p95_str = format_latency(self.put_p95_us);
        let meta_mean_str = format_latency(self.meta_mean_us);

        // Only show META line if there are META operations
        if self.total_meta_ops > 0 {
            format!(
                "{} of {} Agents\n  GET: {:.0} ops/s, {} (mean: {}, p50: {}, p95: {})\n  PUT: {:.0} ops/s, {} (mean: {}, p50: {}, p95: {})\n  META: {:.0} ops/s (mean: {}){}",
                self.num_agents,
                self.expected_agents,
                get_ops_s,
                get_bandwidth,
                get_mean_str,
                get_p50_str,
                get_p95_str,
                put_ops_s,
                put_bandwidth,
                put_mean_str,
                put_p50_str,
                put_p95_str,
                meta_ops_s,
                meta_mean_str,
                cpu_line,
            )
        } else {
            format!(
                "{} of {} Agents\n  GET: {:.0} ops/s, {} (mean: {}, p50: {}, p95: {})\n  PUT: {:.0} ops/s, {} (mean: {}, p50: {}, p95: {}){}",
                self.num_agents,
                self.expected_agents,
                get_ops_s,
                get_bandwidth,
                get_mean_str,
                get_p50_str,
                get_p95_str,
                put_ops_s,
                put_bandwidth,
                put_mean_str,
                put_p50_str,
                put_p95_str,
                cpu_line,
            )
        }
    }
}

/// v0.7.12: Format latency with auto-switching between µs and ms
/// Uses ms when latency >= 10,000µs (10ms) for better readability
fn format_latency(us: f64) -> String {
    if us >= 10_000.0 {
        format!("{:.1}ms", us / 1000.0)
    } else {
        format!("{:.0}µs", us)
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
    /// Optional: can also be specified in config YAML under distributed.agents
    #[arg(long)]
    agents: Option<String>,

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

// v0.7.11: Abort workload on all agents (controller failure, user interrupt, etc.)
async fn abort_all_agents(
    agent_addrs: &[String],
    agent_trackers: &mut std::collections::HashMap<String, AgentTracker>,
    insecure: bool,
    agent_ca: Option<&PathBuf>,
    agent_domain: &str,
) {
    eprintln!("⚠️  Sending abort signal to all agents...");
    
    // v0.7.13: Transition active agents to Aborting state
    for addr in agent_addrs {
        if let Some(tracker) = agent_trackers.get_mut(addr) {
            if matches!(tracker.state, ControllerAgentState::Ready | ControllerAgentState::Preparing | ControllerAgentState::Running) {
                let _ = tracker.transition_to(ControllerAgentState::Aborting, "abort RPC sent");
            }
        }
    }
    
    let mut abort_tasks = Vec::new();
    
    for addr in agent_addrs {
        let addr = addr.clone();
        let insecure = insecure;
        let ca = agent_ca.map(|p| p.clone());  // Clone PathBuf if present
        let domain = agent_domain.to_string();
        
        let task = tokio::spawn(async move {
            match mk_client(&addr, insecure, ca.as_ref(), &domain).await {
                Ok(mut client) => {
                    match client.abort_workload(Empty {}).await {
                        Ok(_) => debug!("Abort signal sent to agent {}", addr),
                        Err(e) => warn!("Failed to abort agent {}: {}", addr, e),
                    }
                }
                Err(e) => warn!("Failed to connect to agent {} for abort: {}", addr, e),
            }
        });
        abort_tasks.push(task);
    }
    
    // Wait for all abort calls (with short timeout)
    let _ = tokio::time::timeout(
        tokio::time::Duration::from_secs(2),
        futures::future::join_all(abort_tasks)
    ).await;
}

async fn mk_client(
    target: &str,
    insecure: bool,
    ca_path: Option<&PathBuf>,
    sni_domain: &str,
) -> Result<AgentClient<Channel>> {
    if insecure {
        // Plain HTTP with robust connection settings
        let ep = format!("http://{}", target);
        let channel = Endpoint::try_from(ep)?
            .connect_timeout(Duration::from_secs(5))
            .tcp_nodelay(true)
            // BUG FIX v0.8.3: Add HTTP/2 keepalive to detect dead connections
            // Without keepalive, broken TCP connections can hang for minutes
            // These settings match gRPC best practices for long-lived streams
            .http2_keep_alive_interval(Duration::from_secs(30))  // Send PING every 30s
            .keep_alive_timeout(Duration::from_secs(10))         // Wait 10s for PONG
            .keep_alive_while_idle(true)                         // Keep alive even when idle
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
        // BUG FIX v0.8.3: Add HTTP/2 keepalive (same as insecure mode)
        .http2_keep_alive_interval(Duration::from_secs(30))
        .keep_alive_timeout(Duration::from_secs(10))
        .keep_alive_while_idle(true)
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
        .as_ref()
        .map(|a| a.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
        .unwrap_or_default();

    info!("Controller starting with {} agent(s) from CLI", agents.len());
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
            
            // v0.7.12: Use comprehensive validation display (same as standalone binary)
            if *dry_run {
                // v0.7.13: Validate workload configuration FIRST (before printing "Configuration is valid")
                if let Err(e) = validate_workload_config(&parsed_config).await {
                    eprintln!("❌ Configuration validation failed: {}", e);
                    bail!("Configuration validation failed: {}", e);
                }
                
                // Only print full summary with "Configuration is valid" if validation passed
                sai3_bench::validation::display_config_summary(&parsed_config, &config.display().to_string())?;
                
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
    // v0.7.12: 10 second coordinated start delay (agents already validated)
    let coordinated_start_delay = 10;
    let total_delay_secs = coordinated_start_delay + start_delay_secs;
    let start_time = SystemTime::now() + Duration::from_secs(total_delay_secs);
    let start_ns = start_time
        .duration_since(UNIX_EPOCH)?
        .as_nanos() as i64;

    debug!("Coordinated start time: {} ns since epoch (coordinated: {}s, user delay: {}s)", 
           start_ns, coordinated_start_delay, start_delay_secs);

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
    // BUG FIX v0.8.3: Use unbounded channel to prevent backpressure deadlock
    // Previous: bounded channel with buffer=100 caused stream tasks to block/fail
    //   - 4 agents × 1 msg/sec = 4 msg/sec
    //   - If controller slow (heavy aggregation), buffer fills in 25 seconds
    //   - tx.send() blocks/fails → stream task exits → no more messages
    //   - Controller sees timeout and marks agent disconnected
    // Solution: Unbounded channel - agents can always send, controller processes at own pace
    //
    // ROBUSTNESS v0.8.3: Message reception runs in dedicated task, processing in main loop
    //   - Stream forwarding → unbounded channel → reception task → bounded channel → main loop
    //   - Reception task ONLY updates timestamps (fast, never blocks)
    //   - Main loop does heavy processing (aggregation, UI, file I/O)
    //   - Guarantees: tracker.touch() happens immediately, agents never timeout due to slow processing
    let (tx_stats, mut rx_stats) = tokio::sync::mpsc::unbounded_channel::<LiveStats>();
    
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
            let stream_result: Result<(), anyhow::Error> = async {
                while let Some(stats_result) = stream.message().await? {
                    stats_count += 1;
                    debug!("Agent {} sent stats update #{}: {} ops, {} bytes", addr, stats_count, 
                           stats_result.get_ops + stats_result.put_ops + stats_result.meta_ops,
                           stats_result.get_bytes + stats_result.put_bytes);
                    // BUG FIX v0.8.3: unbounded_channel uses send() not send().await
                    if let Err(e) = tx.send(stats_result) {
                        error!("Failed to forward stats from agent {}: {}", addr, e);
                        break;
                    }
                }
                Ok(())
            }.await;
            
            // v0.7.12: Handle stream errors by sending error status message
            if let Err(e) = stream_result {
                error!("Agent {} stream error: {}", agent_id, e);
                // Send error message to controller so it knows agent failed
                let error_stats = pb::iobench::LiveStats {
                    agent_id: agent_id.clone(),
                    timestamp_s: 0.0,
                    get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p95_us: 0.0,
                    put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p95_us: 0.0,
                    meta_ops: 0, meta_mean_us: 0.0,
                    elapsed_s: 0.0,
                    completed: true,
                    final_summary: None,
                    status: 3,  // ERROR
                    error_message: format!("Agent workload failed: {}", e),
                    in_prepare_phase: false,
                    prepare_objects_created: 0,
                    prepare_objects_total: 0,
                    prepare_summary: None,
                    cpu_user_percent: 0.0,
                    cpu_system_percent: 0.0,
                    cpu_iowait_percent: 0.0,
                    cpu_total_percent: 0.0,
                    agent_timestamp_ns: 0,
                };
                let _ = tx.send(error_stats);  // BUG FIX v0.8.3: unbounded_channel uses send() not send().await
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
    
    // v0.7.13: STARTUP HANDSHAKE - Track agent state with formal state machine
    let expected_agent_count = stream_handles.len();
    let mut agent_trackers: std::collections::HashMap<String, AgentTracker> = std::collections::HashMap::new();
    
    // Initialize trackers for all expected agents (agent_addrs are "host:port" strings)
    for agent_id in agent_addrs {
        agent_trackers.insert(agent_id.clone(), AgentTracker::new(agent_id.clone()));
    }
    
    // v0.7.6: Stream forwarding tasks are already running (messages forwarded immediately)
    // Just wait for them to complete in background
    for handle in stream_handles {
        tokio::spawn(async move {
            if let Err(e) = handle.await {
                error!("Stream task error: {:?}", e);
            }
        });
    }
    
    // v0.7.13: Setup signal handlers (SIGINT, SIGTERM)
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);
    
    eprintln!("⏳ Waiting for agents to validate configuration...");
    // v0.7.11: Robust startup with retries - scale timeout with agent count
    // Give agents multiple chances to respond (30s base + 5s per agent)
    let startup_timeout_secs = 30 + (agent_addrs.len() as u64 * 5);
    let startup_timeout = tokio::time::Duration::from_secs(startup_timeout_secs);
    let startup_deadline = tokio::time::Instant::now() + startup_timeout;
    
    // Progress check interval - report status every 3 seconds
    let mut progress_interval = tokio::time::interval(tokio::time::Duration::from_secs(3));
    progress_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    
    // v0.7.13: Wait for READY/ERROR status from all agents with state tracking
    'startup: loop {
        tokio::select! {
            Some(stats) = rx_stats.recv() => {
                // Get or create tracker for this agent
                let tracker = agent_trackers
                    .entry(stats.agent_id.clone())
                    .or_insert_with(|| AgentTracker::new(stats.agent_id.clone()));
                
                // First message - transition from Connecting to Validating
                if tracker.state == ControllerAgentState::Connecting {
                    let _ = tracker.transition_to(ControllerAgentState::Validating, "first message received");
                }
                
                // Check status field and update state
                match stats.status {
                    1 => {  // READY
                        if let Err(e) = tracker.transition_to(ControllerAgentState::Ready, "READY status received") {
                            warn!("Failed to transition agent {} to Ready: {}", stats.agent_id, e);
                        }
                        
                        // v0.8.4: Calculate clock offset for coordinated start synchronization
                        // NTP-style: offset = agent_time - controller_time
                        // Agent will later adjust start_time using this offset
                        if stats.agent_timestamp_ns != 0 {
                            let controller_time_ns = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_nanos() as i64;
                            let offset_ns = stats.agent_timestamp_ns - controller_time_ns;
                            tracker.clock_offset_ns = Some(offset_ns);
                            
                            let offset_ms = offset_ns as f64 / 1_000_000.0;
                            if offset_ms.abs() > 1000.0 {
                                warn!("Agent {} clock offset: {:.1}ms (large skew detected)", stats.agent_id, offset_ms);
                            } else {
                                debug!("Agent {} clock offset: {:.1}ms", stats.agent_id, offset_ms);
                            }
                        } else {
                            warn!("Agent {} did not send timestamp in READY message", stats.agent_id);
                        }
                        
                        eprintln!("  ✅ {} ready", stats.agent_id);
                    }
                    3 => {  // ERROR
                        tracker.error_message = Some(stats.error_message.clone());
                        if let Err(e) = tracker.transition_to(ControllerAgentState::Failed, "ERROR status received") {
                            warn!("Failed to transition agent {} to Failed: {}", stats.agent_id, e);
                        }
                        eprintln!("  ❌ {} error: {}", stats.agent_id, stats.error_message);
                    }
                    _ => {
                        // Unexpected status during startup (RUNNING/COMPLETED)
                        // This shouldn't happen, but treat as ready if not already in terminal state
                        if !tracker.is_terminal() && tracker.state != ControllerAgentState::Ready {
                            if let Err(e) = tracker.transition_to(ControllerAgentState::Ready, "unexpected status during startup") {
                                warn!("Failed to transition agent {} to Ready: {}", stats.agent_id, e);
                            }
                        }
                    }
                }
                
                tracker.touch();  // Update last seen
                
                // Check if all agents have responded (Ready or Failed)
                let responded = agent_trackers.values()
                    .filter(|t| t.state == ControllerAgentState::Ready || t.state == ControllerAgentState::Failed)
                    .count();
                if responded >= expected_agent_count {
                    break 'startup;
                }
            }
            _ = progress_interval.tick() => {
                // v0.7.13: Periodic progress update showing which agents are still pending
                let elapsed = startup_deadline.duration_since(tokio::time::Instant::now());
                let responded = agent_trackers.values()
                    .filter(|t| t.state == ControllerAgentState::Ready || t.state == ControllerAgentState::Failed)
                    .count();
                if responded < expected_agent_count {
                    eprintln!("  ⏳ {}/{} agents ready, {} remaining ({:.0}s timeout remaining)", 
                             responded, expected_agent_count, 
                             expected_agent_count - responded,
                             elapsed.as_secs_f64());
                }
            }
            _ = tokio::time::sleep_until(startup_deadline) => {
                eprintln!("\n❌ Startup timeout: Not all agents responded within {}s", startup_timeout.as_secs());
                
                // Count agents by state
                let ready_count = agent_trackers.values().filter(|t| t.state == ControllerAgentState::Ready).count();
                let failed_count = agent_trackers.values().filter(|t| t.state == ControllerAgentState::Failed).count();
                let connecting_count = agent_trackers.values().filter(|t| t.state == ControllerAgentState::Connecting).count();
                
                eprintln!("Agent status: {} ready, {} failed, {} no response", ready_count, failed_count, connecting_count);
                
                // Show per-agent status
                for tracker in agent_trackers.values() {
                    let (icon, status) = match tracker.state {
                        ControllerAgentState::Ready => ("✅", "ready"),
                        ControllerAgentState::Failed => ("❌", "failed"),
                        ControllerAgentState::Connecting => ("⏱️", "no response"),
                        _ => ("⚠️", "unexpected"),
                    };
                    if tracker.state == ControllerAgentState::Failed {
                        eprintln!("  {} {}: {} - {}", icon, tracker.agent_id, status, 
                                 tracker.error_message.as_ref().unwrap_or(&"unknown error".to_string()));
                    } else {
                        eprintln!("  {} {}: {}", icon, tracker.agent_id, status);
                    }
                }
                
                if connecting_count > 0 {
                    eprintln!("\n💡 Troubleshooting:");
                    eprintln!("  - Verify agents are running: check agent logs");
                    eprintln!("  - Check network connectivity to agents");
                    eprintln!("  - Ensure agents can reach storage backend");
                    eprintln!("  - Consider increasing timeout for large agent counts");
                }
                
                // v0.7.13: Abort all agents to prevent orphaned workload execution
                abort_all_agents(&agent_addrs, &mut agent_trackers, insecure, agent_ca, &agent_domain).await;
                
                anyhow::bail!("Agent startup validation timeout");
            }
            _ = &mut shutdown_signal => {
                let sig = shutdown_signal.await;
                eprintln!("\n⚠️  Received {} during startup", sig);
                
                // v0.7.13: Abort all agents to prevent orphaned workload execution
                abort_all_agents(&agent_addrs, &mut agent_trackers, insecure, agent_ca, &agent_domain).await;
                
                anyhow::bail!("Interrupted by {}", sig);
            }
        }
    }
    
    // v0.7.13: Check if any agents reported errors
    let failed_agents: Vec<_> = agent_trackers.values()
        .filter(|t| t.state == ControllerAgentState::Failed)
        .map(|t| (t.agent_id.clone(), t.error_message.clone()))
        .collect();
    
    if !failed_agents.is_empty() {
        eprintln!("\n❌ {} agent(s) failed configuration validation:", failed_agents.len());
        for (agent_id, error_msg_opt) in &failed_agents {
            let error_msg = error_msg_opt.as_deref().unwrap_or("unknown error");
            eprintln!("  ❌ {}: {}", agent_id, error_msg);
        }
        
        let ready_count = agent_trackers.values().filter(|t| t.state == ControllerAgentState::Ready).count();
        eprintln!("\nReady agents: {}/{}", ready_count, expected_agent_count);
        for tracker in agent_trackers.values().filter(|t| t.state == ControllerAgentState::Ready) {
            eprintln!("  ✅ {}", tracker.agent_id);
        }
        
        // v0.7.13: Abort all agents to prevent orphaned workload execution
        abort_all_agents(&agent_addrs, &mut agent_trackers, insecure, agent_ca, &agent_domain).await;
        
        anyhow::bail!("{} agent(s) failed startup validation", failed_agents.len());
    }
    
    let ready_count = agent_trackers.values().filter(|t| t.state == ControllerAgentState::Ready).count();
    eprintln!("✅ All {} agents ready - starting workload execution\n", ready_count);
    
    // v0.7.13: Show countdown to coordinated start (suspend progress bar during countdown)
    if total_delay_secs > 0 {
        progress_bar.suspend(|| {
            for remaining in (1..=total_delay_secs).rev() {
                eprint!("\r⏳ Starting in {}s...  ", remaining);
                std::io::stderr().flush().unwrap();
                std::thread::sleep(Duration::from_secs(1));
            }
            eprint!("\r⏳ Starting in 0s...  ");
            std::io::stderr().flush().unwrap();
            std::thread::sleep(Duration::from_millis(500));
            eprintln!("\n✅ Starting workload now!\n");
        });
    }
    
    // v0.7.6: Track workload start time for progress bar position
    // Note: This will be reset when prepare phase completes to measure actual workload time
    let mut workload_start = std::time::Instant::now();
    
    // Aggregator for live stats display
    let mut aggregator = LiveStatsAggregator::new(agent_addrs.len());  // v0.8.2: Pass expected agent count
    let mut last_update = std::time::Instant::now();
    
    // v0.7.13: agent_trackers already initialized above for startup phase
    // Continue using it for workload execution (replaces completed_agents, dead_agents, agent_last_seen)
    // v0.8.2: Increased timeouts for long prepare phases (400K+ objects)
    // BUG FIX: Previous values (5s/10s) too aggressive for production workloads
    // 
    // Root cause: During large prepare phases, agent's `yield Ok(stats)` can block
    // if controller's gRPC receive buffer fills (backpressure). While blocked,
    // agent cannot send updates, triggering false timeout detection.
    // 
    // Production testing: Saw 379s blockage with 400K objects, all agents working fine
    // User testing: Saw 56% prepare completion (113K/200K) before timeout - agents still working
    // 
    // BUG FIX v0.8.3: Previous 60s timeout was WAY too short for large prepare phases
    // - 200K objects can take 5-10 minutes to create
    // - gRPC backpressure can pause updates for minutes
    // - Agents keep working but controller gives up and exits
    // 
    // New values:
    //   - Warn after 60s (agents should send updates every 0.5s normally)
    //   - Mark disconnected after 10 minutes (truly dead, not just slow)
    let timeout_warn_secs = 60.0;   // Warn after 1 minute (yellow flag)
    let timeout_dead_secs = 600.0;  // Mark dead after 10 minutes (red flag)
    
    // v0.7.5: Collect final summaries for persistence (extracted from completed LiveStats messages)
    let mut agent_summaries: Vec<WorkloadSummary> = Vec::new();
    
    // v0.7.9: Collect prepare summaries for persistence  
    let mut prepare_summaries: Vec<PrepareSummary> = Vec::new();
    
    // v0.7.5: Track last console.log write time (write every 1 second)
    let mut last_console_log = std::time::Instant::now();
    
    // v0.7.9: Track prepare phase state to detect transition to workload
    let mut was_in_prepare_phase = false;
    
    // v0.8.4: Track prepare phase high-water marks (never decrease during prepare)
    let mut max_prepare_created: u64 = 0;
    let mut max_prepare_total: u64 = 0;
    
    // Process live stats stream
    loop {
        // v0.7.13: Check for stalled agents (timeout detection) using agent_trackers
        let mut any_disconnected = false;
        for tracker in agent_trackers.values_mut() {
            if tracker.is_terminal() {
                continue;  // Skip terminal states (Completed, Failed, Disconnected)
            }
            
            if !tracker.is_active() {
                continue;  // Skip non-active states (not Preparing/Running)
            }
            
            let elapsed = tracker.time_since_last_seen().as_secs_f64();
            if elapsed >= timeout_dead_secs {
                if tracker.state != ControllerAgentState::Disconnected {
                    error!("❌ Agent {} STALLED (no updates for {:.1}s) - marking as DISCONNECTED", tracker.agent_id, elapsed);
                    let _ = tracker.transition_to(ControllerAgentState::Disconnected, "timeout");
                    // BUG FIX v0.8.3: Do NOT mark as completed - disconnected != completed!
                    // Previous code called mark_completed() here, causing early exit when all agents timed out
                    // Now: disconnected agents don't count toward all_completed() check
                    any_disconnected = true;
                }
            } else if elapsed >= timeout_warn_secs {
                warn!("⚠️  Agent {} delayed: no updates for {:.1}s", tracker.agent_id, elapsed);
            }
        }
        
        // Update progress bar if we detected any disconnections
        if any_disconnected {
            let dead_count = agent_trackers.values().filter(|t| t.state == ControllerAgentState::Disconnected).count();
            progress_bar.set_message(format!("{} (⚠️ {} dead)", aggregator.aggregate().format_progress(), dead_count));
        }
        
        tokio::select! {
            stats_opt = rx_stats.recv() => {
                match stats_opt {
                    Some(stats) => {
                        // v0.7.13: Update tracker for this agent
                        let tracker = agent_trackers
                            .entry(stats.agent_id.clone())
                            .or_insert_with(|| AgentTracker::new(stats.agent_id.clone()));
                        
                        tracker.touch();  // Update last seen timestamp
                        tracker.latest_stats = Some(stats.clone());
                        
                        // v0.8.2: Agent recovery from Disconnected state
                        // BUG FIX: Previously agents stayed Disconnected even after sending messages
                        // 
                        // Scenario: Agent times out during prepare (gRPC backpressure)
                        //           → marked Disconnected
                        //           → backpressure resolves, agent sends message
                        //           → THIS CODE recovers agent to correct state
                        // 
                        // Recovery is AUTOMATIC and COMPLETE:
                        //   - State: transition_to() performs full state change + timestamp reset
                        //   - Display: dead_count recalculated dynamically
                        //   - Aggregator: update() replaces entry, resets completed flag if needed
                        //   - No persistent degraded state remains
                        if tracker.state == ControllerAgentState::Disconnected {
                            // BUG FIX v0.8.3: If agent was marked completed due to disconnect, unmark it
                            // This ensures all_completed() check waits for actual completion
                            if let Some(agent_stats) = aggregator.agent_stats.get_mut(&stats.agent_id) {
                                if agent_stats.completed && !stats.completed {
                                    agent_stats.completed = false;
                                    info!("Agent {} reconnected - unmarking as completed", stats.agent_id);
                                }
                            }
                            
                            let new_state = if stats.completed {
                                ControllerAgentState::Completed
                            } else if stats.in_prepare_phase {
                                ControllerAgentState::Preparing
                            } else {
                                ControllerAgentState::Running
                            };
                            
                            // v0.8.2: Increment reconnect counter for diagnostics
                            tracker.reconnect_count += 1;
                            
                            warn!("🔄 Agent {} RECOVERED from DISCONNECTED → {:?} (reconnect #{})", stats.agent_id, new_state, tracker.reconnect_count);
                            
                            // Use transition_to with proper validation (now allowed in state machine)
                            // Note: Checks error for visibility, shouldn't fail with state machine fix
                            if let Err(e) = tracker.transition_to(new_state, "recovered from timeout") {
                                error!("Failed to recover agent {}: {}", stats.agent_id, e);
                            }
                        }
                        
                        // v0.7.9: Capture prepare phase info BEFORE moving stats
                        let in_prepare = stats.in_prepare_phase;
                        
                        // v0.8.4: Use high-water marks during prepare phase to prevent backwards movement
                        // (delayed packets should never decrease the displayed counter)
                        if in_prepare {
                            max_prepare_created = max_prepare_created.max(stats.prepare_objects_created);
                            max_prepare_total = max_prepare_total.max(stats.prepare_objects_total);
                        }
                        let prepare_created = max_prepare_created;
                        let prepare_total = max_prepare_total;
                        
                        // v0.7.13: Update agent state based on prepare phase
                        // v0.8.2: Only transition Ready→Preparing (not Running→Preparing, that's invalid)
                        // BUG FIX: Previously checked (Ready || Running), but Running→Preparing is
                        // not an allowed transition. Agents should never go backwards in phases.
                        if in_prepare && tracker.state == ControllerAgentState::Ready {
                            let _ = tracker.transition_to(ControllerAgentState::Preparing, "prepare phase started");
                        } else if !in_prepare && tracker.state == ControllerAgentState::Preparing {
                            let _ = tracker.transition_to(ControllerAgentState::Running, "workload phase started");
                        } else if !in_prepare && tracker.state == ControllerAgentState::Ready {
                            // No prepare phase, direct to running
                            let _ = tracker.transition_to(ControllerAgentState::Running, "workload started (no prepare)");
                        }
                        
                        // v0.7.9: Detect transition from prepare to workload and reset aggregator
                        if was_in_prepare_phase && !in_prepare {
                            debug!("Prepare phase completed, resetting aggregator for workload-only stats");
                            aggregator.reset_stats();
                            // Reset workload timer to measure actual workload duration (not including prepare)
                            workload_start = std::time::Instant::now();
                            was_in_prepare_phase = false;
                            
                            // v0.8.4: Reset prepare counters when transitioning to workload phase
                            max_prepare_created = 0;
                            max_prepare_total = 0;
                        } else if in_prepare {
                            was_in_prepare_phase = true;
                        }
                        
                        // v0.7.5: Extract final summary if completed
                        if stats.completed {
                            // v0.7.13: Check if agent completed with error (status 3)
                            if stats.status == 3 && !stats.error_message.is_empty() {
                                error!("❌ Agent {} FAILED: {}", stats.agent_id, stats.error_message);
                                tracker.error_message = Some(stats.error_message.clone());
                                let _ = tracker.transition_to(ControllerAgentState::Failed, "error status received");
                                aggregator.mark_completed(&stats.agent_id);
                                progress_bar.finish_with_message(format!("❌ Agent {} failed: {}", stats.agent_id, stats.error_message));
                                
                                // v0.8.1: Save partial results before aborting
                                // Aggregate whatever stats we have so far for diagnostic purposes
                                let partial_stats = aggregator.aggregate();
                                
                                // Finalize results directory with partial data
                                if let Err(e) = results_dir.finalize(partial_stats.elapsed_s) {
                                    error!("Failed to finalize results directory: {}", e);
                                }
                                
                                // Write test status showing the failure
                                let status_summary = check_test_status(&agent_trackers, &agent_summaries);
                                if let Err(e) = write_test_status(&results_dir, &status_summary) {
                                    error!("Failed to write STATUS.txt: {}", e);
                                }
                                if let Err(e) = print_test_status(&status_summary, &mut results_dir) {
                                    error!("Failed to print test status: {}", e);
                                }
                                
                                // Inform user that partial results were saved
                                let partial_msg = format!("Partial results saved to: {}", results_dir.path().display());
                                eprintln!("{}", partial_msg);
                                
                                // Abort all agents and exit
                                abort_all_agents(&agent_addrs, &mut agent_trackers, insecure, agent_ca, &agent_domain).await;
                                anyhow::bail!("Agent {} workload execution failed: {}", stats.agent_id, stats.error_message);
                            }
                            
                            // Success case
                            let _ = tracker.transition_to(ControllerAgentState::Completed, "workload completed");
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
                            let dead_count = agent_trackers.values().filter(|t| t.state == ControllerAgentState::Disconnected).count();
                            
                            // v0.7.9: Use captured prepare phase info
                            let msg = if in_prepare && prepare_total > 0 {
                                // Show prepare progress as "created/total (percentage)"
                                let pct = (prepare_created as f64 / prepare_total as f64 * 100.0) as u32;
                                if dead_count == 0 {
                                    format!("📦 Preparing: {}/{} objects ({}%) {}", 
                                            prepare_created, prepare_total, pct, agg.format_progress())
                                } else {
                                    format!("📦 Preparing: {}/{} objects ({}%) (⚠️ {} dead) {}", 
                                            prepare_created, prepare_total, pct, dead_count, agg.format_progress())
                                }
                            } else if dead_count == 0 {
                                agg.format_progress()
                            } else {
                                format!("{} (⚠️ {} dead)", agg.format_progress(), dead_count)
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
            
            _ = &mut shutdown_signal => {
                let sig = shutdown_signal.await;
                warn!("Received {} - aborting all agents", sig);
                progress_bar.finish_with_message(format!("Interrupted by {} - aborting agents...", sig));
                
                // v0.7.13: Abort all agents to stop running workload and reset state
                abort_all_agents(&agent_addrs, &mut agent_trackers, insecure, agent_ca, &agent_domain).await;
                
                // Cleanup deployments if any
                if !deployments.is_empty() {
                    warn!("Cleaning up agents...");
                    let _ = sai3_bench::ssh_deploy::cleanup_agents(deployments);
                }
                
                anyhow::bail!("Interrupted by {}", sig);
            }
        }
    }
    
    // Final aggregation
    let final_stats = aggregator.aggregate();
    progress_bar.finish_with_message(format!("✓ All {} agents completed\n\n", final_stats.num_agents));
    
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
    
    // v0.7.11: Display CPU utilization summary if available
    if final_stats.cpu_total_percent > 0.0 {
        println!("CPU: {:.1}% total (user: {:.1}%, system: {:.1}%, iowait: {:.1}%)",
                 final_stats.cpu_total_percent,
                 final_stats.cpu_user_percent,
                 final_stats.cpu_system_percent,
                 final_stats.cpu_iowait_percent);
    }
    
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
    
    // v0.8.1: Check agent states and write test status summary
    let status_summary = check_test_status(&agent_trackers, &agent_summaries);
    write_test_status(&results_dir, &status_summary)?;
    print_test_status(&status_summary, &mut results_dir)?;
    
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

/// v0.8.1: Test status summary for results directory
struct TestStatus {
    success: bool,
    total_agents: usize,
    completed: usize,
    failed: usize,
    disconnected: usize,
    aborting: usize,
    reconnect_count: usize,  // v0.8.2: Total disconnect/reconnect events across all agents
    total_ops: u64,
    agent_details: Vec<(String, ControllerAgentState, String)>, // (id, state, reason)
}

/// v0.8.1: Check final agent states and determine test success/failure
fn check_test_status(
    agent_trackers: &HashMap<String, AgentTracker>,
    agent_summaries: &[WorkloadSummary],
) -> TestStatus {
    // Filter to only count agent IDs (not IP:port addresses from initialization)
    // Agent IDs are the keys that agents report in their LiveStats messages
    let real_agents: Vec<_> = agent_trackers
        .iter()
        .filter(|(id, _)| !id.contains(':'))  // Exclude "host:port" entries
        .collect();
    
    let total_agents = real_agents.len();
    let completed = real_agents.iter().filter(|(_, t)| t.state == ControllerAgentState::Completed).count();
    let failed = real_agents.iter().filter(|(_, t)| t.state == ControllerAgentState::Failed).count();
    let disconnected = real_agents.iter().filter(|(_, t)| t.state == ControllerAgentState::Disconnected).count();
    let aborting = real_agents.iter().filter(|(_, t)| t.state == ControllerAgentState::Aborting).count();
    
    // v0.8.2: Sum reconnect counts across all agents for diagnostic visibility
    let reconnect_count: usize = real_agents.iter().map(|(_, t)| t.reconnect_count).sum();
    
    // Test succeeds only if ALL agents completed successfully
    let success = total_agents > 0 && completed == total_agents && failed == 0 && disconnected == 0 && aborting == 0;
    
    // Calculate total operations from summaries
    let total_ops: u64 = agent_summaries.iter().map(|s| s.total_ops).sum();
    
    // Collect agent details for status report (only real agent IDs, not addresses)
    let mut agent_details: Vec<(String, ControllerAgentState, String)> = real_agents
        .iter()
        .map(|(id, tracker)| {
            let reason = tracker.error_message
                .clone()
                .unwrap_or_else(|| format!("{:?}", tracker.state));
            ((*id).clone(), tracker.state, reason)
        })
        .collect();
    agent_details.sort_by(|a, b| a.0.cmp(&b.0)); // Sort by agent ID
    
    TestStatus {
        success,
        total_agents,
        completed,
        failed,
        disconnected,
        aborting,
        reconnect_count,  // v0.8.2: Include disconnect/reconnect diagnostic
        total_ops,
        agent_details,
    }
}

/// v0.8.1: Write STATUS.txt file with test outcome
fn write_test_status(results_dir: &ResultsDir, status: &TestStatus) -> anyhow::Result<()> {
    use std::fs;
    
    let status_path = results_dir.path().join("STATUS.txt");
    let mut content = String::new();
    
    // Header
    if status.success {
        content.push_str("TEST STATUS: SUCCESS\n");
    } else {
        content.push_str("TEST STATUS: FAILURE\n");
    }
    content.push_str(&format!("Timestamp: {}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")));
    content.push_str("\n");
    
    // Summary
    content.push_str("=== Summary ===\n");
    content.push_str(&format!("Total agents: {}\n", status.total_agents));
    content.push_str(&format!("Completed: {}\n", status.completed));
    content.push_str(&format!("Failed: {}\n", status.failed));
    content.push_str(&format!("Disconnected: {}\n", status.disconnected));
    content.push_str(&format!("Disconnect/Reconnect Count: {}\n", status.reconnect_count));
    content.push_str(&format!("Aborting: {}\n", status.aborting));
    content.push_str(&format!("Total operations: {}\n", status.total_ops));
    content.push_str("\n");
    
    // Per-agent details
    content.push_str("=== Agent Details ===\n");
    for (agent_id, state, reason) in &status.agent_details {
        content.push_str(&format!("{}: {:?} ({})\n", agent_id, state, reason));
    }
    
    // Failure details (if any)
    if !status.success {
        content.push_str("\n");
        content.push_str("=== Failure Analysis ===\n");
        
        if status.failed > 0 {
            content.push_str(&format!("⚠ {} agent(s) reported ERROR status\n", status.failed));
        }
        if status.disconnected > 0 {
            content.push_str(&format!("⚠ {} agent(s) disconnected or timed out\n", status.disconnected));
        }
        if status.aborting > 0 {
            content.push_str(&format!("⚠ {} agent(s) in aborting state\n", status.aborting));
        }
        if status.total_ops == 0 {
            content.push_str("⚠ Zero operations completed - workload did not execute\n");
        }
        
        content.push_str("\nRecommendations:\n");
        content.push_str("1. Check agent logs in agents/*/console.log\n");
        content.push_str("2. Review controller console.log for error messages\n");
        content.push_str("3. Verify agent connectivity and network stability\n");
        content.push_str("4. Check storage backend availability\n");
    }
    
    fs::write(&status_path, content)
        .with_context(|| format!("Failed to write STATUS.txt: {}", status_path.display()))?;
    
    Ok(())
}

/// v0.8.1: Print test status to console and console.log
fn print_test_status(status: &TestStatus, results_dir: &mut ResultsDir) -> anyhow::Result<()> {
    if status.success {
        let msg = "\n✅ Test PASSED: All agents completed successfully".to_string();
        println!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("   {} agents, {} total operations", status.total_agents, status.total_ops);
        println!("{}", msg);
        results_dir.write_console(&msg)?;
    } else {
        let msg = "\n❌ Test FAILED: One or more agents did not complete successfully".to_string();
        eprintln!("{}", msg);
        results_dir.write_console(&msg)?;
        
        let msg = format!("   Completed: {}/{}", status.completed, status.total_agents);
        eprintln!("{}", msg);
        results_dir.write_console(&msg)?;
        
        if status.failed > 0 {
            let msg = format!("   Failed: {} agent(s) reported errors", status.failed);
            eprintln!("{}", msg);
            results_dir.write_console(&msg)?;
        }
        
        if status.disconnected > 0 {
            let msg = format!("   Disconnected: {} agent(s) lost connection", status.disconnected);
            eprintln!("{}", msg);
            results_dir.write_console(&msg)?;
        }
        
        if status.aborting > 0 {
            let msg = format!("   Aborting: {} agent(s) in abort state", status.aborting);
            eprintln!("{}", msg);
            results_dir.write_console(&msg)?;
        }
        
        if status.total_ops == 0 {
            let msg = "   ⚠ CRITICAL: Zero operations completed - workload did not execute!".to_string();
            eprintln!("{}", msg);
            results_dir.write_console(&msg)?;
        }
        
        let msg = "\n   See STATUS.txt in results directory for full details".to_string();
        eprintln!("{}", msg);
        results_dir.write_console(&msg)?;
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
        
        let bucket_label = BUCKET_LABELS[idx];
        
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

/// v0.7.13: Validate workload configuration before execution
/// Checks for common errors that would cause workload startup to fail
/// This is the same validation that agents perform, so dry-run catches errors early
async fn validate_workload_config(config: &sai3_bench::config::Config) -> Result<()> {
    use sai3_bench::config::OpSpec;
    
    // Check that workload has operations configured
    if config.workload.is_empty() {
        bail!("No operations configured in workload");
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
                            .map_err(|e| anyhow!("Invalid glob pattern '{}': {}", path, e))?
                            .collect();
                        if paths.is_empty() {
                            bail!("No files found matching GET pattern: {}", path);
                        }
                    }
                }
            }
            OpSpec::Put { path, object_size, size_spec, .. } => {
                // PUT operations need size configuration
                if object_size.is_none() && size_spec.is_none() {
                    bail!("PUT operation at '{}' requires either 'object_size' or 'size_spec'", path);
                }
            }
            OpSpec::Delete { path } => {
                // Delete needs a valid path
                if path.is_empty() {
                    bail!("DELETE operation requires non-empty path");
                }
            }
            OpSpec::List { path } => {
                // List needs a valid path
                if path.is_empty() {
                    bail!("LIST operation requires non-empty path");
                }
            }
            OpSpec::Stat { path } => {
                // Stat needs a valid path
                if path.is_empty() {
                    bail!("STAT operation requires non-empty path");
                }
            }
            _ => {
                // Other operations (if any) are assumed valid
            }
        }
    }
    
    Ok(())
}
