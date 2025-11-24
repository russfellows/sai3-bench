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

/// Agent IDLE state timeout (seconds)
/// Agent must receive initial START command within this time
pub const AGENT_IDLE_TIMEOUT_SECS: u64 = 30;

/// Agent prepare phase timeout (seconds)
/// Prepare phase (config validation + file creation) must complete within this time
pub const AGENT_PREPARE_TIMEOUT_SECS: u64 = 30;

/// Agent READY state timeout (seconds)
/// Agent must receive coordinated START command within this time after becoming READY
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
