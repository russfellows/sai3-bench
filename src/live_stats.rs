// v0.7.5: Live stats tracking for distributed execution
//
// This module provides thread-safe tracking of operations during workload execution,
// enabling real-time progress monitoring in distributed environments via gRPC streaming.
//
// v0.8.9: Added flexible stage system for multi-phase execution (prepare, workload, cleanup, etc.)

use hdrhistogram::Histogram;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Execution stage for multi-phase workloads (v0.8.9+)
/// 
/// Matches the WorkloadStage enum in proto/iobench.proto
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i32)]
pub enum WorkloadStage {
    #[default]
    Unknown = 0,
    Prepare = 1,     // Creating objects before workload
    Workload = 2,    // Main benchmark execution
    Cleanup = 3,     // Deleting objects after workload
    Listing = 4,     // v0.8.14: Scanning existing objects before prepare
    Custom = 10,     // User-defined custom stage
}

impl WorkloadStage {
    /// Convert to proto enum value
    pub fn to_proto_i32(&self) -> i32 {
        *self as i32
    }
    
    /// Convert from i32 (for atomic storage)
    pub fn from_i32(value: i32) -> Self {
        match value {
            1 => WorkloadStage::Prepare,
            2 => WorkloadStage::Workload,
            3 => WorkloadStage::Cleanup,
            4 => WorkloadStage::Listing,
            10 => WorkloadStage::Custom,
            _ => WorkloadStage::Unknown,
        }
    }
    
    /// Default stage name for display
    pub fn default_name(&self) -> &'static str {
        match self {
            WorkloadStage::Unknown => "Unknown",
            WorkloadStage::Prepare => "Prepare",
            WorkloadStage::Workload => "Workload",
            WorkloadStage::Cleanup => "Cleanup",
            WorkloadStage::Listing => "Listing",
            WorkloadStage::Custom => "Custom",
        }
    }
}

/// Thread-safe tracker for live workload statistics
///
/// Uses atomic counters for operation counts/bytes and parking_lot::Mutex<Histogram>
/// for latency tracking. Designed for high-throughput workload execution with
/// minimal performance overhead.
#[derive(Clone)]
pub struct LiveStatsTracker {
    // Operation counters (atomic for thread-safe updates)
    get_ops: Arc<AtomicU64>,
    get_bytes: Arc<AtomicU64>,
    put_ops: Arc<AtomicU64>,
    put_bytes: Arc<AtomicU64>,
    meta_ops: Arc<AtomicU64>,
    
    // v0.7.9: Prepare phase progress tracking (DEPRECATED: use stage system)
    in_prepare_phase: Arc<AtomicU64>,  // 0=false, 1=true (atomic bool)
    prepare_objects_created: Arc<AtomicU64>,
    prepare_objects_total: Arc<AtomicU64>,
    
    // v0.8.9: Flexible stage system
    current_stage: Arc<AtomicU64>,       // WorkloadStage as u64
    stage_name: Arc<Mutex<String>>,      // Custom stage name
    stage_progress_current: Arc<AtomicU64>,
    stage_progress_total: Arc<AtomicU64>,
    stage_start_time: Arc<Mutex<Instant>>,
    
    // v0.8.14: Concurrency tracking for total thread count display
    concurrency: u32,

    // Latency histograms (microseconds) - Mutex for snapshot operations
    get_hist: Arc<Mutex<Histogram<u64>>>,
    put_hist: Arc<Mutex<Histogram<u64>>>,
    meta_hist: Arc<Mutex<Histogram<u64>>>,

    // Timing
    start_time: Instant,
}

impl std::fmt::Debug for LiveStatsTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveStatsTracker")
            .field("get_ops", &self.get_ops.load(Ordering::Relaxed))
            .field("get_bytes", &self.get_bytes.load(Ordering::Relaxed))
            .field("put_ops", &self.put_ops.load(Ordering::Relaxed))
            .field("put_bytes", &self.put_bytes.load(Ordering::Relaxed))
            .field("meta_ops", &self.meta_ops.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl LiveStatsTracker {
    /// Create new tracker with histogram configuration matching workload.rs
    ///
    /// Histograms track latencies from 1 microsecond to 1 hour with 3 significant digits.
    pub fn new() -> Self {
        Self::new_with_concurrency(0)  // Default to 0 (unknown)
    }
    
    /// Create new tracker with explicit concurrency value (v0.8.14)
    ///
    /// Use this when you know the concurrency at creation time (e.g., from config).
    pub fn new_with_concurrency(concurrency: u32) -> Self {
        let now = Instant::now();
        Self {
            get_ops: Arc::new(AtomicU64::new(0)),
            get_bytes: Arc::new(AtomicU64::new(0)),
            put_ops: Arc::new(AtomicU64::new(0)),
            put_bytes: Arc::new(AtomicU64::new(0)),
            meta_ops: Arc::new(AtomicU64::new(0)),
            in_prepare_phase: Arc::new(AtomicU64::new(0)),
            prepare_objects_created: Arc::new(AtomicU64::new(0)),
            prepare_objects_total: Arc::new(AtomicU64::new(0)),
            current_stage: Arc::new(AtomicU64::new(WorkloadStage::Unknown as u64)),
            stage_name: Arc::new(Mutex::new(String::new())),
            stage_progress_current: Arc::new(AtomicU64::new(0)),
            stage_progress_total: Arc::new(AtomicU64::new(0)),
            stage_start_time: Arc::new(Mutex::new(now)),
            concurrency,
            get_hist: Arc::new(Mutex::new(
                Histogram::new_with_bounds(1, 3_600_000_000, 3).unwrap(),
            )),
            put_hist: Arc::new(Mutex::new(
                Histogram::new_with_bounds(1, 3_600_000_000, 3).unwrap(),
            )),
            meta_hist: Arc::new(Mutex::new(
                Histogram::new_with_bounds(1, 3_600_000_000, 3).unwrap(),
            )),
            start_time: now,
        }
    }

    /// Record a GET operation (non-blocking atomic update)
    #[inline]
    pub fn record_get(&self, bytes: usize, latency: Duration) {
        self.get_ops.fetch_add(1, Ordering::Relaxed);
        self.get_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        let us = latency.as_micros().min(u64::MAX as u128) as u64;
        // Histogram update requires lock but is fast (O(1) amortized)
        let _ = self.get_hist.lock().record(us);
    }

    /// Record a PUT operation (non-blocking atomic update)
    #[inline]
    pub fn record_put(&self, bytes: usize, latency: Duration) {
        self.put_ops.fetch_add(1, Ordering::Relaxed);
        self.put_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        let us = latency.as_micros().min(u64::MAX as u128) as u64;
        let _ = self.put_hist.lock().record(us);
    }

    /// Record a META operation (HEAD/LIST/DELETE - non-blocking atomic update)
    #[inline]
    pub fn record_meta(&self, latency: Duration) {
        self.meta_ops.fetch_add(1, Ordering::Relaxed);
        let us = latency.as_micros().min(u64::MAX as u128) as u64;
        let _ = self.meta_hist.lock().record(us);
    }
    
    // =========================================================================
    // v0.8.9: Flexible stage system
    // =========================================================================
    
    /// Set the current execution stage (v0.8.9+)
    /// 
    /// Call this when transitioning between stages (prepare → workload → cleanup).
    /// Resets stage progress counters and stage start time.
    pub fn set_stage(&self, stage: WorkloadStage, total: u64) {
        self.current_stage.store(stage as u64, Ordering::Relaxed);
        *self.stage_name.lock() = stage.default_name().to_string();
        self.stage_progress_current.store(0, Ordering::Relaxed);
        self.stage_progress_total.store(total, Ordering::Relaxed);
        *self.stage_start_time.lock() = Instant::now();
        
        // Also update legacy in_prepare_phase for backward compatibility
        if stage == WorkloadStage::Prepare {
            self.in_prepare_phase.store(1, Ordering::Relaxed);
            self.prepare_objects_total.store(total, Ordering::Relaxed);
            self.prepare_objects_created.store(0, Ordering::Relaxed);
        } else {
            self.in_prepare_phase.store(0, Ordering::Relaxed);
        }
    }
    
    /// Set stage with custom name (v0.8.9+)
    pub fn set_stage_with_name(&self, stage: WorkloadStage, name: &str, total: u64) {
        self.set_stage(stage, total);
        *self.stage_name.lock() = name.to_string();
    }
    
    /// Update stage progress (v0.8.9+)
    #[inline]
    pub fn set_stage_progress(&self, current: u64) {
        self.stage_progress_current.store(current, Ordering::Relaxed);
        
        // Also update legacy prepare progress for backward compatibility
        if self.current_stage.load(Ordering::Relaxed) == WorkloadStage::Prepare as u64 {
            self.prepare_objects_created.store(current, Ordering::Relaxed);
        }
    }
    
    /// Increment stage progress by 1 (v0.8.9+)
    #[inline]
    pub fn increment_stage_progress(&self) {
        self.stage_progress_current.fetch_add(1, Ordering::Relaxed);
        
        // Also update legacy prepare progress for backward compatibility
        if self.current_stage.load(Ordering::Relaxed) == WorkloadStage::Prepare as u64 {
            self.prepare_objects_created.fetch_add(1, Ordering::Relaxed);
        }
    }
    
    /// Get current stage (v0.8.9+)
    pub fn get_stage(&self) -> WorkloadStage {
        match self.current_stage.load(Ordering::Relaxed) {
            1 => WorkloadStage::Prepare,
            2 => WorkloadStage::Workload,
            3 => WorkloadStage::Cleanup,
            10 => WorkloadStage::Custom,
            _ => WorkloadStage::Unknown,
        }
    }
    
    /// Get stage elapsed time (v0.8.9+)
    pub fn stage_elapsed(&self) -> Duration {
        self.stage_start_time.lock().elapsed()
    }
    
    // =========================================================================
    // Legacy prepare phase methods (DEPRECATED - use set_stage instead)
    // =========================================================================
    
    /// Set prepare phase progress (v0.7.9+, DEPRECATED)
    ///
    /// Call at start of prepare phase with total object count, then update
    /// created count as objects are prepared. Call set_prepare_complete()
    /// when prepare phase finishes.
    #[inline]
    pub fn set_prepare_progress(&self, created: u64, total: u64) {
        self.in_prepare_phase.store(1, Ordering::Relaxed);
        self.prepare_objects_created.store(created, Ordering::Relaxed);
        self.prepare_objects_total.store(total, Ordering::Relaxed);
        
        // Also update new stage system
        self.current_stage.store(WorkloadStage::Prepare as u64, Ordering::Relaxed);
        self.stage_progress_current.store(created, Ordering::Relaxed);
        self.stage_progress_total.store(total, Ordering::Relaxed);
    }
    
    /// Mark prepare phase as complete (v0.7.9+, DEPRECATED)
    #[inline]
    pub fn set_prepare_complete(&self) {
        self.in_prepare_phase.store(0, Ordering::Relaxed);
    }
    
    /// Reset all counters for workload phase (v0.7.9+)
    /// 
    /// Call this when transitioning from prepare to workload to clear
    /// prepare phase statistics (PUT operations) that shouldn't appear
    /// in workload-only metrics.
    pub fn reset_for_workload(&self) {
        self.get_ops.store(0, Ordering::Relaxed);
        self.get_bytes.store(0, Ordering::Relaxed);
        self.put_ops.store(0, Ordering::Relaxed);
        self.put_bytes.store(0, Ordering::Relaxed);
        self.meta_ops.store(0, Ordering::Relaxed);
        
        // Clear histograms
        self.get_hist.lock().clear();
        self.put_hist.lock().clear();
        self.meta_hist.lock().clear();
    }

    /// Capture current stats snapshot (for gRPC streaming)
    ///
    /// Returns cumulative statistics and latency percentiles. This method
    /// acquires histogram locks briefly but is designed for 1-second intervals.
    pub fn snapshot(&self) -> LiveStatsSnapshot {
        let elapsed = self.start_time.elapsed();

        // Read atomic counters (relaxed ordering sufficient for monitoring)
        let get_ops = self.get_ops.load(Ordering::Relaxed);
        let get_bytes = self.get_bytes.load(Ordering::Relaxed);
        let put_ops = self.put_ops.load(Ordering::Relaxed);
        let put_bytes = self.put_bytes.load(Ordering::Relaxed);
        let meta_ops = self.meta_ops.load(Ordering::Relaxed);
        
        // v0.7.9: Read prepare phase progress
        let in_prepare_phase = self.in_prepare_phase.load(Ordering::Relaxed) != 0;
        let prepare_objects_created = self.prepare_objects_created.load(Ordering::Relaxed);
        let prepare_objects_total = self.prepare_objects_total.load(Ordering::Relaxed);

        // Calculate latency percentiles (requires histogram lock)
        let (get_mean_us, get_p50_us, get_p95_us) = {
            let hist = self.get_hist.lock();
            if !hist.is_empty() {
                (hist.mean() as u64, hist.value_at_quantile(0.50), hist.value_at_quantile(0.95))
            } else {
                (0, 0, 0)
            }
        };

        let (put_mean_us, put_p50_us, put_p95_us) = {
            let hist = self.put_hist.lock();
            if !hist.is_empty() {
                (hist.mean() as u64, hist.value_at_quantile(0.50), hist.value_at_quantile(0.95))
            } else {
                (0, 0, 0)
            }
        };

        let meta_mean_us = {
            let hist = self.meta_hist.lock();
            if !hist.is_empty() {
                hist.mean() as u64
            } else {
                0
            }
        };

        LiveStatsSnapshot {
            elapsed,
            get_ops,
            get_bytes,
            get_mean_us,
            get_p50_us,
            get_p95_us,
            put_ops,
            put_bytes,
            put_mean_us,
            put_p50_us,
            put_p95_us,
            meta_ops,
            meta_mean_us,
            in_prepare_phase,
            prepare_objects_created,
            prepare_objects_total,
            // v0.8.9: Multi-stage tracking
            current_stage: WorkloadStage::from_i32(self.current_stage.load(Ordering::Relaxed) as i32),
            stage_name: self.stage_name.lock().clone(),
            stage_progress_current: self.stage_progress_current.load(Ordering::Relaxed),
            stage_progress_total: self.stage_progress_total.load(Ordering::Relaxed),
            stage_elapsed_s: self.stage_start_time.lock().elapsed().as_secs_f64(),
            // v0.8.14: Concurrency
            concurrency: self.concurrency,
        }
    }
}

impl Default for LiveStatsTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of live stats at a point in time (for proto conversion)
#[derive(Debug, Clone)]
pub struct LiveStatsSnapshot {
    pub elapsed: Duration,
    pub get_ops: u64,
    pub get_bytes: u64,
    pub get_mean_us: u64,
    pub get_p50_us: u64,
    pub get_p95_us: u64,
    pub put_ops: u64,
    pub put_bytes: u64,
    pub put_mean_us: u64,
    pub put_p50_us: u64,
    pub put_p95_us: u64,
    pub meta_ops: u64,
    pub meta_mean_us: u64,
    pub in_prepare_phase: bool,
    pub prepare_objects_created: u64,
    pub prepare_objects_total: u64,
    // v0.8.9: Multi-stage tracking
    pub current_stage: WorkloadStage,
    pub stage_name: String,
    pub stage_progress_current: u64,
    pub stage_progress_total: u64,
    pub stage_elapsed_s: f64,
    // v0.8.14: Concurrency for total thread count display
    pub concurrency: u32,
}

impl LiveStatsSnapshot {
    /// Get elapsed seconds as f64 for proto conversion
    pub fn elapsed_secs(&self) -> f64 {
        self.elapsed.as_secs_f64()
    }

    /// Get wall-clock timestamp
    pub fn timestamp_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_tracker_basic() {
        let tracker = LiveStatsTracker::new();
        
        // Record some operations
        tracker.record_get(1024, Duration::from_micros(100));
        tracker.record_get(2048, Duration::from_micros(150));
        tracker.record_put(512, Duration::from_micros(200));
        tracker.record_meta(Duration::from_micros(50));

        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.get_ops, 2);
        assert_eq!(snapshot.get_bytes, 3072);
        assert_eq!(snapshot.put_ops, 1);
        assert_eq!(snapshot.put_bytes, 512);
        assert_eq!(snapshot.meta_ops, 1);
        
        // Verify latencies are reasonable
        assert!(snapshot.get_mean_us >= 100 && snapshot.get_mean_us <= 150);
        assert!(snapshot.put_mean_us >= 180 && snapshot.put_mean_us <= 220);
    }

    #[test]
    fn test_tracker_threadsafe() {
        let tracker = LiveStatsTracker::new();
        let mut handles = vec![];

        // Spawn 10 threads recording operations concurrently
        for _ in 0..10 {
            let t = tracker.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    t.record_get(1000, Duration::from_micros(100));
                    t.record_put(500, Duration::from_micros(200));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.get_ops, 1000);
        assert_eq!(snapshot.get_bytes, 1_000_000);
        assert_eq!(snapshot.put_ops, 1000);
        assert_eq!(snapshot.put_bytes, 500_000);
    }

    #[test]
    fn test_snapshot_to_proto() {
        let tracker = LiveStatsTracker::new();
        tracker.record_get(1024, Duration::from_micros(100));
        
        let snapshot = tracker.snapshot();
        
        assert_eq!(snapshot.get_ops, 1);
        assert_eq!(snapshot.get_bytes, 1024);
        assert!(snapshot.elapsed_secs() > 0.0);
    }
    
    #[test]
    fn test_stage_tracking() {
        let tracker = LiveStatsTracker::new();
        
        // Initially in Unknown stage
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.current_stage, WorkloadStage::Unknown);
        assert_eq!(snapshot.stage_progress_current, 0);
        assert_eq!(snapshot.stage_progress_total, 0);
        
        // Set to Prepare stage with 100 total objects
        tracker.set_stage(WorkloadStage::Prepare, 100);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.current_stage, WorkloadStage::Prepare);
        assert_eq!(snapshot.stage_name, "Prepare");
        assert_eq!(snapshot.stage_progress_current, 0);
        assert_eq!(snapshot.stage_progress_total, 100);
        assert!(snapshot.in_prepare_phase);  // Legacy compat
        
        // Increment progress
        tracker.increment_stage_progress();
        tracker.increment_stage_progress();
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.stage_progress_current, 2);
        
        // Transition to Workload stage (time-based, total=0)
        tracker.set_stage(WorkloadStage::Workload, 0);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.current_stage, WorkloadStage::Workload);
        assert_eq!(snapshot.stage_name, "Workload");
        assert_eq!(snapshot.stage_progress_current, 0);  // Reset on stage change
        assert_eq!(snapshot.stage_progress_total, 0);
        assert!(!snapshot.in_prepare_phase);  // Legacy compat
        
        // Transition to Cleanup stage
        tracker.set_stage(WorkloadStage::Cleanup, 50);
        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.current_stage, WorkloadStage::Cleanup);
        assert_eq!(snapshot.stage_name, "Cleanup");
        assert_eq!(snapshot.stage_progress_total, 50);
    }
    
    #[test]
    fn test_stage_elapsed_time() {
        let tracker = LiveStatsTracker::new();
        tracker.set_stage(WorkloadStage::Prepare, 10);
        
        // Stage elapsed should be small immediately after setting
        let snapshot = tracker.snapshot();
        assert!(snapshot.stage_elapsed_s < 0.1);
        
        // Wait a bit and check elapsed increased
        std::thread::sleep(Duration::from_millis(50));
        let snapshot = tracker.snapshot();
        assert!(snapshot.stage_elapsed_s >= 0.05);
    }
}
