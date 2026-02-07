//! Parallel prepare strategy with multi-endpoint support
//!
//! High-throughput object creation using concurrent workers (tokio tasks),
//! suitable for large-scale datasets. Includes round-robin distribution for
//! multi-endpoint configurations.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::config::{FillPattern, PrepareConfig};
use crate::constants::{
    DEFAULT_PREPARE_MAX_ERRORS,
    DEFAULT_PREPARE_MAX_CONSECUTIVE_ERRORS,
};
use crate::directory_tree::TreeManifest;
use crate::size_generator::SizeGenerator;
use crate::workload::{MultiEndpointCache, create_store_for_uri, RetryConfig, retry_with_backoff, RetryResult};
use super::error_tracking::PrepareErrorTracker;
use super::retry::retry_failed_objects;
use super::metrics::{PreparedObject, PrepareMetrics};
use super::listing::list_existing_objects_distributed;

pub(crate) async fn prepare_parallel(
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
    shared_storage: bool,  // v0.8.24: Only filter by agent_id in shared storage mode
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
        // Get endpoints for file distribution
        // v0.8.24: Use all endpoints for multi-endpoint mode, not just the first one
        let endpoints: Vec<String> = if spec.use_multi_endpoint {
            if let Some(multi_ep) = multi_endpoint_config {
                multi_ep.endpoints.clone()
            } else {
                // Fallback: use get_base_uri if no multi_endpoint config
                vec![spec.get_base_uri(None).context("Failed to determine base_uri")?]
            }
        } else {
            // Single endpoint mode
            let multi_endpoint_uris = multi_endpoint_config.map(|cfg| cfg.endpoints.as_slice());
            vec![spec.get_base_uri(multi_endpoint_uris).context("Failed to determine base_uri for prepare phase")?]
        };
        
        // Use first endpoint for listing (MultiEndpointStore will handle distribution during listing)
        let base_uri = &endpoints[0];
        
        for (prefix, is_readonly) in &pools_to_create {
            let pool_desc = if needs_separate_pools {
                if *is_readonly { " (readonly pool for GET/STAT)" } else { " (deletable pool for DELETE)" }
            } else {
                ""
            };
            
            info!("Preparing{}: {} objects at {}", pool_desc, spec.count, base_uri);
            
            // v0.8.22: Multi-endpoint support for prepare phase
            // If use_multi_endpoint=true, create MultiEndpointStore instead of single-endpoint store
            // This distributes object creation across all endpoints for maximum network bandwidth
            let store: Box<dyn s3dlio::object_store::ObjectStore> = if spec.use_multi_endpoint {
                if let Some(multi_ep) = multi_endpoint_config {
                    info!("  ✓ Using multi-endpoint configuration: {} endpoints, {} strategy", 
                          multi_ep.endpoints.len(), multi_ep.strategy);
                    
                    // Create cache key for prepare phase multi-endpoint store
                    let cache_key = format!("prepare_par:{}:{}:{}",
                        base_uri,
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
                create_store_for_uri(base_uri)?
            };
            
            // List existing objects with this prefix (unless skip_verification is enabled)
            // Issue #40: skip_verification config option
            // v0.8.24: force_overwrite overrides skip_verification to recreate all files
            // v0.7.9: If tree manifest exists, files are nested in directories (e.g., scan.d0_w0.dir/file_*.dat)
            // v0.7.9: Parse filenames to extract indices for gap-filling
            // v0.8.29: Track whether an actual LIST was performed (for accurate log messages)
            let mut did_list = false;
            let (existing_count, existing_indices) = if config.skip_verification && !config.force_overwrite {
                info!("  ⚡ skip_verification enabled - assuming all {} objects exist", spec.count);
                (spec.count, HashSet::new())  // Assume all files exist, no gaps
            } else if config.force_overwrite {
                info!("  🔨 force_overwrite enabled - creating all {} objects", spec.count);
                (0, HashSet::new())  // Assume no files exist, create everything
            } else if tree_manifest.is_some() {
                did_list = true;
                // v0.8.14: Use distributed listing with progress updates
                let listing_result = list_existing_objects_distributed(
                    store.as_ref(),
                    base_uri,
                    tree_manifest,
                    agent_id,
                    num_agents,
                    live_stats_tracker.as_ref(),
                    spec.count,
                ).await.context("Failed to list existing objects")?;
                
                (listing_result.file_count, listing_result.indices)
            } else {
                // Flat file mode: use streaming list with progress
                let pattern = if base_uri.ends_with('/') {
                    format!("{}{}-", base_uri, prefix)
                } else {
                    format!("{}/{}-", base_uri, prefix)
                };
                
                info!("  [Flat file mode] Listing with pattern: {}", pattern);
                did_list = true;
                
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
            
            // v0.8.29: Only say "Found" when an actual LIST was done
            if did_list {
                info!("  ✓ Listed {} existing {} objects (need {})", existing_count, prefix, spec.count);
            }
            
            // Store existing count and indices for this pool
            let pool_key = (base_uri.clone(), prefix.to_string());
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
                let seed = base_uri.as_bytes().iter().fold(0u64, |acc, &b| acc.wrapping_mul(31).wrapping_add(b as u64));
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
                
                // v0.8.24: Only filter by agent_id in SHARED storage mode
                // In isolated mode, each agent creates ALL files on its own storage
                if shared_storage && num_agents > 1 {
                    missing_indices.retain(|&idx| (idx as usize % num_agents) == agent_id);
                    info!("  [Distributed prepare, shared storage] Agent {}/{} responsible for {} of {} missing objects",
                        agent_id, num_agents, missing_indices.len(), to_create);
                } else if num_agents > 1 {
                    info!("  [Distributed prepare, isolated storage] Agent {}/{} creating all {} objects on own storage",
                        agent_id, num_agents, missing_indices.len());
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
                // v0.8.24: Round-robin across endpoints for multi-endpoint mode
                for &missing_idx in &missing_indices {
                    let size = all_sizes[missing_idx as usize];
                    
                    // Select endpoint using round-robin distribution
                    let endpoint = &endpoints[missing_idx as usize % endpoints.len()];
                    
                    task_specs.push(TaskSpec {
                        size,
                        store_uri: endpoint.clone(),
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
            // Get endpoints for file distribution (same logic as creation)
            // v0.8.24: Use all endpoints for multi-endpoint mode
            let endpoints: Vec<String> = if spec.use_multi_endpoint {
                if let Some(multi_ep) = multi_endpoint_config {
                    multi_ep.endpoints.clone()
                } else {
                    vec![spec.get_base_uri(None).context("Failed to determine base_uri")?]
                }
            } else {
                let multi_endpoint_uris = multi_endpoint_config.map(|cfg| cfg.endpoints.as_slice());
                vec![spec.get_base_uri(multi_endpoint_uris)
                    .context("Failed to determine base_uri for reconstructing existing objects")?]
            };
            
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
                    // Round-robin across endpoints
                    let base_uri = &endpoints[i as usize % endpoints.len()];
                    
                    let uri = if let Some(manifest) = tree_manifest {
                        // Tree mode: use manifest paths
                        if let Some(rel_path) = manifest.get_file_path(i as usize) {
                            if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, rel_path)
                            } else {
                                format!("{}/{}", base_uri, rel_path)
                            }
                        } else {
                            continue;  // Skip if manifest doesn't have this index
                        }
                    } else {
                        // Flat mode: traditional naming
                        let key = format!("{}-{:08}.dat", prefix, i);
                        if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, key)
                        } else {
                            format!("{}/{}", base_uri, key)
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
    // v0.8.24: Skip shuffle for very large task counts (>1M) to avoid blocking
    // Shuffling 16M entries can take 20+ seconds and block the async runtime,
    // causing gRPC connection timeouts. Size distribution is already good enough
    // from lognormal sampling without shuffling.
    if task_specs.len() > 1_000_000 {
        info!("Skipping shuffle for {} tasks (>1M threshold) to avoid blocking", task_specs.len());
    } else {
        info!("Shuffling {} tasks to distribute sizes evenly across directories", task_specs.len());
        let mut rng = rand::rngs::StdRng::seed_from_u64(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs());
        task_specs.shuffle(&mut rng);
    }
    
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
            // Get endpoints for file distribution (same logic as above)
            // v0.8.24: Use all endpoints for multi-endpoint mode
            let endpoints: Vec<String> = if spec.use_multi_endpoint {
                if let Some(multi_ep) = multi_endpoint_config {
                    multi_ep.endpoints.clone()
                } else {
                    vec![spec.get_base_uri(None).context("Failed to determine base_uri")?]
                }
            } else {
                let multi_endpoint_uris = multi_endpoint_config.map(|cfg| cfg.endpoints.as_slice());
                vec![spec.get_base_uri(multi_endpoint_uris)
                    .context("Failed to determine base_uri for assigning URIs to existing objects")?]
            };
            
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
                    // Round-robin across endpoints
                    let base_uri = &endpoints[i as usize % endpoints.len()];
                    
                    let uri = if let Some(manifest) = tree_manifest {
                        // Tree mode: use manifest paths
                        if let Some(rel_path) = manifest.get_file_path(i as usize) {
                            if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, rel_path)
                            } else {
                                format!("{}/{}", base_uri, rel_path)
                            }
                        } else {
                            continue;  // Skip if manifest doesn't have this index
                        }
                    } else {
                        // Flat mode: traditional naming
                        let key = format!("{}-{:08}.dat", prefix, i);
                        if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, key)
                        } else {
                            format!("{}/{}", base_uri, key)
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
    let mut yield_counter = 0u64;  // v0.8.51: Counter for periodic yields
    
    while let Some(result) = futs.next().await {
        // v0.8.51: Yield every 100 operations to prevent executor starvation
        yield_counter += 1;
        if yield_counter.is_multiple_of(100) {
            tokio::task::yield_now().await;
        }
        
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
        
        // v0.8.52: DEFERRED RETRY PHASE - Retry all failed objects
        // This happens AFTER the fast path completes, so no performance impact on main create loop
        let failures = error_tracker.get_failures();
        if !failures.is_empty() {
            info!("🔄 Starting deferred retry phase for {} failed objects...", failures.len());
            
            let retry_results = retry_failed_objects(
                failures,
                &store_cache,
                live_stats_tracker.as_ref(),
                concurrency / 4,  // Use fewer workers for retries to avoid overwhelming backend
                total_to_create as usize,  // Total attempted for adaptive retry
            ).await;
            
            // Update metrics with retry results
            for result in &retry_results.successes {
                metrics.put.bytes += result.size;
                metrics.put.ops += 1;
                metrics.put_bins.add(result.size);
                let bucket = crate::metrics::bucket_index(result.size as usize);
                metrics.put_hists.record(bucket, result.latency);
                
                all_prepared.push(PreparedObject {
                    uri: result.uri.clone(),
                    size: result.size,
                    created: true,
                });
            }
            
            // Report retry statistics
            info!("✅ Retry phase complete: {} succeeded, {} permanently failed", 
                  retry_results.successes.len(), retry_results.permanent_failures.len());
            
            if !retry_results.permanent_failures.is_empty() {
                warn!("❌ {} objects failed even after retries:", retry_results.permanent_failures.len());
                for (uri, error) in retry_results.permanent_failures.iter().take(10) {
                    warn!("  - {}: {}", uri, error);
                }
                if retry_results.permanent_failures.len() > 10 {
                    warn!("  ... and {} more", retry_results.permanent_failures.len() - 10);
                }
            }
        }
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

