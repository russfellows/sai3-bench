// src/constants.rs
//
// Central location for all constants used throughout sai3-bench
// This makes tuning and maintenance easier by having all magic numbers in one place

use std::time::Duration;

// =============================================================================
// Error Handling Defaults (v0.8.0+)
// =============================================================================

/// Maximum total errors before aborting workload
/// User can override via config: error_handling.max_total_errors
pub const DEFAULT_MAX_ERRORS: u64 = 100;

/// Error rate threshold (errors/sec) to trigger backoff
/// User can override via config: error_handling.error_rate_threshold
pub const DEFAULT_ERROR_RATE_THRESHOLD: f64 = 5.0;

/// Duration to back off when error rate threshold is hit
/// User can override via config: error_handling.backoff_duration
pub const DEFAULT_BACKOFF_DURATION: Duration = Duration::from_secs(5);

/// Window for calculating error rate (seconds)
/// User can override via config: error_handling.error_rate_window
pub const DEFAULT_ERROR_RATE_WINDOW: f64 = 1.0;

/// Maximum retries per operation (only used if retry_on_error=true)
/// User can override via config: error_handling.max_retries
pub const DEFAULT_MAX_RETRIES: u32 = 3;

/// Whether to retry failed operations or skip them
/// User can override via config: error_handling.retry_on_error
pub const DEFAULT_RETRY_ON_ERROR: bool = false;

/// Maximum consecutive errors before aborting (v0.8.13)
/// Prevents runaway failures when backend is completely down
/// User can override via config: error_handling.max_consecutive_errors
pub const DEFAULT_MAX_CONSECUTIVE_ERRORS: u64 = 10;

// =============================================================================
// Retry with Exponential Backoff (v0.8.13)
// =============================================================================

/// Initial delay before first retry (milliseconds)
/// User can override via config: error_handling.initial_retry_delay_ms
pub const DEFAULT_INITIAL_RETRY_DELAY_MS: u64 = 100;

/// Maximum delay between retries (milliseconds)
/// Caps exponential growth to prevent excessive waits
/// User can override via config: error_handling.max_retry_delay_ms
pub const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 5000;

/// Multiplier for exponential backoff (delay * multiplier each retry)
/// 2.0 means delay doubles: 100ms -> 200ms -> 400ms -> 800ms -> ...
/// User can override via config: error_handling.retry_backoff_multiplier
pub const DEFAULT_RETRY_BACKOFF_MULTIPLIER: f64 = 2.0;

/// Jitter factor for retry delays (0.0 = no jitter, 1.0 = full jitter)
/// Adds randomness to prevent thundering herd on retries
/// User can override via config: error_handling.retry_jitter_factor
pub const DEFAULT_RETRY_JITTER_FACTOR: f64 = 0.25;

// =============================================================================
// Prepare Phase Error Handling (v0.8.13)
// =============================================================================

/// Maximum total errors during prepare phase before aborting
/// Similar to workload error handling, but for object creation
pub const DEFAULT_PREPARE_MAX_ERRORS: u64 = 100;

/// Maximum consecutive errors during prepare before aborting
/// Indicates backend is completely unreachable
pub const DEFAULT_PREPARE_MAX_CONSECUTIVE_ERRORS: u64 = 10;

// =============================================================================
// Listing Phase Error Handling (v0.8.14)
// =============================================================================

/// Maximum total errors during listing phase before aborting
/// LIST operations can fail on transient network issues
pub const DEFAULT_LISTING_MAX_ERRORS: u64 = 50;

/// Maximum consecutive errors during listing before aborting
/// Indicates backend is completely unreachable
pub const DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS: u64 = 5;

/// Progress update interval for listing (number of files between updates)
/// Balances visibility with performance overhead
pub const LISTING_PROGRESS_INTERVAL: u64 = 1000;

// =============================================================================
// Workload Execution Defaults
// =============================================================================

/// Default workload duration if not specified
pub const DEFAULT_DURATION_SECS: u64 = 60;

/// Default concurrency (number of worker tasks) if not specified
pub const DEFAULT_CONCURRENCY: usize = 16;

/// Optimal chunk size for direct:// reads (4 MiB)
/// Based on testing: 4M chunks achieve 1.73 GiB/s vs 0.01 GiB/s for whole-file
pub const DIRECT_IO_CHUNK_SIZE: usize = 4 * 1024 * 1024;  // 4 MiB

/// Threshold for using chunked reads vs whole-file reads
/// Files larger than this will use chunked reads for direct://
pub const CHUNKED_READ_THRESHOLD: u64 = 8 * 1024 * 1024;  // 8 MiB

// =============================================================================
// RangeEngine Defaults (s3dlio configuration)
// =============================================================================

/// Default chunk size for RangeEngine (64 MiB)
/// User can override via config: range_engine.chunk_size
pub const DEFAULT_CHUNK_SIZE: u64 = 64 * 1024 * 1024;

/// Default maximum concurrent chunks (16)
/// User can override via config: range_engine.max_concurrent
pub const DEFAULT_MAX_CONCURRENT: usize = 16;

/// Default minimum split size (16 MiB)
/// User can override via config: range_engine.min_split_size
pub const DEFAULT_MIN_SPLIT_SIZE: u64 = 16 * 1024 * 1024;

/// Default range request timeout (30 seconds)
/// User can override via config: range_engine.range_timeout
pub const DEFAULT_RANGE_TIMEOUT_SECS: u64 = 30;

// =============================================================================
// Metrics and Histogram Configuration
// =============================================================================

/// Number of size buckets for latency histograms
/// Each operation type (GET/PUT/META) uses these buckets
pub const NUM_BUCKETS: usize = 9;

/// Human-readable labels for size buckets
/// Must match NUM_BUCKETS length
pub const BUCKET_LABELS: [&str; NUM_BUCKETS] = [
    "zero",       // 0 bytes (metadata ops)
    "1B-8KiB",    // Bucket 0
    "8KiB-64KiB", // Bucket 1
    "64KiB-512KiB", // Bucket 2
    "512KiB-4MiB", // Bucket 3
    "4MiB-32MiB",  // Bucket 4
    "32MiB-256MiB", // Bucket 5
    "256MiB-2GiB", // Bucket 6
    ">2GiB",      // Bucket 7
];

/// Size bucket boundaries (in bytes)
/// Used by bucket_index() to map object sizes to histogram buckets
pub const BUCKET_BOUNDARIES: [u64; NUM_BUCKETS] = [
    0,                      // zero bucket (metadata ops)
    8 * 1024,               // 8 KiB
    64 * 1024,              // 64 KiB
    512 * 1024,             // 512 KiB
    4 * 1024 * 1024,        // 4 MiB
    32 * 1024 * 1024,       // 32 MiB
    256 * 1024 * 1024,      // 256 MiB
    2 * 1024 * 1024 * 1024, // 2 GiB
    u64::MAX,               // >2 GiB
];

// =============================================================================
// Progress Bar Configuration
// =============================================================================

/// Progress bar update interval (milliseconds)
pub const PROGRESS_UPDATE_INTERVAL_MS: u64 = 100;

/// Progress bar stats display update interval (seconds)
/// Live stats (ops/sec, MiB/sec, latency) update at this frequency
pub const PROGRESS_STATS_UPDATE_INTERVAL_SECS: f64 = 0.5;

// =============================================================================
// Performance Log Defaults (v0.8.15+)
// =============================================================================

/// Default perf-log sampling interval (1 second)
/// User can override via config: perf_log.interval
pub const DEFAULT_PERF_LOG_INTERVAL_SECS: u64 = 1;

/// Perf-log flush interval (10 seconds)
/// Ensures data is written to disk periodically to minimize data loss on crash
pub const PERF_LOG_FLUSH_INTERVAL_SECS: u64 = 10;

/// TSV header for perf-log files (28 columns)
/// See docs/PERF_LOG_FORMAT.md for complete column specification
/// 
/// Columns by group:
/// - Identity (1): agent_id
/// - Timing (3): timestamp_epoch_ms, elapsed_s, stage
/// - GET metrics (7): ops, bytes, iops, mbps, p50_us, p90_us, p99_us
/// - PUT metrics (7): ops, bytes, iops, mbps, p50_us, p90_us, p99_us  
/// - META metrics (5): ops, iops, p50_us, p90_us, p99_us
/// - CPU metrics (3): cpu_user_pct, cpu_system_pct, cpu_iowait_pct
/// - Status (2): errors, is_warmup
pub const PERF_LOG_HEADER: &str = "agent_id\ttimestamp_epoch_ms\telapsed_s\tstage\tget_ops\tget_bytes\tget_iops\tget_mbps\tget_p50_us\tget_p90_us\tget_p99_us\tput_ops\tput_bytes\tput_iops\tput_mbps\tput_p50_us\tput_p90_us\tput_p99_us\tmeta_ops\tmeta_iops\tmeta_p50_us\tmeta_p90_us\tmeta_p99_us\tcpu_user_pct\tcpu_system_pct\tcpu_iowait_pct\terrors\tis_warmup";

// =============================================================================
// Distributed Testing Defaults
// =============================================================================

/// Default gRPC port for agents
pub const DEFAULT_AGENT_PORT: u16 = 7760;

/// Default coordinated start delay (seconds)
/// User can override via config: distributed.start_delay
pub const DEFAULT_START_DELAY_SECS: u64 = 5;

/// Default coordinated start delay (max seconds agents will wait)
pub const DEFAULT_MAX_START_DELAY_SECS: u64 = 60;

/// Default SSH timeout (seconds)
/// User can override via config: distributed.ssh_timeout
pub const DEFAULT_SSH_TIMEOUT_SECS: u64 = 10;

/// Default partition overlap (0.3 = 30%)
/// User can override via config: distributed.partition_overlap
pub const DEFAULT_PARTITION_OVERLAP: f64 = 0.3;

/// Agent startup timeout (seconds)
/// Controller waits this long for all agents to become ready
pub const DEFAULT_AGENT_STARTUP_TIMEOUT_SECS: u64 = 60;

/// Agent heartbeat interval (seconds)
/// Agents send live stats at this frequency during workload execution
pub const DEFAULT_AGENT_HEARTBEAT_SECS: u64 = 1;

// =============================================================================
// Bidirectional Streaming Timeouts (v0.8.4+)
// =============================================================================

// NOTE: IDLE state has NO timeout - agents wait indefinitely for controllers.
// This allows agents to run as long-lived services that accept connections on demand.

/// Agent READY state timeout (seconds)
/// Agent must receive coordinated START command within this time after becoming READY
/// If exceeded, agent returns to IDLE state and is ready for new connections
pub const AGENT_READY_TIMEOUT_SECS: u64 = 60;

/// Controller connection timeout (seconds)
/// Controller waits this long for gRPC connection to agent
pub const CONTROLLER_CONNECTION_TIMEOUT_SECS: u64 = 5;

/// Controller connection retry count
/// Number of times to retry failed connections with exponential backoff
pub const CONTROLLER_CONNECTION_RETRIES: u32 = 3;

/// Controller prepare phase timeout (seconds)
/// Controller waits this long for all agents to send READY
pub const CONTROLLER_PREPARE_TIMEOUT_SECS: u64 = 30;

/// Controller coordinated start timeout (seconds)
/// Controller waits this long for all agents to send first RUNNING after START(timestamp)
pub const CONTROLLER_START_TIMEOUT_SECS: u64 = 10;

/// Controller PING timeout (seconds)
/// Controller waits this long for ACKNOWLEDGE after sending PING
pub const CONTROLLER_PING_TIMEOUT_SECS: u64 = 5;

/// Controller PING retry count
/// Number of times to retry PING before marking agent as failed
pub const CONTROLLER_PING_RETRIES: u32 = 3;

/// Controller no-stats timeout (seconds)
/// If no stats received for this long, controller sends PING
pub const CONTROLLER_NO_STATS_PING_THRESHOLD_SECS: u64 = 10;

/// Controller agent hang timeout (seconds)
/// If no stats received for this long (after PING retries), mark agent as failed
pub const CONTROLLER_AGENT_HANG_TIMEOUT_SECS: u64 = 60;

/// Timeout monitor check interval (seconds)
/// How often to check for timeouts in agent control reader
pub const TIMEOUT_MONITOR_INTERVAL_SECS: u64 = 5;

/// Error message flush delay (seconds)
/// After sending critical messages (ERROR, COMPLETED, ABORTED), agent waits
/// this long before transitioning to Idle to ensure gRPC stream has time to
/// transmit the message to the controller. Prevents race condition where
/// stream closes before controller receives the error.
pub const AGENT_ERROR_FLUSH_DELAY_SECS: u64 = 5;

// =============================================================================
// Prepare Phase Defaults
// =============================================================================

/// Default minimum object size (1 MiB)
/// User can override via config: prepare.min_size
pub const DEFAULT_MIN_SIZE: u64 = 1024 * 1024;

/// Default maximum object size (1 MiB)
/// User can override via config: prepare.max_size
pub const DEFAULT_MAX_SIZE: u64 = 1024 * 1024;

/// Default deduplication factor (1 = all unique)
/// User can override via config: prepare.dedup_factor
pub const DEFAULT_DEDUP_FACTOR: usize = 1;

/// Default compression factor (1 = uncompressible)
/// User can override via config: prepare.compress_factor
pub const DEFAULT_COMPRESS_FACTOR: usize = 1;

/// Default post-prepare delay (seconds)
/// After creating objects, wait this long before starting workload
/// Helps ensure object metadata is fully propagated in distributed storage
pub const DEFAULT_POST_PREPARE_DELAY_SECS: u64 = 15;

/// Default directory tree width (subdirectories per level)
pub const DEFAULT_TREE_WIDTH: usize = 10;

/// Default directory tree depth (levels)
pub const DEFAULT_TREE_DEPTH: usize = 3;

/// Default files per directory
pub const DEFAULT_FILES_PER_DIR: usize = 100;

// =============================================================================
// I/O Rate Control Defaults
// =============================================================================

/// Default I/O rate (ops/sec) when not specified
/// None = unlimited (current behavior)
pub const DEFAULT_IO_RATE_OPS_SEC: Option<f64> = None;

// =============================================================================
// Replay Backpressure Defaults (v0.8.9+)
// =============================================================================

/// Lag threshold before switching to best-effort mode
/// When lag exceeds this, timing is abandoned and ops are issued as fast as possible
/// User can override via config: replay.lag_threshold
pub const DEFAULT_REPLAY_LAG_THRESHOLD: Duration = Duration::from_secs(5);

/// Recovery threshold for switching back to normal mode
/// Must be below lag_threshold to prevent flapping (hysteresis)
/// User can override via config: replay.recovery_threshold
pub const DEFAULT_REPLAY_RECOVERY_THRESHOLD: Duration = Duration::from_secs(1);

/// Maximum mode transitions per minute before graceful exit
/// Prevents oscillation between normal and best-effort modes
/// User can override via config: replay.max_flaps_per_minute
pub const DEFAULT_REPLAY_MAX_FLAPS_PER_MINUTE: u32 = 3;

/// Timeout for draining in-flight operations on flap-exit
/// After flap limit hit, wait this long for pending ops to complete
/// User can override via config: replay.drain_timeout
pub const DEFAULT_REPLAY_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum concurrent operations for replay
/// User can override via config: replay.max_concurrent
pub const DEFAULT_REPLAY_MAX_CONCURRENT: usize = 1000;

// =============================================================================
// Sleep Durations (polling, monitoring, rate limiting)
// =============================================================================

/// Progress bar update sleep interval (100ms)
/// Used in prepare phase progress monitoring
pub const PROGRESS_MONITOR_SLEEP_MS: u64 = 100;

/// Progress stats refresh interval (0.5 seconds)
/// How often to recalculate ops/sec and throughput
pub const PROGRESS_STATS_REFRESH_SECS: f64 = 0.5;

/// API rate limiting delay (10ms)
/// Small delay to avoid overwhelming cloud storage APIs (GCS list operations)
pub const API_RATE_LIMIT_DELAY_MS: u64 = 10;

/// Container startup wait duration (3 second)
/// Wait time after starting agent container via SSH
pub const CONTAINER_STARTUP_WAIT_SECS: u64 = 3;

/// SSH key propagation wait duration (3 second)
/// Wait time after copying SSH key to remote host
pub const SSH_KEY_PROPAGATION_WAIT_SECS: u64 = 3;

/// CPU monitor sample interval for testing (100ms)
/// Used in unit tests to verify sampling behavior
pub const CPU_MONITOR_TEST_SLEEP_MS: u64 = 100;

// =============================================================================
// Validation and Limits
// =============================================================================

/// Maximum coordinated start delay (prevent accidental huge delays)
pub const MAX_START_DELAY_SECS: u64 = 300;  // 5 minutes

/// Minimum concurrency (at least 1 worker)
pub const MIN_CONCURRENCY: usize = 1;

/// Maximum concurrency (prevent resource exhaustion)
pub const MAX_CONCURRENCY: usize = 10_000;

/// Minimum workload duration
pub const MIN_DURATION_SECS: u64 = 1;

/// Maximum workload duration (prevent runaway tests)
pub const MAX_DURATION_SECS: u64 = 86400;  // 24 hours

// =============================================================================
// File Naming Conventions
// =============================================================================

/// Results directory name prefix
pub const RESULTS_DIR_PREFIX: &str = "sai3-";

/// Agent results subdirectory name
pub const AGENT_RESULTS_DIR: &str = "sai3-agent-results";

/// Workload results filename
pub const WORKLOAD_RESULTS_FILENAME: &str = "workload_results.tsv";

/// Prepare results filename
pub const PREPARE_RESULTS_FILENAME: &str = "prepare_results.tsv";

/// Consolidated results filename (controller merges agent results)
pub const CONSOLIDATED_RESULTS_FILENAME: &str = "consolidated_results.tsv";

// =============================================================================
// Size Units (for human-readable display)
// =============================================================================

pub const KIB: u64 = 1024;
pub const MIB: u64 = 1024 * KIB;
pub const GIB: u64 = 1024 * MIB;
pub const TIB: u64 = 1024 * GIB;

pub const KB: u64 = 1000;
pub const MB: u64 = 1000 * KB;
pub const GB: u64 = 1000 * MB;
pub const TB: u64 = 1000 * GB;

// =============================================================================
// Helper Functions
// =============================================================================

/// Convert bytes to human-readable string (binary units: KiB, MiB, GiB)
pub fn format_bytes_binary(bytes: u64) -> String {
    if bytes >= TIB {
        format!("{:.2} TiB", bytes as f64 / TIB as f64)
    } else if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Convert bytes to human-readable string (decimal units: KB, MB, GB)
pub fn format_bytes_decimal(bytes: u64) -> String {
    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes_binary() {
        assert_eq!(format_bytes_binary(0), "0 B");
        assert_eq!(format_bytes_binary(512), "512 B");
        assert_eq!(format_bytes_binary(1024), "1.00 KiB");
        assert_eq!(format_bytes_binary(1536), "1.50 KiB");
        assert_eq!(format_bytes_binary(1_048_576), "1.00 MiB");
        assert_eq!(format_bytes_binary(1_073_741_824), "1.00 GiB");
    }

    #[test]
    fn test_bucket_boundaries_sorted() {
        // Verify bucket boundaries are in ascending order
        for i in 1..BUCKET_BOUNDARIES.len() {
            assert!(BUCKET_BOUNDARIES[i] > BUCKET_BOUNDARIES[i-1],
                    "Bucket boundaries must be strictly increasing");
        }
    }

    #[test]
    fn test_bucket_labels_match_boundaries() {
        assert_eq!(BUCKET_LABELS.len(), BUCKET_BOUNDARIES.len(),
                   "Number of labels must match number of boundaries");
        assert_eq!(BUCKET_LABELS.len(), NUM_BUCKETS,
                   "Number of labels must match NUM_BUCKETS");
    }
}
