// src/perf_log.rs
//
// Performance Log (perf-log) - Time-series performance metrics capture
//
// v0.8.15: New module for capturing interval-based performance metrics
//
// Unlike op-log (which records individual operations), perf-log captures
// aggregate metrics at configurable intervals (e.g., every 1 second) for
// time-series analysis of performance over the duration of a workload.
//
// Features:
// - Delta-based metrics (ops/bytes per interval, not cumulative)
// - Warmup period flagging (is_warmup column)
// - Stage tracking (prepare, workload, cleanup)
// - Latency percentiles per interval (p50, p90, p99)
// - CPU utilization metrics (user, system, iowait)
// - Agent identification for distributed workloads
// - Optional zstd compression (.zst extension)
//
// IMPORTANT: In distributed mode, aggregate perf_log percentiles are computed
// using weighted averaging, which is a mathematical approximation. For statistically
// valid percentile analysis, use:
//   1. Per-agent perf_log files (accurate - computed from local HDR histograms)
//   2. Final workload_results.tsv (accurate - uses HDR histogram merging)
// Aggregate perf_log percentiles are suitable for monitoring/visualization only.
//
// Format: TSV (tab-separated values) - 31 columns (v0.8.17+)
// See docs/PERF_LOG_FORMAT.md for complete column specification
// See src/constants.rs for PERF_LOG_HEADER constant

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// Import and re-export PERF_LOG_HEADER for backwards compatibility
// The constant is defined in src/constants.rs
pub use crate::constants::PERF_LOG_HEADER;
use crate::live_stats::WorkloadStage;

/// Parameters for compute_delta to avoid too many function arguments (v0.8.19)
#[derive(Debug, Clone)]
pub struct PerfMetrics {
    pub get_ops: u64,
    pub get_bytes: u64,
    pub put_ops: u64,
    pub put_bytes: u64,
    pub meta_ops: u64,
    pub errors: u64,
    pub get_mean_us: u64,
    pub get_p50_us: u64,
    pub get_p90_us: u64,
    pub get_p99_us: u64,
    pub put_mean_us: u64,
    pub put_p50_us: u64,
    pub put_p90_us: u64,
    pub put_p99_us: u64,
    pub meta_mean_us: u64,
    pub meta_p50_us: u64,
    pub meta_p90_us: u64,
    pub meta_p99_us: u64,
    pub cpu_user_percent: f64,
    pub cpu_system_percent: f64,
    pub cpu_iowait_percent: f64,
}

/// Performance log entry representing metrics for a single interval
#[derive(Debug, Clone)]
pub struct PerfLogEntry {
    /// Agent/worker identifier (e.g., "agent-1", "local")
    pub agent_id: String,
    /// Unix timestamp in milliseconds
    pub timestamp_epoch_ms: u64,
    /// Elapsed time since workload start in seconds
    pub elapsed_s: f64,
    /// Current execution stage
    pub stage: WorkloadStage,
    /// Stage name (for custom stages)
    pub stage_name: String,
    
    // GET operation deltas (this interval only)
    pub get_ops: u64,
    pub get_bytes: u64,
    pub get_iops: f64,
    pub get_mbps: f64,
    pub get_mean_us: u64,
    pub get_p50_us: u64,
    pub get_p90_us: u64,
    pub get_p99_us: u64,
    
    // PUT operation deltas (this interval only)
    pub put_ops: u64,
    pub put_bytes: u64,
    pub put_iops: f64,
    pub put_mbps: f64,
    pub put_mean_us: u64,
    pub put_p50_us: u64,
    pub put_p90_us: u64,
    pub put_p99_us: u64,
    
    // META operation deltas (this interval only)
    pub meta_ops: u64,
    pub meta_iops: f64,
    pub meta_mean_us: u64,
    pub meta_p50_us: u64,
    pub meta_p90_us: u64,
    pub meta_p99_us: u64,
    
    // CPU utilization (percentage)
    pub cpu_user_percent: f64,
    pub cpu_system_percent: f64,
    pub cpu_iowait_percent: f64,
    
    /// Error count in this interval
    pub errors: u64,
}

impl PerfLogEntry {
    /// Format as TSV row (without newline)
    pub fn to_tsv(&self) -> String {
        let stage_str = if self.stage_name.is_empty() {
            self.stage.default_name().to_string()
        } else {
            self.stage_name.clone()
        };
        
        format!(
            "{}\t{}\t{:.3}\t{}\t{}\t{}\t{:.1}\t{:.2}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{:.2}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{}\t{}\t{}\t{}\t{:.1}\t{:.1}\t{:.1}\t{}",
            self.agent_id,
            self.timestamp_epoch_ms,
            self.elapsed_s,
            stage_str,
            self.get_ops,
            self.get_bytes,
            self.get_iops,
            self.get_mbps,
            self.get_mean_us,
            self.get_p50_us,
            self.get_p90_us,
            self.get_p99_us,
            self.put_ops,
            self.put_bytes,
            self.put_iops,
            self.put_mbps,
            self.put_mean_us,
            self.put_p50_us,
            self.put_p90_us,
            self.put_p99_us,
            self.meta_ops,
            self.meta_iops,
            self.meta_mean_us,
            self.meta_p50_us,
            self.meta_p90_us,
            self.meta_p99_us,
            self.cpu_user_percent,
            self.cpu_system_percent,
            self.cpu_iowait_percent,
            self.errors,
        )
    }
}

// Note: PERF_LOG_HEADER constant is defined in src/constants.rs
// See docs/PERF_LOG_FORMAT.md for complete column specification

/// Writer for performance log files
/// 
/// Supports both plain TSV and zstd-compressed output based on file extension.
pub struct PerfLogWriter {
    writer: Box<dyn Write + Send>,
    path: PathBuf,
    is_compressed: bool,
}

impl PerfLogWriter {
    /// Create a new perf-log writer
    /// 
    /// If path ends with `.zst`, output will be zstd-compressed.
    pub fn new(path: &Path) -> Result<Self> {
        let is_compressed = path.extension()
            .map(|ext| ext == "zst")
            .unwrap_or(false);
        
        let file = File::create(path)
            .with_context(|| format!("Failed to create perf-log file: {}", path.display()))?;
        
        let writer: Box<dyn Write + Send> = if is_compressed {
            // Use zstd compression (level 3 for good speed/ratio balance)
            let encoder = zstd::stream::Encoder::new(file, 3)
                .with_context(|| "Failed to create zstd encoder")?
                .auto_finish();
            Box::new(BufWriter::with_capacity(64 * 1024, encoder))
        } else {
            Box::new(BufWriter::with_capacity(64 * 1024, file))
        };
        
        let mut writer = Self {
            writer,
            path: path.to_path_buf(),
            is_compressed,
        };
        
        // Write header
        writer.write_header()?;
        
        Ok(writer)
    }
    
    /// Write TSV header
    fn write_header(&mut self) -> Result<()> {
        writeln!(self.writer, "{}", PERF_LOG_HEADER)
            .with_context(|| "Failed to write perf-log header")?;
        Ok(())
    }
    
    /// Write a single entry
    pub fn write_entry(&mut self, entry: &PerfLogEntry) -> Result<()> {
        writeln!(self.writer, "{}", entry.to_tsv())
            .with_context(|| "Failed to write perf-log entry")?;
        Ok(())
    }
    
    /// Flush buffered data to disk
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush()
            .with_context(|| "Failed to flush perf-log")?;
        Ok(())
    }
    
    /// Get the path of the perf-log file
    pub fn path(&self) -> &Path {
        &self.path
    }
    
    /// Check if output is compressed
    pub fn is_compressed(&self) -> bool {
        self.is_compressed
    }
}

impl std::fmt::Debug for PerfLogWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerfLogWriter")
            .field("path", &self.path)
            .field("is_compressed", &self.is_compressed)
            .finish()
    }
}

/// Tracker for computing delta metrics between intervals
/// 
/// Stores previous cumulative values to compute per-interval deltas.
#[derive(Debug, Clone, Default)]
pub struct PerfLogDeltaTracker {
    // Previous cumulative values
    prev_get_ops: u64,
    prev_get_bytes: u64,
    prev_put_ops: u64,
    prev_put_bytes: u64,
    prev_meta_ops: u64,
    prev_errors: u64,
    
    // Timing
    prev_timestamp: Option<Instant>,
    workload_start: Option<Instant>,
    warmup_end: Option<Instant>,
}

impl PerfLogDeltaTracker {
    /// Create a new delta tracker
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Initialize tracker at workload start
    pub fn start(&mut self, warmup_duration: Option<Duration>) {
        let now = Instant::now();
        self.workload_start = Some(now);
        self.prev_timestamp = Some(now);
        self.warmup_end = warmup_duration.map(|d| now + d);
    }
    
    /// Reset the warmup timer for the main workload phase
    /// 
    /// Call this when transitioning from prepare to workload phase.
    /// The warmup period should be measured from when the actual workload starts,
    /// not from when prepare began.
    pub fn reset_warmup_for_workload(&mut self, warmup_duration: Option<Duration>) {
        let now = Instant::now();
        // Reset the workload start time to now (for elapsed_s calculation)
        self.workload_start = Some(now);
        self.prev_timestamp = Some(now);
        // Reset warmup end based on new start time
        self.warmup_end = warmup_duration.map(|d| now + d);
        // Note: We don't reset prev_* counters - those track cumulative deltas
    }
    
    /// Compute delta entry from current cumulative stats
    /// 
    /// Returns a PerfLogEntry with delta values (ops/bytes in this interval)
    /// and computed rates (IOPS, MB/s).
    /// 
    /// v0.8.19: Refactored to use PerfMetrics struct to reduce argument count
    pub fn compute_delta(
        &mut self,
        agent_id: &str,
        metrics: &PerfMetrics,
        stage: WorkloadStage,
        stage_name: String,
    ) -> PerfLogEntry {
        let now = Instant::now();
        let timestamp_epoch_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        
        // Calculate elapsed time
        let elapsed_s = self.workload_start
            .map(|start| (now - start).as_secs_f64())
            .unwrap_or(0.0);
        
        // Calculate interval duration
        let interval_s = self.prev_timestamp
            .map(|prev| (now - prev).as_secs_f64())
            .unwrap_or(1.0)
            .max(0.001); // Prevent division by zero
        
        // Calculate deltas
        let delta_get_ops = metrics.get_ops.saturating_sub(self.prev_get_ops);
        let delta_get_bytes = metrics.get_bytes.saturating_sub(self.prev_get_bytes);
        let delta_put_ops = metrics.put_ops.saturating_sub(self.prev_put_ops);
        let delta_put_bytes = metrics.put_bytes.saturating_sub(self.prev_put_bytes);
        let delta_meta_ops = metrics.meta_ops.saturating_sub(self.prev_meta_ops);
        let delta_errors = metrics.errors.saturating_sub(self.prev_errors);
        
        // Calculate rates
        let get_iops = delta_get_ops as f64 / interval_s;
        let get_mbps = (delta_get_bytes as f64 / (1024.0 * 1024.0)) / interval_s;
        let put_iops = delta_put_ops as f64 / interval_s;
        let put_mbps = (delta_put_bytes as f64 / (1024.0 * 1024.0)) / interval_s;
        let meta_iops = delta_meta_ops as f64 / interval_s;
        
        // Update previous values
        self.prev_get_ops = metrics.get_ops;
        self.prev_get_bytes = metrics.get_bytes;
        self.prev_put_ops = metrics.put_ops;
        self.prev_put_bytes = metrics.put_bytes;
        self.prev_meta_ops = metrics.meta_ops;
        self.prev_errors = metrics.errors;
        self.prev_timestamp = Some(now);
        
        PerfLogEntry {
            agent_id: agent_id.to_string(),
            timestamp_epoch_ms,
            elapsed_s,
            stage,
            stage_name,
            get_ops: delta_get_ops,
            get_bytes: delta_get_bytes,
            get_iops,
            get_mbps,
            get_mean_us: metrics.get_mean_us,
            get_p50_us: metrics.get_p50_us,
            get_p90_us: metrics.get_p90_us,
            get_p99_us: metrics.get_p99_us,
            put_ops: delta_put_ops,
            put_bytes: delta_put_bytes,
            put_iops,
            put_mbps,
            put_mean_us: metrics.put_mean_us,
            put_p50_us: metrics.put_p50_us,
            put_p90_us: metrics.put_p90_us,
            put_p99_us: metrics.put_p99_us,
            meta_ops: delta_meta_ops,
            meta_iops,
            meta_mean_us: metrics.meta_mean_us,
            meta_p50_us: metrics.meta_p50_us,
            meta_p90_us: metrics.meta_p90_us,
            meta_p99_us: metrics.meta_p99_us,
            cpu_user_percent: metrics.cpu_user_percent,
            cpu_system_percent: metrics.cpu_system_percent,
            cpu_iowait_percent: metrics.cpu_iowait_percent,
            errors: delta_errors,
        }
    }
    
    /// Check if we're currently in the warmup period
    pub fn is_warmup(&self) -> bool {
        self.warmup_end
            .map(|end| Instant::now() < end)
            .unwrap_or(false)
    }
    
    /// Get elapsed time since workload start
    pub fn elapsed(&self) -> Duration {
        self.workload_start
            .map(|start| Instant::now() - start)
            .unwrap_or(Duration::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::tempdir;
    
    #[test]
    fn test_perf_log_entry_to_tsv() {
        let entry = PerfLogEntry {
            agent_id: "agent-1".to_string(),
            timestamp_epoch_ms: 1733836800000,
            elapsed_s: 1.5,
            stage: WorkloadStage::Workload,
            stage_name: String::new(),
            get_ops: 100,
            get_bytes: 102400,
            get_iops: 100.0,
            get_mbps: 0.098,
            get_mean_us: 800,
            get_p50_us: 1000,
            get_p90_us: 3000,
            get_p99_us: 5000,
            put_ops: 50,
            put_bytes: 51200,
            put_iops: 50.0,
            put_mbps: 0.049,
            put_mean_us: 1500,
            put_p50_us: 2000,
            put_p90_us: 5000,
            put_p99_us: 8000,
            meta_ops: 10,
            meta_iops: 10.0,
            meta_mean_us: 400,
            meta_p50_us: 500,
            meta_p90_us: 1500,
            meta_p99_us: 2000,
            cpu_user_percent: 25.5,
            cpu_system_percent: 10.2,
            cpu_iowait_percent: 5.3,
            errors: 0,
        };
        
        let tsv = entry.to_tsv();
        let parts: Vec<&str> = tsv.split('\t').collect();
        
        assert_eq!(parts.len(), 30);  // Updated: removed is_warmup column
        assert_eq!(parts[0], "agent-1");
        assert_eq!(parts[1], "1733836800000");
        assert_eq!(parts[3], "Workload");
        assert_eq!(parts[4], "100");  // get_ops
        assert_eq!(parts[29], "0");   // errors (last column)
    }
    
    #[test]
    fn test_perf_log_writer_plain() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.tsv");
        
        let mut writer = PerfLogWriter::new(&path).unwrap();
        assert!(!writer.is_compressed());
        
        let entry = PerfLogEntry {
            agent_id: "agent-2".to_string(),
            timestamp_epoch_ms: 1733836800000,
            elapsed_s: 1.0,
            stage: WorkloadStage::Workload,
            stage_name: String::new(),
            get_ops: 100,
            get_bytes: 102400,
            get_iops: 100.0,
            get_mbps: 0.1,
            get_mean_us: 800,
            get_p50_us: 1000,
            get_p90_us: 3000,
            get_p99_us: 5000,
            put_ops: 0,
            put_bytes: 0,
            put_iops: 0.0,
            put_mbps: 0.0,
            put_mean_us: 0,
            put_p50_us: 0,
            put_p90_us: 0,
            put_p99_us: 0,
            meta_ops: 0,
            meta_iops: 0.0,
            meta_mean_us: 0,
            meta_p50_us: 0,
            meta_p90_us: 0,
            meta_p99_us: 0,
            cpu_user_percent: 15.0,
            cpu_system_percent: 5.0,
            cpu_iowait_percent: 2.0,
            errors: 0,
        };
        
        writer.write_entry(&entry).unwrap();
        writer.flush().unwrap();
        drop(writer);
        
        // Read and verify
        let mut content = String::new();
        File::open(&path).unwrap().read_to_string(&mut content).unwrap();
        
        assert!(content.starts_with("agent_id\t"));
        assert!(content.contains("agent-2"));
    }
    
    #[test]
    fn test_delta_tracker_basic() {
        let mut tracker = PerfLogDeltaTracker::new();
        tracker.start(None);
        
        // First interval: 100 ops, 100KB
        let metrics1 = PerfMetrics {
            get_ops: 100, get_bytes: 102400,
            put_ops: 0, put_bytes: 0,
            meta_ops: 0, errors: 0,
            get_mean_us: 500, get_p50_us: 1000, get_p90_us: 3000, get_p99_us: 5000,
            put_mean_us: 0, put_p50_us: 0, put_p90_us: 0, put_p99_us: 0,
            meta_mean_us: 0, meta_p50_us: 0, meta_p90_us: 0, meta_p99_us: 0,
            cpu_user_percent: 25.0, cpu_system_percent: 10.0, cpu_iowait_percent: 5.0,
        };
        let entry1 = tracker.compute_delta(
            "agent-1",
            &metrics1,
            WorkloadStage::Workload,
            String::new(),
        );
        
        assert_eq!(entry1.agent_id, "agent-1");
        assert_eq!(entry1.get_ops, 100);
        assert_eq!(entry1.get_bytes, 102400);
        
        // Second interval: 250 ops total (delta = 150)
        std::thread::sleep(std::time::Duration::from_millis(10));
        let metrics2 = PerfMetrics {
            get_ops: 250, get_bytes: 256000,
            put_ops: 0, put_bytes: 0,
            meta_ops: 0, errors: 0,
            get_mean_us: 550, get_p50_us: 1100, get_p90_us: 3500, get_p99_us: 5500,
            put_mean_us: 0, put_p50_us: 0, put_p90_us: 0, put_p99_us: 0,
            meta_mean_us: 0, meta_p50_us: 0, meta_p90_us: 0, meta_p99_us: 0,
            cpu_user_percent: 30.0, cpu_system_percent: 12.0, cpu_iowait_percent: 8.0,
        };
        let entry2 = tracker.compute_delta(
            "agent-1",
            &metrics2,
            WorkloadStage::Workload,
            String::new(),
        );
        
        assert_eq!(entry2.get_ops, 150);  // 250 - 100 = 150
        assert_eq!(entry2.get_bytes, 153600);  // 256000 - 102400
    }
    
    #[test]
    fn test_header_format() {
        let parts: Vec<&str> = PERF_LOG_HEADER.split('\t').collect();
        assert_eq!(parts.len(), 30);  // Updated: removed is_warmup column
        assert_eq!(parts[0], "agent_id");
        assert_eq!(parts[29], "errors");  // Last column
    }
}
