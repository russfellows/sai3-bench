//! Object listing with distributed progress tracking (v0.8.14)

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use anyhow::{anyhow, Result};
use futures::StreamExt;
use tracing::{debug, info, warn};

use crate::constants::{
    DEFAULT_LISTING_MAX_ERRORS,
    DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS,
    LISTING_PROGRESS_INTERVAL,
};
use crate::directory_tree::TreeManifest;
use crate::live_stats::{LiveStatsTracker, WorkloadStage};
use super::error_tracking::ListingErrorTracker;

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
                                
                                // Warn about slow listing that could cause barrier timeouts
                                if rate < 500.0 && current > 10000 {
                                    warn!("⚠ Slow listing: {:.0} files/s (expected >500/s). \
                                         May cause barrier timeout with large directory counts. \
                                         Consider increasing barrier_sync.prepare.agent_barrier_timeout", rate);
                                }
                                
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

