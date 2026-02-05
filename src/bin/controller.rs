// src/bin/controller.rs
//
// Distributed workload controller with bidirectional streaming (v0.8.4+)
//
// Uses execute_workload RPC with separate control/stats channels to eliminate
// repeated READY messages and enable proper PING/PONG, ABORT, and state management.
// Replaces old run_workload_with_live_stats unidirectional streaming approach.

use anyhow::{anyhow, Context, Result, bail};
use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use tokio_stream::wrappers::ReceiverStream;
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
use sai3_bench::constants::{BUCKET_LABELS, CONTROLLER_PERF_LOG_INTERVAL_MS, CONTROLLER_DISPLAY_UPDATE_INTERVAL_MS};
// v0.8.15: Performance logging
use sai3_bench::perf_log::{PerfLogWriter, PerfLogDeltaTracker};
use sai3_bench::live_stats::WorkloadStage as LiveWorkloadStage;

pub mod pb {
    pub mod iobench {
        include!("../pb/iobench.rs");
    }
}

use pb::iobench::agent_client::AgentClient;
use pb::iobench::{ControlMessage, Empty, LiveStats, PrepareSummary, RunGetRequest, RunPutRequest, WorkloadSummary, control_message, live_stats::WorkloadStage, PreFlightRequest, BarrierRequest, BarrierResponse, AgentQueryRequest, AgentQueryResponse, PhaseProgress, WorkloadPhase};

// v0.8.25: Barrier synchronization
use std::time::Instant;
use std::collections::HashSet;
use sai3_bench::config::{BarrierType, PhaseBarrierConfig};

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
    Preparing,    // in_prepare_phase=true (STAGE_PREPARE)
    Running,      // RUNNING(2) received (STAGE_WORKLOAD)
    Cleaning,     // v0.8.9: Cleanup phase (STAGE_CLEANUP)
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
            | (Preparing, Failed)      // v0.8.13: Error during prepare phase
            | (Running, Cleaning)      // v0.8.9: Workload → cleanup
            | (Running, Completed)     // Success (no cleanup)
            | (Cleaning, Completed)    // v0.8.9: Cleanup → success
            | (Running, Failed)        // Error
            | (Cleaning, Failed)       // v0.8.9: Error during cleanup
            // Disconnect/timeout
            | (Connecting, Disconnected)
            | (Validating, Disconnected)
            | (Ready, Disconnected)
            | (Preparing, Disconnected)
            | (Running, Disconnected)
            | (Cleaning, Disconnected) // v0.8.9: Lost during cleanup
            // Abort paths
            | (Ready, Aborting)
            | (Preparing, Aborting)
            | (Running, Aborting)
            | (Cleaning, Aborting)     // v0.8.9: Abort during cleanup
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
            | (Disconnected, Cleaning)   // v0.8.9: Reconnect during cleanup phase
            | (Disconnected, Completed)  // Reconnect with completion message
            | (Disconnected, Failed)     // v0.8.13: Reconnect with error message
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
        //
        // WARNING: Weighted averaging of percentiles is mathematically incorrect.
        // Percentiles should be computed from merged HDR histograms, not averaged.
        // This limitation exists because LiveStats proto sends pre-computed percentiles
        // instead of serialized histograms. For statistically valid percentiles, use
        // the final workload_results.tsv which uses HDR histogram merging.
        //
        // The aggregate percentiles in perf_log.tsv are approximations for monitoring
        // purposes only. Per-agent perf_log files contain accurate percentiles computed
        // from each agent's local HDR histogram.
        let mut get_mean_weighted = 0.0f64;
        let mut get_p50_weighted = 0.0f64;
        let mut get_p90_weighted = 0.0f64;
        let mut get_p95_weighted = 0.0f64;
        let mut get_p99_weighted = 0.0f64;
        let mut put_mean_weighted = 0.0f64;
        let mut put_p50_weighted = 0.0f64;
        let mut put_p90_weighted = 0.0f64;
        let mut put_p95_weighted = 0.0f64;
        let mut put_p99_weighted = 0.0f64;
        let mut meta_mean_weighted = 0.0f64;
        let mut meta_p50_weighted = 0.0f64;
        let mut meta_p90_weighted = 0.0f64;
        let mut meta_p99_weighted = 0.0f64;

        // v0.7.11: CPU utilization sums (simple average across agents)
        let mut cpu_user_sum = 0.0f64;
        let mut cpu_system_sum = 0.0f64;
        let mut cpu_iowait_sum = 0.0f64;
        let mut cpu_total_sum = 0.0f64;
        
        // v0.8.14: Total concurrency across all agents
        let mut total_concurrency = 0u32;

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
                get_p90_weighted += stats.get_p90_us * weight;
                get_p95_weighted += stats.get_p95_us * weight;
                get_p99_weighted += stats.get_p99_us * weight;
            }
            if stats.put_ops > 0 {
                let weight = stats.put_ops as f64;
                put_mean_weighted += stats.put_mean_us * weight;
                put_p50_weighted += stats.put_p50_us * weight;
                put_p90_weighted += stats.put_p90_us * weight;
                put_p95_weighted += stats.put_p95_us * weight;
                put_p99_weighted += stats.put_p99_us * weight;
            }
            if stats.meta_ops > 0 {
                let weight = stats.meta_ops as f64;
                meta_mean_weighted += stats.meta_mean_us * weight;
                meta_p50_weighted += stats.meta_p50_us * weight;
                meta_p90_weighted += stats.meta_p90_us * weight;
                meta_p99_weighted += stats.meta_p99_us * weight;
            }
            
            // v0.7.11: Accumulate CPU metrics
            cpu_user_sum += stats.cpu_user_percent;
            cpu_system_sum += stats.cpu_system_percent;
            cpu_iowait_sum += stats.cpu_iowait_percent;
            cpu_total_sum += stats.cpu_total_percent;
            
            // v0.8.14: Accumulate concurrency
            total_concurrency += stats.concurrency;
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
        let get_p90_us = if total_get_ops > 0 {
            get_p90_weighted / total_get_ops as f64
        } else {
            0.0
        };
        let get_p95_us = if total_get_ops > 0 {
            get_p95_weighted / total_get_ops as f64
        } else {
            0.0
        };
        let get_p99_us = if total_get_ops > 0 {
            get_p99_weighted / total_get_ops as f64
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
        let put_p90_us = if total_put_ops > 0 {
            put_p90_weighted / total_put_ops as f64
        } else {
            0.0
        };
        let put_p95_us = if total_put_ops > 0 {
            put_p95_weighted / total_put_ops as f64
        } else {
            0.0
        };
        let put_p99_us = if total_put_ops > 0 {
            put_p99_weighted / total_put_ops as f64
        } else {
            0.0
        };
        let meta_mean_us = if total_meta_ops > 0 {
            meta_mean_weighted / total_meta_ops as f64
        } else {
            0.0
        };
        let meta_p50_us = if total_meta_ops > 0 {
            meta_p50_weighted / total_meta_ops as f64
        } else {
            0.0
        };
        let meta_p90_us = if total_meta_ops > 0 {
            meta_p90_weighted / total_meta_ops as f64
        } else {
            0.0
        };
        let meta_p99_us = if total_meta_ops > 0 {
            meta_p99_weighted / total_meta_ops as f64
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
            get_p90_us,
            get_p95_us,
            get_p99_us,
            total_put_ops,
            total_put_bytes,
            put_mean_us,
            put_p50_us,
            put_p90_us,
            put_p95_us,
            put_p99_us,
            total_meta_ops,
            meta_mean_us,
            meta_p50_us,
            meta_p90_us,
            meta_p99_us,
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
            // v0.8.14: Total concurrency
            total_concurrency,
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
    get_p90_us: f64,
    get_p95_us: f64,
    get_p99_us: f64,
    total_put_ops: u64,
    total_put_bytes: u64,
    put_mean_us: f64,
    put_p50_us: f64,
    put_p90_us: f64,
    put_p95_us: f64,
    put_p99_us: f64,
    total_meta_ops: u64,
    meta_mean_us: f64,
    meta_p50_us: f64,
    meta_p90_us: f64,
    meta_p99_us: f64,
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
    // v0.8.14: Total concurrency across all agents
    total_concurrency: u32,
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

        // Always show META line (for cleanup/list operations visibility)
        // v0.8.14: Show total threads across all agents
        let threads_str = if self.total_concurrency > 0 {
            format!(" ({} threads)", self.total_concurrency)
        } else {
            String::new()
        };
        
        format!(
            "{} of {} Agents{}\n  GET: {:.0} ops/s, {} (mean: {}, p50: {}, p95: {})\n  PUT: {:.0} ops/s, {} (mean: {}, p50: {}, p95: {})\n  META: {:.0} ops/s (mean: {}){}",
                self.num_agents,
                self.expected_agents,
                threads_str,
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
        let ca = agent_ca.cloned();  // Clone PathBuf if present
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
    // Map verbosity to appropriate levels for both controller and s3dlio:
    // -v (1): controller=info, s3dlio=warn (default passthrough)
    // -vv (2): controller=debug, s3dlio=info (detailed controller, operational s3dlio)
    // -vvv (3+): controller=trace, s3dlio=debug (full debugging both crates)
    let (ctl_level, s3dlio_level) = match cli.verbose {
        0 => ("warn", "warn"),   // Default: only warnings and errors
        1 => ("info", "warn"),   // -v: info level for controller, minimal s3dlio
        2 => ("debug", "info"),  // -vv: debug controller, info s3dlio
        _ => ("trace", "debug"), // -vvv+: trace controller, debug s3dlio
    };
    
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::new(format!("sai3bench_ctl={},s3dlio={}", ctl_level, s3dlio_level));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    debug!("Logging initialized at level: {}", ctl_level);

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

/// Run pre-flight validation on all agents
/// 
/// Returns Ok(()) if all agents pass validation (errors=0)
/// Returns Err if any agent reports errors
async fn run_preflight_validation(
    agent_clients: &mut [(String, AgentClient<Channel>)],
    config_yaml: &str,
) -> Result<()> {
    info!("Running pre-flight validation on {} agents", agent_clients.len());
    println!("\n🔍 Pre-flight Validation");
    println!("━━━━━━━━━━━━━━━━━━━━━━");
    
    let mut all_passed = true;
    let mut total_errors = 0;
    let mut total_warnings = 0;
    
    for (agent_id, client) in agent_clients.iter_mut() {
        // Send pre-flight request
        let request = PreFlightRequest {
            config_yaml: config_yaml.to_string(),
            agent_id: agent_id.clone(),
        };
        
        match client.pre_flight_validation(request).await {
            Ok(response) => {
                let result = response.into_inner();
                
                // Display agent results
                let status_icon = if result.passed { "✅" } else { "❌" };
                println!("\n{} Agent: {}", status_icon, agent_id);
                
                if result.error_count > 0 || result.warning_count > 0 || result.info_count > 0 {
                    // Convert proto results to internal ValidationResult for shared display
                    use sai3_bench::preflight::ValidationResult;
                    
                    let validation_results: Vec<ValidationResult> = result.results.iter().map(|vr| {
                        let level = match pb::iobench::ResultLevel::try_from(vr.level) {
                            Ok(pb::iobench::ResultLevel::Success) => sai3_bench::preflight::ResultLevel::Success,
                            Ok(pb::iobench::ResultLevel::Info) => sai3_bench::preflight::ResultLevel::Info,
                            Ok(pb::iobench::ResultLevel::Warning) => sai3_bench::preflight::ResultLevel::Warning,
                            Ok(pb::iobench::ResultLevel::Error) => sai3_bench::preflight::ResultLevel::Error,
                            _ => sai3_bench::preflight::ResultLevel::Error,
                        };
                        
                        let error_type = match pb::iobench::ErrorType::try_from(vr.error_type) {
                            Ok(pb::iobench::ErrorType::Authentication) => Some(sai3_bench::preflight::ErrorType::Authentication),
                            Ok(pb::iobench::ErrorType::Permission) => Some(sai3_bench::preflight::ErrorType::Permission),
                            Ok(pb::iobench::ErrorType::Network) => Some(sai3_bench::preflight::ErrorType::Network),
                            Ok(pb::iobench::ErrorType::Configuration) => Some(sai3_bench::preflight::ErrorType::Configuration),
                            Ok(pb::iobench::ErrorType::Resource) => Some(sai3_bench::preflight::ErrorType::Resource),
                            Ok(pb::iobench::ErrorType::System) => Some(sai3_bench::preflight::ErrorType::System),
                            _ => None,
                        };
                        
                        ValidationResult {
                            level,
                            error_type,
                            message: vr.message.clone(),
                            suggestion: vr.suggestion.clone(),
                            details: if vr.details.is_empty() { None } else { Some(vr.details.clone()) },
                            test_phase: vr.test_phase.clone(),
                        }
                    }).collect();
                    
                    // Use shared display function
                    let (passed, _errors, _warnings) = 
                        sai3_bench::preflight::display_validation_results(&validation_results, None);
                    
                    // Update totals (use proto counts which are authoritative)
                    total_errors += result.error_count;
                    total_warnings += result.warning_count;
                    if !passed {
                        all_passed = false;
                    }
                } else {
                    // All passed with no messages
                    total_errors += result.error_count;
                    total_warnings += result.warning_count;
                    if !result.passed {
                        all_passed = false;
                    }
                }
            }
            Err(e) => {
                error!("Pre-flight validation failed for agent {}: {}", agent_id, e);
                println!("❌ Agent: {} - RPC error: {}", agent_id, e);
                all_passed = false;
                total_errors += 1;
            }
        }
    }
    
    // Display summary
    println!("\n━━━━━━━━━━━━━━━━━━━━━━");
    if all_passed {
        println!("✅ Pre-flight validation passed ({} agents)", agent_clients.len());
        if total_warnings > 0 {
            println!("⚠️  {} warnings detected (non-fatal)", total_warnings);
        }
        Ok(())
    } else {
        println!("❌ Pre-flight validation FAILED");
        println!("   {} errors, {} warnings across {} agents", total_errors, total_warnings, agent_clients.len());
        println!("\nFix the above errors before running the workload.");
        bail!("Pre-flight validation failed with {} errors", total_errors)
    }
}

// ============================================================================
// v0.8.25: Barrier Synchronization Manager
// ============================================================================

/// Agent heartbeat tracking for barrier synchronization
#[derive(Debug, Clone)]
struct AgentHeartbeat {
    agent_id: String,
    last_heartbeat: Instant,
    last_progress: Option<PhaseProgress>,
    missed_count: u32,
    is_alive: bool,
    query_in_progress: bool,
}

/// Barrier status result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarrierStatus {
    Ready,     // All required agents at barrier
    Waiting,   // Still waiting for agents
    Degraded,  // Some failed, proceeding with survivors
    Failed,    // Insufficient agents ready
}

/// Barrier manager coordinates phase transitions across distributed agents
struct BarrierManager {
    agents: HashMap<String, AgentHeartbeat>,
    config: PhaseBarrierConfig,
    barrier_start: Instant,
    ready_agents: HashSet<String>,
    failed_agents: HashSet<String>,
}

impl BarrierManager {
    /// Create new barrier manager for a specific phase
    fn new(agent_ids: Vec<String>, config: PhaseBarrierConfig) -> Self {
        let mut agents = HashMap::new();
        for id in agent_ids {
            agents.insert(id.clone(), AgentHeartbeat {
                agent_id: id,
                last_heartbeat: Instant::now(),
                last_progress: None,
                missed_count: 0,
                is_alive: true,
                query_in_progress: false,
            });
        }
        
        Self {
            agents,
            config,
            barrier_start: Instant::now(),
            ready_agents: HashSet::new(),
            failed_agents: HashSet::new(),
        }
    }
    
    /// Process heartbeat from agent
    fn process_heartbeat(&mut self, agent_id: &str, progress: PhaseProgress) {
        if let Some(hb) = self.agents.get_mut(agent_id) {
            hb.last_heartbeat = Instant::now();
            hb.last_progress = Some(progress.clone());
            hb.missed_count = 0;  // Reset missed counter
            hb.is_alive = true;
            hb.query_in_progress = false;
            
            // Check if agent reached barrier (phase completed)
            if progress.phase_completed || progress.at_barrier {
                self.ready_agents.insert(agent_id.to_string());
            }
        }
    }
    
    /// Check for agents with missed heartbeats and query them
    async fn check_liveness(&mut self, client_pool: &HashMap<String, AgentClient<Channel>>) {
        let now = Instant::now();
        let heartbeat_interval = Duration::from_secs(self.config.heartbeat_interval);
        
        // First pass: identify agents to query (avoid borrow checker issues)
        let mut agents_to_query = Vec::new();
        
        for (agent_id, hb) in self.agents.iter_mut() {
            // Skip agents already marked as failed
            if self.failed_agents.contains(agent_id) {
                continue;
            }
            
            // Skip if heartbeat is recent
            if now.duration_since(hb.last_heartbeat) < heartbeat_interval {
                continue;
            }
            
            // Increment missed count
            hb.missed_count += 1;
            
            // If threshold exceeded and no query in progress, mark for querying
            if hb.missed_count >= self.config.missed_threshold && !hb.query_in_progress {
                warn!(
                    "Agent {} missed {} heartbeats ({}s), querying status...",
                    agent_id,
                    hb.missed_count,
                    now.duration_since(hb.last_heartbeat).as_secs()
                );
                
                hb.query_in_progress = true;
                agents_to_query.push(agent_id.clone());
            }
        }
        
        // Second pass: query agents (can now call &self methods)
        for agent_id in agents_to_query {
            match self.query_agent_with_retry(&agent_id, client_pool).await {
                Ok(response) => {
                    info!("✅ Agent {} responded to query: {}", agent_id, response.status_message);
                    if let Some(progress) = response.current_progress {
                        self.process_heartbeat(&agent_id, progress);
                    }
                }
                Err(e) => {
                    error!(
                        "❌ Agent {} failed to respond after {} retries: {}",
                        agent_id, self.config.query_retries, e
                    );
                    if let Some(hb) = self.agents.get_mut(&agent_id) {
                        hb.is_alive = false;
                        hb.query_in_progress = false;
                    }
                    self.failed_agents.insert(agent_id);
                }
            }
        }
    }
    
    /// Query agent with exponential backoff retry
    async fn query_agent_with_retry(
        &self,
        agent_id: &str,
        client_pool: &HashMap<String, AgentClient<Channel>>,
    ) -> Result<AgentQueryResponse> {
        let client = client_pool.get(agent_id)
            .ok_or_else(|| anyhow!("No client for agent {}", agent_id))?;
        
        let mut retries = 0;
        let mut backoff_ms = 1000;  // Start with 1s
        
        loop {
            let request = AgentQueryRequest {
                agent_id: agent_id.to_string(),
                reason: "missed_heartbeats".to_string(),
            };
            
            match tokio::time::timeout(
                Duration::from_secs(self.config.query_timeout),
                client.clone().query_agent_status(request)
            ).await {
                Ok(Ok(response)) => return Ok(response.into_inner()),
                Ok(Err(e)) => {
                    if retries >= self.config.query_retries {
                        return Err(anyhow!("Query failed after {} retries: {}", retries, e));
                    }
                    warn!(
                        "Query attempt {} failed for {}: {}, retrying in {}ms",
                        retries + 1, agent_id, e, backoff_ms
                    );
                }
                Err(_) => {
                    if retries >= self.config.query_retries {
                        return Err(anyhow!("Query timeout after {} retries", retries));
                    }
                    warn!(
                        "Query attempt {} timeout for {}, retrying in {}ms",
                        retries + 1, agent_id, backoff_ms
                    );
                }
            }
            
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            retries += 1;
            backoff_ms = (backoff_ms * 2).min(10_000);  // Cap at 10s
        }
    }
    
    /// Check if barrier is satisfied (all/majority/best-effort)
    fn check_barrier(&self) -> BarrierStatus {
        let ready_count = self.ready_agents.len();
        let total_count = self.agents.len();
        let failed_count = self.failed_agents.len();
        let alive_count = total_count - failed_count;
        
        match self.config.barrier_type {
            BarrierType::AllOrNothing => {
                if ready_count == alive_count {
                    BarrierStatus::Ready
                } else if failed_count > 0 {
                    // Any failure in AllOrNothing mode = abort
                    BarrierStatus::Failed
                } else {
                    BarrierStatus::Waiting
                }
            }
            
            BarrierType::Majority => {
                if ready_count > alive_count / 2 {
                    BarrierStatus::Ready
                } else if alive_count - ready_count < alive_count / 2 {
                    // Not enough agents left to reach majority
                    BarrierStatus::Failed
                } else {
                    BarrierStatus::Waiting
                }
            }
            
            BarrierType::BestEffort => {
                if ready_count == alive_count {
                    BarrierStatus::Ready
                } else if ready_count > 0 && failed_count > 0 {
                    // Some ready, some failed - proceed with ready ones
                    BarrierStatus::Degraded
                } else {
                    BarrierStatus::Waiting
                }
            }
        }
    }
    
    /// Wait for barrier with heartbeat-based liveness checking
    async fn wait_for_barrier(
        &mut self,
        phase_name: &str,
        client_pool: &HashMap<String, AgentClient<Channel>>,
    ) -> Result<BarrierStatus> {
        let start = Instant::now();
        let mut last_liveness_check = Instant::now();
        let liveness_check_interval = Duration::from_secs(
            self.config.heartbeat_interval.max(10)
        );
        
        loop {
            // Check barrier status
            let status = self.check_barrier();
            
            match status {
                BarrierStatus::Ready => {
                    info!(
                        "✅ Barrier '{}' ready: all {} agents synchronized (elapsed: {:?})",
                        phase_name,
                        self.ready_agents.len(),
                        start.elapsed()
                    );
                    return Ok(status);
                }
                
                BarrierStatus::Degraded => {
                    warn!(
                        "⚠️  Barrier '{}' degraded: {}/{} agents ready, {} failed (elapsed: {:?})",
                        phase_name,
                        self.ready_agents.len(),
                        self.agents.len(),
                        self.failed_agents.len(),
                        start.elapsed()
                    );
                    return Ok(status);
                }
                
                BarrierStatus::Failed => {
                    error!(
                        "❌ Barrier '{}' failed: only {}/{} agents ready, {} failed",
                        phase_name,
                        self.ready_agents.len(),
                        self.agents.len(),
                        self.failed_agents.len()
                    );
                    return Err(anyhow!("Barrier failed - insufficient agents"));
                }
                
                BarrierStatus::Waiting => {
                    // Check liveness periodically
                    if last_liveness_check.elapsed() >= liveness_check_interval {
                        self.check_liveness(client_pool).await;
                        last_liveness_check = Instant::now();
                    }
                    
                    // Poll every 100ms
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    
                    // Log progress every 10 seconds
                    if start.elapsed().as_secs() % 10 == 0 && start.elapsed().as_millis() % 10000 < 100 {
                        let waiting: Vec<_> = self.agents.keys()
                            .filter(|a| !self.ready_agents.contains(*a) && !self.failed_agents.contains(*a))
                            .map(|s| {
                                let hb = &self.agents[s];
                                let prog = hb.last_progress.as_ref()
                                    .map(|p| p.operations_completed)
                                    .unwrap_or(0);
                                format!(
                                    "{} ({}s ago, {} ops)",
                                    s,
                                    Instant::now().duration_since(hb.last_heartbeat).as_secs(),
                                    prog
                                )
                            })
                            .collect();
                        
                        info!(
                            "Barrier '{}': {}/{} ready, waiting for: {}",
                            phase_name,
                            self.ready_agents.len(),
                            self.agents.len() - self.failed_agents.len(),
                            waiting.join(", ")
                        );
                    }
                }
            }
        }
    }
}

// ============================================================================
// End Barrier Synchronization Manager
// ============================================================================

/// Execute a distributed workload across multiple agents
#[allow(clippy::too_many_arguments)]
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
    // 2. Config file's distributed.shared_filesystem setting (REQUIRED)
    // Note: shared_filesystem applies to ANY storage type (file://, s3://, az://, gs://, etc.)
    let is_shared_storage = if let Some(explicit) = shared_prepare {
        debug!("Using explicit --shared-prepare CLI flag: {}", explicit);
        explicit
    } else if let Some(ref distributed) = config.distributed {
        debug!("Using config file's shared_filesystem setting: {}", distributed.shared_filesystem);
        distributed.shared_filesystem
    } else {
        bail!("Distributed execution requires either --shared-prepare CLI flag or distributed.shared_filesystem in config.\n\nExample config:\n  distributed:\n    shared_filesystem: true  # or false for per-agent storage\n    tree_creation_mode: concurrent\n    path_selection: random");
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
    
    let storage_msg = format!("Storage mode: {}", if is_shared_storage { "shared (all agents access same data)" } else { "per-agent (isolated storage)" });
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
    
    // v0.8.7: Calculate num_agents once outside the loop (avoid borrowing agent_addrs in async tasks)
    let num_agents = agent_addrs.len() as u32;
    
    // v0.8.23: PRE-FLIGHT VALIDATION - Connect to all agents and validate before spawning streams
    info!("Connecting to {} agents for pre-flight validation", agent_addrs.len());
    let mut agent_clients: Vec<(String, AgentClient<Channel>)> = Vec::new();
    
    for (idx, agent_addr) in agent_addrs.iter().enumerate() {
        let agent_id = ids[idx].clone();
        debug!("Connecting to agent {} at {}", agent_id, agent_addr);
        
        match mk_client(agent_addr, insecure, agent_ca, agent_domain).await {
            Ok(client) => {
                agent_clients.push((agent_id.clone(), client));
                debug!("Connected to agent {} successfully", agent_id);
            }
            Err(e) => {
                error!("Failed to connect to agent {} at {}: {}", agent_id, agent_addr, e);
                bail!("Failed to connect to agent {}: {}", agent_id, e);
            }
        }
    }
    
    // v0.8.23: DISTRIBUTED CONFIG VALIDATION - Check for common configuration errors
    // Validates base_uri usage with multi-endpoint in isolated mode
    info!("Validating distributed configuration");
    match sai3_bench::preflight::distributed::validate_distributed_config(&config) {
        Ok(validation_results) => {
            if !validation_results.is_empty() {
                println!("\n🔍 Distributed Config Validation");
                println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
                
                let (passed, errors, warnings) = 
                    sai3_bench::preflight::display_validation_results(&validation_results, None);
                
                if !passed {
                    println!("\n❌ Distributed configuration validation FAILED");
                    println!("   {} errors, {} warnings", errors, warnings);
                    println!("\nFix the above configuration errors before running the workload.");
                    bail!("Distributed configuration validation failed with {} errors", errors);
                } else if warnings > 0 {
                    println!("\n⚠️  {} warnings detected in distributed configuration (non-fatal)", warnings);
                    println!();
                }
            } else {
                debug!("Distributed configuration validation passed (no issues detected)");
            }
        }
        Err(e) => {
            error!("Distributed config validation error: {}", e);
            bail!("Failed to validate distributed configuration: {}", e);
        }
    }
    
    // Run pre-flight validation on all connected agents
    run_preflight_validation(&mut agent_clients, &config_yaml).await?;
    
    info!("Pre-flight validation passed - proceeding with workload execution");
    
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
        
        // v0.8.22: Apply per-agent multi_endpoint override if specified
        // This allows static endpoint mapping: each agent uses specific subset of endpoints
        let agent_specific_config = if let Some(ref distributed) = config.distributed {
            if idx < distributed.agents.len() {
                if let Some(ref agent_multi_ep) = distributed.agents[idx].multi_endpoint {
                    // Agent has multi_endpoint override - replace global config
                    let mut modified_config = config.clone();
                    modified_config.multi_endpoint = Some(agent_multi_ep.clone());
                    
                    // Serialize modified config to YAML
                    match serde_yaml::to_string(&modified_config) {
                        Ok(yaml) => {
                            debug!("Agent {}: Using per-agent multi_endpoint override ({} endpoints)", 
                                   agent_id, agent_multi_ep.endpoints.len());
                            yaml
                        }
                        Err(e) => {
                            error!("Failed to serialize agent-specific config: {}", e);
                            config_yaml.clone()  // Fallback to original config
                        }
                    }
                } else {
                    // No override - use global config
                    config_yaml.clone()
                }
            } else {
                config_yaml.clone()
            }
        } else {
            config_yaml.clone()
        };
        
        let addr = agent_addr.clone();
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

            debug!("Connected to agent {}, opening bidirectional stream", addr);

            // PHASE 4: Open bidirectional stream (control + stats channels)
            let (tx_control, rx_control) = tokio::sync::mpsc::channel::<ControlMessage>(32);
            let control_stream = ReceiverStream::new(rx_control);
            
            let mut stream = client
                .execute_workload(control_stream)
                .await
                .with_context(|| format!("execute_workload on agent {}", addr))?
                .into_inner();

            debug!("Agent {} bidirectional stream opened", addr);
            
            // Phase 1: Send START command with config
            let config_msg = ControlMessage {
                command: control_message::Command::Start as i32,
                config_yaml: agent_specific_config.clone(),
                agent_id: agent_id.clone(),
                path_prefix: effective_prefix.clone(),
                shared_storage: shared,
                start_timestamp_ns: 0,  // Phase 1: no timestamp yet
                ack_sequence: 0,
                agent_index: idx as u32,  // v0.8.7: For distributed cleanup
                num_agents,               // v0.8.7: For distributed cleanup
            };
            
            if let Err(e) = tx_control.send(config_msg).await {
                return Err(anyhow!("Failed to send config to agent {}: {}", agent_id, e));
            }
            
            debug!("Agent {} sent config via control channel", addr);
            
            // Wait for READY status (status=1) with agent timestamp
            let mut ready_received = false;
            let ready_timeout = tokio::time::Duration::from_secs(30);
            let ready_deadline = tokio::time::Instant::now() + ready_timeout;
            
            while !ready_received && tokio::time::Instant::now() < ready_deadline {
                match tokio::time::timeout(
                    tokio::time::Duration::from_secs(1),
                    stream.message()
                ).await {
                    Ok(Ok(Some(stats))) => {
                        if stats.status == 1 {  // READY
                            ready_received = true;
                            debug!("Agent {} sent READY with timestamp {}ns", agent_id, stats.agent_timestamp_ns);
                            
                            // Forward READY to main loop for state tracking
                            if let Err(e) = tx.send(stats) {
                                error!("Failed to forward READY from agent {}: {}", addr, e);
                                break;
                            }
                        } else if stats.status == 3 {  // ERROR
                            return Err(anyhow!("Agent {} rejected config: {}", agent_id, stats.error_message));
                        } else {
                            // Unexpected status during startup - forward and continue
                            let _ = tx.send(stats);
                        }
                    }
                    Ok(Ok(None)) => {
                        // Stream ended unexpectedly
                        return Err(anyhow!("Agent {} stream ended before READY", agent_id));
                    }
                    Ok(Err(e)) => {
                        // gRPC error
                        return Err(anyhow!("Agent {} gRPC error: {}", agent_id, e));
                    }
                    Err(_) => {
                        // Timeout - continue waiting
                        continue;
                    }
                }
            }
            
            if !ready_received {
                return Err(anyhow!("Agent {} did not send READY within {}s", agent_id, ready_timeout.as_secs()));
            }
            
            // Phase 2: Send START command with coordinated timestamp
            // This happens after ALL agents are READY (controlled by main loop)
            // For now, send it immediately after READY for backward compatibility with test
            let start_msg = ControlMessage {
                command: control_message::Command::Start as i32,
                config_yaml: String::new(),  // Already sent in Phase 1
                agent_id: agent_id.clone(),
                path_prefix: String::new(),
                shared_storage: false,
                start_timestamp_ns: start_ns,  // Phase 2: coordinated start time
                ack_sequence: 0,
                agent_index: idx as u32,  // v0.8.7: Redundant but consistent
                num_agents,               // v0.8.7: Redundant but consistent
            };
            
            if let Err(e) = tx_control.send(start_msg).await {
                return Err(anyhow!("Failed to send start timestamp to agent {}: {}", agent_id, e));
            }
            
            debug!("Agent {} sent start timestamp {}ns via control channel", addr, start_ns);
            
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
                    get_ops: 0, get_bytes: 0, get_mean_us: 0.0, get_p50_us: 0.0, get_p90_us: 0.0, get_p95_us: 0.0, get_p99_us: 0.0,
                    put_ops: 0, put_bytes: 0, put_mean_us: 0.0, put_p50_us: 0.0, put_p90_us: 0.0, put_p95_us: 0.0, put_p99_us: 0.0,
                    meta_ops: 0, meta_mean_us: 0.0, meta_p50_us: 0.0, meta_p90_us: 0.0, meta_p99_us: 0.0,
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
                    sequence: 0,
                    // v0.8.9: Stage tracking (error state)
                    current_stage: 0, stage_name: String::new(), stage_progress_current: 0, stage_progress_total: 0, stage_elapsed_s: 0.0,
                    // v0.8.14: Concurrency (0 for error messages)
                    concurrency: 0,
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
                    5 => {  // ABORTED - v0.8.13: Handle during startup (shouldn't happen but be safe)
                        warn!("Agent {} sent ABORTED status during startup (unexpected)", stats.agent_id);
                        tracker.error_message = Some("Agent aborted during startup".to_string());
                        if let Err(e) = tracker.transition_to(ControllerAgentState::Failed, "ABORTED status during startup") {
                            warn!("Failed to transition agent {} to Failed: {}", stats.agent_id, e);
                        }
                        eprintln!("  ❌ {} aborted during startup", stats.agent_id);
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
                abort_all_agents(agent_addrs, &mut agent_trackers, insecure, agent_ca, agent_domain).await;
                
                anyhow::bail!("Agent startup validation timeout");
            }
            sig = &mut shutdown_signal => {
                eprintln!("\n⚠️  Received {} during startup", sig);
                
                // v0.7.13: Abort all agents to prevent orphaned workload execution
                abort_all_agents(agent_addrs, &mut agent_trackers, insecure, agent_ca, agent_domain).await;
                
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
        abort_all_agents(agent_addrs, &mut agent_trackers, insecure, agent_ca, agent_domain).await;
        
        anyhow::bail!("{} agent(s) failed startup validation", failed_agents.len());
    }
    
    let ready_count = agent_trackers.values().filter(|t| t.state == ControllerAgentState::Ready).count();
    eprintln!("✅ All {} agents ready - starting workload execution\n", ready_count);
    
    // v0.8.25: Initialize BarrierManager if barrier_sync is enabled
    // TODO: Complete integration in next phase (requires client pool)
    let mut barrier_manager: Option<BarrierManager> = if let Some(ref distributed_config) = config.distributed {
        if distributed_config.barrier_sync.enabled {
            let agent_ids: Vec<String> = agent_addrs.iter().cloned().collect();
            let default_config = distributed_config.barrier_sync.get_phase_config("prepare");
            eprintln!("🔄 Barrier synchronization enabled");
            Some(BarrierManager::new(agent_ids, default_config))
        } else {
            None
        }
    } else {
        None
    };
    
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
    
    // v0.8.19: Performance logging is always enabled with 1-second interval
    // - Aggregate perf_log.tsv in results directory root
    // - Per-agent perf_log.tsv in agents/{agent-id}/ subdirectories
    let perf_log_path = results_dir.path().join("perf_log.tsv");
    let mut perf_log_writer = match PerfLogWriter::new(&perf_log_path) {
        Ok(writer) => {
            info!("Created aggregate perf-log at: {}", perf_log_path.display());
            writer
        }
        Err(e) => {
            anyhow::bail!("Failed to create aggregate perf-log at {}: {}", perf_log_path.display(), e);
        }
    };
    let mut perf_log_tracker = PerfLogDeltaTracker::new();
    
    // v0.8.16: Per-agent perf log writers and trackers
    // Lazily initialized when first stats arrive from each agent
    let agents_dir = results_dir.path().join("agents");
    let mut agent_perf_writers: std::collections::HashMap<String, PerfLogWriter> = std::collections::HashMap::new();
    let mut agent_perf_trackers: std::collections::HashMap<String, PerfLogDeltaTracker> = std::collections::HashMap::new();
    
    // Initialize tracker with warmup duration
    let warmup_duration = config.warmup_period.unwrap_or(Duration::ZERO);
    let warmup_opt = if warmup_duration > Duration::ZERO {
        Some(warmup_duration)
    } else {
        None
    };
    perf_log_tracker.start(warmup_opt);
    let mut last_perf_log_flush = std::time::Instant::now();
    
    // v0.8.19: CRITICAL - Precise 1-second interval timer for perf_log
    // Uses MissedTickBehavior::Burst to ensure exact 1-second intervals even if processing takes time
    // Display updates can drift, but perf_log MUST be precise for accurate time-series analysis
    let mut perf_log_timer = tokio::time::interval(Duration::from_millis(CONTROLLER_PERF_LOG_INTERVAL_MS));
    perf_log_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    perf_log_timer.tick().await; // First tick is immediate, consume it
    
    // Flush every 10 seconds to minimize data loss on crash
    let perf_log_flush_interval = Duration::from_secs(
        sai3_bench::constants::PERF_LOG_FLUSH_INTERVAL_SECS
    );
    
    // v0.8.9: Track stage for transition detection (replaces was_in_prepare_phase)
    let mut last_stage = WorkloadStage::StageUnknown;
    
    // v0.8.4: Track prepare phase high-water marks (never decrease during prepare)
    let mut max_prepare_created: u64 = 0;
    let mut max_prepare_total: u64 = 0;
    
    // v0.8.9: Track cleanup phase high-water marks (never decrease during cleanup)
    let mut max_cleanup_current: u64 = 0;
    let mut max_cleanup_total: u64 = 0;
    
    // v0.8.19: Cache last aggregate computed by perf_log timer for display use
    // This prevents race condition where display and perf_log both call aggregate()
    let mut last_aggregate: Option<AggregateStats> = None;
    
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
                        
                        // v0.8.9: Compute current_stage early for state transitions
                        let current_stage = WorkloadStage::try_from(stats.current_stage)
                            .unwrap_or(WorkloadStage::StageUnknown);
                        
                        // v0.8.25: Process stats as heartbeat for barrier manager (disabled until client pool available)
                        // TODO: Uncomment when implementing full barrier integration
                        /*
                        if let Some(ref mut bm) = barrier_manager {
                            let phase_progress = pb::iobench::PhaseProgress {
                                agent_id: stats.agent_id.clone(),
                                current_phase: match current_stage {
                                    WorkloadStage::StagePrepare => pb::iobench::WorkloadPhase::PhasePreparing as i32,
                                    WorkloadStage::StageWorkload => pb::iobench::WorkloadPhase::PhaseExecuting as i32,
                                    WorkloadStage::StageCleanup => pb::iobench::WorkloadPhase::PhaseCleaning as i32,
                                    _ => pb::iobench::WorkloadPhase::PhaseIdle as i32,
                                },
                                phase_start_time_ms: 0,
                                heartbeat_time_ms: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64,
                                objects_created: stats.prepare_objects_created,
                                objects_total: stats.prepare_objects_total,
                                current_operation: stats.stage_name.clone(),
                                bytes_written: stats.put_bytes,
                                errors_encountered: 0,
                                operations_completed: stats.get_ops + stats.put_ops + stats.meta_ops,
                                current_throughput: if stats.elapsed_s > 0.0 {
                                    (stats.get_ops + stats.put_ops) as f64 / stats.elapsed_s
                                } else { 0.0 },
                                phase_elapsed_ms: (stats.stage_elapsed_s * 1000.0) as u64,
                                objects_deleted: if current_stage == WorkloadStage::StageCleanup {
                                    stats.stage_progress_current
                                } else { 0 },
                                objects_remaining: if current_stage == WorkloadStage::StageCleanup {
                                    stats.stage_progress_total.saturating_sub(stats.stage_progress_current)
                                } else { 0 },
                                is_stuck: false,
                                stuck_reason: String::new(),
                                progress_rate: if stats.stage_elapsed_s > 0.0 {
                                    stats.stage_progress_current as f64 / stats.stage_elapsed_s
                                } else { 0.0 },
                                phase_completed: stats.completed,
                                at_barrier: false,
                            };
                            bm.process_heartbeat(&stats.agent_id, phase_progress);
                        }
                        */
                        
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
                            
                            // v0.8.9: Use current_stage for recovery state
                            let recovery_stage = WorkloadStage::try_from(stats.current_stage)
                                .unwrap_or(WorkloadStage::StageUnknown);
                            let new_state = if stats.completed {
                                ControllerAgentState::Completed
                            } else {
                                match recovery_stage {
                                    WorkloadStage::StagePrepare => ControllerAgentState::Preparing,
                                    WorkloadStage::StageCleanup => ControllerAgentState::Cleaning,
                                    _ => ControllerAgentState::Running,
                                }
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
                        
                        // v0.8.9: current_stage already computed above for barrier tracking
                        // v0.8.4: Use high-water marks during prepare phase to prevent backwards movement
                        // (delayed packets should never decrease the displayed counter)
                        if current_stage == WorkloadStage::StagePrepare {
                            max_prepare_created = max_prepare_created.max(stats.prepare_objects_created);
                            max_prepare_total = max_prepare_total.max(stats.prepare_objects_total);
                        }
                        let prepare_created = max_prepare_created;
                        let prepare_total = max_prepare_total;
                        
                        // v0.8.9: Track cleanup progress with high-water marks
                        if current_stage == WorkloadStage::StageCleanup {
                            max_cleanup_current = max_cleanup_current.max(stats.stage_progress_current);
                            max_cleanup_total = max_cleanup_total.max(stats.stage_progress_total);
                        }
                        let cleanup_current = max_cleanup_current;
                        let cleanup_total = max_cleanup_total;
                        let stage_elapsed_s = stats.stage_elapsed_s;  // Capture before move
                        
                        // v0.8.9: Update agent state based on current_stage enum
                        match current_stage {
                            WorkloadStage::StagePrepare => {
                                if tracker.state == ControllerAgentState::Ready {
                                    let _ = tracker.transition_to(ControllerAgentState::Preparing, "prepare phase started");
                                }
                            }
                            WorkloadStage::StageWorkload => {
                                if tracker.state == ControllerAgentState::Preparing {
                                    let _ = tracker.transition_to(ControllerAgentState::Running, "workload phase started");
                                } else if tracker.state == ControllerAgentState::Ready {
                                    let _ = tracker.transition_to(ControllerAgentState::Running, "workload started (no prepare)");
                                }
                            }
                            WorkloadStage::StageCleanup => {
                                if tracker.state == ControllerAgentState::Running {
                                    let _ = tracker.transition_to(ControllerAgentState::Cleaning, "cleanup phase started");
                                }
                            }
                            _ => {}  // Unknown or Custom stages don't trigger state changes
                        }
                        
                        // v0.8.9: Track last known stage for phase transition detection
                        let prev_stage = last_stage;
                        last_stage = current_stage;
                        
                        // v0.8.19: Lazily initialize per-agent perf-log writer and tracker (always enabled)
                        // Per-agent perf-log creation for each active agent
                        if !stats.completed && stats.status != 3 && stats.status != 5 {
                            let agent_id = stats.agent_id.clone();
                            
                            // Create agent directory and writer if not exists
                            if !agent_perf_writers.contains_key(&agent_id) {
                                let agent_dir = agents_dir.join(&agent_id);
                                if let Err(e) = std::fs::create_dir_all(&agent_dir) {
                                    warn!("Failed to create agent directory {}: {}", agent_dir.display(), e);
                                } else {
                                    let agent_perf_path = agent_dir.join("perf_log.tsv");
                                    match PerfLogWriter::new(&agent_perf_path) {
                                        Ok(writer) => {
                                            debug!("Created per-agent perf-log at: {}", agent_perf_path.display());
                                            agent_perf_writers.insert(agent_id.clone(), writer);
                                            
                                            // Initialize tracker with warmup duration
                                            let mut tracker = PerfLogDeltaTracker::new();
                                            tracker.start(warmup_opt);
                                            agent_perf_trackers.insert(agent_id.clone(), tracker);
                                        }
                                        Err(e) => {
                                            warn!("Failed to create per-agent perf-log for {}: {}", agent_id, e);
                                        }
                                    }
                                }
                            }
                        }
                        
                        // v0.7.9: Detect transition from prepare to workload and reset aggregator
                        if prev_stage == WorkloadStage::StagePrepare && current_stage == WorkloadStage::StageWorkload {
                            debug!("Prepare phase completed, resetting aggregator for workload-only stats");
                            aggregator.reset_stats();
                            // Reset workload timer to measure actual workload duration (not including prepare)
                            workload_start = std::time::Instant::now();
                            
                            // v0.8.16: Reset perf_log warmup timers for workload phase
                            // Warmup should be measured from workload start, not prepare start
                            perf_log_tracker.reset_warmup_for_workload(warmup_opt);
                            for tracker in agent_perf_trackers.values_mut() {
                                tracker.reset_warmup_for_workload(warmup_opt);
                            }
                            debug!("Reset perf_log warmup timers for workload phase");
                            
                            // v0.8.4: Reset prepare counters when transitioning to workload phase
                            max_prepare_created = 0;
                            max_prepare_total = 0;
                            
                            // v0.8.25: Wait for prepare barrier if enabled
                            // TODO: Uncomment when client pool is available
                            // if let Some(ref mut bm) = barrier_manager {
                            //     eprintln!("\n🔄 Prepare phase complete - waiting for all agents at barrier...");
                            //     match bm.wait_for_barrier("prepare", &agent_clients).await {
                            //         Ok(BarrierStatus::Ready) => eprintln!("✅ All agents synchronized"),
                            //         Ok(BarrierStatus::Degraded) => eprintln!("⚠️  Barrier degraded"),
                            //         Ok(BarrierStatus::Failed) => anyhow::bail!("Barrier failed"),
                            //         Ok(BarrierStatus::Waiting) => warn!("Still waiting"),
                            //         Err(e) => anyhow::bail!("Barrier error: {}", e),
                            //     }
                            // }
                        }
                        
                        // v0.8.13: Check for ERROR status FIRST, regardless of completed flag
                        // Agent sends completed=false with status=3 on error, so we must check status first
                        // Check status code first, error_message may be empty in some edge cases
                        if stats.status == 3 {
                            let error_msg = if stats.error_message.is_empty() {
                                "Unknown error (no message provided)".to_string()
                            } else {
                                stats.error_message.clone()
                            };
                            error!("❌ Agent {} FAILED: {}", stats.agent_id, error_msg);
                            tracker.error_message = Some(error_msg.clone());
                            let _ = tracker.transition_to(ControllerAgentState::Failed, "error status received");
                            aggregator.mark_completed(&stats.agent_id);
                            progress_bar.finish_with_message(format!("❌ Agent {} failed: {}", stats.agent_id, error_msg));
                            
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
                            abort_all_agents(agent_addrs, &mut agent_trackers, insecure, agent_ca, agent_domain).await;
                            anyhow::bail!("Agent {} workload execution failed: {}", stats.agent_id, error_msg);
                        }
                        
                        // v0.8.13: Handle ABORTED status (status=5)
                        // Agent sends this when it receives and processes an abort command
                        if stats.status == 5 {
                            info!("Agent {} acknowledged abort", stats.agent_id);
                            let _ = tracker.transition_to(ControllerAgentState::Completed, "abort acknowledged");
                            aggregator.mark_completed(&stats.agent_id);
                            // Abort acknowledgment handled - skip regular completion check
                            continue;
                        }
                        
                        // v0.8.13: Defensive check for COMPLETED status (backup for completed flag)
                        // Agent should always send completed=true with status=4, but be defensive
                        if stats.status == 4 && !stats.completed {
                            warn!("Agent {} sent status=4 (COMPLETED) but completed flag is false - treating as completed", stats.agent_id);
                            let _ = tracker.transition_to(ControllerAgentState::Completed, "status 4 received (completed flag mismatch)");
                            aggregator.mark_completed(&stats.agent_id);
                            continue;
                        }
                        
                        // v0.7.5: Extract final summary if completed successfully
                        if stats.completed {
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
                        
                        // v0.8.19: Display update uses cached aggregate from perf_log timer
                        // Only perf_log timer calls aggregate() to ensure precise 1-second windowing
                        if last_update.elapsed() > std::time::Duration::from_millis(CONTROLLER_DISPLAY_UPDATE_INTERVAL_MS) {
                            // Use last aggregate computed by perf_log timer, or compute initial one
                            let agg = if let Some(ref cached) = last_aggregate {
                                cached.clone()
                            } else {
                                // First update before timer fires - compute initial aggregate
                                aggregator.aggregate()
                            };
                            let dead_count = agent_trackers.values().filter(|t| t.state == ControllerAgentState::Disconnected).count();
                            
                            // v0.8.9: Use current_stage for stage-aware display
                            let (msg, progress_pos, progress_len, progress_unit) = match current_stage {
                                WorkloadStage::StagePrepare if prepare_total > 0 => {
                                    // Show prepare progress as "created/total (percentage)"
                                    let pct = (prepare_created as f64 / prepare_total as f64 * 100.0) as u32;
                                    let msg = if dead_count == 0 {
                                        format!("📦 Preparing: {}/{} objects ({}%) {}", 
                                                prepare_created, prepare_total, pct, agg.format_progress())
                                    } else {
                                        format!("📦 Preparing: {}/{} objects ({}%) (⚠️ {} dead) {}", 
                                                prepare_created, prepare_total, pct, dead_count, agg.format_progress())
                                    };
                                    (msg, prepare_created, prepare_total, "objects")
                                }
                                WorkloadStage::StageCleanup if cleanup_total > 0 => {
                                    // Show cleanup progress as "deleted/total (percentage)"
                                    let pct = (cleanup_current as f64 / cleanup_total as f64 * 100.0) as u32;
                                    // v0.8.9: Format cleanup-specific progress (DELETE ops/s)
                                    let cleanup_rate = if stage_elapsed_s > 0.0 {
                                        cleanup_current as f64 / stage_elapsed_s
                                    } else {
                                        0.0
                                    };
                                    let msg = if dead_count == 0 {
                                        format!("🧹 Cleanup: {}/{} deleted ({}%) | {:.0} DEL/s", 
                                                cleanup_current, cleanup_total, pct, cleanup_rate)
                                    } else {
                                        format!("🧹 Cleanup: {}/{} deleted ({}%) | {:.0} DEL/s (⚠️ {} dead)", 
                                                cleanup_current, cleanup_total, pct, cleanup_rate, dead_count)
                                    };
                                    (msg, cleanup_current, cleanup_total, "deleted")
                                }
                                _ => {
                                    // Workload phase (or unknown): show elapsed time and ops/s
                                    let msg = if dead_count == 0 {
                                        agg.format_progress()
                                    } else {
                                        format!("{} (⚠️ {} dead)", agg.format_progress(), dead_count)
                                    };
                                    let elapsed_secs = workload_start.elapsed().as_secs().min(duration_secs);
                                    (msg, elapsed_secs, duration_secs, "s")
                                }
                            };
                            progress_bar.set_message(msg.clone());
                            
                            // v0.8.9: Update progress bar based on current stage
                            progress_bar.set_length(progress_len);
                            progress_bar.set_position(progress_pos);
                            let template = format!("{{bar:40.cyan/blue}} {{pos:>7}}/{{len:7}} {}\n{{msg}}", progress_unit);
                            progress_bar.set_style(
                                ProgressStyle::default_bar()
                                    .template(&template)
                                    .expect("Invalid progress template")
                                    .progress_chars("█▓▒░ ")
                            );
                            
                            last_update = std::time::Instant::now();
                            
                            // v0.8.19: Always write console_log.txt on every 1-second display update
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            let log_line = format!("[{}] {}", timestamp, msg);
                            if let Err(e) = results_dir.write_console(&log_line) {
                                warn!("Failed to write live stats to console_log.txt: {}", e);
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
            
            // v0.8.19: CRITICAL - Precise 1-second timer for perf_log (separate from display updates)
            // This ensures perf_log.tsv has EXACTLY 1-second intervals for accurate time-series analysis
            // Uses MissedTickBehavior::Burst so if aggregate() takes >1s, we catch up immediately
            _ = perf_log_timer.tick() => {
                let agg = aggregator.aggregate();
                last_aggregate = Some(agg.clone());  // Cache for display use
                
                // Determine stage for perf-log
                let perf_stage = match last_stage {
                    WorkloadStage::StagePrepare => LiveWorkloadStage::Prepare,
                    WorkloadStage::StageWorkload => LiveWorkloadStage::Workload,
                    WorkloadStage::StageCleanup => LiveWorkloadStage::Cleanup,
                    WorkloadStage::StageListing => LiveWorkloadStage::Listing,
                    _ => LiveWorkloadStage::Workload,
                };
                
                // v0.8.19: Create perf_log entry directly from windowed values
                // This ensures console_log.txt and perf_log.tsv show IDENTICAL values
                let timestamp_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                
                // Compute delta ops/bytes from windowed values
                let delta_get_ops = (agg.windowed_get_ops_s * agg.windowed_delta_time) as u64;
                let delta_put_ops = (agg.windowed_put_ops_s * agg.windowed_delta_time) as u64;
                
                let entry = sai3_bench::perf_log::PerfLogEntry {
                    agent_id: "controller".to_string(),
                    timestamp_epoch_ms: timestamp_ms,
                    elapsed_s: agg.elapsed_s,
                    stage: perf_stage,
                    stage_name: String::new(),
                    
                    // Use windowed values directly (same as display)
                    get_ops: delta_get_ops,
                    get_bytes: agg.windowed_get_bytes,
                    get_iops: agg.windowed_get_ops_s,
                    get_mbps: if agg.windowed_delta_time > 0.0 {
                        (agg.windowed_get_bytes as f64 / agg.windowed_delta_time) / (1024.0 * 1024.0)
                    } else {
                        0.0
                    },
                    get_mean_us: agg.get_mean_us as u64,
                    get_p50_us: agg.get_p50_us as u64,
                    get_p90_us: agg.get_p90_us as u64,
                    get_p99_us: agg.get_p99_us as u64,
                    
                    put_ops: delta_put_ops,
                    put_bytes: agg.windowed_put_bytes,
                    put_iops: agg.windowed_put_ops_s,
                    put_mbps: if agg.windowed_delta_time > 0.0 {
                        (agg.windowed_put_bytes as f64 / agg.windowed_delta_time) / (1024.0 * 1024.0)
                    } else {
                        0.0
                    },
                    put_mean_us: agg.put_mean_us as u64,
                    put_p50_us: agg.put_p50_us as u64,
                    put_p90_us: agg.put_p90_us as u64,
                    put_p99_us: agg.put_p99_us as u64,
                    
                    meta_ops: 0,  // META not windowed in aggregate
                    meta_iops: if agg.elapsed_s > 0.0 { agg.total_meta_ops as f64 / agg.elapsed_s } else { 0.0 },
                    meta_mean_us: agg.meta_mean_us as u64,
                    meta_p50_us: agg.meta_p50_us as u64,
                    meta_p90_us: agg.meta_p90_us as u64,
                    meta_p99_us: agg.meta_p99_us as u64,
                    
                    cpu_user_percent: agg.cpu_user_percent,
                    cpu_system_percent: agg.cpu_system_percent,
                    cpu_iowait_percent: agg.cpu_iowait_percent,
                    errors: 0,
                };
                        
                if let Err(e) = perf_log_writer.write_entry(&entry) {
                    warn!("Failed to write aggregate perf-log entry: {}", e);
                }
                
                // Write per-agent perf-log entries at the same precise interval
                for (agent_id, agent_stats) in aggregator.agent_stats.iter() {
                    if let (Some(agent_writer), Some(agent_tracker)) = (
                        agent_perf_writers.get_mut(agent_id),
                        agent_perf_trackers.get_mut(agent_id)
                    ) {
                        let agent_metrics = sai3_bench::perf_log::PerfMetrics {
                            get_ops: agent_stats.get_ops,
                            get_bytes: agent_stats.get_bytes,
                            put_ops: agent_stats.put_ops,
                            put_bytes: agent_stats.put_bytes,
                            meta_ops: agent_stats.meta_ops,
                            errors: 0,
                            get_mean_us: agent_stats.get_mean_us as u64,
                            get_p50_us: agent_stats.get_p50_us as u64,
                            get_p90_us: agent_stats.get_p90_us as u64,
                            get_p99_us: agent_stats.get_p99_us as u64,
                            put_mean_us: agent_stats.put_mean_us as u64,
                            put_p50_us: agent_stats.put_p50_us as u64,
                            put_p90_us: agent_stats.put_p90_us as u64,
                            put_p99_us: agent_stats.put_p99_us as u64,
                            meta_mean_us: agent_stats.meta_mean_us as u64,
                            meta_p50_us: agent_stats.meta_p50_us as u64,
                            meta_p90_us: agent_stats.meta_p90_us as u64,
                            meta_p99_us: agent_stats.meta_p99_us as u64,
                            cpu_user_percent: agent_stats.cpu_user_percent,
                            cpu_system_percent: agent_stats.cpu_system_percent,
                            cpu_iowait_percent: agent_stats.cpu_iowait_percent,
                        };
                        let agent_entry = agent_tracker.compute_delta(
                            agent_id,
                            &agent_metrics,
                            perf_stage,
                            String::new(),
                        );
                        
                        if let Err(e) = agent_writer.write_entry(&agent_entry) {
                            warn!("Failed to write per-agent perf-log entry for {}: {}", agent_id, e);
                        }
                    }
                }
                
                // Periodic flush every 10 seconds to minimize data loss on crash
                if last_perf_log_flush.elapsed() >= perf_log_flush_interval {
                    if let Err(e) = perf_log_writer.flush() {
                        warn!("Failed to flush aggregate perf-log: {}", e);
                    }
                    
                    // Also flush per-agent perf logs
                    for (agent_id, agent_writer) in agent_perf_writers.iter_mut() {
                        if let Err(e) = agent_writer.flush() {
                            warn!("Failed to flush per-agent perf-log for {}: {}", agent_id, e);
                        }
                    }
                    
                    last_perf_log_flush = std::time::Instant::now();
                }
            }
            
            sig = &mut shutdown_signal => {
                warn!("Received {} - aborting all agents", sig);
                progress_bar.finish_with_message(format!("Interrupted by {} - aborting agents...", sig));
                
                // v0.7.13: Abort all agents to stop running workload and reset state
                abort_all_agents(agent_addrs, &mut agent_trackers, insecure, agent_ca, agent_domain).await;
                
                // Cleanup deployments if any
                if !deployments.is_empty() {
                    warn!("Cleaning up agents...");
                    let _ = sai3_bench::ssh_deploy::cleanup_agents(deployments);
                }
                
                anyhow::bail!("Interrupted by {}", sig);
            }
        }
    }
    
    // v0.8.25: Wait for execute barrier if enabled
    // TODO: Uncomment when client pool is available
    // if let Some(ref mut bm) = barrier_manager {
    //     eprintln!("\n🔄 Workload execution complete - waiting for all agents at barrier...");
    //     match bm.wait_for_barrier("execute", &agent_clients).await {
    //         Ok(BarrierStatus::Ready) => eprintln!("✅ All agents synchronized"),
    //         Ok(BarrierStatus::Degraded) => eprintln!("⚠️  Barrier degraded"),
    //         Ok(BarrierStatus::Failed) => anyhow::bail!("Barrier failed"),
    //         Ok(BarrierStatus::Waiting) => warn!("Still waiting"),
    //         Err(e) => anyhow::bail!("Barrier error: {}", e),
    //     }
    // }
    
    // Final aggregation
    let final_stats = aggregator.aggregate();
    progress_bar.finish_with_message(format!("✓ All {} agents completed\n\n", final_stats.num_agents));
    
    // v0.8.19: Flush and close perf-log (always enabled)
    if let Err(e) = perf_log_writer.flush() {
        warn!("Failed to flush aggregate perf-log: {}", e);
    } else {
        info!("Aggregate perf-log written to: {}", perf_log_path.display());
    }
    
    // v0.8.16: Flush and close per-agent perf logs
    for (agent_id, writer) in agent_perf_writers.iter_mut() {
        if let Err(e) = writer.flush() {
            warn!("Failed to flush per-agent perf-log for {}: {}", agent_id, e);
        } else {
            debug!("Per-agent perf-log written for: {}", agent_id);
        }
    }
    if !agent_perf_writers.is_empty() {
        info!("Per-agent perf-logs written to: {}/*/perf_log.tsv", agents_dir.display());
    }
    
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
    println!("META: {:.0} ops/s (mean: {:.0}µs)",
             final_stats.total_meta_ops as f64 / final_stats.elapsed_s,
             final_stats.meta_mean_us);
    
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
        
        // v0.8.22: Aggregate and export endpoint stats if any agent used multi-endpoint
        let all_endpoint_stats: Vec<_> = agent_summaries.iter()
            .map(|s| proto_to_endpoint_stats(&s.endpoint_stats))
            .filter(|stats| !stats.is_empty())
            .collect();
        
        if !all_endpoint_stats.is_empty() {
            let aggregated_stats = aggregate_endpoint_stats(&all_endpoint_stats);
            let ep_tsv_path = results_dir.endpoint_stats_tsv_path();
            match sai3_bench::tsv_export::TsvExporter::with_path(&ep_tsv_path) {
                Ok(exporter) => {
                    if let Err(e) = exporter.export_endpoint_stats(&aggregated_stats) {
                        error!("Failed to export aggregated endpoint stats: {}", e);
                    } else {
                        info!("✓ Aggregated endpoint stats written to: {}", ep_tsv_path.display());
                    }
                }
                Err(e) => {
                    error!("Failed to create endpoint stats TSV exporter: {}", e);
                }
            }
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
        
        // v0.8.22: Aggregate and export prepare endpoint stats if any agent used multi-endpoint
        let all_prepare_endpoint_stats: Vec<_> = prepare_summaries.iter()
            .map(|s| proto_to_endpoint_stats(&s.endpoint_stats))
            .filter(|stats| !stats.is_empty())
            .collect();
        
        if !all_prepare_endpoint_stats.is_empty() {
            let aggregated_stats = aggregate_endpoint_stats(&all_prepare_endpoint_stats);
            let ep_tsv_path = results_dir.prepare_endpoint_stats_tsv_path();
            match sai3_bench::tsv_export::TsvExporter::with_path(&ep_tsv_path) {
                Ok(exporter) => {
                    if let Err(e) = exporter.export_endpoint_stats(&aggregated_stats) {
                        error!("Failed to export aggregated prepare endpoint stats: {}", e);
                    } else {
                        info!("✓ Aggregated prepare endpoint stats written to: {}", ep_tsv_path.display());
                    }
                }
                Err(e) => {
                    error!("Failed to create prepare endpoint stats TSV exporter: {}", e);
                }
            }
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
    content.push('\n');
    
    // Summary
    content.push_str("=== Summary ===\n");
    content.push_str(&format!("Total agents: {}\n", status.total_agents));
    content.push_str(&format!("Completed: {}\n", status.completed));
    content.push_str(&format!("Failed: {}\n", status.failed));
    content.push_str(&format!("Disconnected: {}\n", status.disconnected));
    content.push_str(&format!("Disconnect/Reconnect Count: {}\n", status.reconnect_count));
    content.push_str(&format!("Aborting: {}\n", status.aborting));
    content.push_str(&format!("Total operations: {}\n", status.total_ops));
    content.push('\n');
    
    // Per-agent details
    content.push_str("=== Agent Details ===\n");
    for (agent_id, state, reason) in &status.agent_details {
        content.push_str(&format!("{}: {:?} ({})\n", agent_id, state, reason));
    }
    
    // Failure details (if any)
    if !status.success {
        content.push('\n');
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
        content.push_str("1. Check agent logs in agents/*/console_log.txt\n");
        content.push_str("2. Review controller console_log.txt for error messages\n");
        content.push_str("3. Verify agent connectivity and network stability\n");
        content.push_str("4. Check storage backend availability\n");
    }
    
    fs::write(&status_path, content)
        .with_context(|| format!("Failed to write STATUS.txt: {}", status_path.display()))?;
    
    Ok(())
}

/// v0.8.1: Print test status to console and console_log.txt
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
    
    // v0.8.22: Write endpoint stats TSV if multi-endpoint was used
    if !summary.endpoint_stats.is_empty() {
        let ep_stats = proto_to_endpoint_stats(&summary.endpoint_stats);
        let ep_tsv_path = agent_dir.join("workload_endpoint_stats.tsv");
        let ep_exporter = sai3_bench::tsv_export::TsvExporter::with_path(&ep_tsv_path)
            .with_context(|| format!("Failed to create endpoint stats TSV exporter: {}", ep_tsv_path.display()))?;
        ep_exporter.export_endpoint_stats(&ep_stats)
            .with_context(|| format!("Failed to export endpoint stats: {}", ep_tsv_path.display()))?;
    }
    
    // Write console_log.txt (if provided - currently agents don't generate this)
    if !summary.console_log.is_empty() {
        let console_path = agent_dir.join("console_log.txt");
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
    
    // v0.8.22: Write endpoint stats TSV if multi-endpoint was used during prepare
    if !summary.endpoint_stats.is_empty() {
        let ep_stats = proto_to_endpoint_stats(&summary.endpoint_stats);
        let ep_tsv_path = agent_dir.join("prepare_endpoint_stats.tsv");
        let ep_exporter = sai3_bench::tsv_export::TsvExporter::with_path(&ep_tsv_path)
            .with_context(|| format!("Failed to create endpoint stats TSV exporter: {}", ep_tsv_path.display()))?;
        ep_exporter.export_endpoint_stats(&ep_stats)
            .with_context(|| format!("Failed to export prepare endpoint stats: {}", ep_tsv_path.display()))?;
    }
    
    // Write a note about the agent's local results path
    if !summary.results_path.is_empty() {
        let note_path = agent_dir.join("prepare_local_path.txt");
        fs::write(&note_path, &summary.results_path)
            .with_context(|| format!("Failed to write prepare path note: {}", note_path.display()))?;
    }
    
    Ok(())
}

/// Convert protobuf EndpointStatsSnapshot to local EndpointStatsSnapshot (v0.8.22)
fn proto_to_endpoint_stats(
    proto_stats: &[pb::iobench::EndpointStatsSnapshot]
) -> Vec<sai3_bench::workload::EndpointStatsSnapshot> {
    proto_stats.iter().map(|s| sai3_bench::workload::EndpointStatsSnapshot {
        uri: s.uri.clone(),
        total_requests: s.total_requests,
        bytes_read: s.bytes_read,
        bytes_written: s.bytes_written,
        error_count: s.error_count,
        active_requests: s.active_requests as usize,
    }).collect()
}

/// Aggregate endpoint stats from multiple agents (v0.8.22)
/// Combines stats for endpoints with same URI
fn aggregate_endpoint_stats(
    all_agent_stats: &[Vec<sai3_bench::workload::EndpointStatsSnapshot>]
) -> Vec<sai3_bench::workload::EndpointStatsSnapshot> {
    use std::collections::HashMap;
    
    let mut aggregated: HashMap<String, sai3_bench::workload::EndpointStatsSnapshot> = HashMap::new();
    
    for agent_stats in all_agent_stats {
        for stat in agent_stats {
            aggregated.entry(stat.uri.clone())
                .and_modify(|agg| {
                    agg.total_requests += stat.total_requests;
                    agg.bytes_read += stat.bytes_read;
                    agg.bytes_written += stat.bytes_written;
                    agg.error_count += stat.error_count;
                    agg.active_requests += stat.active_requests;
                })
                .or_insert(stat.clone());
        }
    }
    
    let mut result: Vec<_> = aggregated.into_values().collect();
    result.sort_by(|a, b| a.uri.cmp(&b.uri));  // Sort by URI for consistent output
    result
}

/// Convert protobuf SizeBins to local SizeBins (v0.8.18)
fn proto_to_size_bins(proto_bins: &pb::iobench::SizeBins) -> sai3_bench::workload::SizeBins {
    use std::collections::HashMap;
    
    let mut by_bucket = HashMap::new();
    for bucket_data in &proto_bins.buckets {
        by_bucket.insert(bucket_data.bucket_idx as usize, (bucket_data.ops, bucket_data.bytes));
    }
    
    sai3_bench::workload::SizeBins { by_bucket }
}

/// Merge SizeBins from multiple agents (v0.8.18)
fn merge_size_bins(bins_list: &[sai3_bench::workload::SizeBins]) -> sai3_bench::workload::SizeBins {
    let mut merged = sai3_bench::workload::SizeBins::default();
    
    for bins in bins_list {
        merged.merge_from(bins);
    }
    
    merged
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
        for (bucket_idx, accumulator) in put_accumulators.iter_mut().enumerate() {
            match deserializer.deserialize::<u64, _>(&mut cursor) {
                Ok(hist) => {
                    accumulator.add(&hist)
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
    
    // v0.8.18: Merge SizeBins from all agents for accurate per-bucket avg_bytes
    let mut put_bins_list = Vec::new();
    
    for summary in summaries {
        if let Some(ref put) = summary.put {
            total_ops += put.ops;
            total_bytes += put.bytes;
        }
        total_objects_created += summary.objects_created;
        total_objects_existed += summary.objects_existed;
        
        // Collect SizeBins for merging
        if let Some(ref bins) = summary.put_bins {
            put_bins_list.push(proto_to_size_bins(bins));
        }
    }
    
    // Merge all SizeBins for accurate per-bucket calculations
    let merged_put_bins = merge_size_bins(&put_bins_list);
    
    // Write consolidated TSV
    let tsv_path = results_dir.join("prepare_results.tsv");
    let mut writer = BufWriter::new(File::create(&tsv_path)?);
    
    // Write header
    writeln!(writer, "operation\tsize_bucket\tbucket_idx\tmean_us\tp50_us\tp90_us\tp95_us\tp99_us\tmax_us\tavg_bytes\tops_per_sec\tthroughput_mibps\tcount")?;
    
    // Write PUT bucket rows (from merged histograms)
    for (idx, hist) in put_accumulators.iter().enumerate() {
        if hist.is_empty() {
            continue;
        }
        
        let mean_us = hist.mean();
        let p50_us = hist.value_at_quantile(0.50);
        let p90_us = hist.value_at_quantile(0.90);
        let p95_us = hist.value_at_quantile(0.95);
        let p99_us = hist.value_at_quantile(0.99);
        let max_us = hist.max();
        let count = hist.len();
        
        // v0.8.18: Use actual per-bucket bytes from merged SizeBins (NOT total!)
        let (bucket_ops, bucket_bytes) = merged_put_bins.by_bucket.get(&idx).copied().unwrap_or((0, 0));
        let avg_bytes = if bucket_ops > 0 {
            bucket_bytes as f64 / bucket_ops as f64
        } else {
            0.0
        };
        let ops_per_sec = count as f64 / total_wall_seconds;
        let throughput_mibps = (bucket_bytes as f64 / 1_048_576.0) / total_wall_seconds;
        
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
            if !bucket_hist.is_empty() {
                all_hist.add(bucket_hist)?;
            }
        }
        
        if !all_hist.is_empty() {
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
        for (bucket_idx, accumulator) in get_accumulators.iter_mut().enumerate() {
            let hist: Histogram<u64> = deserializer.deserialize(&mut cursor)
                .with_context(|| format!("Failed to deserialize GET histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
            accumulator.add(hist)
                .with_context(|| format!("Failed to merge GET histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
        }
        
        // Deserialize PUT histograms
        let mut cursor = &summary.histogram_put[..];
        for (bucket_idx, accumulator) in put_accumulators.iter_mut().enumerate() {
            let hist: Histogram<u64> = deserializer.deserialize(&mut cursor)
                .with_context(|| format!("Failed to deserialize PUT histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
            accumulator.add(hist)
                .with_context(|| format!("Failed to merge PUT histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
        }
        
        // Deserialize META histograms
        let mut cursor = &summary.histogram_meta[..];
        for (bucket_idx, accumulator) in meta_accumulators.iter_mut().enumerate() {
            let hist: Histogram<u64> = deserializer.deserialize(&mut cursor)
                .with_context(|| format!("Failed to deserialize META histogram bucket {} from agent {}", 
                                        bucket_idx, summary.agent_id))?;
            accumulator.add(hist)
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
    
    // v0.8.18: Merge SizeBins from all agents for accurate per-bucket avg_bytes
    let mut get_bins_list = Vec::new();
    let mut put_bins_list = Vec::new();
    let mut meta_bins_list = Vec::new();
    
    for summary in summaries {
        if let Some(ref bins) = summary.get_bins {
            get_bins_list.push(proto_to_size_bins(bins));
        }
        if let Some(ref bins) = summary.put_bins {
            put_bins_list.push(proto_to_size_bins(bins));
        }
        if let Some(ref bins) = summary.meta_bins {
            meta_bins_list.push(proto_to_size_bins(bins));
        }
    }
    
    let merged_get_bins = merge_size_bins(&get_bins_list);
    let merged_put_bins = merge_size_bins(&put_bins_list);
    let merged_meta_bins = merge_size_bins(&meta_bins_list);
    
    // Write consolidated TSV
    let tsv_path = results_dir.path().join("results.tsv");
    let mut f = File::create(&tsv_path)
        .with_context(|| format!("Failed to create consolidated TSV: {}", tsv_path.display()))?;
    
    // Write header
    writeln!(f, "operation\tsize_bucket\tbucket_idx\tmean_us\tp50_us\tp90_us\tp95_us\tp99_us\tmax_us\tavg_bytes\tops_per_sec\tthroughput_mibps\tcount")?;
    
    // Collect all rows for sorting
    let mut rows = Vec::new();
    
    // Write per-bucket rows (merged across all agents)
    collect_op_rows(&mut rows, "GET", &get_accumulators, total_get_ops, total_get_bytes, &merged_get_bins, wall_seconds)?;
    collect_op_rows(&mut rows, "PUT", &put_accumulators, total_put_ops, total_put_bytes, &merged_put_bins, wall_seconds)?;
    collect_op_rows(&mut rows, "META", &meta_accumulators, total_meta_ops, total_meta_bytes, &merged_meta_bins, wall_seconds)?;
    
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
    _total_ops: u64,  // v0.8.18: Unused - kept for API compatibility
    _total_bytes: u64,  // v0.8.18: Unused - kept for API compatibility
    merged_bins: &sai3_bench::workload::SizeBins,  // v0.8.18: Use per-bucket bytes!
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
        
        // v0.8.18: Use actual per-bucket bytes from merged SizeBins (NOT estimated!)
        let (bucket_ops, bucket_bytes) = merged_bins.by_bucket.get(&bucket_idx).copied().unwrap_or((0, 0));
        let avg_bytes = if bucket_ops > 0 {
            bucket_bytes as f64 / bucket_ops as f64
        } else {
            0.0
        };
        
        // Calculate throughput
        let ops_per_sec = count as f64 / wall_seconds;
        let throughput_mibps = (bucket_bytes as f64 / 1_048_576.0) / wall_seconds;
        
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
        if !hist.is_empty() {
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

// Note: detect_shared_storage() and is_shared_uri() functions removed.
// Shared storage configuration is now EXPLICIT via CLI flag or config file.
// Any storage type (file://, s3://, az://, gs://, direct://) can be shared or per-agent.
// Users must specify their setup correctly in the config.

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
            OpSpec::Get { path, .. } => {
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
            OpSpec::Delete { path, .. } => {
                // Delete needs a valid path
                if path.is_empty() {
                    bail!("DELETE operation requires non-empty path");
                }
            }
            OpSpec::List { path, .. } => {
                // List needs a valid path
                if path.is_empty() {
                    bail!("LIST operation requires non-empty path");
                }
            }
            OpSpec::Stat { path, .. } => {
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

// v0.8.22: Unit tests for per-agent multi_endpoint override
#[cfg(test)]
mod tests {
    use sai3_bench::config::Config;

    /// Test that per-agent multi_endpoint override correctly replaces global config
    #[test]
    fn test_agent_multi_endpoint_override() {
        // Create a config with global multi_endpoint and per-agent overrides
        let config_yaml = r#"
duration: 10s
concurrency: 4

multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file:///tmp/global-a/"
    - "file:///tmp/global-b/"
    - "file:///tmp/global-c/"
    - "file:///tmp/global-d/"

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  agents:
    - address: "localhost:7761"
      id: "agent-1"
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - "file:///tmp/agent1-a/"
          - "file:///tmp/agent1-b/"
    - address: "localhost:7762"
      id: "agent-2"
      multi_endpoint:
        strategy: least_connections
        endpoints:
          - "file:///tmp/agent2-c/"
          - "file:///tmp/agent2-d/"
    - address: "localhost:7763"
      id: "agent-3"
      # No override - should use global

workload:
  - op: get
    path: "testdata/*"
    weight: 100
"#;

        let config: Config = serde_yaml::from_str(config_yaml)
            .expect("Failed to parse base config");

        // Test agent 0 (has override)
        let agent0_config = if let Some(ref distributed) = config.distributed {
            if let Some(ref agent_multi_ep) = distributed.agents[0].multi_endpoint {
                let mut modified = config.clone();
                modified.multi_endpoint = Some(agent_multi_ep.clone());
                modified
            } else {
                panic!("Agent 0 should have multi_endpoint override");
            }
        } else {
            panic!("Config should have distributed section");
        };

        // Verify agent 0's config has the override endpoints
        assert!(agent0_config.multi_endpoint.is_some());
        let agent0_ep = agent0_config.multi_endpoint.as_ref().unwrap();
        assert_eq!(agent0_ep.endpoints.len(), 2);
        assert_eq!(agent0_ep.endpoints[0], "file:///tmp/agent1-a/");
        assert_eq!(agent0_ep.endpoints[1], "file:///tmp/agent1-b/");
        assert_eq!(agent0_ep.strategy, "round_robin");

        // Test agent 1 (has different override)
        let agent1_config = if let Some(ref distributed) = config.distributed {
            if let Some(ref agent_multi_ep) = distributed.agents[1].multi_endpoint {
                let mut modified = config.clone();
                modified.multi_endpoint = Some(agent_multi_ep.clone());
                modified
            } else {
                panic!("Agent 1 should have multi_endpoint override");
            }
        } else {
            panic!("Config should have distributed section");
        };

        let agent1_ep = agent1_config.multi_endpoint.as_ref().unwrap();
        assert_eq!(agent1_ep.endpoints.len(), 2);
        assert_eq!(agent1_ep.endpoints[0], "file:///tmp/agent2-c/");
        assert_eq!(agent1_ep.endpoints[1], "file:///tmp/agent2-d/");
        assert_eq!(agent1_ep.strategy, "least_connections");

        // Test agent 2 (no override - should keep global)
        let agent2_has_override = config.distributed.as_ref()
            .and_then(|d| d.agents.get(2))
            .and_then(|a| a.multi_endpoint.as_ref())
            .is_some();
        
        assert!(!agent2_has_override, "Agent 2 should not have multi_endpoint override");
        
        // Agent 2 should use global config unchanged
        let global_ep = config.multi_endpoint.as_ref().unwrap();
        assert_eq!(global_ep.endpoints.len(), 4);
        assert_eq!(global_ep.endpoints[0], "file:///tmp/global-a/");
    }

    /// Test that modified config can be serialized back to YAML and parsed correctly
    #[test]
    fn test_agent_config_yaml_round_trip() {
        let config_yaml = r#"
duration: 10s
concurrency: 4

multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file:///tmp/global-a/"
    - "file:///tmp/global-b/"

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  agents:
    - address: "localhost:7761"
      id: "agent-1"
      multi_endpoint:
        strategy: least_connections
        endpoints:
          - "file:///tmp/agent1-x/"
          - "file:///tmp/agent1-y/"

workload:
  - op: get
    path: "testdata/*"
    weight: 100
"#;

        let base_config: Config = serde_yaml::from_str(config_yaml)
            .expect("Failed to parse base config");

        // Simulate controller applying agent-specific override
        let agent_override = base_config.distributed.as_ref()
            .and_then(|d| d.agents.first())
            .and_then(|a| a.multi_endpoint.as_ref())
            .expect("Agent should have multi_endpoint override");

        let mut agent_config = base_config.clone();
        agent_config.multi_endpoint = Some(agent_override.clone());

        // Serialize to YAML (this is what controller sends to agent)
        let agent_yaml = serde_yaml::to_string(&agent_config)
            .expect("Failed to serialize agent config");

        println!("Agent-specific YAML:\n{}", agent_yaml);

        // Parse it back (simulating agent receiving the config)
        let parsed_config: Config = serde_yaml::from_str(&agent_yaml)
            .expect("Failed to parse agent-specific YAML");

        // Verify the agent received the correct multi_endpoint config
        assert!(parsed_config.multi_endpoint.is_some());
        let parsed_ep = parsed_config.multi_endpoint.as_ref().unwrap();
        assert_eq!(parsed_ep.endpoints.len(), 2);
        assert_eq!(parsed_ep.endpoints[0], "file:///tmp/agent1-x/");
        assert_eq!(parsed_ep.endpoints[1], "file:///tmp/agent1-y/");
        assert_eq!(parsed_ep.strategy, "least_connections");

        // Verify the global endpoints are NOT in the modified config
        assert_ne!(parsed_ep.endpoints[0], "file:///tmp/global-a/");
        assert_ne!(parsed_ep.endpoints[1], "file:///tmp/global-b/");
    }

    /// Test that agent without override receives global config unchanged
    #[test]
    fn test_agent_without_override_uses_global() {
        let config_yaml = r#"
duration: 10s
concurrency: 4

multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file:///tmp/global-1/"
    - "file:///tmp/global-2/"

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  agents:
    - address: "localhost:7761"
      id: "agent-1"
      # No multi_endpoint override

workload:
  - op: get
    path: "testdata/*"
    weight: 100
"#;

        let config: Config = serde_yaml::from_str(config_yaml)
            .expect("Failed to parse base config");

        // Check that agent has no override
        let agent_has_override = config.distributed.as_ref()
            .and_then(|d| d.agents.first())
            .and_then(|a| a.multi_endpoint.as_ref())
            .is_some();

        assert!(!agent_has_override);

        // Agent should use the config as-is (global multi_endpoint)
        let global_ep = config.multi_endpoint.as_ref().unwrap();
        assert_eq!(global_ep.endpoints.len(), 2);
        assert_eq!(global_ep.endpoints[0], "file:///tmp/global-1/");
        assert_eq!(global_ep.endpoints[1], "file:///tmp/global-2/");
        assert_eq!(global_ep.strategy, "round_robin");
    }

    /// Test mixed scenario: some agents with overrides, some without
    #[test]
    fn test_mixed_agent_overrides() {
        let config_yaml = r#"
duration: 10s
concurrency: 4

multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file:///tmp/global-1/"
    - "file:///tmp/global-2/"
    - "file:///tmp/global-3/"
    - "file:///tmp/global-4/"

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  agents:
    - address: "localhost:7761"
      id: "agent-1"
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - "file:///tmp/agent1-a/"
          - "file:///tmp/agent1-b/"
    - address: "localhost:7762"
      id: "agent-2"
      # No override
    - address: "localhost:7763"
      id: "agent-3"
      multi_endpoint:
        strategy: least_connections
        endpoints:
          - "file:///tmp/agent3-c/"
          - "file:///tmp/agent3-d/"

workload:
  - op: get
    path: "testdata/*"
    weight: 100
"#;

        let config: Config = serde_yaml::from_str(config_yaml)
            .expect("Failed to parse config");

        let distributed = config.distributed.as_ref().expect("Should have distributed config");

        // Agent 0: has override
        assert!(distributed.agents[0].multi_endpoint.is_some());
        let agent0_ep = distributed.agents[0].multi_endpoint.as_ref().unwrap();
        assert_eq!(agent0_ep.endpoints.len(), 2);
        assert!(agent0_ep.endpoints[0].contains("agent1-a"));

        // Agent 1: no override
        assert!(distributed.agents[1].multi_endpoint.is_none());

        // Agent 2: has override
        assert!(distributed.agents[2].multi_endpoint.is_some());
        let agent2_ep = distributed.agents[2].multi_endpoint.as_ref().unwrap();
        assert_eq!(agent2_ep.endpoints.len(), 2);
        assert!(agent2_ep.endpoints[0].contains("agent3-c"));
        assert_eq!(agent2_ep.strategy, "least_connections");
    }

    /// Test that the logic matches what the controller actually does
    #[test]
    fn test_controller_logic_simulation() {
        let config_yaml = r#"
duration: 10s
concurrency: 4

multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file:///tmp/ep-a/"
    - "file:///tmp/ep-b/"
    - "file:///tmp/ep-c/"
    - "file:///tmp/ep-d/"

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  agents:
    - address: "localhost:7761"
      id: "agent-dc-a"
      multi_endpoint:
        endpoints:
          - "file:///tmp/ep-a/"
          - "file:///tmp/ep-b/"
        strategy: round_robin
    - address: "localhost:7762"
      id: "agent-dc-b"
      multi_endpoint:
        endpoints:
          - "file:///tmp/ep-c/"
          - "file:///tmp/ep-d/"
        strategy: least_connections

workload:
  - op: get
    path: "testdata/*"
    use_multi_endpoint: true
    weight: 100
"#;

        let config: Config = serde_yaml::from_str(config_yaml)
            .expect("Failed to parse config");

        // Simulate controller loop for each agent
        for (idx, _agent_addr) in ["localhost:7761", "localhost:7762"].iter().enumerate() {
            // This is the exact logic from the controller
            let agent_specific_yaml = if let Some(ref distributed) = config.distributed {
                if idx < distributed.agents.len() {
                    if let Some(ref agent_multi_ep) = distributed.agents[idx].multi_endpoint {
                        // Agent has multi_endpoint override - replace global config
                        let mut modified_config = config.clone();
                        modified_config.multi_endpoint = Some(agent_multi_ep.clone());
                        
                        // Serialize modified config to YAML
                        serde_yaml::to_string(&modified_config)
                            .expect("Failed to serialize agent config")
                    } else {
                        // No override - use global config
                        config_yaml.to_string()
                    }
                } else {
                    config_yaml.to_string()
                }
            } else {
                config_yaml.to_string()
            };

            // Parse back to verify
            let agent_config: Config = serde_yaml::from_str(&agent_specific_yaml)
                .expect("Agent should be able to parse the YAML");

            // Verify agent got correct endpoints
            let agent_ep = agent_config.multi_endpoint.as_ref()
                .expect("Agent config should have multi_endpoint");
            
            match idx {
                0 => {
                    // Agent 0 should have endpoints a & b
                    assert_eq!(agent_ep.endpoints.len(), 2);
                    assert!(agent_ep.endpoints[0].contains("ep-a"));
                    assert!(agent_ep.endpoints[1].contains("ep-b"));
                    assert_eq!(agent_ep.strategy, "round_robin");
                }
                1 => {
                    // Agent 1 should have endpoints c & d
                    assert_eq!(agent_ep.endpoints.len(), 2);
                    assert!(agent_ep.endpoints[0].contains("ep-c"));
                    assert!(agent_ep.endpoints[1].contains("ep-d"));
                    assert_eq!(agent_ep.strategy, "least_connections");
                }
                _ => panic!("Unexpected agent index"),
            }

            // CRITICAL: Verify agent does NOT have all 4 global endpoints
            assert_ne!(agent_ep.endpoints.len(), 4, 
                      "Agent {} should NOT have all 4 global endpoints!", idx);
        }
    }
}

