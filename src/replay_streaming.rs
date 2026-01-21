//! Op-log replay functionality for sai3-bench v0.5.0
//!
//! Implements timing-faithful workload replay using s3dlio-oplog streaming reader.
//! This version uses constant memory (~1.5 MB) regardless of op-log size.
//!
//! ## Backpressure System (v0.8.9+)
//!
//! When the target storage cannot sustain the recorded I/O rate, replay switches
//! to "best-effort" mode where timing constraints are abandoned. If mode oscillation
//! (flapping) is detected, replay exits gracefully to prevent endless cycling.

use anyhow::{Context, Result};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// Use s3dlio-oplog types instead of our own
pub use s3dlio_oplog::{OpLogEntry, OpType, OpLogStreamReader};

use crate::workload::{
    StoreCache,  // v0.8.9: Use shared cache type from workload
    get_object_cached_simple, 
    put_object_cached_simple, 
    delete_object_cached_simple,
    list_objects_cached_simple,
    stat_object_cached_simple,
};
use crate::remap::{RemapConfig, RemapEngine};

// =============================================================================
// Backpressure Types (v0.8.9+)
// =============================================================================

/// Replay execution mode for backpressure control
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayMode {
    /// Normal mode: operations issued at recorded timing (scaled by speed)
    Normal,
    /// Best-effort mode: timing abandoned, operations issued as fast as possible
    BestEffort,
}

impl std::fmt::Display for ReplayMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayMode::Normal => write!(f, "normal"),
            ReplayMode::BestEffort => write!(f, "best-effort"),
        }
    }
}

/// Tracks mode transitions to detect flapping (oscillation)
#[derive(Debug)]
pub struct FlappingTracker {
    /// Timestamps of mode transitions within the current minute window
    transitions: Vec<Instant>,
    /// Maximum transitions per minute before triggering graceful exit
    max_per_minute: u32,
}

impl FlappingTracker {
    /// Create a new flapping tracker
    pub fn new(max_per_minute: u32) -> Self {
        Self {
            transitions: Vec::with_capacity(max_per_minute as usize + 1),
            max_per_minute,
        }
    }
    
    /// Record a mode transition and return true if flap limit exceeded
    pub fn record_transition(&mut self) -> bool {
        let now = Instant::now();
        
        // Remove transitions older than 1 minute
        let one_minute_ago = now - Duration::from_secs(60);
        self.transitions.retain(|&t| t > one_minute_ago);
        
        // Add current transition
        self.transitions.push(now);
        
        // Check if we've exceeded the limit
        self.transitions.len() > self.max_per_minute as usize
    }
    
    /// Get the current count of transitions in the last minute
    pub fn transition_count(&self) -> usize {
        self.transitions.len()
    }
}

/// Controls backpressure behavior during replay
#[derive(Debug)]
pub struct BackpressureController {
    /// Current execution mode
    mode: ReplayMode,
    /// Lag threshold to switch to best-effort mode
    lag_threshold: Duration,
    /// Recovery threshold to switch back to normal mode
    recovery_threshold: Duration,
    /// Tracks mode transitions to detect flapping
    flap_tracker: FlappingTracker,
    /// Drain timeout when exiting due to flapping
    drain_timeout: Duration,
    /// Total mode transitions during this replay
    total_transitions: u64,
    /// Whether flap limit was exceeded (triggers graceful exit)
    flap_limit_exceeded: bool,
}

impl BackpressureController {
    /// Create a new backpressure controller with given thresholds
    pub fn new(
        lag_threshold: Duration,
        recovery_threshold: Duration,
        max_flaps_per_minute: u32,
        drain_timeout: Duration,
    ) -> Self {
        // Validate thresholds
        if recovery_threshold >= lag_threshold {
            warn!(
                "recovery_threshold ({:?}) should be less than lag_threshold ({:?}) for proper hysteresis",
                recovery_threshold, lag_threshold
            );
        }
        
        Self {
            mode: ReplayMode::Normal,
            lag_threshold,
            recovery_threshold,
            flap_tracker: FlappingTracker::new(max_flaps_per_minute),
            drain_timeout,
            total_transitions: 0,
            flap_limit_exceeded: false,
        }
    }
    
    /// Create from YAML backpressure config
    pub fn from_config(config: &crate::config::ReplayConfig) -> Self {
        Self::new(
            config.lag_threshold,
            config.recovery_threshold,
            config.max_flaps_per_minute,
            config.drain_timeout,
        )
    }
    
    /// Create with default settings
    pub fn default_controller() -> Self {
        Self::new(
            crate::constants::DEFAULT_REPLAY_LAG_THRESHOLD,
            crate::constants::DEFAULT_REPLAY_RECOVERY_THRESHOLD,
            crate::constants::DEFAULT_REPLAY_MAX_FLAPS_PER_MINUTE,
            crate::constants::DEFAULT_REPLAY_DRAIN_TIMEOUT,
        )
    }
    
    /// Update mode based on current lag, returns true if should continue replay
    pub fn update(&mut self, current_lag: Duration) -> bool {
        let old_mode = self.mode;
        
        match self.mode {
            ReplayMode::Normal => {
                if current_lag > self.lag_threshold {
                    self.mode = ReplayMode::BestEffort;
                    self.total_transitions += 1;
                    
                    info!(
                        "Backpressure: switching to best-effort mode (lag={:?} > threshold={:?})",
                        current_lag, self.lag_threshold
                    );
                    
                    if self.flap_tracker.record_transition() {
                        warn!(
                            "Backpressure: flap limit exceeded ({} transitions/minute), initiating graceful exit",
                            self.flap_tracker.transition_count()
                        );
                        self.flap_limit_exceeded = true;
                        return false; // Stop replay
                    }
                }
            }
            ReplayMode::BestEffort => {
                if current_lag < self.recovery_threshold {
                    self.mode = ReplayMode::Normal;
                    self.total_transitions += 1;
                    
                    info!(
                        "Backpressure: recovering to normal mode (lag={:?} < threshold={:?})",
                        current_lag, self.recovery_threshold
                    );
                    
                    if self.flap_tracker.record_transition() {
                        warn!(
                            "Backpressure: flap limit exceeded ({} transitions/minute), initiating graceful exit",
                            self.flap_tracker.transition_count()
                        );
                        self.flap_limit_exceeded = true;
                        return false; // Stop replay
                    }
                }
            }
        }
        
        // Log mode changes
        if old_mode != self.mode {
            debug!(
                "Backpressure mode changed: {} -> {} (total transitions: {})",
                old_mode, self.mode, self.total_transitions
            );
        }
        
        true // Continue replay
    }
    
    /// Get current replay mode
    pub fn mode(&self) -> ReplayMode {
        self.mode
    }
    
    /// Check if flap limit was exceeded
    pub fn is_flap_limited(&self) -> bool {
        self.flap_limit_exceeded
    }
    
    /// Get drain timeout for graceful exit
    pub fn drain_timeout(&self) -> Duration {
        self.drain_timeout
    }
    
    /// Get total transitions during replay
    pub fn total_transitions(&self) -> u64 {
        self.total_transitions
    }
}

// =============================================================================
// Replay Runtime Configuration
// =============================================================================

/// Replay runtime configuration (distinct from YAML ReplayConfig)
///
/// This struct configures a specific replay execution, while `config::ReplayConfig`
/// controls YAML-based backpressure settings.
#[derive(Debug, Clone)]
pub struct ReplayRunConfig {
    pub op_log_path: PathBuf,
    pub target_uri: Option<String>,
    pub speed: f64,
    pub continue_on_error: bool,
    /// Maximum concurrent operations (to prevent unbounded task growth)
    pub max_concurrent: Option<usize>,
    /// Optional remap configuration for advanced URI transformations
    pub remap_config: Option<RemapConfig>,
    /// Optional backpressure configuration (v0.8.9+)
    /// If None, uses default backpressure settings
    pub backpressure: Option<crate::config::ReplayConfig>,
}

impl Default for ReplayRunConfig {
    fn default() -> Self {
        Self {
            op_log_path: PathBuf::new(),
            target_uri: None,
            speed: 1.0,
            continue_on_error: false,
            max_concurrent: Some(1000), // Reasonable default to prevent memory issues
            remap_config: None,
            backpressure: None, // Use default backpressure settings
        }
    }
}

/// Statistics from replay execution
#[derive(Debug, Default)]
pub struct ReplayStats {
    pub total_operations: u64,
    pub completed_operations: u64,
    pub failed_operations: u64,
    pub skipped_operations: u64,
    /// Backpressure statistics (v0.8.9+)
    pub mode_transitions: u64,
    /// Whether replay exited due to flap limit
    pub flap_exit: bool,
    /// Peak lag observed during replay
    pub peak_lag: Duration,
    /// Time spent in best-effort mode
    pub best_effort_time: Duration,
}

/// Main replay orchestrator with streaming op-log processing
///
/// This version uses OpLogStreamReader for constant memory usage (~1.5 MB)
/// regardless of op-log file size, supporting multi-GB operation logs.
///
/// # Memory Efficiency
///
/// - **Old (v0.4.0)**: Loads entire op-log into Vec (e.g., 1M ops = ~100 MB)
/// - **New (v0.5.0)**: Streams operations (constant ~1.5 MB via s3dlio-oplog)
///
/// # Timing Model
///
/// Uses on-demand spawning with absolute timeline scheduling:
/// 1. First operation becomes time=0 (epoch)
/// 2. Each subsequent operation scheduled at: epoch + (op.start - first.start) / speed
/// 3. Tasks spawned as we stream, with max_concurrent limit to prevent unbounded growth
///
/// # Backpressure (v0.8.9+)
///
/// When replay falls behind the recorded timing:
/// 1. If lag > lag_threshold (default 5s), switch to best-effort mode
/// 2. If lag < recovery_threshold (default 1s), recover to normal mode
/// 3. If mode flaps > max_flaps_per_minute (default 3), exit gracefully
///
/// # Example
///
/// ```no_run
/// use sai3_bench::replay_streaming::{ReplayRunConfig, replay_workload_streaming};
/// use std::path::PathBuf;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), anyhow::Error> {
/// let config = ReplayRunConfig {
///     op_log_path: PathBuf::from("large_workload.tsv.zst"),
///     target_uri: Some("s3://test-bucket/replay/".to_string()),
///     speed: 2.0,  // 2x faster
///     continue_on_error: true,
///     max_concurrent: Some(500),
///     remap_config: None,
///     backpressure: None, // Use default settings
/// };
///
/// let stats = replay_workload_streaming(config).await?;
/// println!("Completed: {}/{}", stats.completed_operations, stats.total_operations);
/// # Ok(())
/// # }
/// ```
pub async fn replay_workload_streaming(config: ReplayRunConfig) -> Result<ReplayStats> {
    info!("Starting streaming replay with config: {:?}", config);

    // Create streaming reader (constant memory)
    let stream = OpLogStreamReader::from_file(&config.op_log_path)
        .context("Failed to open streaming op-log reader")?;

    let mut stats = ReplayStats::default();
    let mut tasks = FuturesUnordered::new();
    let max_concurrent = config.max_concurrent.unwrap_or(1000);
    
    // v0.8.9: Create backpressure controller
    let mut backpressure = match &config.backpressure {
        Some(bp_config) => BackpressureController::from_config(bp_config),
        None => BackpressureController::default_controller(),
    };
    
    // Track time spent in best-effort mode
    let mut best_effort_start: Option<Instant> = None;
    
    // v0.8.9: Create shared store cache to avoid per-operation store creation
    let store_cache: StoreCache = Arc::new(std::sync::Mutex::new(HashMap::new()));

    // First pass: Get the first operation to establish epoch
    let mut stream_iter = stream;
    let first_entry = match stream_iter.next() {
        Some(Ok(entry)) => entry,
        Some(Err(e)) => return Err(e).context("Failed to parse first op-log entry"),
        None => {
            info!("Empty op-log file");
            return Ok(stats);
        }
    };

    let first_time = first_entry.start;
    let replay_epoch = Instant::now();
    
    info!(
        "Replay epoch established. First operation: {:?} at {}",
        first_entry.op, first_entry.start
    );

    // Process first entry
    stats.total_operations += 1;
    let task = spawn_operation(
        first_entry,
        replay_epoch,
        Duration::ZERO, // First operation has no delay
        backpressure.mode(),
        config.clone(),
        store_cache.clone(),
    );
    tasks.push(task);

    // Track previous timestamp for order validation
    let mut prev_time = first_time;

    // Stream remaining operations
    for entry_result in stream_iter {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to parse op-log entry: {}", e);
                stats.skipped_operations += 1;
                if !config.continue_on_error {
                    return Err(e).context("Failed to parse op-log entry");
                }
                continue;
            }
        };

        stats.total_operations += 1;

        // Validate chronological order
        if entry.start < prev_time {
            let time_delta = prev_time.signed_duration_since(entry.start);
            warn!(
                "Out-of-order op-log entry detected during replay: operation #{} has start={}, previous={} (went back by {:?})",
                stats.total_operations,
                entry.start,
                prev_time,
                time_delta
            );
            // Continue anyway - we'll execute immediately (delay will be negative/zero)
        }
        prev_time = entry.start;

        // Calculate absolute delay from first operation
        let elapsed = entry.start.signed_duration_since(first_time);
        let delay = match elapsed.to_std() {
            Ok(d) => Duration::from_secs_f64(d.as_secs_f64() / config.speed),
            Err(_) => {
                warn!("Negative timestamp offset detected, using zero delay");
                Duration::ZERO
            }
        };

        // v0.8.9: Calculate current lag and update backpressure
        let now = Instant::now();
        let scheduled_time = replay_epoch + delay;
        let current_lag = if now > scheduled_time {
            now - scheduled_time
        } else {
            Duration::ZERO
        };
        
        // Track peak lag
        if current_lag > stats.peak_lag {
            stats.peak_lag = current_lag;
        }
        
        // Update backpressure mode based on lag
        let prev_mode = backpressure.mode();
        if !backpressure.update(current_lag) {
            // Flap limit exceeded - initiate graceful exit
            warn!(
                "Backpressure: flap limit exceeded after {} ops, draining {} in-flight tasks (timeout: {:?})",
                stats.total_operations,
                tasks.len(),
                backpressure.drain_timeout()
            );
            
            // Drain with timeout
            let drain_start = Instant::now();
            while !tasks.is_empty() && drain_start.elapsed() < backpressure.drain_timeout() {
                if let Ok(Some(result)) = tokio::time::timeout(
                    Duration::from_millis(100),
                    tasks.next()
                ).await {
                    handle_task_result(result, &mut stats, true)?; // Always continue on error during drain
                }
            }
            
            if !tasks.is_empty() {
                warn!(
                    "Backpressure: drain timeout reached, {} tasks still in-flight (abandoned)",
                    tasks.len()
                );
            }
            
            stats.flap_exit = true;
            break;
        }
        
        // Track best-effort mode time
        let current_mode = backpressure.mode();
        if prev_mode != current_mode {
            match current_mode {
                ReplayMode::BestEffort => {
                    best_effort_start = Some(Instant::now());
                }
                ReplayMode::Normal => {
                    if let Some(start) = best_effort_start.take() {
                        stats.best_effort_time += start.elapsed();
                    }
                }
            }
        }

        // Spawn operation with current mode
        let task = spawn_operation(
            entry, 
            replay_epoch, 
            delay, 
            backpressure.mode(),
            config.clone(), 
            store_cache.clone()
        );
        tasks.push(task);

        // Poll completed tasks to prevent unbounded growth
        while tasks.len() >= max_concurrent {
            if let Some(result) = tasks.next().await {
                handle_task_result(result, &mut stats, config.continue_on_error)?;
            }
        }

        // Log progress periodically
        if stats.total_operations % 1000 == 0 {
            debug!(
                "Progress: {} operations processed, {} tasks active, mode={}, lag={:?}",
                stats.total_operations,
                tasks.len(),
                backpressure.mode(),
                current_lag
            );
        }
    }
    
    // Finalize best-effort time if still in that mode
    if let Some(start) = best_effort_start {
        stats.best_effort_time += start.elapsed();
    }

    info!(
        "Finished streaming {} operations, waiting for {} remaining tasks",
        stats.total_operations,
        tasks.len()
    );

    // Wait for all remaining tasks to complete
    while let Some(result) = tasks.next().await {
        handle_task_result(result, &mut stats, config.continue_on_error)?;
    }
    
    // Record backpressure stats
    stats.mode_transitions = backpressure.total_transitions();

    info!(
        "Replay complete: {} total, {} completed, {} failed, {} skipped",
        stats.total_operations,
        stats.completed_operations,
        stats.failed_operations,
        stats.skipped_operations
    );
    
    if stats.mode_transitions > 0 || stats.peak_lag > Duration::ZERO {
        info!(
            "Backpressure stats: {} mode transitions, peak_lag={:?}, best_effort_time={:?}, flap_exit={}",
            stats.mode_transitions,
            stats.peak_lag,
            stats.best_effort_time,
            stats.flap_exit
        );
    }

    Ok(stats)
}

/// Spawn a single operation at its scheduled time
fn spawn_operation(
    entry: OpLogEntry,
    replay_epoch: Instant,
    delay: Duration,
    mode: ReplayMode,
    config: ReplayRunConfig,
    store_cache: StoreCache,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        // Only apply timing in Normal mode
        if mode == ReplayMode::Normal {
            // Calculate absolute target time
            let target_time = replay_epoch + delay;
            let now = Instant::now();

            // Sleep until target time (microsecond precision via std::thread::sleep)
            if target_time > now {
                let sleep_dur = target_time - now;
                tokio::task::spawn_blocking(move || {
                    std::thread::sleep(sleep_dur);
                })
                .await?;
            }
        }
        // In BestEffort mode, skip timing and execute immediately

        // Apply URI transformation (priority: remap > simple retargeting)
        let uri = if let Some(ref remap_config) = config.remap_config {
            // Advanced remapping with rules
            let engine = RemapEngine::new(remap_config.clone());
            let original_uri = format!("{}{}", entry.endpoint, entry.file);
            engine.remap(&original_uri)
                .context("Failed to apply remap rules")?
        } else if let Some(ref target) = config.target_uri {
            // Simple 1→1 retargeting (legacy behavior)
            translate_uri(&entry.file, &entry.endpoint, target)?
        } else {
            // No transformation
            format!("{}{}", entry.endpoint, entry.file)
        };

        debug!("Executing {:?} on {}", entry.op, uri);

        // Execute operation with cached store (v0.8.9: efficient store reuse)
        execute_operation(&entry, &uri, &store_cache).await
    })
}

/// Handle task result and update statistics
fn handle_task_result(
    result: Result<Result<()>, tokio::task::JoinError>,
    stats: &mut ReplayStats,
    continue_on_error: bool,
) -> Result<()> {
    match result {
        Ok(Ok(())) => {
            stats.completed_operations += 1;
        }
        Ok(Err(e)) => {
            stats.failed_operations += 1;
            if continue_on_error {
                warn!("Operation failed (continuing): {}", e);
            } else {
                return Err(e).context("Operation failed");
            }
        }
        Err(e) => {
            stats.failed_operations += 1;
            if continue_on_error {
                warn!("Task panicked (continuing): {}", e);
            } else {
                return Err(e.into());
            }
        }
    }
    Ok(())
}

/// Execute a single operation using cached store (v0.8.9: efficient store reuse)
async fn execute_operation(entry: &OpLogEntry, uri: &str, cache: &StoreCache) -> Result<()> {
    match entry.op {
        OpType::GET => {
            get_object_cached_simple(uri, cache).await?;
        }
        OpType::PUT => {
            // Generate data with s3dlio (dedup=1, compress=1 for random)
            // OPTIMIZED v0.8.20+: Use cached generator pool for 50+ GB/s
            let data = crate::data_gen_pool::generate_data_optimized(entry.bytes as usize, 1, 1);
            put_object_cached_simple(uri, &data, cache).await?;
        }
        OpType::DELETE => {
            delete_object_cached_simple(uri, cache).await?;
        }
        OpType::LIST => {
            list_objects_cached_simple(uri, cache).await?;
        }
        OpType::STAT => {
            stat_object_cached_simple(uri, cache).await?;
        }
    }
    Ok(())
}

/// Simple 1:1 URI translation from original endpoint to target
fn translate_uri(file: &str, endpoint: &str, target: &str) -> Result<String> {
    // Remove endpoint prefix from file path
    let relative = file.strip_prefix(endpoint).unwrap_or(file);
    let clean = relative.trim_start_matches('/');

    // Construct new URI with target
    let target_clean = target.trim_end_matches('/');
    Ok(format!("{}/{}", target_clean, clean))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_uri() {
        let file = "/bucket/data/file.bin";
        let endpoint = "/bucket/";
        let target = "s3://newbucket";

        let result = translate_uri(file, endpoint, target).unwrap();
        assert_eq!(result, "s3://newbucket/data/file.bin");
    }

    #[test]
    fn test_translate_uri_with_scheme() {
        let file = "file:///tmp/test/data.bin";
        let endpoint = "file://";
        let target = "s3://bucket/prefix";

        let result = translate_uri(file, endpoint, target).unwrap();
        assert_eq!(result, "s3://bucket/prefix/tmp/test/data.bin");
    }
    
    // ==========================================================================
    // Backpressure Tests (v0.8.9+)
    // ==========================================================================
    
    #[test]
    fn test_replay_mode_display() {
        assert_eq!(format!("{}", ReplayMode::Normal), "normal");
        assert_eq!(format!("{}", ReplayMode::BestEffort), "best-effort");
    }
    
    #[test]
    fn test_replay_mode_equality() {
        assert_eq!(ReplayMode::Normal, ReplayMode::Normal);
        assert_eq!(ReplayMode::BestEffort, ReplayMode::BestEffort);
        assert_ne!(ReplayMode::Normal, ReplayMode::BestEffort);
    }
    
    #[test]
    fn test_flapping_tracker_no_flapping() {
        let mut tracker = FlappingTracker::new(3);
        
        // First transition should be OK
        assert!(!tracker.record_transition());
        assert_eq!(tracker.transition_count(), 1);
        
        // Second transition should be OK
        assert!(!tracker.record_transition());
        assert_eq!(tracker.transition_count(), 2);
        
        // Third transition should be OK
        assert!(!tracker.record_transition());
        assert_eq!(tracker.transition_count(), 3);
    }
    
    #[test]
    fn test_flapping_tracker_flap_limit() {
        let mut tracker = FlappingTracker::new(3);
        
        // 3 transitions should be OK
        assert!(!tracker.record_transition());
        assert!(!tracker.record_transition());
        assert!(!tracker.record_transition());
        
        // 4th transition should trigger flap limit
        assert!(tracker.record_transition());
        assert_eq!(tracker.transition_count(), 4);
    }
    
    #[test]
    fn test_backpressure_controller_initial_state() {
        let controller = BackpressureController::default_controller();
        assert_eq!(controller.mode(), ReplayMode::Normal);
        assert!(!controller.is_flap_limited());
        assert_eq!(controller.total_transitions(), 0);
    }
    
    #[test]
    fn test_backpressure_controller_switch_to_best_effort() {
        let mut controller = BackpressureController::new(
            Duration::from_secs(5),  // lag threshold
            Duration::from_secs(1),  // recovery threshold
            3,                        // max flaps
            Duration::from_secs(10), // drain timeout
        );
        
        // Low lag - should stay in Normal mode
        assert!(controller.update(Duration::from_secs(2)));
        assert_eq!(controller.mode(), ReplayMode::Normal);
        assert_eq!(controller.total_transitions(), 0);
        
        // High lag - should switch to BestEffort
        assert!(controller.update(Duration::from_secs(6)));
        assert_eq!(controller.mode(), ReplayMode::BestEffort);
        assert_eq!(controller.total_transitions(), 1);
    }
    
    #[test]
    fn test_backpressure_controller_recovery() {
        let mut controller = BackpressureController::new(
            Duration::from_secs(5),
            Duration::from_secs(1),
            10, // High flap limit so we don't trigger it
            Duration::from_secs(10),
        );
        
        // Switch to BestEffort
        controller.update(Duration::from_secs(6));
        assert_eq!(controller.mode(), ReplayMode::BestEffort);
        
        // Still above recovery threshold - stay in BestEffort
        controller.update(Duration::from_secs(2));
        assert_eq!(controller.mode(), ReplayMode::BestEffort);
        
        // Below recovery threshold - switch back to Normal
        controller.update(Duration::from_millis(500));
        assert_eq!(controller.mode(), ReplayMode::Normal);
        assert_eq!(controller.total_transitions(), 2);
    }
    
    #[test]
    fn test_backpressure_controller_flap_exit() {
        let mut controller = BackpressureController::new(
            Duration::from_secs(5),
            Duration::from_secs(1),
            2, // Only allow 2 transitions per minute
            Duration::from_secs(10),
        );
        
        // First cycle: Normal -> BestEffort
        assert!(controller.update(Duration::from_secs(6)));
        assert_eq!(controller.mode(), ReplayMode::BestEffort);
        
        // Second transition: BestEffort -> Normal
        assert!(controller.update(Duration::from_millis(500)));
        assert_eq!(controller.mode(), ReplayMode::Normal);
        
        // Third transition should trigger flap exit
        let should_continue = controller.update(Duration::from_secs(6));
        assert!(!should_continue);
        assert!(controller.is_flap_limited());
    }
    
    #[test]
    fn test_backpressure_controller_from_config() {
        let config = crate::config::ReplayConfig {
            lag_threshold: Duration::from_secs(10),
            recovery_threshold: Duration::from_secs(2),
            max_flaps_per_minute: 5,
            drain_timeout: Duration::from_secs(30),
            max_concurrent: 500,
        };
        
        let controller = BackpressureController::from_config(&config);
        assert_eq!(controller.drain_timeout(), Duration::from_secs(30));
        assert_eq!(controller.mode(), ReplayMode::Normal);
    }
    
    #[test]
    fn test_replay_stats_default() {
        let stats = ReplayStats::default();
        assert_eq!(stats.total_operations, 0);
        assert_eq!(stats.completed_operations, 0);
        assert_eq!(stats.failed_operations, 0);
        assert_eq!(stats.skipped_operations, 0);
        assert_eq!(stats.mode_transitions, 0);
        assert!(!stats.flap_exit);
        assert_eq!(stats.peak_lag, Duration::ZERO);
        assert_eq!(stats.best_effort_time, Duration::ZERO);
    }
    
    #[test]
    fn test_replay_run_config_default() {
        let config = ReplayRunConfig::default();
        assert_eq!(config.speed, 1.0);
        assert!(!config.continue_on_error);
        assert_eq!(config.max_concurrent, Some(1000));
        assert!(config.remap_config.is_none());
        assert!(config.backpressure.is_none());
    }
}
