// src/prepare.rs
//! Prepare phase implementation for object pre-population and directory tree creation
//!
//! This module handles the "prepare" phase of benchmark execution, including:
//! - Object creation with configurable size, dedup, and compression
//! - Sequential vs parallel execution strategies
//! - Directory tree creation for structured file access patterns
//! - Object cleanup after benchmarks
//!
//! Separated from workload.rs in v0.7.2 to improve code organization as the file
//! grew beyond 2500 lines.

use anyhow::{anyhow, Context, Result};
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tracing::{info, warn, debug};

use crate::config::{FillPattern, PrepareConfig, PrepareStrategy};
use crate::directory_tree::TreeManifest;
use crate::live_stats::{LiveStatsTracker, WorkloadStage};
use crate::size_generator::SizeGenerator;
use crate::workload::MultiEndpointCache;
use crate::constants::{
    DEFAULT_PREPARE_MAX_ERRORS, 
    DEFAULT_PREPARE_MAX_CONSECUTIVE_ERRORS,
    DEFAULT_LISTING_MAX_ERRORS,
    DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS,
    LISTING_PROGRESS_INTERVAL,
};
use crate::workload::{RetryConfig, RetryResult, retry_with_backoff};

// Re-export for backward compatibility (so workload.rs can use via workload::PreparedObject)
pub use crate::workload::{create_store_for_uri, detect_pool_requirements};

/// Error tracking for prepare phase resilience (v0.8.13)
/// 
/// Tracks errors during object creation to implement configurable thresholds:
/// - Total error count (abort if exceeded)
/// - Consecutive error count (abort if backend seems completely down)
/// - Failed objects list for potential retry or reporting
#[derive(Clone)]
pub struct PrepareErrorTracker {
    total_errors: Arc<AtomicU64>,
    consecutive_errors: Arc<AtomicU64>,
    max_total_errors: u64,
    max_consecutive_errors: u64,
    failed_objects: Arc<std::sync::Mutex<Vec<PrepareFailure>>>,
}

/// Record of a failed prepare operation
#[derive(Debug, Clone)]
pub struct PrepareFailure {
    pub uri: String,
    pub size: u64,
    pub error: String,
    pub timestamp: Instant,
}

impl Default for PrepareErrorTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl PrepareErrorTracker {
    pub fn new() -> Self {
        Self::with_thresholds(DEFAULT_PREPARE_MAX_ERRORS, DEFAULT_PREPARE_MAX_CONSECUTIVE_ERRORS)
    }
    
    pub fn with_thresholds(max_total: u64, max_consecutive: u64) -> Self {
        Self {
            total_errors: Arc::new(AtomicU64::new(0)),
            consecutive_errors: Arc::new(AtomicU64::new(0)),
            max_total_errors: max_total,
            max_consecutive_errors: max_consecutive,
            failed_objects: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
    
    /// Record a successful operation (resets consecutive error counter)
    pub fn record_success(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
    }
    
    /// Record an error and check if thresholds are exceeded
    /// Returns: (should_abort, total_errors, consecutive_errors)
    pub fn record_error(&self, uri: &str, size: u64, error: &str) -> (bool, u64, u64) {
        let total = self.total_errors.fetch_add(1, Ordering::Relaxed) + 1;
        let consecutive = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
        
        // Record failure for potential retry/reporting
        {
            let mut failures = self.failed_objects.lock().unwrap();
            failures.push(PrepareFailure {
                uri: uri.to_string(),
                size,
                error: error.to_string(),
                timestamp: Instant::now(),
            });
        }
        
        let should_abort = total >= self.max_total_errors || consecutive >= self.max_consecutive_errors;
        
        (should_abort, total, consecutive)
    }
    
    pub fn get_stats(&self) -> (u64, u64) {
        let total = self.total_errors.load(Ordering::Relaxed);
        let consecutive = self.consecutive_errors.load(Ordering::Relaxed);
        (total, consecutive)
    }
    
    pub fn get_failures(&self) -> Vec<PrepareFailure> {
        self.failed_objects.lock().unwrap().clone()
    }
    
    pub fn total_errors(&self) -> u64 {
        self.total_errors.load(Ordering::Relaxed)
    }
}

/// Information about a prepared object
#[derive(Debug, Clone)]
pub struct PreparedObject {
    pub uri: String,
    pub size: u64,
    pub created: bool,  // True if we created it, false if it already existed
}

/// Metrics collected during prepare phase
/// 
/// Tracks PUT operations and directory creation with full HDR histogram support.
/// Follows same structure as workload metrics (OpAgg + OpHists + SizeBins).
#[derive(Debug, Clone)]
pub struct PrepareMetrics {
    /// Wall clock time for entire prepare phase (seconds)
    pub wall_seconds: f64,
    
    /// PUT operation aggregate metrics
    pub put: crate::workload::OpAgg,
    
    /// PUT operation size bins
    pub put_bins: crate::workload::SizeBins,
    
    /// PUT operation HDR histograms (9 size buckets)
    pub put_hists: crate::metrics::OpHists,
    
    /// Directory operations (mkdir) - treated as metadata ops
    pub mkdir: crate::workload::OpAgg,
    
    /// Number of directories created (not tracked per-size, always zero-byte ops)
    pub mkdir_count: u64,
    
    /// Total objects created (excludes pre-existing objects)
    pub objects_created: u64,
    
    /// Total objects that already existed (skipped)
    pub objects_existed: u64,
    
    /// Prepare strategy used
    pub strategy: PrepareStrategy,
    
    /// v0.8.23: Per-endpoint statistics (if multi-endpoint was used)
    pub endpoint_stats: Option<Vec<crate::workload::EndpointStatsSnapshot>>,
}

impl Default for PrepareMetrics {
    fn default() -> Self {
        Self {
            wall_seconds: 0.0,
            put: crate::workload::OpAgg::default(),
            put_bins: crate::workload::SizeBins::default(),
            put_hists: crate::metrics::OpHists::new(),
            mkdir: crate::workload::OpAgg::default(),
            mkdir_count: 0,
            objects_created: 0,
            objects_existed: 0,
            strategy: PrepareStrategy::Sequential,
            endpoint_stats: None,
        }
    }
}

/// Helper to compute OpAgg from histogram data
fn compute_op_agg(hists: &crate::metrics::OpHists, total_bytes: u64, total_ops: u64) -> crate::workload::OpAgg {
    if total_ops == 0 {
        return crate::workload::OpAgg::default();
    }
    
    // Merge all size bucket histograms into one combined histogram
    let combined = hists.combined_histogram();
    
    crate::workload::OpAgg {
        bytes: total_bytes,
        ops: total_ops,
        mean_us: combined.mean() as u64,
        p50_us: combined.value_at_quantile(0.50),
        p95_us: combined.value_at_quantile(0.95),
        p99_us: combined.value_at_quantile(0.99),
    }
}

// ============================================================================
// Distributed Listing with Progress (v0.8.14)
// ============================================================================

/// Error tracking for listing phase resilience (v0.8.14)
/// 
/// Similar to PrepareErrorTracker, but for LIST operations.
/// LIST can fail on transient network issues; we don't want to abort immediately.
#[derive(Clone)]
pub struct ListingErrorTracker {
    total_errors: Arc<AtomicU64>,
    consecutive_errors: Arc<AtomicU64>,
    max_total_errors: u64,
    max_consecutive_errors: u64,
    error_messages: Arc<std::sync::Mutex<Vec<String>>>,
}

impl ListingErrorTracker {
    pub fn new() -> Self {
        Self::with_thresholds(DEFAULT_LISTING_MAX_ERRORS, DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS)
    }
    
    pub fn with_thresholds(max_total: u64, max_consecutive: u64) -> Self {
        Self {
            total_errors: Arc::new(AtomicU64::new(0)),
            consecutive_errors: Arc::new(AtomicU64::new(0)),
            max_total_errors: max_total,
            max_consecutive_errors: max_consecutive,
            error_messages: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
    
    /// Record a successful item retrieval (resets consecutive error counter)
    pub fn record_success(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
    }
    
    /// Record an error and check if thresholds are exceeded
    /// Returns: (should_abort, total_errors, consecutive_errors)
    pub fn record_error(&self, error_msg: &str) -> (bool, u64, u64) {
        let total = self.total_errors.fetch_add(1, Ordering::Relaxed) + 1;
        let consecutive = self.consecutive_errors.fetch_add(1, Ordering::Relaxed) + 1;
        
        // Store first 20 error messages for debugging
        {
            let mut errors = self.error_messages.lock().unwrap();
            if errors.len() < 20 {
                errors.push(error_msg.to_string());
            }
        }
        
        let should_abort = total >= self.max_total_errors || consecutive >= self.max_consecutive_errors;
        
        (should_abort, total, consecutive)
    }
    
    pub fn get_stats(&self) -> (u64, u64) {
        (self.total_errors.load(Ordering::Relaxed), 
         self.consecutive_errors.load(Ordering::Relaxed))
    }
    
    pub fn total_errors(&self) -> u64 {
        self.total_errors.load(Ordering::Relaxed)
    }
    
    pub fn get_error_messages(&self) -> Vec<String> {
        self.error_messages.lock().unwrap().clone()
    }
}

impl Default for ListingErrorTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of listing operation with parsed file indices and error tracking
#[derive(Debug, Default)]
pub struct ListingResult {
    /// Total files found
    pub file_count: u64,
    /// Parsed file indices (for gap-aware creation)
    pub indices: HashSet<u64>,
    /// Directories listed (for distributed mode)
    pub dirs_listed: u64,
    /// Total listing errors encountered (non-fatal)
    pub errors_encountered: u64,
    /// Whether listing was aborted due to error threshold
    pub aborted: bool,
    /// Duration of listing phase
    pub elapsed_secs: f64,
}

/// List existing objects with streaming progress updates (v0.8.14)
/// 
/// This function replaces the blocking `store.list()` call with a streaming
/// implementation that provides progress updates during long-running list operations.
/// 
/// For directory tree mode with multiple agents:
/// - Each agent lists only its assigned top-level directories
/// - Progress is reported via LiveStatsTracker in the "Listing" stage
/// - Uses depth-first listing per directory for better distribution
/// 
/// # Error Handling (v0.8.14)
/// - Individual LIST errors are tracked and logged
/// - Consecutive errors reset on success (transient network issues)
/// - Aborts if total errors exceed DEFAULT_LISTING_MAX_ERRORS (50)
/// - Aborts if consecutive errors exceed DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS (5)
/// 
/// # Arguments
/// * `store` - Object store to list from
/// * `base_uri` - Base URI to list (e.g., "gs://bucket/prefix/")
/// * `tree_manifest` - Optional tree manifest for distributed directory assignment
/// * `agent_id` - 0-based agent index
/// * `num_agents` - Total number of agents
/// * `live_stats_tracker` - Optional tracker for progress updates
/// * `expected_total` - Expected total files (for progress percentage)
/// 
/// # Returns
/// ListingResult with file count, parsed indices, and error statistics
pub async fn list_existing_objects_distributed(
    store: &dyn s3dlio::object_store::ObjectStore,
    base_uri: &str,
    tree_manifest: Option<&TreeManifest>,
    agent_id: usize,
    num_agents: usize,
    live_stats_tracker: Option<&Arc<LiveStatsTracker>>,
    expected_total: u64,
) -> Result<ListingResult> {
    let start_time = Instant::now();
    
    // Error tracker for this listing operation
    let error_tracker = ListingErrorTracker::new();
    
    // Set the Listing stage if we have a stats tracker
    if let Some(tracker) = live_stats_tracker {
        tracker.set_stage(WorkloadStage::Listing, expected_total);
        tracker.set_stage_with_name(WorkloadStage::Listing, "Scanning existing files", expected_total);
    }
    
    let list_base = if base_uri.ends_with('/') {
        base_uri.to_string()
    } else {
        format!("{}/", base_uri)
    };
    
    let mut result = ListingResult::default();
    let files_found = Arc::new(AtomicU64::new(0));
    let last_report = Arc::new(AtomicU64::new(0));
    
    // Helper closure to process a single list item with error tracking
    let process_list_item = |path: &str, result: &mut ListingResult| {
        result.file_count += 1;
        
        // Parse file index from filename
        if let Some(filename) = path.rsplit('/').next() {
            // Try file_NNNN.dat format (tree mode)
            if let Some(idx_str) = filename.strip_prefix("file_")
                .and_then(|s| s.strip_suffix(".dat")) 
            {
                if let Ok(idx) = idx_str.parse::<u64>() {
                    result.indices.insert(idx);
                }
            }
            // Try prepared-NNNN.dat format (flat mode)
            else if let Some(idx_str) = filename.strip_prefix("prepared-")
                .and_then(|s| s.strip_suffix(".dat")) 
            {
                if let Ok(idx) = idx_str.parse::<u64>() {
                    result.indices.insert(idx);
                }
            }
            // Try deletable-NNNN.dat format (flat mode with separate pools)
            else if let Some(idx_str) = filename.strip_prefix("deletable-")
                .and_then(|s| s.strip_suffix(".dat")) 
            {
                if let Ok(idx) = idx_str.parse::<u64>() {
                    result.indices.insert(idx);
                }
            }
        }
    };
    
    // For distributed tree mode, each agent lists their assigned directories
    if let Some(manifest) = tree_manifest {
        if num_agents > 1 {
            // Get directories assigned to this agent
            let agent_dirs = manifest.get_agent_dirs(agent_id);
            let total_dirs = agent_dirs.len();
            
            if total_dirs == 0 {
                info!("  [Agent {}/{}] No directories assigned for listing", agent_id, num_agents);
                result.elapsed_secs = start_time.elapsed().as_secs_f64();
                return Ok(result);
            }
            
            info!("  [Agent {}/{}] Listing {} directories (of {} total)", 
                agent_id, num_agents, total_dirs, manifest.all_directories.len());
            
            // List each assigned directory
            'dir_loop: for (dir_idx, dir_path) in agent_dirs.iter().enumerate() {
                let dir_uri = format!("{}{}/", list_base, dir_path);
                
                debug!("  [{}/{}] Listing directory: {}", dir_idx + 1, total_dirs, dir_path);
                
                // Use streaming list for this directory
                let mut stream = store.list_stream(&dir_uri, true);
                
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(path) => {
                            // Reset consecutive errors on success
                            error_tracker.record_success();
                            
                            process_list_item(&path, &mut result);
                            files_found.fetch_add(1, Ordering::Relaxed);
                            
                            // Update progress at intervals
                            let current = files_found.load(Ordering::Relaxed);
                            let last = last_report.load(Ordering::Relaxed);
                            if current - last >= LISTING_PROGRESS_INTERVAL {
                                last_report.store(current, Ordering::Relaxed);
                                
                                if let Some(tracker) = live_stats_tracker {
                                    tracker.set_prepare_progress(current, expected_total);
                                }
                                
                                let elapsed = start_time.elapsed().as_secs_f64();
                                let rate = current as f64 / elapsed;
                                let (total_errs, _) = error_tracker.get_stats();
                                if total_errs > 0 {
                                    debug!("  Progress: {} files ({:.0}/s), {} errors - dir {}/{}", 
                                        current, rate, total_errs, dir_idx + 1, total_dirs);
                                } else {
                                    debug!("  Progress: {} files ({:.0}/s) - dir {}/{}", 
                                        current, rate, dir_idx + 1, total_dirs);
                                }
                            }
                        }
                        Err(e) => {
                            let error_msg = format!("{}: {}", dir_uri, e);
                            let (should_abort, total_errs, consecutive_errs) = 
                                error_tracker.record_error(&error_msg);
                            
                            result.errors_encountered = total_errs;
                            
                            if should_abort {
                                warn!("❌ Listing aborted: {} total errors, {} consecutive (thresholds: {}/{})",
                                    total_errs, consecutive_errs,
                                    DEFAULT_LISTING_MAX_ERRORS, DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS);
                                result.aborted = true;
                                break 'dir_loop;
                            } else {
                                debug!("  LIST error (non-fatal {}/{}): {}", 
                                    total_errs, DEFAULT_LISTING_MAX_ERRORS, e);
                            }
                        }
                    }
                }
                
                if !result.aborted {
                    result.dirs_listed += 1;
                }
            }
            
            let elapsed = start_time.elapsed().as_secs_f64();
            result.elapsed_secs = elapsed;
            let rate = if elapsed > 0.0 { result.file_count as f64 / elapsed } else { 0.0 };
            
            if result.errors_encountered > 0 {
                if result.aborted {
                    warn!("  [Agent {}/{}] Listing ABORTED: {} files in {} dirs, {} errors ({:.1}s)",
                        agent_id, num_agents, result.file_count, result.dirs_listed, 
                        result.errors_encountered, elapsed);
                } else {
                    info!("  [Agent {}/{}] Listed {} files in {} dirs, {} errors ({:.1}s, {:.0}/s)", 
                        agent_id, num_agents, result.file_count, result.dirs_listed, 
                        result.errors_encountered, elapsed, rate);
                }
            } else {
                info!("  [Agent {}/{}] Listed {} files in {} dirs ({:.1}s, {:.0} files/s)", 
                    agent_id, num_agents, result.file_count, result.dirs_listed, elapsed, rate);
            }
            
        } else {
            // Single agent: list everything with streaming progress
            info!("  [Directory tree mode] Listing recursively from: {}", list_base);
            
            let mut stream = store.list_stream(&list_base, true);
            
            while let Some(item) = stream.next().await {
                match item {
                    Ok(path) => {
                        error_tracker.record_success();
                        process_list_item(&path, &mut result);
                        files_found.fetch_add(1, Ordering::Relaxed);
                        
                        // Update progress at intervals
                        let current = files_found.load(Ordering::Relaxed);
                        let last = last_report.load(Ordering::Relaxed);
                        if current - last >= LISTING_PROGRESS_INTERVAL {
                            last_report.store(current, Ordering::Relaxed);
                            
                            if let Some(tracker) = live_stats_tracker {
                                tracker.set_prepare_progress(current, expected_total);
                            }
                            
                            let elapsed = start_time.elapsed().as_secs_f64();
                            let rate = current as f64 / elapsed;
                            let (total_errs, _) = error_tracker.get_stats();
                            if total_errs > 0 {
                                info!("  Listing progress: {} files ({:.0}/s), {} errors", 
                                    current, rate, total_errs);
                            } else {
                                info!("  Listing progress: {} files ({:.0}/s)", current, rate);
                            }
                        }
                    }
                    Err(e) => {
                        let error_msg = format!("{}: {}", list_base, e);
                        let (should_abort, total_errs, consecutive_errs) = 
                            error_tracker.record_error(&error_msg);
                        
                        result.errors_encountered = total_errs;
                        
                        if should_abort {
                            warn!("❌ Listing aborted: {} total errors, {} consecutive (thresholds: {}/{})",
                                total_errs, consecutive_errs,
                                DEFAULT_LISTING_MAX_ERRORS, DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS);
                            result.aborted = true;
                            break;
                        } else {
                            debug!("  LIST error (non-fatal {}/{}): {}", 
                                total_errs, DEFAULT_LISTING_MAX_ERRORS, e);
                        }
                    }
                }
            }
            
            let elapsed = start_time.elapsed().as_secs_f64();
            result.elapsed_secs = elapsed;
            let rate = if elapsed > 0.0 { result.file_count as f64 / elapsed } else { 0.0 };
            
            if result.errors_encountered > 0 {
                if result.aborted {
                    warn!("  Listing ABORTED: {} files, {} errors ({:.1}s)",
                        result.file_count, result.errors_encountered, elapsed);
                } else {
                    info!("  Listed {} files, {} non-fatal errors ({:.1}s, {:.0}/s)", 
                        result.file_count, result.errors_encountered, elapsed, rate);
                }
            } else {
                info!("  Listed {} files ({:.1}s, {:.0}/s)", result.file_count, elapsed, rate);
            }
        }
    } else {
        // Non-tree mode: simple streaming list
        info!("  [Flat file mode] Listing from: {}", list_base);
        
        let mut stream = store.list_stream(&list_base, true);
        
        while let Some(item) = stream.next().await {
            match item {
                Ok(path) => {
                    error_tracker.record_success();
                    process_list_item(&path, &mut result);
                    files_found.fetch_add(1, Ordering::Relaxed);
                    
                    // Update progress at intervals
                    let current = files_found.load(Ordering::Relaxed);
                    let last = last_report.load(Ordering::Relaxed);
                    if current - last >= LISTING_PROGRESS_INTERVAL {
                        last_report.store(current, Ordering::Relaxed);
                        
                        if let Some(tracker) = live_stats_tracker {
                            tracker.set_prepare_progress(current, expected_total);
                        }
                        
                        let elapsed = start_time.elapsed().as_secs_f64();
                        let rate = current as f64 / elapsed;
                        let (total_errs, _) = error_tracker.get_stats();
                        if total_errs > 0 {
                            info!("  Listing progress: {} files ({:.0}/s), {} errors", 
                                current, rate, total_errs);
                        } else {
                            info!("  Listing progress: {} files ({:.0}/s)", current, rate);
                        }
                    }
                }
                Err(e) => {
                    let error_msg = format!("{}: {}", list_base, e);
                    let (should_abort, total_errs, consecutive_errs) = 
                        error_tracker.record_error(&error_msg);
                    
                    result.errors_encountered = total_errs;
                    
                    if should_abort {
                        warn!("❌ Listing aborted: {} total errors, {} consecutive (thresholds: {}/{})",
                            total_errs, consecutive_errs,
                            DEFAULT_LISTING_MAX_ERRORS, DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS);
                        result.aborted = true;
                        break;
                    } else {
                        debug!("  LIST error (non-fatal {}/{}): {}", 
                            total_errs, DEFAULT_LISTING_MAX_ERRORS, e);
                    }
                }
            }
        }
        
        let elapsed = start_time.elapsed().as_secs_f64();
        result.elapsed_secs = elapsed;
        let rate = if elapsed > 0.0 { result.file_count as f64 / elapsed } else { 0.0 };
        
        if result.errors_encountered > 0 {
            if result.aborted {
                warn!("  Listing ABORTED: {} files, {} errors ({:.1}s)",
                    result.file_count, result.errors_encountered, elapsed);
            } else {
                info!("  Listed {} files, {} non-fatal errors ({:.1}s, {:.0}/s)", 
                    result.file_count, result.errors_encountered, elapsed, rate);
            }
        } else {
            info!("  Listed {} files ({:.1}s, {:.0}/s)", result.file_count, elapsed, rate);
        }
    }
    
    // Final progress update
    if let Some(tracker) = live_stats_tracker {
        tracker.set_prepare_progress(result.file_count, expected_total);
    }
    
    debug!("  Parsed {} valid file indices from filenames", result.indices.len());
    
    // If aborted, return error so caller knows listing was incomplete
    if result.aborted {
        let error_msgs = error_tracker.get_error_messages();
        let sample_errors = error_msgs.iter().take(3).cloned().collect::<Vec<_>>().join("; ");
        return Err(anyhow!(
            "Listing aborted after {} errors (found {} files so far). Sample errors: {}",
            result.errors_encountered, result.file_count, sample_errors
        ));
    }
    
    Ok(result)
}

/// Execute prepare step: ensure objects exist for testing
/// 
/// v0.5.7+: Automatically creates separate object pools when workload contains
/// both DELETE and (GET|STAT) operations to prevent race conditions:
/// - prepared-NNNN.dat: Readonly pool for GET/STAT operations (never deleted)
/// - deletable-NNNN.dat: Consumable pool for DELETE operations
/// 
/// v0.7.0+: Returns TreeManifest when directory_structure is configured
/// 
/// v0.7.2+: Supports prepare_strategy for sequential vs parallel execution
/// 
/// v0.7.2+: Returns PrepareMetrics with full HDR histogram metrics collection
/// Prepare objects for workload execution
/// 
/// # Arguments
/// * `config` - Prepare configuration (object counts, sizes, etc.)
/// * `workload` - Optional workload operations (used to determine if separate pools are needed)
/// * `live_stats_tracker` - Optional live stats tracker for progress reporting
/// * `concurrency` - Number of parallel workers for object creation
/// * `agent_id` - 0-based agent index (0, 1, 2, ...)
/// * `num_agents` - Total number of agents in distributed execution
/// 
/// # Behavior
/// - If num_agents == 1: Creates all objects (standalone mode)
/// - If num_agents > 1: Each agent creates only its assigned subset using modulo distribution
///   - Agent i creates object j if (j % num_agents == agent_id)
///   - Ensures no overlap and complete coverage across all agents
pub async fn prepare_objects(
    config: &PrepareConfig,
    workload: Option<&[crate::config::WeightedOp]>,
    live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
    multi_endpoint_config: Option<&crate::config::MultiEndpointConfig>,
    multi_ep_cache: &crate::workload::MultiEndpointCache,
    concurrency: usize,
    agent_id: usize,
    num_agents: usize,
) -> Result<(Vec<PreparedObject>, Option<TreeManifest>, PrepareMetrics)> {
    let prepare_start = Instant::now();
    
    // Detect if we need separate readonly and deletable pools
    let (has_delete, has_readonly) = if let Some(wl) = workload {
        detect_pool_requirements(wl)
    } else {
        (false, false)
    };
    
    let needs_separate_pools = has_delete && has_readonly;
    
    if needs_separate_pools {
        info!("Mixed workload detected with DELETE + (GET|STAT): Creating separate readonly and deletable object pools");
    }
    
    // Initialize metrics
    let mut metrics = PrepareMetrics {
        strategy: config.prepare_strategy,
        ..Default::default()
    };
    
    // v0.7.9: Create tree manifest FIRST if directory_structure is configured
    // This way file creation can use proper directory paths
    let tree_manifest = if config.directory_structure.is_some() {
        info!("Creating directory tree structure (agent {}/{})...", agent_id, num_agents);
        
        let base_uri = config.ensure_objects.first()
            .map(|spec| spec.base_uri.as_str())
            .ok_or_else(|| anyhow!("directory_structure requires at least one ensure_objects entry for base_uri"))?;
        
        let manifest = create_tree_manifest_only(config, agent_id, num_agents, base_uri)?;
        
        if num_agents > 1 {
            info!("Tree structure: {} directories, {} files total ({} assigned to this agent)", 
                manifest.total_dirs, manifest.total_files, 
                manifest.get_agent_file_indices(agent_id, num_agents).len());
        } else {
            info!("Tree structure: {} directories, {} files total", 
                manifest.total_dirs, manifest.total_files);
        }
        
        Some(manifest)
    } else {
        None
    };
    
    // Choose execution strategy based on config
    let all_prepared = match config.prepare_strategy {
        PrepareStrategy::Sequential => {
            info!("Using sequential prepare strategy (one size group at a time)");
            prepare_sequential(config, needs_separate_pools, &mut metrics, live_stats_tracker.clone(), multi_endpoint_config, multi_ep_cache, tree_manifest.as_ref(), concurrency, agent_id, num_agents).await?
        }
        PrepareStrategy::Parallel => {
            info!("Using parallel prepare strategy (all sizes interleaved)");
            prepare_parallel(config, needs_separate_pools, &mut metrics, live_stats_tracker.clone(), multi_endpoint_config, multi_ep_cache, tree_manifest.as_ref(), concurrency, agent_id, num_agents).await?
        }
    };
    
    // Create directories if needed (after files exist with correct paths)
    if tree_manifest.is_some() {
        let base_uri = config.ensure_objects.first().unwrap().base_uri.as_str();
        finalize_tree_with_mkdir(config, base_uri, &mut metrics, live_stats_tracker).await?;
    }
    
    // Finalize metrics
    metrics.wall_seconds = prepare_start.elapsed().as_secs_f64();
    metrics.objects_created = all_prepared.iter().filter(|obj| obj.created).count() as u64;
    metrics.objects_existed = all_prepared.iter().filter(|obj| !obj.created).count() as u64;
    
    // v0.8.23: Collect per-endpoint statistics from multi-endpoint stores
    metrics.endpoint_stats = crate::workload::collect_endpoint_stats(&multi_ep_cache);
    
    // Compute aggregates from histograms
    if metrics.put.ops > 0 {
        metrics.put = compute_op_agg(&metrics.put_hists, metrics.put.bytes, metrics.put.ops);
    }
    if metrics.mkdir_count > 0 {
        // For mkdir we don't have histograms (not tracked per-operation currently)
        // Just leave the ops count we accumulated
        metrics.mkdir.ops = metrics.mkdir_count;
    }
    
    info!("Prepare complete: {} objects ready ({} created, {} existed), wall time: {:.2}s", 
        all_prepared.len(), metrics.objects_created, metrics.objects_existed, metrics.wall_seconds);
    
    Ok((all_prepared, tree_manifest, metrics))
}

/// Sequential prepare strategy: Process each ensure_objects entry one at a time
/// This is the original behavior - predictable, separate progress bars per size
#[allow(clippy::too_many_arguments)]
async fn prepare_sequential(
    config: &PrepareConfig,
    needs_separate_pools: bool,
    metrics: &mut PrepareMetrics,
    live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
    multi_endpoint_config: Option<&crate::config::MultiEndpointConfig>,
    multi_ep_cache: &MultiEndpointCache,
    tree_manifest: Option<&TreeManifest>,
    concurrency: usize,
    agent_id: usize,
    num_agents: usize,
) -> Result<Vec<PreparedObject>> {
    let mut all_prepared = Vec::new();
    
    for spec in &config.ensure_objects {
        // Determine which pool(s) to create based on workload requirements
        let pools_to_create = if needs_separate_pools {
            vec![("prepared", true), ("deletable", false)]  // (prefix, is_readonly)
        } else {
            vec![("prepared", false)]  // Single pool (backward compatible)
        };
        
        for (prefix, is_readonly) in pools_to_create {
            let pool_desc = if needs_separate_pools {
                if is_readonly { " (readonly pool for GET/STAT)" } else { " (deletable pool for DELETE)" }
            } else {
                ""
            };
            
            info!("Preparing objects{}: {} at {}", pool_desc, spec.count, spec.base_uri);
            
            // v0.8.22: Multi-endpoint support for prepare phase
            // If use_multi_endpoint=true, create MultiEndpointStore instead of single-endpoint store
            // This distributes object creation across all endpoints for maximum network bandwidth
            // v0.8.23: Use Arc<Box<...>> to allow sharing store across concurrent PUT tasks
            let shared_store: Arc<Box<dyn s3dlio::object_store::ObjectStore>> = if spec.use_multi_endpoint {
                if let Some(multi_ep) = multi_endpoint_config {
                    info!("  ✓ Using multi-endpoint configuration: {} endpoints, {} strategy", 
                          multi_ep.endpoints.len(), multi_ep.strategy);
                    
                    // Create cache key for prepare phase multi-endpoint store
                    let cache_key = format!("prepare_seq:{}:{}:{}",
                        spec.base_uri,
                        multi_ep.strategy,
                        multi_ep.endpoints.join(","));
                    
                    // Create Arc<MultiEndpointStore> - can be used for both operations and stats
                    let arc_multi_store = crate::workload::create_multi_endpoint_store(multi_ep, None, None)?;
                    
                    // Store in multi_ep_cache for stats collection
                    {
                        let mut cache_lock = multi_ep_cache.lock().unwrap();
                        cache_lock.insert(cache_key, Arc::clone(&arc_multi_store));
                    }
                    
                    // Wrap for Arc<Box<dyn ObjectStore>>
                    Arc::new(Box::new(crate::workload::ArcMultiEndpointWrapper(arc_multi_store)) as Box<dyn s3dlio::object_store::ObjectStore>)
                } else {
                    anyhow::bail!("use_multi_endpoint=true but no multi_endpoint configuration provided");
                }
            } else {
                Arc::new(create_store_for_uri(&spec.base_uri)?)
            };
            
            // 1. List existing objects with this prefix (unless skip_verification is enabled)
            // Issue #40: skip_verification config option
            // v0.7.9: If tree manifest exists, files are nested in directories (e.g., scan.d0_w0.dir/file_*.dat)
            // v0.7.9: Parse filenames to extract indices for gap-filling
            let (existing_count, existing_indices) = if config.skip_verification {
                info!("  ⚡ skip_verification enabled - assuming all {} objects exist (skipping LIST)", spec.count);
                (spec.count, HashSet::new())  // Assume all files exist, no gaps
            } else if tree_manifest.is_some() {
                // v0.8.14: Use distributed listing with progress updates
                let listing_result = list_existing_objects_distributed(
                    shared_store.as_ref().as_ref(),
                    &spec.base_uri,
                    tree_manifest,
                    agent_id,
                    num_agents,
                    live_stats_tracker.as_ref(),
                    spec.count,
                ).await.context("Failed to list existing objects")?;
                
                (listing_result.file_count, listing_result.indices)
            } else {
                // Flat file mode: use streaming list with progress
                let list_pattern = if spec.base_uri.ends_with('/') {
                    format!("{}{}-", spec.base_uri, prefix)
                } else {
                    format!("{}/{}-", spec.base_uri, prefix)
                };
                
                info!("  [Flat file mode] Listing with pattern: {}", list_pattern);
                
                // Use streaming list for flat mode too
                let listing_result = list_existing_objects_distributed(
                    shared_store.as_ref().as_ref(),
                    &list_pattern,
                    None,  // No tree manifest for flat mode
                    agent_id,
                    num_agents,
                    live_stats_tracker.as_ref(),
                    spec.count,
                ).await.context("Failed to list existing objects")?;
                
                (listing_result.file_count, listing_result.indices)
            };
            
            info!("  ✓ Found {} existing {} objects (need {})", existing_count, prefix, spec.count);
            
            // 2. Calculate how many to create
            let to_create = if existing_count >= spec.count {
                info!("  Sufficient {} objects already exist", prefix);
                0
            } else {
                spec.count - existing_count
            };
            
            
            // 2.5. Record existing objects if all requirements met
            if to_create == 0 {
                if let Some(manifest) = tree_manifest {
                    // All objects exist - reconstruct PreparedObject entries from manifest
                    let size_spec = spec.get_size_spec();
                    let mut size_generator = SizeGenerator::new(&size_spec)
                        .context("Failed to create size generator")?;
                    
                    for i in 0..spec.count {
                        if let Some(rel_path) = manifest.get_file_path(i as usize) {
                            let uri = if spec.base_uri.ends_with('/') {
                                format!("{}{}", spec.base_uri, rel_path)
                            } else {
                                format!("{}/{}", spec.base_uri, rel_path)
                            };
                            let size = size_generator.generate();
                            all_prepared.push(PreparedObject {
                                uri,
                                size,
                                created: false,  // Existed already
                            });
                        }
                    }
                } else {
                    // Flat mode: all exist
                    let size_spec = spec.get_size_spec();
                    let mut size_generator = SizeGenerator::new(&size_spec)
                        .context("Failed to create size generator")?;
                    
                    for i in 0..spec.count {
                        let key = format!("{}-{:08}.dat", prefix, i);
                        let uri = if spec.base_uri.ends_with('/') {
                            format!("{}{}", spec.base_uri, key)
                        } else {
                            format!("{}/{}", spec.base_uri, key)
                        };
                        let size = size_generator.generate();
                        all_prepared.push(PreparedObject {
                            uri,
                            size,
                            created: false,
                        });
                    }
                }
            }
            
            // 3. Create missing objects
            if to_create > 0 {
                use futures::stream::{FuturesUnordered, StreamExt};
                
                // v0.7.9: Pre-generate ALL sizes with deterministic seeded generator
                // This ensures: (1) deterministic sizes, (2) gap-filling uses correct sizes
                let size_spec = spec.get_size_spec();
                let seed = spec.base_uri.as_bytes().iter().fold(0u64, |acc, &b| acc.wrapping_mul(31).wrapping_add(b as u64));
                let mut size_generator = SizeGenerator::new_with_seed(&size_spec, seed)
                    .context("Failed to create size generator")?;
                
                info!("  [v0.7.9] Pre-generating all {} sizes with seed {} for deterministic gap-filling", spec.count, seed);
                let mut all_sizes: Vec<u64> = Vec::with_capacity(spec.count as usize);
                for i in 0..spec.count {
                    all_sizes.push(size_generator.generate());
                    if i == 0 {
                    }
                }
                
                // Identify missing indices (gaps to fill)
                let mut missing_indices: Vec<u64> = (0..spec.count)
                    .filter(|i| !existing_indices.contains(i))
                    .collect();
                
                // v0.8.7: Filter for distributed prepare
                // Each agent only creates its assigned subset using modulo distribution
                if num_agents > 1 {
                    missing_indices.retain(|&idx| (idx as usize % num_agents) == agent_id);
                    info!("  [Distributed prepare] Agent {}/{} responsible for {} of {} missing objects",
                        agent_id, num_agents, missing_indices.len(), to_create);
                }
                
                // v0.8.7: After filtering, update to_create to reflect actual count for this agent
                let actual_to_create = missing_indices.len() as u64;
                
                info!("  [v0.7.9] Identified {} missing indices (first 10: {:?})", 
                    missing_indices.len(), 
                    &missing_indices[..std::cmp::min(10, missing_indices.len())]);
                
                if actual_to_create != to_create && num_agents == 1 {
                    warn!("  Missing indices count ({}) != to_create ({}) - this indicates detection logic issue",
                        actual_to_create, to_create);
                }
                
                // Use workload concurrency for prepare phase (passed from config)
                // Note: concurrency parameter comes from Config.concurrency
                
                info!("  Creating {} additional {} objects with {} workers (sizes: {}, fill: {:?}, dedup: {}, compress: {})", 
                    actual_to_create, prefix, concurrency, size_generator.description(), spec.fill, 
                    spec.dedup_factor, spec.compress_factor);
                
                // v0.7.9: Set prepare phase progress in live stats tracker
                if let Some(ref tracker) = live_stats_tracker {
                    tracker.set_prepare_progress(0, actual_to_create);
                }
                
                // Create atomic counters for live stats
                let live_ops = Arc::new(AtomicU64::new(0));
                let live_bytes = Arc::new(AtomicU64::new(0));
                
                // Create progress bar for preparation - use actual_to_create for correct length
                let pb = ProgressBar::new(actual_to_create);
                pb.set_style(ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects {msg}"
                )?);
                pb.set_message(format!("{} workers (starting...)", concurrency));
                
                // Start live stats monitoring task
                let pb_monitor = pb.clone();
                let ops_monitor = live_ops.clone();
                let bytes_monitor = live_bytes.clone();
                let monitor_handle = tokio::spawn(async move {
                    let mut last_ops = 0u64;
                    let mut last_bytes = 0u64;
                    let mut last_time = Instant::now();
                    
                    loop {
                        tokio::time::sleep(Duration::from_millis(crate::constants::PROGRESS_MONITOR_SLEEP_MS)).await;
                        
                        // Break when all objects created
                        if pb_monitor.position() >= pb_monitor.length().unwrap_or(u64::MAX) {
                            break;
                        }
                        
                        let elapsed = last_time.elapsed();
                        if elapsed.as_secs_f64() >= 0.5 {
                            let current_ops = ops_monitor.load(Ordering::Relaxed);
                            let current_bytes = bytes_monitor.load(Ordering::Relaxed);
                            
                            let ops_delta = current_ops.saturating_sub(last_ops);
                            let bytes_delta = current_bytes.saturating_sub(last_bytes);
                            let time_delta = elapsed.as_secs_f64();
                            
                            if ops_delta > 0 {
                                let ops_per_sec = ops_delta as f64 / time_delta;
                                let mib_per_sec = (bytes_delta as f64 / 1_048_576.0) / time_delta;
                                let avg_latency_ms = (time_delta * 1000.0 * concurrency as f64) / ops_delta as f64;
                                
                                pb_monitor.set_message(format!(
                                    "{} workers | {:.0} ops/s | {:.1} MiB/s | avg {:.2}ms",
                                    concurrency, ops_per_sec, mib_per_sec, avg_latency_ms
                                ));
                            }
                            
                            last_ops = current_ops;
                            last_bytes = current_bytes;
                            last_time = Instant::now();
                        }
                    }
                });
                
                // v0.7.9: Generate tasks for missing indices with pre-generated sizes
                let mut tasks: Vec<(String, u64)> = Vec::with_capacity(missing_indices.len());
                
                // Use tree manifest for file paths if available
                if let Some(manifest) = tree_manifest {
                    // Create files at specific missing indices in directory structure
                    for &missing_idx in &missing_indices {
                        if let Some(rel_path) = manifest.get_file_path(missing_idx as usize) {
                            let uri = if spec.base_uri.ends_with('/') {
                                format!("{}{}", spec.base_uri, rel_path)
                            } else {
                                format!("{}/{}", spec.base_uri, rel_path)
                            };
                            let size = all_sizes[missing_idx as usize];
                            tasks.push((uri, size));
                        } else {
                            warn!("No file path for missing index {} in tree manifest", missing_idx);
                        }
                    }
                } else {
                    // Flat file mode: create at specific missing indices
                    for &missing_idx in &missing_indices {
                        let key = format!("{}-{:08}.dat", prefix, missing_idx);
                        let uri = if spec.base_uri.ends_with('/') {
                            format!("{}{}", spec.base_uri, key)
                        } else {
                            format!("{}/{}", spec.base_uri, key)
                        };
                        let size = all_sizes[missing_idx as usize];
                        tasks.push((uri, size));
                    }
                }
                
                // Execute PUT operations in parallel with semaphore-controlled concurrency
                let sem = Arc::new(Semaphore::new(concurrency));
                let mut futs = FuturesUnordered::new();
                let pb_clone = pb.clone();
                let tracker_clone = live_stats_tracker.clone();
                
                // v0.8.13: Error tracking for resilient prepare phase
                let error_tracker = Arc::new(PrepareErrorTracker::new());
                
                // v0.8.23: shared_store is now created at beginning of loop (supports multi-endpoint)
                // Removed redundant create_store_for_uri() call that bypassed multi-endpoint
                
                for (uri, size) in tasks {
                    let sem2 = sem.clone();
                    let store = shared_store.clone();
                    let fill = spec.fill;
                    let dedup = spec.dedup_factor;
                    let compress = spec.compress_factor;
                    let pb2 = pb_clone.clone();
                    let ops_counter = live_ops.clone();
                    let bytes_counter = live_bytes.clone();
                    let tracker = tracker_clone.clone();
                    let err_tracker = error_tracker.clone();
                    let uri_clone = uri.clone();
                    
                    futs.push(tokio::spawn(async move {
                        let _permit = sem2.acquire_owned().await.unwrap();
                        
                        // Generate data using s3dlio's controlled data generation
                        // OPTIMIZED v0.8.20+: Use cached generator pool for 50+ GB/s
                        let data: bytes::Bytes = match fill {
                            FillPattern::Zero => {
                                let buf = bytes::BytesMut::zeroed(size as usize);
                                // Buffer already zeroed, just freeze to Bytes
                                buf.freeze()
                            }
                            FillPattern::Random => {
                                // Already returns Bytes - zero-copy
                                crate::data_gen_pool::generate_data_optimized(size as usize, dedup, compress)
                            }
                            FillPattern::Prand => {
                                // Use fill_controlled_data() for in-place generation (86-163 GB/s)
                                let mut buf = bytes::BytesMut::zeroed(size as usize);
                                s3dlio::fill_controlled_data(&mut buf, dedup, compress);
                                buf.freeze()
                            }
                        };
                        
                        // v0.8.13: PUT object with retry and exponential backoff
                        let retry_config = RetryConfig::default();
                        let uri_for_retry = uri_clone.clone();
                        let put_start = Instant::now();
                        
                        let put_result = retry_with_backoff(
                            &format!("PUT {}", &uri_clone),
                            &retry_config,
                            || {
                                let store_ref = store.clone();
                                let uri_ref = uri_for_retry.clone();
                                let data_ref = data.clone();  // Cheap: Bytes is Arc-like
                                async move {
                                    store_ref.put(&uri_ref, data_ref).await  // Zero-copy: Bytes passed directly
                                        .map_err(|e| anyhow::anyhow!("{}", e))
                                }
                            }
                        ).await;
                        
                        match put_result {
                            RetryResult::Success(_) => {
                                let latency = put_start.elapsed();
                                let latency_us = latency.as_micros() as u64;
                                
                                // Record success - resets consecutive error counter
                                err_tracker.record_success();
                                
                                // Record stats for live streaming (if tracker provided)
                                if let Some(ref t) = tracker {
                                    t.record_put(size as usize, latency);
                                }
                                
                                // Update live counters
                                ops_counter.fetch_add(1, Ordering::Relaxed);
                                bytes_counter.fetch_add(size, Ordering::Relaxed);
                                
                                pb2.inc(1);
                                
                                // v0.7.9: Update prepare progress in live stats tracker
                                if let Some(ref t) = tracker {
                                    let created = pb2.position();
                                    let total = pb2.length().unwrap_or(actual_to_create);
                                    t.set_prepare_progress(created, total);
                                }
                                
                                Ok::<Option<(String, u64, u64)>, anyhow::Error>(Some((uri_clone, size, latency_us)))
                            }
                            RetryResult::Failed(e) => {
                                // v0.8.13: All retries failed - record error and check thresholds
                                let error_msg = format!("{}", e);
                                let (should_abort, total_errors, consecutive_errors) = 
                                    err_tracker.record_error(&uri_clone, size, &error_msg);
                                
                                tracing::debug!("❌ PUT failed for {} after retries: {} [total: {}, consecutive: {}]",
                                    uri_clone, error_msg, total_errors, consecutive_errors);
                                
                                pb2.inc(1);
                                if let Some(ref t) = tracker {
                                    let created = pb2.position();
                                    let total = pb2.length().unwrap_or(actual_to_create);
                                    t.set_prepare_progress(created, total);
                                }
                                
                                if should_abort {
                                    Err(anyhow::anyhow!(
                                        "Prepare aborted: {} total errors or {} consecutive errors",
                                        total_errors, consecutive_errors
                                    ))
                                } else {
                                    Ok(None)
                                }
                            }
                        }
                    }));
                }
                
                // Collect results as they complete - v0.8.13: Handle errors gracefully
                let mut created_objects = Vec::with_capacity(actual_to_create as usize);
                let mut error_result: Option<anyhow::Error> = None;
                
                while let Some(result) = futs.next().await {
                    match result {
                        Ok(Ok(Some((uri, size, latency_us)))) => {
                            metrics.put.bytes += size;
                            metrics.put.ops += 1;
                            metrics.put_bins.add(size);
                            let bucket = crate::metrics::bucket_index(size as usize);
                            metrics.put_hists.record(bucket, Duration::from_micros(latency_us));
                            
                            created_objects.push(PreparedObject {
                                uri,
                                size,
                                created: true,
                            });
                        }
                        Ok(Ok(None)) => {
                            // PUT failed but below threshold - continue
                        }
                        Ok(Err(e)) => {
                            if error_result.is_none() {
                                error_result = Some(e);
                            }
                        }
                        Err(e) => {
                            warn!("Prepare task failed: {}", e);
                        }
                    }
                }
                
                // Wait for monitoring task to complete cleanly
                monitor_handle.await.ok();
                
                // Check if we hit the error threshold
                if let Some(e) = error_result {
                    let (total_errors, _) = error_tracker.get_stats();
                    warn!("Prepare phase failed with {} errors", total_errors);
                    return Err(e);
                }
                
                // Log any partial failures
                let (total_errors, _) = error_tracker.get_stats();
                if total_errors > 0 {
                    warn!("⚠️ Pool {} completed with {} failed objects", prefix, total_errors);
                }
                
                pb.finish_with_message(format!("created {} {} objects", actual_to_create, prefix));
                
                // v0.7.9: Clear prepare progress after pool complete
                if let Some(ref tracker) = live_stats_tracker {
                    tracker.set_prepare_complete();
                }
                
                // Add created objects to all_prepared
                all_prepared.extend(created_objects);
            }
        }
    }
    
    Ok(all_prepared)
}

/// Parallel prepare strategy: Interleave all ensure_objects entries for maximum throughput
/// Creates all file sizes concurrently with better storage pipeline utilization
/// 
/// v0.7.2: Shuffles tasks to ensure each directory receives a mix of all file sizes
/// rather than clustering sizes together (all 32KB, then all 64KB, etc.)
#[allow(clippy::too_many_arguments)]
async fn prepare_parallel(
    config: &PrepareConfig,
    needs_separate_pools: bool,
    metrics: &mut PrepareMetrics,
    live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
    multi_endpoint_config: Option<&crate::config::MultiEndpointConfig>,
    multi_ep_cache: &MultiEndpointCache,
    tree_manifest: Option<&TreeManifest>,
    concurrency: usize,
    agent_id: usize,
    num_agents: usize,
) -> Result<Vec<PreparedObject>> {
    use futures::stream::{FuturesUnordered, StreamExt};
    use rand::seq::SliceRandom;
    use rand::SeedableRng;
    
    // Structure to hold task information BEFORE URI assignment
    struct TaskSpec {
        size: u64,
        store_uri: String,
        fill: FillPattern,
        dedup: usize,
        compress: usize,
        prefix: String,  // "prepared" or "deletable"
        index: u64,      // v0.7.9: Specific index for gap-filling
    }
    
    // Structure to hold complete task with URI
    struct PrepareTask {
        uri: String,
        size: u64,
        store_uri: String,
        fill: FillPattern,
        dedup: usize,
        compress: usize,
    }
    
    // Collect all task specs (without URIs) from all ensure_objects entries
    let mut task_specs: Vec<TaskSpec> = Vec::new();
    let mut total_to_create: u64 = 0;
    let mut existing_count_per_pool: std::collections::HashMap<(String, String), u64> = std::collections::HashMap::new();
    // v0.7.9: Track existing indices per pool for gap-filling
    let mut existing_indices_per_pool: std::collections::HashMap<(String, String), std::collections::HashSet<u64>> = std::collections::HashMap::new();
    
    // Determine which pool(s) to create based on workload requirements
    let pools_to_create = if needs_separate_pools {
        vec![("prepared", true), ("deletable", false)]  // (prefix, is_readonly)
    } else {
        vec![("prepared", false)]  // Single pool (backward compatible)
    };
    
    // Phase 1: List existing objects and build task specs for all sizes
    for spec in &config.ensure_objects {
        for (prefix, is_readonly) in &pools_to_create {
            let pool_desc = if needs_separate_pools {
                if *is_readonly { " (readonly pool for GET/STAT)" } else { " (deletable pool for DELETE)" }
            } else {
                ""
            };
            
            info!("Checking{}: {} at {}", pool_desc, spec.count, spec.base_uri);
            
            // v0.8.22: Multi-endpoint support for prepare phase
            // If use_multi_endpoint=true, create MultiEndpointStore instead of single-endpoint store
            // This distributes object creation across all endpoints for maximum network bandwidth
            let store: Box<dyn s3dlio::object_store::ObjectStore> = if spec.use_multi_endpoint {
                if let Some(multi_ep) = multi_endpoint_config {
                    info!("  ✓ Using multi-endpoint configuration: {} endpoints, {} strategy", 
                          multi_ep.endpoints.len(), multi_ep.strategy);
                    
                    // Create cache key for prepare phase multi-endpoint store
                    let cache_key = format!("prepare_par:{}:{}:{}",
                        spec.base_uri,
                        multi_ep.strategy,
                        multi_ep.endpoints.join(","));
                    
                    // Create Arc<MultiEndpointStore> - can be used for both operations and stats
                    let arc_multi_store = crate::workload::create_multi_endpoint_store(multi_ep, None, None)?;
                    
                    // Store in multi_ep_cache for stats collection
                    {
                        let mut cache_lock = multi_ep_cache.lock().unwrap();
                        cache_lock.insert(cache_key, Arc::clone(&arc_multi_store));
                    }
                    
                    // Wrap for Box<dyn ObjectStore>
                    Box::new(crate::workload::ArcMultiEndpointWrapper(arc_multi_store)) as Box<dyn s3dlio::object_store::ObjectStore>
                } else {
                    anyhow::bail!("use_multi_endpoint=true but no multi_endpoint configuration provided");
                }
            } else {
                create_store_for_uri(&spec.base_uri)?
            };
            
            // List existing objects with this prefix (unless skip_verification is enabled)
            // Issue #40: skip_verification config option
            // v0.7.9: If tree manifest exists, files are nested in directories (e.g., scan.d0_w0.dir/file_*.dat)
            // v0.7.9: Parse filenames to extract indices for gap-filling
            let (existing_count, existing_indices) = if config.skip_verification {
                info!("  ⚡ skip_verification enabled - assuming all {} objects exist (skipping LIST)", spec.count);
                (spec.count, HashSet::new())  // Assume all files exist, no gaps
            } else if tree_manifest.is_some() {
                // v0.8.14: Use distributed listing with progress updates
                let listing_result = list_existing_objects_distributed(
                    store.as_ref(),
                    &spec.base_uri,
                    tree_manifest,
                    agent_id,
                    num_agents,
                    live_stats_tracker.as_ref(),
                    spec.count,
                ).await.context("Failed to list existing objects")?;
                
                (listing_result.file_count, listing_result.indices)
            } else {
                // Flat file mode: use streaming list with progress
                let pattern = if spec.base_uri.ends_with('/') {
                    format!("{}{}-", spec.base_uri, prefix)
                } else {
                    format!("{}/{}-", spec.base_uri, prefix)
                };
                
                info!("  [Flat file mode] Listing with pattern: {}", pattern);
                
                // Use streaming list for flat mode too
                let listing_result = list_existing_objects_distributed(
                    store.as_ref(),
                    &pattern,
                    None,  // No tree manifest for flat mode
                    agent_id,
                    num_agents,
                    live_stats_tracker.as_ref(),
                    spec.count,
                ).await.context("Failed to list existing objects")?;
                
                (listing_result.file_count, listing_result.indices)
            };
            
            info!("  ✓ Found {} existing {} objects (need {})", existing_count, prefix, spec.count);
            
            // Store existing count and indices for this pool
            let pool_key = (spec.base_uri.clone(), prefix.to_string());
            existing_count_per_pool.insert(pool_key.clone(), existing_count);
            existing_indices_per_pool.insert(pool_key.clone(), existing_indices.clone());
            
            // Calculate how many to create
            let to_create = if existing_count >= spec.count {
                info!("  Sufficient {} objects already exist", prefix);
                0
            } else {
                spec.count - existing_count
            };
            
            // Note: In parallel mode, we can't record existing objects here
            // because all_prepared is created later after Phase 2 (URI assignment)
            // The existing_count_per_pool tracking is sufficient for skipping creation
            
            // v0.7.9: Generate task specs with gap-aware index assignment
            if to_create > 0 {
                // Pre-generate ALL sizes with deterministic seeded generator
                let size_spec = spec.get_size_spec();
                let seed = spec.base_uri.as_bytes().iter().fold(0u64, |acc, &b| acc.wrapping_mul(31).wrapping_add(b as u64));
                let mut size_generator = SizeGenerator::new_with_seed(&size_spec, seed)
                    .context("Failed to create size generator")?;
                
                info!("  [v0.7.9] Pre-generating all {} sizes with seed {} for deterministic gap-filling", spec.count, seed);
                let mut all_sizes: Vec<u64> = Vec::with_capacity(spec.count as usize);
                for _ in 0..spec.count {
                    all_sizes.push(size_generator.generate());
                }
                
                // Identify missing indices (gaps to fill)
                let mut missing_indices: Vec<u64> = (0..spec.count)
                    .filter(|i| !existing_indices.contains(i))
                    .collect();
                
                // v0.8.7: Filter for distributed prepare
                // Each agent only creates its assigned subset using modulo distribution
                if num_agents > 1 {
                    missing_indices.retain(|&idx| (idx as usize % num_agents) == agent_id);
                    info!("  [Distributed prepare] Agent {}/{} responsible for {} of {} missing objects",
                        agent_id, num_agents, missing_indices.len(), to_create);
                }
                
                // v0.8.7: After filtering, update to_create to reflect actual count for this agent
                let actual_to_create = missing_indices.len() as u64;
                
                info!("  [v0.7.9] Identified {} missing indices (first 10: {:?})",
                    missing_indices.len(),
                    &missing_indices[..std::cmp::min(10, missing_indices.len())]);
                
                if actual_to_create != to_create && num_agents == 1 {
                    warn!("  Missing indices count ({}) != to_create ({}) - this indicates detection logic issue",
                        actual_to_create, to_create);
                }
                
                info!("  Will create {} additional {} objects (sizes: {}, fill: {:?}, dedup: {}, compress: {})",
                    actual_to_create, prefix, size_generator.description(), spec.fill,
                    spec.dedup_factor, spec.compress_factor);
                
                // Generate task specs for missing indices with pre-generated sizes
                for &missing_idx in &missing_indices {
                    let size = all_sizes[missing_idx as usize];
                    
                    task_specs.push(TaskSpec {
                        size,
                        store_uri: spec.base_uri.clone(),
                        fill: spec.fill,
                        dedup: spec.dedup_factor,
                        compress: spec.compress_factor,
                        prefix: prefix.to_string(),
                        index: missing_idx,  // Store the specific missing index
                    });
                }
                
                total_to_create += actual_to_create;
            }
        }
    }
    
    if task_specs.is_empty() {
        info!("All objects already exist - reconstructing PreparedObject list from existing files");
        let mut all_prepared = Vec::new();
        
        // Reconstruct existing objects from specs
        for spec in &config.ensure_objects {
            let pools_to_create = if needs_separate_pools {
                vec![("prepared", true), ("deletable", false)]
            } else {
                vec![("prepared", false)]
            };
            
            for (prefix, _is_readonly) in &pools_to_create {
                let size_spec = spec.get_size_spec();
                let mut size_generator = SizeGenerator::new(&size_spec)
                    .context("Failed to create size generator")?;
                
                for i in 0..spec.count {
                    let uri = if let Some(manifest) = tree_manifest {
                        // Tree mode: use manifest paths
                        if let Some(rel_path) = manifest.get_file_path(i as usize) {
                            if spec.base_uri.ends_with('/') {
                                format!("{}{}", spec.base_uri, rel_path)
                            } else {
                                format!("{}/{}", spec.base_uri, rel_path)
                            }
                        } else {
                            continue;  // Skip if manifest doesn't have this index
                        }
                    } else {
                        // Flat mode: traditional naming
                        let key = format!("{}-{:08}.dat", prefix, i);
                        if spec.base_uri.ends_with('/') {
                            format!("{}{}", spec.base_uri, key)
                        } else {
                            format!("{}/{}", spec.base_uri, key)
                        }
                    };
                    
                    let size = size_generator.generate();
                    all_prepared.push(PreparedObject {
                        uri,
                        size,
                        created: false,  // All existed
                    });
                }
            }
        }
        
        info!("Reconstructed {} existing objects for workload", all_prepared.len());
        return Ok(all_prepared);
    }
    
    // Phase 2: Shuffle task specs to mix sizes across directories
    // Use StdRng which is Send-safe for async contexts
    info!("Shuffling {} tasks to distribute sizes evenly across directories", task_specs.len());
    let mut rng = rand::rngs::StdRng::seed_from_u64(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs());
    task_specs.shuffle(&mut rng);
    
    // Phase 3: Assign URIs to shuffled tasks using their specific indices (gap-aware)
    // v0.7.9: Tasks already have specific indices assigned during creation
    
    let mut all_tasks: Vec<PrepareTask> = Vec::with_capacity(task_specs.len());
    for spec in task_specs {
        // v0.7.9: Use the specific index stored in TaskSpec (no sequential assignment)
        let idx = spec.index;
        
        // Use tree manifest for file paths if available
        let uri = if let Some(manifest) = tree_manifest {
            // Create files in directory structure at specific index
            let global_idx = idx as usize;
            if let Some(rel_path) = manifest.get_file_path(global_idx) {
                if spec.store_uri.ends_with('/') {
                    format!("{}{}", spec.store_uri, rel_path)
                } else {
                    format!("{}/{}", spec.store_uri, rel_path)
                }
            } else {
                // Fallback to flat naming if manifest doesn't have this index
                let key = format!("{}-{:08}.dat", spec.prefix, idx);
                if spec.store_uri.ends_with('/') {
                    format!("{}{}", spec.store_uri, key)
                } else {
                    format!("{}/{}", spec.store_uri, key)
                }
            }
        } else {
            // Flat file naming at specific index
            let key = format!("{}-{:08}.dat", spec.prefix, idx);
            if spec.store_uri.ends_with('/') {
                format!("{}{}", spec.store_uri, key)
            } else {
                format!("{}/{}", spec.store_uri, key)
            }
        };
        
        all_tasks.push(PrepareTask {
            uri,
            size: spec.size,
            store_uri: spec.store_uri,
            fill: spec.fill,
            dedup: spec.dedup,
            compress: spec.compress,
        });
    }
    
    // Phase 3.5: Handle case where all objects already exist (total_to_create == 0)
    if total_to_create == 0 {
        info!("All objects already exist - reconstructing PreparedObject list");
        let mut all_prepared = Vec::new();
        
        // Reconstruct existing objects from specs
        for spec in &config.ensure_objects {
            let pools_to_create = if needs_separate_pools {
                vec![("prepared", true), ("deletable", false)]
            } else {
                vec![("prepared", false)]
            };
            
            for (prefix, _is_readonly) in &pools_to_create {
                let size_spec = spec.get_size_spec();
                let mut size_generator = SizeGenerator::new(&size_spec)
                    .context("Failed to create size generator")?;
                
                for i in 0..spec.count {
                    let uri = if let Some(manifest) = tree_manifest {
                        // Tree mode: use manifest paths
                        if let Some(rel_path) = manifest.get_file_path(i as usize) {
                            if spec.base_uri.ends_with('/') {
                                format!("{}{}", spec.base_uri, rel_path)
                            } else {
                                format!("{}/{}", spec.base_uri, rel_path)
                            }
                        } else {
                            continue;  // Skip if manifest doesn't have this index
                        }
                    } else {
                        // Flat mode: traditional naming
                        let key = format!("{}-{:08}.dat", prefix, i);
                        if spec.base_uri.ends_with('/') {
                            format!("{}{}", spec.base_uri, key)
                        } else {
                            format!("{}/{}", spec.base_uri, key)
                        }
                    };
                    
                    let size = size_generator.generate();
                    all_prepared.push(PreparedObject {
                        uri,
                        size,
                        created: false,  // All existed
                    });
                }
            }
        }
        
        return Ok(all_prepared);
    }
    
    // Phase 4: Execute all tasks in parallel with unified progress bar
    info!("Creating {} total objects in parallel (sizes shuffled for even distribution)", total_to_create);
    
    // Use workload concurrency for prepare phase (passed from config)
    // Note: concurrency parameter comes from Config.concurrency
    
    // v0.7.9: Set prepare phase progress in live stats tracker
    if let Some(ref tracker) = live_stats_tracker {
        tracker.set_prepare_progress(0, total_to_create);
    }
    
    // Create atomic counters for live stats
    let live_ops = Arc::new(AtomicU64::new(0));
    let live_bytes = Arc::new(AtomicU64::new(0));
    
    let pb = ProgressBar::new(total_to_create);
    pb.set_style(ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects {msg}"
    )?);
    pb.set_message(format!("{} workers (starting...)", concurrency));
    
    // Start live stats monitoring task
    let pb_monitor = pb.clone();
    let ops_monitor = live_ops.clone();
    let bytes_monitor = live_bytes.clone();
    let monitor_handle = tokio::spawn(async move {
        let mut last_ops = 0u64;
        let mut last_bytes = 0u64;
        let mut last_time = Instant::now();
        
        loop {
            tokio::time::sleep(Duration::from_millis(crate::constants::PROGRESS_MONITOR_SLEEP_MS)).await;
            
            // Break when all objects created
            if pb_monitor.position() >= pb_monitor.length().unwrap_or(u64::MAX) {
                break;
            }
            
            let elapsed = last_time.elapsed();
            if elapsed.as_secs_f64() >= crate::constants::PROGRESS_STATS_REFRESH_SECS {
                let current_ops = ops_monitor.load(Ordering::Relaxed);
                let current_bytes = bytes_monitor.load(Ordering::Relaxed);
                
                let ops_delta = current_ops.saturating_sub(last_ops);
                let bytes_delta = current_bytes.saturating_sub(last_bytes);
                let time_delta = elapsed.as_secs_f64();
                
                if ops_delta > 0 {
                    let ops_per_sec = ops_delta as f64 / time_delta;
                    let mib_per_sec = (bytes_delta as f64 / 1_048_576.0) / time_delta;
                    let avg_latency_ms = (time_delta * 1000.0 * concurrency as f64) / ops_delta as f64;
                    
                    pb_monitor.set_message(format!(
                        "{} workers | {:.0} ops/s | {:.1} MiB/s | avg {:.2}ms",
                        concurrency, ops_per_sec, mib_per_sec, avg_latency_ms
                    ));
                }
                
                last_ops = current_ops;
                last_bytes = current_bytes;
                last_time = Instant::now();
            }
        }
    });
    
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut futs = FuturesUnordered::new();
    let pb_clone = pb.clone();
    let tracker_clone = live_stats_tracker.clone();
    
    // v0.8.13: Error tracking for resilient prepare phase
    let error_tracker = Arc::new(PrepareErrorTracker::new());
    
    // v0.8.9: Create store cache to avoid creating per-object (was causing massive overhead)
    // Build cache of unique store_uris before entering the loop
    let mut store_cache: std::collections::HashMap<String, Arc<Box<dyn s3dlio::ObjectStore>>> = std::collections::HashMap::new();
    for task in &all_tasks {
        if !store_cache.contains_key(&task.store_uri) {
            let store = create_store_for_uri(&task.store_uri)
                .with_context(|| format!("Failed to create store for {}", task.store_uri))?;
            store_cache.insert(task.store_uri.clone(), Arc::new(store));
        }
    }
    let store_cache = Arc::new(store_cache);
    info!("Created {} cached object store(s) for prepare phase", store_cache.len());
    
    for task in all_tasks {
        let sem2 = sem.clone();
        let pb2 = pb_clone.clone();
        let ops_counter = live_ops.clone();
        let bytes_counter = live_bytes.clone();
        let tracker = tracker_clone.clone();
        let stores = store_cache.clone();
        let err_tracker = error_tracker.clone();
        
        futs.push(tokio::spawn(async move {
            let _permit = sem2.acquire_owned().await.unwrap();
            
            // Generate data using s3dlio's controlled data generation
            // OPTIMIZED v0.8.20+: Use cached generator pool for 50+ GB/s
            let data = match task.fill {
                FillPattern::Zero => {
                    let buf = bytes::BytesMut::zeroed(task.size as usize);
                    buf.freeze()  // Zero-copy: BytesMut→Bytes
                }
                FillPattern::Random => {
                    // Already returns Bytes - zero-copy
                    crate::data_gen_pool::generate_data_optimized(task.size as usize, task.dedup, task.compress)
                }
                FillPattern::Prand => {
                    #[allow(unused_mut)]  // Suppress false warning - mut required for fill_controlled_data
                    let mut buf = bytes::BytesMut::zeroed(task.size as usize);
                    s3dlio::fill_controlled_data(&mut buf, task.dedup, task.compress);
                    buf.freeze()
                }
            };
            
            // Get cached store instance
            let store = stores.get(&task.store_uri)
                .ok_or_else(|| anyhow::anyhow!("Store not found in cache for {}", task.store_uri))?;
            
            // v0.8.13: PUT object with retry and exponential backoff
            let retry_config = RetryConfig::default();
            let uri_for_retry = task.uri.clone();
            let put_start = Instant::now();
            
            let put_result = retry_with_backoff(
                &format!("PUT {}", &task.uri),
                &retry_config,
                || {
                    let store_ref = store.clone();
                    let uri_ref = uri_for_retry.clone();
                    let data_ref = data.clone();  // Cheap: Bytes is Arc-like
                    async move {
                        store_ref.put(&uri_ref, data_ref).await  // Zero-copy: Bytes passed directly
                            .map_err(|e| anyhow::anyhow!("{}", e))
                    }
                }
            ).await;
            
            match put_result {
                RetryResult::Success(_) => {
                    let latency = put_start.elapsed();
                    let latency_us = latency.as_micros() as u64;
                    
                    // Record success - resets consecutive error counter
                    err_tracker.record_success();
                    
                    // Record stats for live streaming (if tracker provided)
                    if let Some(ref t) = tracker {
                        t.record_put(task.size as usize, latency);
                    }
                    
                    // Update live counters
                    ops_counter.fetch_add(1, Ordering::Relaxed);
                    bytes_counter.fetch_add(task.size, Ordering::Relaxed);
                    
                    pb2.inc(1);
                    
                    // v0.7.9: Update prepare progress in live stats tracker
                    if let Some(ref t) = tracker {
                        let created = pb2.position();
                        let total = pb2.length().unwrap_or(total_to_create);
                        t.set_prepare_progress(created, total);
                    }
                    
                    // Return success with latency
                    Ok::<Option<(String, u64, u64)>, anyhow::Error>(Some((task.uri, task.size, latency_us)))
                }
                RetryResult::Failed(e) => {
                    // v0.8.13: All retries failed - record error and check thresholds
                    let error_msg = format!("{}", e);
                    let (should_abort, total_errors, consecutive_errors) = 
                        err_tracker.record_error(&task.uri, task.size, &error_msg);
                    
                    // Log at debug level (visible with -vv) - individual errors are expected
                    tracing::debug!("❌ PUT failed for {} after retries: {} [total: {}, consecutive: {}]",
                        task.uri, error_msg, total_errors, consecutive_errors);
                    
                    // Still increment progress bar (object skipped)
                    pb2.inc(1);
                    if let Some(ref t) = tracker {
                        let created = pb2.position();
                        let total = pb2.length().unwrap_or(total_to_create);
                        t.set_prepare_progress(created, total);
                    }
                    
                    if should_abort {
                        // Threshold exceeded - abort entire prepare
                        Err(anyhow::anyhow!(
                            "Prepare aborted: {} total errors (max: {}) or {} consecutive errors (max: {})",
                            total_errors, DEFAULT_PREPARE_MAX_ERRORS,
                            consecutive_errors, DEFAULT_PREPARE_MAX_CONSECUTIVE_ERRORS
                        ))
                    } else {
                        // Error logged but continue with other objects
                        Ok(None)
                    }
                }
            }
        }));
    }
    
    // Collect results as they complete - v0.8.13: Handle errors gracefully
    let mut all_prepared = Vec::with_capacity(total_to_create as usize);
    let mut error_result: Option<anyhow::Error> = None;
    
    while let Some(result) = futs.next().await {
        match result {
            Ok(Ok(Some((uri, size, latency_us)))) => {
                // Successful PUT
                metrics.put.bytes += size;
                metrics.put.ops += 1;
                metrics.put_bins.add(size);
                let bucket = crate::metrics::bucket_index(size as usize);
                metrics.put_hists.record(bucket, Duration::from_micros(latency_us));
                
                all_prepared.push(PreparedObject {
                    uri,
                    size,
                    created: true,
                });
            }
            Ok(Ok(None)) => {
                // PUT failed but below threshold - object skipped, continue
            }
            Ok(Err(e)) => {
                // Threshold exceeded - record error but continue draining futures
                if error_result.is_none() {
                    error_result = Some(e);
                }
            }
            Err(e) => {
                // Task panic or join error
                warn!("Prepare task failed: {}", e);
            }
        }
    }
    
    // Check if we hit the error threshold
    if let Some(e) = error_result {
        // Wait for monitoring task before returning error
        monitor_handle.await.ok();
        
        // Log summary of failures
        let (total_errors, _) = error_tracker.get_stats();
        let failures = error_tracker.get_failures();
        if !failures.is_empty() {
            warn!("Prepare phase failed with {} errors. First 5 failures:", total_errors);
            for failure in failures.iter().take(5) {
                warn!("  - {}: {}", failure.uri, failure.error);
            }
        }
        
        return Err(e);
    }
    
    // Log any partial failures that didn't exceed threshold
    let (total_errors, _) = error_tracker.get_stats();
    if total_errors > 0 {
        warn!("⚠️ Prepare completed with {} failed objects (below threshold, continuing)", total_errors);
    }
    
    // Wait for monitoring task to complete cleanly
    monitor_handle.await.ok();
    
    pb.finish_with_message(format!("created {} objects (all sizes)", total_to_create));
    
    // v0.7.9: Clear prepare progress after parallel prepare complete
    if let Some(ref tracker) = live_stats_tracker {
        tracker.set_prepare_complete();
    }
    
    Ok(all_prepared)
}

/// Create tree manifest without creating files or directories
/// v0.7.9: Split from create_directory_tree to support file creation first, mkdir second
/// Create tree manifest without executing any I/O operations
/// 
/// This is used for cleanup-only mode where we need to reconstruct
/// the directory structure to determine which files should be deleted.
pub fn create_tree_manifest_only(
    config: &PrepareConfig,
    _agent_id: usize,
    num_agents: usize,
    _base_uri: &str,
) -> Result<TreeManifest> {
    use crate::directory_tree::DirectoryTree;
    
    let dir_config = config.directory_structure.as_ref()
        .ok_or_else(|| anyhow!("No directory_structure specified in PrepareConfig"))?;
    
    info!("Creating directory tree: width={}, depth={}, files_per_dir={}, distribution={}", 
        dir_config.width, dir_config.depth, dir_config.files_per_dir, dir_config.distribution);
    
    // Generate tree structure
    let tree = DirectoryTree::new(dir_config.clone())
        .context("Failed to create DirectoryTree")?;
    
    // Create manifest with agent assignments
    let mut manifest = TreeManifest::from_tree(&tree);
    manifest.assign_agents(num_agents);
    
    Ok(manifest)
}

/// Create directories after files have been created
/// v0.7.9: Split from create_directory_tree to support file creation first, mkdir second
async fn finalize_tree_with_mkdir(
    config: &PrepareConfig,
    base_uri: &str,
    metrics: &mut PrepareMetrics,
    _live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
) -> Result<()> {
    let dir_config = config.directory_structure.as_ref()
        .ok_or_else(|| anyhow!("No directory_structure specified"))?;
    
    // Create ObjectStore for this base URI
    let store = create_store_for_uri(base_uri)?;
    
    // Determine if backend requires explicit directory creation
    // Object storage (S3/Azure/GCS) doesn't need mkdir - directories are implicit in object keys
    // File systems (file://, direct://) need explicit mkdir
    let needs_mkdir = base_uri.starts_with("file://") || base_uri.starts_with("direct://");
    
    if needs_mkdir {
        use crate::directory_tree::DirectoryTree;
        let tree = DirectoryTree::new(dir_config.clone())?;
        let manifest = TreeManifest::from_tree(&tree);
        
        info!("Creating {} directories...", manifest.all_directories.len());
        let pb = ProgressBar::new(manifest.all_directories.len() as u64);
        pb.set_style(ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} dirs {msg}"
        )?);
        
        for dir_path in &manifest.all_directories {
            let full_uri = if base_uri.ends_with('/') {
                format!("{}{}", base_uri, dir_path)
            } else {
                format!("{}/{}", base_uri, dir_path)
            };
            
            store.mkdir(&full_uri).await
                .with_context(|| format!("Failed to create directory: {}", full_uri))?;
            
            metrics.mkdir_count += 1;
            metrics.mkdir.ops += 1;
            
            pb.inc(1);
        }
        
        pb.finish_with_message("directories created");
    } else {
        info!("Skipping directory creation for object storage (directories are implicit in object keys)");
    }
    
    Ok(())
}

/// Create directory tree structure with optional file population
/// 
/// v0.7.0: Supports distributed agent coordination with proper file indexing
/// v0.7.2: Collects mkdir metrics during directory creation
pub async fn create_directory_tree(
    config: &PrepareConfig,
    agent_id: usize,
    num_agents: usize,
    base_uri: &str,
    metrics: &mut PrepareMetrics,
    live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
) -> Result<TreeManifest> {
    use crate::directory_tree::DirectoryTree;
    
    let dir_config = config.directory_structure.as_ref()
        .ok_or_else(|| anyhow!("No directory_structure specified in PrepareConfig"))?;
    
    info!("Creating directory tree: width={}, depth={}, files_per_dir={}, distribution={}", 
        dir_config.width, dir_config.depth, dir_config.files_per_dir, dir_config.distribution);
    
    // 1. Generate tree structure
    let tree = DirectoryTree::new(dir_config.clone())
        .context("Failed to create DirectoryTree")?;
    
    // 2. Create manifest with agent assignments
    let mut manifest = TreeManifest::from_tree(&tree);
    manifest.assign_agents(num_agents);
    
    info!("Tree structure: {} directories, {} files total", 
        manifest.total_dirs, manifest.total_files);
    
    if num_agents > 1 {
        let my_dirs = manifest.get_agent_dirs(agent_id);
        info!("Agent {}/{}: Assigned {} directories", 
            agent_id, num_agents, my_dirs.len());
    }
    
    // 3. Create ObjectStore for this base URI
    let store = create_store_for_uri(base_uri)?;
    
    // 4. Get directories this agent should create
    let dirs_to_create = if num_agents == 1 {
        // Single agent - create all directories
        manifest.all_directories.clone()
    } else {
        // Multiple agents - only create assigned directories
        manifest.get_agent_dirs(agent_id)
    };
    
    // 5. Create directories
    if !dirs_to_create.is_empty() {
        // Determine if backend requires explicit directory creation
        // Object storage (S3/Azure/GCS) doesn't need mkdir - directories are implicit in object keys
        // File systems (file://, direct://) need explicit mkdir
        let needs_mkdir = base_uri.starts_with("file://") || base_uri.starts_with("direct://");
        
        if needs_mkdir {
            info!("Creating {} directories...", dirs_to_create.len());
            let pb = ProgressBar::new(dirs_to_create.len() as u64);
            pb.set_style(ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} dirs {msg}"
            )?);
            
            for dir_path in &dirs_to_create {
                let full_uri = if base_uri.ends_with('/') {
                    format!("{}{}", base_uri, dir_path)
                } else {
                    format!("{}/{}", base_uri, dir_path)
                };
                
                let mkdir_start = Instant::now();
                store.mkdir(&full_uri).await
                    .with_context(|| format!("Failed to create directory: {}", full_uri))?;
                let _mkdir_latency_us = mkdir_start.elapsed().as_micros() as u64;
                
                // Update mkdir metrics (treating mkdir as metadata operation)
                // TODO: Could track mkdir latencies in future version if needed
                metrics.mkdir_count += 1;
                // For mkdir, we don't track per-size since it's always zero-byte metadata
                // Just accumulate total latency for mean calculation later
                metrics.mkdir.bytes += 0;  // Directories have no size
                metrics.mkdir.ops += 1;
                
                pb.inc(1);
            }
            
            pb.finish_with_message("directories created");
        } else {
            info!("Skipping directory creation for object storage (directories are implicit in object keys)");
        }
    }
    
    // 6. Create files if specified
    if manifest.files_per_dir > 0 {
        // CRITICAL: Use global file indexing to avoid rdf-bench collision bug
        // Each file gets a unique global index, then modulo distribution assigns to agents
        
        // Get list of directories that have files (in consistent order from manifest)
        let dirs_with_files: Vec<&String> = manifest.file_ranges
            .iter()
            .map(|(dir, _)| dir)
            .collect();
        let total_files = manifest.total_files;
        
        info!("Verifying {} files across {} directories...", 
            total_files, dirs_with_files.len());
        
        // Step 1: Build set of expected file paths
        info!("Building expected file list...");
        let mut expected_files = std::collections::HashSet::new();
        for global_idx in 0..total_files {
            if let Some(file_path) = manifest.get_file_path(global_idx) {
                expected_files.insert(file_path);
            }
        }
        info!("  Expected {} files in tree structure", expected_files.len());
        
        // Step 2: List existing files in all directories
        info!("Checking existing files in {} directories...", dirs_with_files.len());
        let mut existing_files = std::collections::HashSet::new();
        let list_pb = ProgressBar::new(dirs_with_files.len() as u64);
        list_pb.set_style(ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} dirs {msg}"
        )?);
        list_pb.set_message("listing");
        
        for dir_path in &dirs_with_files {
            // For GCS/S3 object storage, list files with directory prefix + "/"
            // This tells the storage to list objects whose keys start with this prefix
            let dir_prefix = if base_uri.ends_with('/') {
                format!("{}{}/", base_uri, dir_path)
            } else {
                format!("{}/{}/", base_uri, dir_path)
            };
            
            // List files with this directory prefix
            // recursive=false to list only direct children (no subdirs)
            // This avoids pagination issues with large directory trees
            
            // DEBUG: Log the exact parameters we're passing
            debug!("BEFORE list() call - dir_prefix: '{}', recursive: false", dir_prefix);
            
            // Add a small delay to avoid overwhelming GCS API
            tokio::time::sleep(tokio::time::Duration::from_millis(crate::constants::API_RATE_LIMIT_DELAY_MS)).await;
            
            match store.list(&dir_prefix, false).await {
                Ok(files) => {
                    // With recursive=false, we get only direct children
                    // No filtering needed since there are no subdirectories
                    
                    debug!("AFTER list() call - returned {} files for {}", files.len(), dir_prefix);
                    
                    // DEBUG: For problematic directories (< 130 files), show ALL files returned
                    if files.len() < 130 && !files.is_empty() {
                        warn!("⚠️  Directory {} returned only {} files (expected 130):", dir_prefix, files.len());
                        for (i, f) in files.iter().enumerate().take(10) {
                            warn!("    File {}: {}", i, f);
                        }
                        if files.len() > 10 {
                            warn!("    ... and {} more", files.len() - 10);
                        }
                    } else if files.is_empty() {
                        warn!("❌ Directory {} returned ZERO files (expected 130)", dir_prefix);
                    }
                    
                    if !files.is_empty() && files.len() <= 3 {
                        debug!("  Files: {:?}", files);
                    } else if !files.is_empty() {
                        debug!("  First file example: {}", files[0]);
                    }
                    
                    for file_uri in files {
                        // Extract relative path from full URI
                        let relative_path = if let Some(stripped) = file_uri.strip_prefix(base_uri) {
                            stripped.trim_start_matches('/')
                        } else {
                            warn!("File URI doesn't match base_uri: file={}, base={}", file_uri, base_uri);
                            continue;
                        };
                        
                        existing_files.insert(relative_path.to_string());
                    }
                }
                Err(e) => {
                    // Directory might not exist yet - that's OK, we'll create files later
                    debug!("Could not list directory {}: {}", dir_prefix, e);
                }
            }
            list_pb.inc(1);
        }
        list_pb.finish_with_message(format!("{} files found", existing_files.len()));
        
        // Step 3: Find missing files
        let missing_files: Vec<String> = expected_files
            .difference(&existing_files)
            .cloned()
            .collect();
        
        info!("  Found {} existing files, {} missing files", 
            existing_files.len(), missing_files.len());
        
        // Step 4: Create only missing files (or all if none exist)
        let to_create = missing_files.len();
        
        if to_create == 0 {
            info!("All directory tree files already exist - skipping creation");
        } else {
            info!("Creating {} missing files...", to_create);
        
        // Get file generation configuration from ensure_objects (if configured)
        // Use same pattern as regular prepare_objects for consistency
        let (size_spec, fill_pattern, dedup_factor, compress_factor) = 
            if let Some(ensure_spec) = config.ensure_objects.first() {
                (
                    ensure_spec.get_size_spec(),
                    ensure_spec.fill,
                    ensure_spec.dedup_factor,
                    ensure_spec.compress_factor,
                )
            } else {
                // Default: 1KB fixed size, zero fill, no dedup/compression
                use crate::size_generator::SizeSpec;
                (SizeSpec::Fixed(1024), FillPattern::Zero, 1, 1)
            };
        
        // Create size generator
        use crate::size_generator::SizeGenerator;
        let mut size_generator = SizeGenerator::new(&size_spec)
            .context("Failed to create size generator for tree files")?;
        
            info!("File size: {}, fill: {:?}, dedup: {}, compress: {}", 
                size_generator.description(), fill_pattern, dedup_factor, compress_factor);
            
            let pb = ProgressBar::new(to_create as u64);
            pb.set_style(ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} files {msg}"
            )?);
            
            let mut global_file_idx = 0usize;
            
            for dir_path in dirs_with_files {
                let dir_uri = if base_uri.ends_with('/') {
                    format!("{}{}", base_uri, dir_path)
                } else {
                    format!("{}/{}", base_uri, dir_path)
                };
                
                // Create files_per_dir files in this directory
                for _local_idx in 0..manifest.files_per_dir {
                    // Check if this file is missing
                    let file_name = manifest.get_file_name(global_file_idx);
                    let file_relative_path = format!("{}/{}", dir_path, file_name);
                    let file_uri = format!("{}/{}", dir_uri, file_name);
                    
                    // Only create if this file is in the missing list
                    if missing_files.contains(&file_relative_path) {
                        // CORRECT PATTERN: Check if this global file index belongs to this agent
                        let assigned_agent = global_file_idx % num_agents;
                        
                        if assigned_agent == agent_id {
                            // This file belongs to us - create it
                            
                            // Generate file data using EXACT same pattern as regular prepare_objects
                            // OPTIMIZED v0.8.20+: Use cached generator pool for 50+ GB/s
                            let size = size_generator.generate();
                            let data = match fill_pattern {
                                FillPattern::Zero => {
                                    let buf = bytes::BytesMut::zeroed(size as usize);
                                    buf.freeze()  // Zero-copy: BytesMut→Bytes
                                }
                                FillPattern::Random => {
                                    // Already returns Bytes - zero-copy
                                    crate::data_gen_pool::generate_data_optimized(size as usize, dedup_factor, compress_factor)
                                }
                                FillPattern::Prand => {
                                    // Zero-copy data generation using BytesMut→Bytes pattern
                                    #[allow(unused_mut)]  // Suppress false warning - mut required for fill_controlled_data
                                    let mut buf = bytes::BytesMut::zeroed(size as usize);
                                    s3dlio::fill_controlled_data(&mut buf, dedup_factor, compress_factor);
                                    buf.freeze()
                                }
                            };
                            
                            let put_start = Instant::now();
                            store.put(&file_uri, data).await  // Zero-copy: Bytes passed directly
                                .with_context(|| format!("Failed to create file: {}", file_uri))?;
                            let latency = put_start.elapsed();
                            
                            // Record stats for live streaming (if tracker provided)
                            if let Some(ref tracker) = live_stats_tracker {
                                tracker.record_put(size as usize, latency);
                            }
                            
                            pb.inc(1);
                        }
                    }
                    
                    // CRITICAL: Always increment global index, even if we skip this file
                    global_file_idx += 1;
                }
            }
            
            pb.finish_with_message(format!("missing files created (agent {}/{})", agent_id, num_agents));
        }
    }
    
    info!("Directory tree creation complete");
    Ok(manifest)
}

// ============================================================================
// Path Selection for Directory-based Workloads
// ============================================================================

use crate::config::PathSelectionStrategy;

/// Path selector for directory-based workload operations
/// 
/// **IMPORTANT**: PathSelector is ONLY used when directory_structure is configured.
/// For simple mkdir/rmdir throughput testing without a tree, use random naming directly.
/// 
/// Implements 4 selection strategies that control contention level:
/// - Random: All agents pick any directory from tree (max contention)
/// - Partitioned: Agents prefer assigned dirs but can use others (medium contention)
/// - Exclusive: Agents only use assigned dirs (minimal contention)
/// - Weighted: Probabilistic mix based on partition_overlap
#[derive(Clone)]
pub struct PathSelector {
    /// Directory manifest with all paths (REQUIRED - PathSelector doesn't exist without tree)
    manifest: TreeManifest,
    
    /// This agent's ID (0-indexed)
    agent_id: usize,
    
    /// Total number of agents (used for validation)
    num_agents: usize,
    
    /// Path selection strategy
    strategy: PathSelectionStrategy,
    
    /// Overlap probability for weighted mode (0.0 = exclusive, 1.0 = random)
    partition_overlap: f64,
}

impl PathSelector {
    /// Create a new path selector for structured directory testing
    /// 
    /// # Arguments
    /// - `manifest`: TreeManifest with directory structure (REQUIRED)
    /// - `agent_id`: This agent's ID (0-indexed)
    /// - `num_agents`: Total number of agents
    /// - `strategy`: Path selection strategy
    /// - `partition_overlap`: Overlap probability for weighted mode
    pub fn new(
        manifest: TreeManifest,
        agent_id: usize,
        num_agents: usize,
        strategy: PathSelectionStrategy,
        partition_overlap: f64,
    ) -> Self {
        // Validate agent configuration
        if num_agents == 0 {
            warn!("PathSelector created with num_agents=0, setting to 1");
        }
        
        if agent_id >= num_agents && num_agents > 0 {
            warn!("PathSelector: agent_id ({}) >= num_agents ({}), path selection may not work correctly", 
                agent_id, num_agents);
        }
        
        Self {
            manifest,
            agent_id,
            num_agents,
            strategy,
            partition_overlap,
        }
    }
    
    /// Select a directory path based on the configured strategy
    /// 
    /// Always returns Some() since manifest is guaranteed to exist
    pub fn select_directory(&self) -> String {
        if self.manifest.all_directories.is_empty() {
            // Shouldn't happen with valid TreeManifest, but handle gracefully
            warn!("PathSelector has empty manifest - this indicates a bug");
            return "fallback_dir".to_string();
        }
        
        match self.strategy {
            PathSelectionStrategy::Random => self.select_random(),
            PathSelectionStrategy::Partitioned => self.select_partitioned(),
            PathSelectionStrategy::Exclusive => self.select_exclusive(),
            PathSelectionStrategy::Weighted => self.select_weighted(),
        }
    }
    
    /// Random: Pick any directory uniformly from the tree
    fn select_random(&self) -> String {
        use rand::{rng, Rng};
        
        let mut rng = rng();
        let idx = rng.random_range(0..self.manifest.all_directories.len());
        self.manifest.all_directories[idx].clone()
    }
    
    /// Partitioned: Prefer assigned directories, but can pick others
    /// Uses 70/30 split to reduce contention while allowing flexibility
    fn select_partitioned(&self) -> String {
        use rand::{rng, Rng};
        
        let mut rng = rng();
        
        // 70% chance to pick from assigned directories
        // 30% chance to pick from any directory
        if rng.random::<f64>() < 0.7 {
            // Pick from assigned directories
            let assigned = self.manifest.get_agent_dirs(self.agent_id);
            if !assigned.is_empty() {
                let idx = rng.random_range(0..assigned.len());
                return assigned[idx].clone();
            }
        }
        
        // Fall back to random selection
        self.select_random()
    }
    
    /// Exclusive: Only pick from assigned directories
    /// Minimal contention - each agent has its own namespace
    fn select_exclusive(&self) -> String {
        use rand::{rng, Rng};
        
        let assigned = self.manifest.get_agent_dirs(self.agent_id);
        
        if assigned.is_empty() {
            warn!("Agent {}/{} has no assigned directories in exclusive mode (total dirs: {}), falling back to random", 
                self.agent_id, self.num_agents, self.manifest.all_directories.len());
            return self.select_random();
        }
        
        let mut rng = rng();
        let idx = rng.random_range(0..assigned.len());
        assigned[idx].clone()
    }
    
    /// Weighted: Probabilistic mix based on partition_overlap
    /// - overlap=0.0: Exclusive (0% from other agents)
    /// - overlap=0.3: 30% from other agents, 70% from assigned
    /// - overlap=1.0: Random (100% from any directory)
    fn select_weighted(&self) -> String {
        use rand::{rng, Rng};
        
        let mut rng = rng();
        
        let use_assigned_probability = 1.0 - self.partition_overlap;
        
        if rng.random::<f64>() < use_assigned_probability {
            // Pick from assigned directories
            let assigned = self.manifest.get_agent_dirs(self.agent_id);
            if !assigned.is_empty() {
                let idx = rng.random_range(0..assigned.len());
                return assigned[idx].clone();
            }
        }
        
        // Pick from any directory (not assigned to this agent)
        // This creates controlled contention
        let all_dirs = &self.manifest.all_directories;
        let assigned_dirs = self.manifest.get_agent_dirs(self.agent_id);
        let assigned_set: std::collections::HashSet<_> = assigned_dirs.iter().collect();
        
        let other_dirs: Vec<_> = all_dirs.iter()
            .filter(|d| !assigned_set.contains(d))
            .collect();
        
        if !other_dirs.is_empty() {
            let idx = rng.random_range(0..other_dirs.len());
            other_dirs[idx].clone()
        } else {
            // No other dirs available, use assigned
            self.select_exclusive()
        }
    }
    
    /// Select a file path (directory + file) based on strategy
    /// 
    /// Returns full relative path: "d1_w1.dir/d2_w1.dir/d3_w1.dir/file_00001.dat"
    /// This is the primary method for GET/PUT/STAT/DELETE operations
    pub fn select_file(&self) -> String {
        use rand::{rng, Rng};
        
        // 1. Select directory using existing strategy
        let dir = self.select_directory_with_files();
        
        // 2. Pick a file within that directory using file ranges
        // Get file range for this directory
        if let Some((start_idx, end_idx)) = self.manifest.get_file_range(&dir) {
            if end_idx > start_idx {
                // Pick random file in range
                let mut rng = rng();
                let global_idx = rng.random_range(*start_idx..*end_idx);
                // Use get_file_path which returns directory + file based on global index
                if let Some(path) = self.manifest.get_file_path(global_idx) {
                    return path;
                }
            }
        }
        
        // Fallback (shouldn't happen with proper config)
        warn!("Directory {} has no files in manifest", dir);
        if dir.is_empty() {
            "fallback_file_00000.dat".to_string()
        } else {
            format!("{}/fallback_file_00000.dat", dir)
        }
    }
    
    /// Select a directory that has files (respects distribution strategy)
    /// Used when operation REQUIRES files (GET/STAT/DELETE)
    pub fn select_directory_with_files(&self) -> String {
        use rand::{rng, Rng};
        
        // Filter to directories that actually have files
        let dirs_with_files: Vec<&String> = self.manifest.file_ranges
            .iter()
            .map(|(dir, _)| dir)
            .collect();
        
        if dirs_with_files.is_empty() {
            warn!("No directories with files in manifest");
            return self.select_directory();
        }
        
        // Apply strategy to filtered list
        match self.strategy {
            PathSelectionStrategy::Random => {
                let mut rng = rng();
                let idx = rng.random_range(0..dirs_with_files.len());
                dirs_with_files[idx].clone()
            }
            PathSelectionStrategy::Exclusive => {
                // Pick from assigned directories that have files
                let assigned = self.manifest.get_agent_dirs(self.agent_id);
                let assigned_with_files: Vec<String> = assigned.into_iter()
                    .filter(|d| self.manifest.get_file_range(d).is_some())
                    .collect();
                    
                if assigned_with_files.is_empty() {
                    // Fallback to any directory with files
                    let mut rng = rng();
                    let idx = rng.random_range(0..dirs_with_files.len());
                    return dirs_with_files[idx].clone();
                }
                
                let mut rng = rng();
                let idx = rng.random_range(0..assigned_with_files.len());
                assigned_with_files[idx].clone()
            }
            PathSelectionStrategy::Partitioned => {
                let mut rng = rng();
                
                // 70% chance to pick from assigned directories with files
                if rng.random::<f64>() < 0.7 {
                    let assigned = self.manifest.get_agent_dirs(self.agent_id);
                    let assigned_with_files: Vec<String> = assigned.into_iter()
                        .filter(|d| self.manifest.get_file_range(d).is_some())
                        .collect();
                    
                    if !assigned_with_files.is_empty() {
                        let idx = rng.random_range(0..assigned_with_files.len());
                        return assigned_with_files[idx].clone();
                    }
                }
                
                // Fall back to random from all dirs with files
                let idx = rng.random_range(0..dirs_with_files.len());
                dirs_with_files[idx].clone()
            }
            PathSelectionStrategy::Weighted => {
                let mut rng = rng();
                let use_assigned_probability = 1.0 - self.partition_overlap;
                
                if rng.random::<f64>() < use_assigned_probability {
                    let assigned = self.manifest.get_agent_dirs(self.agent_id);
                    let assigned_with_files: Vec<String> = assigned.into_iter()
                        .filter(|d| self.manifest.get_file_range(d).is_some())
                        .collect();
                    
                    if !assigned_with_files.is_empty() {
                        let idx = rng.random_range(0..assigned_with_files.len());
                        return assigned_with_files[idx].clone();
                    }
                }
                
                // Pick from any directory with files
                let idx = rng.random_range(0..dirs_with_files.len());
                dirs_with_files[idx].clone()
            }
        }
    }
}

/// Cleanup prepared objects
/// Cleanup prepared objects with distributed execution support (v0.8.7+)
/// 
/// Supports three execution modes:
/// 1. Single-agent mode: num_agents == 1, processes all objects
/// 2. Distributed flat mode: tree_manifest is None, uses index-based distribution
/// 3. Distributed tree mode: tree_manifest provided, uses deterministic file assignment
/// 
/// Error handling modes (cleanup_mode):
/// - Strict: Report all errors (best for first-time cleanup)
/// - Tolerant: Ignore "not found" errors (best for resuming interrupted cleanup)
/// - BestEffort: Ignore all errors (best for uncertain object state)
pub async fn cleanup_prepared_objects(
    objects: &[PreparedObject],
    tree_manifest: Option<&crate::directory_tree::TreeManifest>,
    agent_id: usize,
    num_agents: usize,
    cleanup_mode: crate::config::CleanupMode,
    live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
) -> Result<()> {
    if objects.is_empty() {
        return Ok(());
    }
    
    use futures::stream::{FuturesUnordered, StreamExt};
    use std::collections::HashSet;
    
    // Filter to objects this agent should handle
    let my_objects: Vec<_> = if let Some(manifest) = tree_manifest {
        // Tree mode: use deterministic file assignment
        let my_paths: HashSet<String> = manifest
            .get_agent_file_paths(agent_id, num_agents)
            .into_iter()
            .collect();
        
        objects.iter()
            .filter(|obj| obj.created)
            .filter(|obj| {
                // Extract relative path from URI for comparison
                // URI format: "scheme://host/path/to/file.dat"
                // Need to extract: "path/to/file.dat" portion
                if let Some(pos) = obj.uri.rfind('/') {
                    let filename = &obj.uri[pos+1..];
                    // Check if this file is in our assigned paths
                    my_paths.iter().any(|p| p.ends_with(filename))
                } else {
                    false
                }
            })
            .collect()
    } else {
        // Flat mode: distribute by file index (parsed from URI)
        // URI format: "file:///path/to/prepared-00000042.dat" or "deletable-00000042.dat"
        objects.iter()
            .filter(|obj| {
                if !obj.created {
                    return false;
                }
                
                if num_agents <= 1 {
                    return true;
                }
                
                // Extract file index from URI
                // Look for pattern: "prepared-NNNNNNNN.dat" or "deletable-NNNNNNNN.dat"
                if let Some(filename_start) = obj.uri.rfind('/') {
                    let filename = &obj.uri[filename_start + 1..];
                    // Parse index from "prepared-00000042.dat" or "deletable-00000042.dat"
                    if let Some(dash_pos) = filename.find('-') {
                        if let Some(dot_pos) = filename.find('.') {
                            let index_str = &filename[dash_pos + 1..dot_pos];
                            if let Ok(file_index) = index_str.parse::<usize>() {
                                return file_index % num_agents == agent_id;
                            }
                        }
                    }
                }
                
                // Fallback: if we can't parse index, don't delete
                tracing::warn!("Could not parse file index from URI: {}", obj.uri);
                false
            })
            .collect()
    };
    
    if my_objects.is_empty() {
        if num_agents > 1 {
            info!("Agent {}/{}: No objects to clean up", agent_id + 1, num_agents);
        } else {
            info!("No objects to clean up");
        }
        return Ok(());
    }
    
    let delete_count = my_objects.len();
    
    // TODO: Accept concurrency parameter from caller (for now, use reasonable default)
    // This function is called during cleanup and doesn't have access to config
    let concurrency = 32;
    
    if num_agents > 1 {
        info!("Agent {}/{}: Cleaning up {} objects with {} workers", 
            agent_id + 1, num_agents, delete_count, concurrency);
    } else {
        info!("Cleaning up {} prepared objects with {} workers", delete_count, concurrency);
    }
    
    // Create progress bar for cleanup
    let pb = ProgressBar::new(delete_count as u64);
    pb.set_style(ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects ({per_sec}) {msg}"
    )?);
    
    let msg = if num_agents > 1 {
        format!("agent {}/{} cleaning with {} workers", agent_id + 1, num_agents, concurrency)
    } else {
        format!("cleaning with {} workers", concurrency)
    };
    pb.set_message(msg);
    
    // Track error statistics
    let error_count = Arc::new(AtomicU64::new(0));
    let not_found_count = Arc::new(AtomicU64::new(0));
    let success_count = Arc::new(AtomicU64::new(0));
    
    // Execute DELETE operations in parallel with semaphore-controlled concurrency
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut futs = FuturesUnordered::new();
    let pb_clone = pb.clone();
    
    for obj in my_objects {
        let sem2 = sem.clone();
        let pb2 = pb_clone.clone();
        let uri = obj.uri.clone();
        let mode = cleanup_mode;
        let errors = error_count.clone();
        let not_founds = not_found_count.clone();
        let successes = success_count.clone();
        let tracker = live_stats_tracker.clone();
        
        futs.push(tokio::spawn(async move {
            let _permit = sem2.acquire_owned().await.unwrap();
            
            // Start timing for latency tracking
            let start = std::time::Instant::now();
            
            // Create store and delete object
            match create_store_for_uri(&uri) {
                Ok(store) => {
                    match store.delete(&uri).await {
                        Ok(_) => {
                            successes.fetch_add(1, Ordering::Relaxed);
                            // Record successful DELETE as META operation
                            if let Some(ref t) = tracker {
                                t.record_meta(start.elapsed());
                            }
                        }
                        Err(e) => {
                            let err_str = e.to_string().to_lowercase();
                            let is_not_found = err_str.contains("not found") 
                                || err_str.contains("404") 
                                || err_str.contains("nosuchkey")
                                || err_str.contains("does not exist");
                            
                            if is_not_found {
                                not_founds.fetch_add(1, Ordering::Relaxed);
                                // Record "not found" DELETE as META operation (tolerant/best-effort mode)
                                if let Some(ref t) = tracker {
                                    t.record_meta(start.elapsed());
                                }
                                match mode {
                                    crate::config::CleanupMode::Strict => {
                                        tracing::warn!("Object not found (strict mode): {}", uri);
                                        errors.fetch_add(1, Ordering::Relaxed);
                                    }
                                    crate::config::CleanupMode::Tolerant | crate::config::CleanupMode::BestEffort => {
                                        tracing::debug!("Object already deleted: {}", uri);
                                    }
                                }
                            } else {
                                errors.fetch_add(1, Ordering::Relaxed);
                                // Record failed DELETE as META operation
                                if let Some(ref t) = tracker {
                                    t.record_meta(start.elapsed());
                                }
                                match mode {
                                    crate::config::CleanupMode::BestEffort => {
                                        tracing::warn!("Failed to delete {} (best-effort, continuing): {}", uri, e);
                                    }
                                    _ => {
                                        tracing::warn!("Failed to delete {}: {}", uri, e);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    // Record store creation failure as META operation
                    if let Some(ref t) = tracker {
                        t.record_meta(start.elapsed());
                    }
                    tracing::warn!("Failed to create store for {}: {}", uri, e);
                }
            }
            
            pb2.inc(1);
        }));
    }
    
    // Wait for all deletion tasks to complete
    // Tasks don't return errors (they log warnings instead), so we just need to detect panics
    while let Some(res) = futs.next().await {
        if let Err(e) = res {
            tracing::error!("Cleanup task panicked: {}", e);
            // Continue with remaining tasks even if one panicked
        }
    }
    
    let final_errors = error_count.load(Ordering::Relaxed);
    let final_not_found = not_found_count.load(Ordering::Relaxed);
    let final_success = success_count.load(Ordering::Relaxed);
    
    let summary = format!(
        "deleted {} objects ({} succeeded, {} already deleted, {} errors)",
        delete_count, final_success, final_not_found, final_errors
    );
    pb.finish_with_message(summary.clone());
    
    // Log final statistics
    if num_agents > 1 {
        info!("Agent {}/{}: {}", agent_id + 1, num_agents, summary);
    } else {
        info!("{}", summary);
    }
    
    // Return error only in strict mode with actual errors
    if cleanup_mode == crate::config::CleanupMode::Strict && final_errors > 0 {
        anyhow::bail!("Cleanup failed with {} errors (strict mode)", final_errors);
    }
    
    Ok(())
}

/// Generate list of objects for cleanup-only mode (v0.8.7+)
/// 
/// Creates PreparedObject list based on config WITHOUT listing existing objects.
/// Uses same deterministic algorithm as prepare phase (modulo distribution).
/// 
/// This function is used when:
/// - cleanup_only mode (duration=0, workload=[], cleanup=true)
/// - skip_verification=true (do NOT list existing objects)
/// 
/// Each agent generates ONLY the objects it would have created:
/// - Agent 0: indices 0, 2, 4, 6, ... (i % num_agents == 0)
/// - Agent 1: indices 1, 3, 5, 7, ... (i % num_agents == 1)
pub fn generate_cleanup_objects(
    config: &PrepareConfig,
    agent_id: usize,
    num_agents: usize,
) -> Result<Vec<PreparedObject>> {
    let mut objects = Vec::new();
    
    for spec in &config.ensure_objects {
        // Determine which pool(s) to clean
        // For simplicity, cleanup both prepared and deletable if they exist
        let pools = vec!["prepared", "deletable"];
        
        for prefix in pools {
            for i in 0..spec.count {
                let index = i as usize;
                
                // Distributed mode: only handle objects for this agent
                if num_agents > 1 && index % num_agents != agent_id {
                    continue;
                }
                
                // Generate URI using same naming convention as prepare
                let key = format!("{}-{:08}.dat", prefix, i);
                let uri = if spec.base_uri.ends_with('/') {
                    format!("{}{}", spec.base_uri, key)
                } else {
                    format!("{}/{}", spec.base_uri, key)
                };
                
                // Mark as created=true so cleanup will process them
                objects.push(PreparedObject {
                    uri,
                    size: 0,  // Size doesn't matter for cleanup
                    created: true,
                });
            }
        }
    }
    
    if num_agents > 1 {
        info!("Agent {}/{}: Generated {} objects for cleanup (skip_verification=true)", 
              agent_id + 1, num_agents, objects.len());
    } else {
        info!("Generated {} objects for cleanup (skip_verification=true)", objects.len());
    }
    
    Ok(objects)
}

/// Verify that prepared objects exist and are accessible
pub async fn verify_prepared_objects(config: &PrepareConfig) -> Result<()> {
    info!("Starting verification of prepared objects");
    
    for spec in &config.ensure_objects {
        let store = create_store_for_uri(&spec.base_uri)?;
        
        // List existing objects
        info!("Verifying objects at {}", spec.base_uri);
        let existing = store.list(&spec.base_uri, true).await
            .context("Failed to list objects during verification")?;
        
        let found_count = existing.len();
        let expected_count = spec.count as usize;
        
        if found_count < expected_count {
            anyhow::bail!("Verification failed: Found {} objects but expected {} at {}",
                  found_count, expected_count, spec.base_uri);
        }
        
        info!("Found {}/{} objects, verifying accessibility...", found_count, expected_count);
        
        // Verify accessibility by attempting to stat each object
        let pb = ProgressBar::new(expected_count as u64);
        pb.set_style(ProgressStyle::with_template(
            "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} verified {msg}"
        )?);
        
        let mut accessible_count = 0;
        let mut inaccessible = Vec::new();
        
        for uri in existing.iter().take(expected_count) {
            // Try to stat the object using the same store
            match store.stat(uri).await {
                Ok(_metadata) => {
                    accessible_count += 1;
                    pb.inc(1);
                }
                Err(e) => {
                    inaccessible.push(format!("{}: {}", uri, e));
                    pb.inc(1);
                }
            }
        }
        
        pb.finish_and_clear();
        
        if !inaccessible.is_empty() {
            eprintln!("\nInaccessible objects:");
            for issue in &inaccessible {
                eprintln!("  ✗ {}", issue);
            }
            anyhow::bail!("Verification failed: {}/{} objects accessible at {}",
                  accessible_count, expected_count, spec.base_uri);
        }
        
        println!("✓ {}/{} objects verified and accessible at {}", 
                 accessible_count, expected_count, spec.base_uri);
        info!("Verification successful: {}/{} objects", accessible_count, expected_count);
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PrepareConfig;
    
    #[test]
    fn test_concurrency_parameter_passed() {
        // Test that concurrency parameter is accepted and would be used
        // We can't fully test prepare_objects without a real storage backend,
        // but we can verify the function signature accepts the parameter
        
        let config = PrepareConfig {
            ensure_objects: vec![],
            cleanup: false,
            cleanup_mode: crate::config::CleanupMode::Tolerant,
            cleanup_only: Some(false),
            post_prepare_delay: 0,
            directory_structure: None,
            prepare_strategy: crate::config::PrepareStrategy::Sequential,
            skip_verification: false,
        };
        
        // This test verifies the function compiles with the concurrency parameter
        // In a real scenario, we'd mock the storage backend to verify the value is used
        let test_concurrency = 64;
        
        // Create a simple async runtime to test the function signature
        let rt = tokio::runtime::Runtime::new().unwrap();
        
        // Test that the function can be called with different concurrency values
        // Without actual storage, this will just verify parameter passing
        let multi_ep_cache = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let result = rt.block_on(async {
            // Verify function signature accepts concurrency parameter
            // This will return immediately with empty results since no objects to prepare
            prepare_objects(&config, None, None, None, &multi_ep_cache, 1, test_concurrency, 0).await
        });
        
        // Should succeed with empty object list
        assert!(result.is_ok());
        let (prepared, _, _) = result.unwrap();
        assert_eq!(prepared.len(), 0);
    }
    
    #[test]
    fn test_different_concurrency_values() {
        // Verify we can pass different concurrency values
        let rt = tokio::runtime::Runtime::new().unwrap();
        
        let config = PrepareConfig {
            ensure_objects: vec![],
            cleanup: false,
            cleanup_mode: crate::config::CleanupMode::Tolerant,
            cleanup_only: Some(false),
            post_prepare_delay: 0,
            directory_structure: None,
            prepare_strategy: crate::config::PrepareStrategy::Sequential,
            skip_verification: false,
        };
        
        // Test with various concurrency values
        let multi_ep_cache = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        for concurrency in [1, 16, 32, 64, 128] {
            let result = rt.block_on(async {
                prepare_objects(&config, None, None, None, &multi_ep_cache, 1, concurrency, 0).await
            });
            
            assert!(result.is_ok(), "Failed with concurrency={}", concurrency);
        }
    }
    
    // =========================================================================
    // PrepareErrorTracker Tests (v0.8.13)
    // =========================================================================
    
    #[test]
    fn test_prepare_error_tracker_new() {
        let tracker = PrepareErrorTracker::new();
        let (total, consecutive) = tracker.get_stats();
        
        assert_eq!(total, 0);
        assert_eq!(consecutive, 0);
        assert_eq!(tracker.total_errors(), 0);
    }
    
    #[test]
    fn test_prepare_error_tracker_with_thresholds() {
        let tracker = PrepareErrorTracker::with_thresholds(50, 5);
        let (total, consecutive) = tracker.get_stats();
        
        assert_eq!(total, 0);
        assert_eq!(consecutive, 0);
    }
    
    #[test]
    fn test_prepare_error_tracker_record_error() {
        let tracker = PrepareErrorTracker::new();
        
        let (should_abort, total, consecutive) = 
            tracker.record_error("file:///test/obj1", 1024, "Connection refused");
        
        assert!(!should_abort);  // Single error shouldn't trigger abort
        assert_eq!(total, 1);
        assert_eq!(consecutive, 1);
    }
    
    #[test]
    fn test_prepare_error_tracker_record_success_resets_consecutive() {
        let tracker = PrepareErrorTracker::new();
        
        // Record some errors
        for i in 0..5 {
            let (_, _, consecutive) = tracker.record_error(
                &format!("file:///test/obj{}", i), 
                1024, 
                "Error"
            );
            assert_eq!(consecutive, i as u64 + 1);
        }
        
        let (_, consecutive_before) = tracker.get_stats();
        assert_eq!(consecutive_before, 5);
        
        // Success should reset consecutive counter
        tracker.record_success();
        
        let (total, consecutive_after) = tracker.get_stats();
        assert_eq!(total, 5);  // Total is cumulative
        assert_eq!(consecutive_after, 0);  // Consecutive reset
    }
    
    #[test]
    fn test_prepare_error_tracker_total_threshold() {
        let tracker = PrepareErrorTracker::with_thresholds(5, 100);  // 5 max total
        
        // Record 4 errors - should not abort
        for i in 0..4 {
            let (should_abort, total, _) = tracker.record_error(
                &format!("file:///test/obj{}", i),
                1024,
                "Error"
            );
            assert!(!should_abort, "Should not abort at {} errors", total);
        }
        
        // 5th error should trigger abort
        let (should_abort, total, _) = tracker.record_error(
            "file:///test/obj5",
            1024,
            "Error"
        );
        assert!(should_abort, "Should abort at {} errors", total);
        assert_eq!(total, 5);
    }
    
    #[test]
    fn test_prepare_error_tracker_consecutive_threshold() {
        let tracker = PrepareErrorTracker::with_thresholds(100, 3);  // 3 max consecutive
        
        // Record 2 errors - should not abort
        for i in 0..2 {
            let (should_abort, _, consecutive) = tracker.record_error(
                &format!("file:///test/obj{}", i),
                1024,
                "Error"
            );
            assert!(!should_abort, "Should not abort at {} consecutive", consecutive);
        }
        
        // 3rd consecutive error should trigger abort
        let (should_abort, _, consecutive) = tracker.record_error(
            "file:///test/obj3",
            1024,
            "Error"
        );
        assert!(should_abort, "Should abort at {} consecutive errors", consecutive);
        assert_eq!(consecutive, 3);
    }
    
    #[test]
    fn test_prepare_error_tracker_consecutive_reset_prevents_abort() {
        let tracker = PrepareErrorTracker::with_thresholds(100, 3);  // 3 max consecutive
        
        // Error, error, success, error, error - should not abort
        tracker.record_error("file:///test/obj1", 1024, "Error");
        tracker.record_error("file:///test/obj2", 1024, "Error");
        tracker.record_success();  // Reset consecutive
        tracker.record_error("file:///test/obj3", 1024, "Error");
        let (should_abort, total, consecutive) = tracker.record_error(
            "file:///test/obj4",
            1024,
            "Error"
        );
        
        assert!(!should_abort, "Should not abort - consecutive was reset");
        assert_eq!(total, 4);
        assert_eq!(consecutive, 2);
    }
    
    #[test]
    fn test_prepare_error_tracker_get_failures() {
        let tracker = PrepareErrorTracker::new();
        
        tracker.record_error("file:///test/obj1", 1024, "Connection refused");
        tracker.record_error("file:///test/obj2", 2048, "Timeout");
        tracker.record_success();  // Doesn't affect failures list
        tracker.record_error("file:///test/obj3", 512, "Access denied");
        
        let failures = tracker.get_failures();
        
        assert_eq!(failures.len(), 3);
        assert_eq!(failures[0].uri, "file:///test/obj1");
        assert_eq!(failures[0].size, 1024);
        assert_eq!(failures[0].error, "Connection refused");
        
        assert_eq!(failures[1].uri, "file:///test/obj2");
        assert_eq!(failures[1].size, 2048);
        assert_eq!(failures[1].error, "Timeout");
        
        assert_eq!(failures[2].uri, "file:///test/obj3");
        assert_eq!(failures[2].size, 512);
        assert_eq!(failures[2].error, "Access denied");
    }
    
    #[test]
    fn test_prepare_error_tracker_clone() {
        let tracker = PrepareErrorTracker::new();
        tracker.record_error("file:///test/obj1", 1024, "Error");
        
        // Clone should share state (Arc)
        let tracker2 = tracker.clone();
        tracker2.record_error("file:///test/obj2", 1024, "Error");
        
        // Both should see the same total
        assert_eq!(tracker.total_errors(), 2);
        assert_eq!(tracker2.total_errors(), 2);
    }
    
    #[test]
    fn test_prepare_error_tracker_thread_safety() {
        use std::thread;
        
        let tracker = PrepareErrorTracker::new();
        let mut handles = vec![];
        
        // Spawn 10 threads, each recording 10 errors
        for t in 0..10 {
            let tracker_clone = tracker.clone();
            handles.push(thread::spawn(move || {
                for i in 0..10 {
                    tracker_clone.record_error(
                        &format!("file:///test/thread{}/obj{}", t, i),
                        1024,
                        "Error"
                    );
                }
            }));
        }
        
        for handle in handles {
            handle.join().unwrap();
        }
        
        assert_eq!(tracker.total_errors(), 100);
        assert_eq!(tracker.get_failures().len(), 100);
    }
    
    // =========================================================================
    // ListingErrorTracker Tests (v0.8.14)
    // =========================================================================
    
    #[test]
    fn test_listing_error_tracker_new() {
        let tracker = ListingErrorTracker::new();
        let (total, consecutive) = tracker.get_stats();
        
        assert_eq!(total, 0);
        assert_eq!(consecutive, 0);
        assert_eq!(tracker.total_errors(), 0);
    }
    
    #[test]
    fn test_listing_error_tracker_with_thresholds() {
        let tracker = ListingErrorTracker::with_thresholds(25, 3);
        let (total, consecutive) = tracker.get_stats();
        
        assert_eq!(total, 0);
        assert_eq!(consecutive, 0);
    }
    
    #[test]
    fn test_listing_error_tracker_record_error() {
        let tracker = ListingErrorTracker::new();
        
        let (should_abort, total, consecutive) = 
            tracker.record_error("gs://bucket/path/: Connection reset");
        
        assert!(!should_abort);  // Single error shouldn't trigger abort
        assert_eq!(total, 1);
        assert_eq!(consecutive, 1);
    }
    
    #[test]
    fn test_listing_error_tracker_record_success_resets_consecutive() {
        let tracker = ListingErrorTracker::new();
        
        // Record some errors
        for i in 0..3 {
            let (_, _, consecutive) = tracker.record_error(&format!("Error {}", i));
            assert_eq!(consecutive, i + 1);
        }
        
        // Record success - should reset consecutive counter
        tracker.record_success();
        
        // Next error should have consecutive = 1
        let (_, total, consecutive) = tracker.record_error("New error after success");
        assert_eq!(total, 4);        // Total still accumulates
        assert_eq!(consecutive, 1);  // Consecutive reset by success
    }
    
    #[test]
    fn test_listing_error_tracker_total_threshold() {
        // Use lower thresholds for testing
        let tracker = ListingErrorTracker::with_thresholds(5, 100);  // 5 total before abort
        
        // Record 4 errors - should not abort
        for i in 0..4 {
            tracker.record_success();  // Reset consecutive between errors
            let (should_abort, total, _) = tracker.record_error(&format!("Error {}", i));
            assert!(!should_abort, "Should not abort at {} errors", total);
        }
        
        // 5th error should trigger abort
        tracker.record_success();
        let (should_abort, total, _) = tracker.record_error("Final error");
        assert!(should_abort, "Should abort at {} total errors", total);
        assert_eq!(total, 5);
    }
    
    #[test]
    fn test_listing_error_tracker_consecutive_threshold() {
        // Use lower thresholds for testing
        let tracker = ListingErrorTracker::with_thresholds(100, 3);  // 3 consecutive before abort
        
        // Record 2 consecutive errors - should not abort
        for i in 0..2 {
            let (should_abort, _, consecutive) = tracker.record_error(&format!("Error {}", i));
            assert!(!should_abort, "Should not abort at {} consecutive", consecutive);
        }
        
        // 3rd consecutive error should trigger abort
        let (should_abort, _, consecutive) = tracker.record_error("Third consecutive error");
        assert!(should_abort, "Should abort at {} consecutive errors", consecutive);
        assert_eq!(consecutive, 3);
    }
    
    #[test]
    fn test_listing_error_tracker_consecutive_reset_prevents_abort() {
        let tracker = ListingErrorTracker::with_thresholds(100, 3);  // 3 consecutive before abort
        
        // Record 2 errors
        tracker.record_error("Error 1");
        tracker.record_error("Error 2");
        
        // Success resets consecutive
        tracker.record_success();
        
        // Record 2 more - still shouldn't abort (consecutive is reset)
        let (should_abort, total, consecutive) = tracker.record_error("Error 3");
        assert!(!should_abort, "Should not abort after reset, consecutive={}", consecutive);
        assert_eq!(consecutive, 1);
        assert_eq!(total, 3);  // Total still 3
        
        let (should_abort, _, _) = tracker.record_error("Error 4");
        assert!(!should_abort, "Still should not abort (consecutive=2)");
    }
    
    #[test]
    fn test_listing_error_tracker_get_error_messages() {
        let tracker = ListingErrorTracker::new();
        
        tracker.record_error("Error in gs://bucket/dir1/");
        tracker.record_success();
        tracker.record_error("Timeout reading gs://bucket/dir2/");
        tracker.record_error("Connection reset gs://bucket/dir3/");
        
        let messages = tracker.get_error_messages();
        
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], "Error in gs://bucket/dir1/");
        assert_eq!(messages[1], "Timeout reading gs://bucket/dir2/");
        assert_eq!(messages[2], "Connection reset gs://bucket/dir3/");
    }
    
    #[test]
    fn test_listing_error_tracker_clone() {
        let tracker = ListingErrorTracker::new();
        tracker.record_error("Error 1");
        
        // Clone should share state (Arc)
        let tracker2 = tracker.clone();
        tracker2.record_error("Error 2");
        
        // Both should see the same total
        assert_eq!(tracker.total_errors(), 2);
        assert_eq!(tracker2.total_errors(), 2);
    }
    
    #[test]
    fn test_listing_error_tracker_thread_safety() {
        use std::thread;
        
        let tracker = ListingErrorTracker::new();
        let mut handles = vec![];
        
        // Spawn 10 threads, each recording 2 errors (less than threshold)
        for t in 0..10 {
            let tracker_clone = tracker.clone();
            handles.push(thread::spawn(move || {
                for i in 0..2 {
                    tracker_clone.record_error(&format!("Thread {} error {}", t, i));
                    tracker_clone.record_success();  // Reset consecutive to avoid abort
                }
            }));
        }
        
        for handle in handles {
            handle.join().unwrap();
        }
        
        assert_eq!(tracker.total_errors(), 20);
        assert_eq!(tracker.get_error_messages().len(), 20);
    }
    
    #[test]
    fn test_listing_error_tracker_default() {
        let tracker = ListingErrorTracker::default();
        let (total, consecutive) = tracker.get_stats();
        
        assert_eq!(total, 0);
        assert_eq!(consecutive, 0);
    }
    
    #[test]
    fn test_listing_result_default() {
        let result = ListingResult::default();
        
        assert_eq!(result.file_count, 0);
        assert!(result.indices.is_empty());
        assert_eq!(result.dirs_listed, 0);
        assert_eq!(result.errors_encountered, 0);
        assert!(!result.aborted);
        assert_eq!(result.elapsed_secs, 0.0);
    }
}
