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
use tracing::{debug, info, warn}; // v0.8.60: Added debug/warn for cache handling

use crate::config::{PrepareConfig, PrepareStrategy};
use crate::directory_tree::TreeManifest;
use crate::metadata_cache::endpoints_share_storage;  // v0.8.62: Shared storage detection

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
/// v0.8.60+: Metadata cache integration for state tracking and resume capability
///
/// # Arguments
/// * `config` - Prepare configuration (object counts, sizes, etc.)
/// * `workload` - Optional workload operations (used to determine if separate pools are needed)
/// * `live_stats_tracker` - Optional live stats tracker for progress reporting
/// * `multi_endpoint_config` - Multi-endpoint configuration
/// * `multi_ep_cache` - Multi-endpoint object store cache
/// * `concurrency` - Number of parallel workers for object creation
/// * `agent_id` - 0-based agent index (0, 1, 2, ...)
/// * `num_agents` - Total number of agents in distributed execution
/// * `shared_storage` - v0.8.24: Only filter by agent_id in shared storage mode
/// * `results_dir` - v0.8.60: Results directory for metadata cache (None disables cache)
/// * `full_config` - v0.8.60: Full config for cache hash generation (None disables cache)
/// 
/// # Behavior
/// - If num_agents == 1: Creates all objects (standalone mode)
/// - If num_agents > 1: Each agent creates only its assigned subset using modulo distribution
///   - Agent i creates object j if (j % num_agents == agent_id)
///   - Ensures no overlap and complete coverage across all agents
/// - If results_dir and full_config provided: Creates metadata cache for state tracking
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
    results_dir: Option<&std::path::Path>,  // v0.8.60: For metadata cache
    full_config: Option<&crate::config::Config>,  // v0.8.60: For cache hash generation
) -> Result<(Vec<PreparedObject>, Option<TreeManifest>, PrepareMetrics)> {
    let prepare_start = Instant::now();
    
    // v0.8.60: Create metadata cache if enabled (with comprehensive logging)
    let metadata_cache = if let (Some(res_dir), Some(cfg)) = (results_dir, full_config) {
        // Extract endpoint URIs for cache creation
        let endpoint_uris: Vec<String> = if let Some(multi_ep) = multi_endpoint_config {
            multi_ep.endpoints.clone()
        } else if let Some(first_spec) = config.ensure_objects.first() {
            // Single endpoint mode - try to extract from base_uri
            vec![first_spec.get_base_uri(None).unwrap_or_else(|_| "unknown://".to_string())]
        } else {
            Vec::new()
        };
        
        if !endpoint_uris.is_empty() {
            let config_hash = crate::metadata_cache::generate_config_hash(cfg);
            
            info!("╔══════════════════════════════════════════════════════════════╗");
            info!("║  METADATA CACHE: Creating distributed KV cache (fjall v3)   ║");
            info!("╚══════════════════════════════════════════════════════════════╝");
            info!("");
            info!("📂 Results directory: {}", res_dir.display());
            info!("🔑 Config hash: {}", config_hash);
            info!("📊 Endpoints: {} total", endpoint_uris.len());
            
            // v0.8.60: Pass agent_id to create agent-specific endpoint cache
            let agent_id_opt = if num_agents > 1 { Some(agent_id) } else { None };
            
            // v0.8.60: Extract KV cache base directory from config
            // Prefer per-agent override, fallback to global default, then system temp
            let kv_cache_base_dir = cfg.distributed.as_ref().and_then(|d| {
                // Try per-agent override first
                d.agents.get(agent_id).and_then(|a| a.kv_cache_dir.as_ref())
                    // Fallback to global distributed config
                    .or(d.kv_cache_dir.as_ref())
            });
            
            match crate::metadata_cache::MetadataCache::new(
                res_dir,
                &endpoint_uris,
                config_hash,
                agent_id_opt,
                kv_cache_base_dir.map(|p| p.as_path()),
            ).await {
                Ok(cache) => {
                    info!("");
                    info!("✅ Metadata cache initialization complete!");
                    info!("   This cache will track object states (Planned → Creating → Created)");
                    info!("   and enable resume capability for interrupted prepare operations.");
                    info!("");
                    Some(Arc::new(tokio::sync::Mutex::new(cache)))
                }
                Err(e) => {
                    warn!("Failed to create metadata cache (will proceed without cache): {}", e);
                    warn!("This is not fatal - prepare will work without resume capability.");
                    None
                }
            }
        } else {
            warn!("No endpoints detected for metadata cache - cache disabled");
            None
        }
    } else {
        // Cache disabled (results_dir or full_config not provided)
        if results_dir.is_none() {
            info!("Metadata cache disabled: no results directory provided");
        }
        if full_config.is_none() {
            info!("Metadata cache disabled: no config provided for hash generation");
        }
        None
    };
    
    // v0.8.60: Spawn periodic checkpoint tasks if enabled
    // Default: 300 seconds (5 minutes) to protect against data loss
    // Works for BOTH standalone (sai3-bench run) and distributed (sai3bench-ctl) modes
    // Top-level field has precedence; distributed.cache_checkpoint_interval_secs is for backward compat
    let checkpoint_interval = full_config
        .map(|cfg| cfg.cache_checkpoint_interval_secs)
        .unwrap_or(300);  // Default to 5 minutes if no config provided
    
    let mut checkpoint_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    if checkpoint_interval > 0 {
        if let Some(cache_arc) = metadata_cache.as_ref() {
            let cache = cache_arc.lock().await;
            let agent_id_opt = if num_agents > 1 { Some(agent_id) } else { None };
            
            info!("");
            info!("⏰ Periodic checkpointing ENABLED: {} second interval", checkpoint_interval);
            info!("   Protects against data loss in long-running prepares");
            
            // v0.8.62: Detect if endpoints share storage to avoid checkpoint race conditions
            let num_endpoints = cache.endpoints().len();
            let endpoint_uris: Vec<String> = cache.endpoints()
                .values()
                .map(|ep| ep.endpoint_uri().to_string())
                .collect();
            
            let shared_storage = if num_endpoints > 1 && !endpoint_uris.is_empty() {
                endpoints_share_storage(&endpoint_uris)
            } else {
                false  // Single endpoint can't have sharing issues
            };
            
            if num_endpoints > 1 {
                info!("   Multi-endpoint configuration ({} endpoints)", num_endpoints);
                if shared_storage {
                    info!("   ✓ Shared storage detected - using single checkpoint (endpoint 0)");
                    info!("     All endpoints can restore from same checkpoint");
                } else {
                    info!("   ✓ Independent storage detected - checkpointing each endpoint");
                    info!("     Each endpoint maintains separate checkpoint for its data");
                }
            }
            
            if shared_storage {
                // Shared storage: Only checkpoint first endpoint
                if let Some((_idx, first_endpoint)) = cache.endpoints().iter().next() {
                    let handle = first_endpoint.spawn_periodic_checkpoint(checkpoint_interval, agent_id_opt);
                    checkpoint_handles.push(handle);
                    debug!("Spawned periodic checkpoint task for endpoint 0 (shared storage mode)");
                }
            } else {
                // Independent storage: Checkpoint each endpoint separately
                for (idx, endpoint_cache) in cache.endpoints() {
                    let handle = endpoint_cache.spawn_periodic_checkpoint(checkpoint_interval, agent_id_opt);
                    checkpoint_handles.push(handle);
                    debug!("Spawned periodic checkpoint task for endpoint {}", idx);
                }
            }
        }
    } else {
        debug!("Periodic checkpointing disabled (interval = 0)");
    }
    
    // Keep checkpoint handles alive during prepare
    let _checkpoint_guards = checkpoint_handles;
    
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
            prepare_sequential(config, needs_separate_pools, &mut metrics, live_stats_tracker.clone(), multi_endpoint_config, multi_ep_cache, tree_manifest.as_ref(), concurrency, agent_id, num_agents, metadata_cache.clone()).await?
        }
        PrepareStrategy::Parallel => {
            info!("Using parallel prepare strategy (all sizes interleaved)");
            prepare_parallel(config, needs_separate_pools, &mut metrics, live_stats_tracker.clone(), multi_endpoint_config, multi_ep_cache, tree_manifest.as_ref(), concurrency, agent_id, num_agents, shared_storage, metadata_cache.clone()).await?
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
    
    // v0.8.60: Cancel periodic checkpoint tasks before final checkpoint
    // This prevents race between background tasks and final checkpoint
    for handle in _checkpoint_guards.into_iter() {
        handle.abort();
    }
    debug!("Cancelled periodic checkpoint tasks");
    
    // v0.8.60: CRITICAL - Persist KV cache to storage under test (FINAL checkpoint)
    // The metadata cache MUST be checkpointed from isolated location (e.g., /tmp/)
    // to the storage under test for resume capability and data integrity
    // Uses tar.zst archives that work for BOTH filesystem and cloud storage
    if let Some(cache_arc) = metadata_cache.as_ref() {
        let cache = cache_arc.lock().await;
        let agent_id_opt = if num_agents > 1 { Some(agent_id) } else { None };
        
        info!("");
        info!("╔══════════════════════════════════════════════════════════════╗");
        info!("║  CRITICAL: Persisting KV cache checkpoint to storage        ║");
        info!("╚══════════════════════════════════════════════════════════════╝");
        
        // v0.8.62: Detect shared vs independent storage for checkpoint strategy
        let num_endpoints = cache.endpoints().len();
        let endpoint_uris: Vec<String> = cache.endpoints()
            .values()
            .map(|ep| ep.endpoint_uri().to_string())
            .collect();
        
        let shared_storage = if num_endpoints > 1 && !endpoint_uris.is_empty() {
            endpoints_share_storage(&endpoint_uris)
        } else {
            false
        };
        
        if num_endpoints > 1 {
            if shared_storage {
                info!("   Shared storage: checkpointing via endpoint 0 only");
            } else {
                info!("   Independent storage: checkpointing all {} endpoints", num_endpoints);
            }
        }
        
        let mut checkpoint_success_count = 0;
        let mut checkpoint_failure_count = 0;
        
        if shared_storage {
            // Shared storage: Only checkpoint first endpoint
            if let Some((_idx, first_endpoint)) = cache.endpoints().iter().next() {
                match first_endpoint.write_checkpoint(agent_id_opt).await {
                    Ok(checkpoint_location) => {
                        info!("✅ Checkpoint created: {}", checkpoint_location);
                        info!("   All {} endpoints can restore from this checkpoint", num_endpoints);
                        checkpoint_success_count += 1;
                    }
                    Err(e) => {
                        warn!("⚠️  Failed to create final checkpoint: {}", e);
                        warn!("   Prepare completed successfully but resume capability unavailable");
                        checkpoint_failure_count += 1;
                    }
                }
            }
        } else {
            // Independent storage: Checkpoint each endpoint separately
            for (idx, endpoint_cache) in cache.endpoints() {
                match endpoint_cache.write_checkpoint(agent_id_opt).await {
                    Ok(checkpoint_location) => {
                        info!("✅ Endpoint {} checkpoint created: {}", idx, checkpoint_location);
                        checkpoint_success_count += 1;
                    }
                    Err(e) => {
                        warn!("⚠️  Endpoint {} checkpoint failed: {}", idx, e);
                        warn!("   Resume capability unavailable for endpoint {}", idx);
                        checkpoint_failure_count += 1;
                    }
                }
            }
        }
        
        info!("");
        info!("✅ Prepare phase complete - all objects created successfully");
        if checkpoint_success_count > 0 {
            info!("   Resume capability: enabled ({} checkpoint(s) created)", checkpoint_success_count);
        }
        if checkpoint_failure_count > 0 {
            warn!("   ⚠️  {} checkpoint(s) failed (non-fatal - prepared objects are intact)", checkpoint_failure_count);
        }
        info!("");
    }
    
    Ok((all_prepared, tree_manifest, metrics))
}
