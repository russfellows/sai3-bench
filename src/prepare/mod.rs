//! Prepare phase implementation for object pre-population and directory tree creation
//!
//! This module handles the "prepare" phase of benchmark execution, including:
//! - Object creation with configurable size, dedup, and compression
//! - Sequential vs parallel execution strategies
//! - Directory tree creation for structured file access patterns
//! - Object cleanup after benchmarks
//! - Deferred retry with adaptive strategy (v0.8.52)
//!
//! Separated from workload.rs in v0.7.2 to improve code organization.
//! Split into submodules in v0.8.52 to improve maintainability.

use std::sync::Arc;
use std::time::Instant;
use anyhow::{anyhow, Context, Result};
use tracing::info;

use crate::config::{PrepareConfig, PrepareStrategy};
use crate::directory_tree::TreeManifest;

// Module declarations
mod error_tracking;
mod retry;
mod metrics;
mod listing;
mod sequential;
mod parallel;
pub mod directory_tree;  // Public for create_directory_tree
pub mod cleanup;  // Public for cleanup_prepared_objects, verify_prepared_objects

#[cfg(test)]
mod tests;

// Re-exports for public API
pub use error_tracking::{PrepareErrorTracker, PrepareFailure, ListingErrorTracker};
pub use retry::{RetrySuccess, RetryResults};
pub use metrics::{PreparedObject, PrepareMetrics};
pub use listing::{ListingResult, list_existing_objects_distributed};
pub use directory_tree::{create_tree_manifest_only, create_directory_tree, PathSelector};
pub use cleanup::{cleanup_prepared_objects, generate_cleanup_objects, verify_prepared_objects};

// Import module functions for internal use  
use metrics::compute_op_agg;
use sequential::prepare_sequential;
use parallel::prepare_parallel;
use directory_tree::finalize_tree_with_mkdir;

// Re-export for backward compatibility (so workload.rs can use imports)
pub use crate::workload::{create_store_for_uri, detect_pool_requirements};

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
///
/// v0.8.52+: Deferred retry with adaptive strategy based on failure rates
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
#[allow(clippy::too_many_arguments)]  // TODO: Refactor to params struct in future release
pub async fn prepare_objects(
    config: &PrepareConfig,
    workload: Option<&[crate::config::WeightedOp]>,
    live_stats_tracker: Option<Arc<crate::live_stats::LiveStatsTracker>>,
    multi_endpoint_config: Option<&crate::config::MultiEndpointConfig>,
    multi_ep_cache: &crate::workload::MultiEndpointCache,
    concurrency: usize,
    agent_id: usize,
    num_agents: usize,
    shared_storage: bool,  // v0.8.24: Only filter by agent_id in shared storage mode
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
            .and_then(|spec| {
                // Get effective base_uri - use first endpoint if base_uri is None
                let multi_endpoint_uris = multi_endpoint_config.map(|cfg| cfg.endpoints.as_slice());
                spec.get_base_uri(multi_endpoint_uris).ok()
            })
            .ok_or_else(|| anyhow!("directory_structure requires at least one ensure_objects entry for base_uri"))?;
        
        // Pass live_stats_tracker for progress reporting during tree generation (prevents timeout)
        let manifest = create_tree_manifest_only(
            config,
            agent_id,
            num_agents,
            &base_uri,
            live_stats_tracker.as_ref().map(|arc| arc.as_ref())
        )
        .context("Failed to create tree manifest. This may indicate:
            1. Invalid directory_structure config (check width/depth/files_per_dir)
            2. Memory exhaustion for very large trees (>1M directories)
            3. Filesystem path length limits exceeded")?;
        
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
            prepare_parallel(config, needs_separate_pools, &mut metrics, live_stats_tracker.clone(), multi_endpoint_config, multi_ep_cache, tree_manifest.as_ref(), concurrency, agent_id, num_agents, shared_storage).await?
        }
    };
    
    // Create directories if needed (after files exist with correct paths)
    if tree_manifest.is_some() {
        let base_uri = config.ensure_objects.first()
            .and_then(|spec| {
                let multi_endpoint_uris = multi_endpoint_config.map(|cfg| cfg.endpoints.as_slice());
                spec.get_base_uri(multi_endpoint_uris).ok()
            })
            .ok_or_else(|| anyhow!("Failed to get base_uri for finalizing tree"))?;
        finalize_tree_with_mkdir(config, &base_uri, &mut metrics, live_stats_tracker).await?;
    }
    
    // Finalize metrics
    metrics.wall_seconds = prepare_start.elapsed().as_secs_f64();
    metrics.objects_created = all_prepared.iter().filter(|obj| obj.created).count() as u64;
    metrics.objects_existed = all_prepared.iter().filter(|obj| !obj.created).count() as u64;
    
    // v0.8.23: Collect per-endpoint statistics from multi-endpoint stores
    metrics.endpoint_stats = crate::workload::collect_endpoint_stats(multi_ep_cache);
    
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
