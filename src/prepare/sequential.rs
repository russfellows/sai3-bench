//! Sequential prepare strategy
//!
//! Single-threaded object creation with deterministic ordering, suitable for 
//! small datasets or when ordering is important.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::config::{FillPattern, PrepareConfig};
use crate::directory_tree::TreeManifest;
use crate::size_generator::SizeGenerator;
use crate::workload::{MultiEndpointCache, create_store_for_uri, RetryConfig, retry_with_backoff, RetryResult};
use super::error_tracking::PrepareErrorTracker;
use super::retry::retry_failed_objects;
use super::metrics::{PreparedObject, PrepareMetrics};
use super::listing::list_existing_objects_distributed;

pub(crate) async fn prepare_sequential(
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
    metadata_cache: Option<Arc<tokio::sync::Mutex<crate::metadata_cache::MetadataCache>>>,  // v0.8.60: KV cache
) -> Result<Vec<PreparedObject>> {
    // v0.8.60: Log cache status
    if metadata_cache.is_some() {
        info!("📊 Metadata cache ENABLED for sequential prepare");
    } else {
        info!("Metadata cache disabled for sequential prepare");
    }
    
    let mut all_prepared = Vec::new();
    
    for spec in &config.ensure_objects {
        // Determine which pool(s) to create based on workload requirements
        let pools_to_create = if needs_separate_pools {
            vec![("prepared", true), ("deletable", false)]  // (prefix, is_readonly)
        } else {
            vec![("prepared", false)]  // Single pool (backward compatible)
        };
        
        // Get effective base_uri for this agent (may use first endpoint in isolated mode)
        let multi_endpoint_uris = multi_endpoint_config.map(|cfg| cfg.endpoints.as_slice());
        let base_uri = spec.get_base_uri(multi_endpoint_uris)
            .context("Failed to determine base_uri for prepare phase")?;
        
        for (prefix, is_readonly) in pools_to_create {
            let pool_desc = if needs_separate_pools {
                if is_readonly { " (readonly pool for GET/STAT)" } else { " (deletable pool for DELETE)" }
            } else {
                ""
            };
            
            info!("Preparing objects{}: {} at {}", pool_desc, spec.count, base_uri);
            
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
                    
                    // Wrap for Arc<Box<dyn ObjectStore>>
                    Arc::new(Box::new(crate::workload::ArcMultiEndpointWrapper(arc_multi_store)) as Box<dyn s3dlio::object_store::ObjectStore>)
                } else {
                    anyhow::bail!("use_multi_endpoint=true but no multi_endpoint configuration provided");
                }
            } else {
                Arc::new(create_store_for_uri(&base_uri)?)
            };
            
            // 1. List existing objects with this prefix (unless skip_verification is enabled)
            // Issue #40: skip_verification config option
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
                    shared_store.as_ref().as_ref(),
                    &base_uri,
                    tree_manifest,
                    agent_id,
                    num_agents,
                    live_stats_tracker.as_ref(),
                    spec.count,
                ).await.context("Failed to list existing objects")?;
                
                (listing_result.file_count, listing_result.indices)
            } else {
                // Flat file mode: use streaming list with progress
                let list_pattern = if base_uri.ends_with('/') {
                    format!("{}{}-", base_uri, prefix)
                } else {
                    format!("{}/{}-", base_uri, prefix)
                };
                
                info!("  [Flat file mode] Listing with pattern: {}", list_pattern);
                did_list = true;
                
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
            
            // v0.8.29: Only say "Found" when an actual LIST was done
            if did_list {
                info!("  ✓ Listed {} existing {} objects (need {})", existing_count, prefix, spec.count);
            }
            
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
                            let uri = if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, rel_path)
                            } else {
                                format!("{}/{}", base_uri, rel_path)
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
                        let uri = if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, key)
                        } else {
                            format!("{}/{}", base_uri, key)
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
                
                // v0.7.9: Stream deterministic sizes instead of pre-generating all
                let size_spec = spec.get_size_spec();
                let seed = base_uri.as_bytes().iter().fold(0u64, |acc, &b| acc.wrapping_mul(31).wrapping_add(b as u64));
                let mut size_generator = SizeGenerator::new_with_seed(&size_spec, seed)
                    .context("Failed to create size generator")?;
                
                info!("  [v0.7.9] Streaming size generation with seed {} for deterministic gap-filling", seed);
                
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
                
                // v0.7.9: Generate tasks for missing indices with streamed sizes
                let mut tasks: Vec<(String, u64)> = Vec::with_capacity(missing_indices.len());
                
                if let Some(manifest) = tree_manifest {
                    for idx in 0..spec.count {
                        let size = size_generator.generate();
                        if existing_indices.contains(&idx) {
                            continue;
                        }
                        if num_agents > 1 && (idx as usize % num_agents) != agent_id {
                            continue;
                        }
                        if let Some(rel_path) = manifest.get_file_path(idx as usize) {
                            let uri = if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, rel_path)
                            } else {
                                format!("{}/{}", base_uri, rel_path)
                            };
                            tasks.push((uri, size));
                        } else {
                            warn!("No file path for missing index {} in tree manifest", idx);
                        }
                    }
                } else {
                    for idx in 0..spec.count {
                        let size = size_generator.generate();
                        if existing_indices.contains(&idx) {
                            continue;
                        }
                        if num_agents > 1 && (idx as usize % num_agents) != agent_id {
                            continue;
                        }
                        let key = format!("{}-{:08}.dat", prefix, idx);
                        let uri = if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, key)
                        } else {
                            format!("{}/{}", base_uri, key)
                        };
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
                let mut yield_counter = 0u64;  // v0.8.51: Counter for periodic yields
                
                while let Some(result) = futs.next().await {
                    // v0.8.51: Yield every 100 operations to prevent executor starvation
                    yield_counter += 1;
                    if yield_counter.is_multiple_of(100) {
                        tokio::task::yield_now().await;
                    }
                    
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
                    
                    // v0.8.52: DEFERRED RETRY PHASE - Retry all failed objects
                    // Create minimal store cache for this pool's URI
                    let mut store_map = std::collections::HashMap::new();
                    store_map.insert(base_uri.clone(), shared_store.clone());
                    let store_cache = Arc::new(store_map);
                    
                    let failures = error_tracker.get_failures();
                    if !failures.is_empty() {
                        info!("🔄 Starting deferred retry phase for {} failed objects in pool {}...", 
                              failures.len(), prefix);
                        
                        let retry_results = retry_failed_objects(
                            failures,
                            &store_cache,
                            live_stats_tracker.as_ref(),
                            concurrency / 4,  // Use fewer workers for retries
                            actual_to_create as usize,  // Total attempted for adaptive retry
                        ).await;
                        
                        // Update metrics with retry results
                        for result in &retry_results.successes {
                            metrics.put.bytes += result.size;
                            metrics.put.ops += 1;
                            metrics.put_bins.add(result.size);
                            let bucket = crate::metrics::bucket_index(result.size as usize);
                            metrics.put_hists.record(bucket, result.latency);
                            
                            created_objects.push(PreparedObject {
                                uri: result.uri.clone(),
                                size: result.size,
                                created: true,
                            });
                        }
                        
                        // Report retry statistics
                        info!("✅ Retry phase for pool {} complete: {} succeeded, {} permanently failed",
                              prefix, retry_results.successes.len(), retry_results.permanent_failures.len());
                        
                        if !retry_results.permanent_failures.is_empty() {
                            warn!("❌ {} objects in pool {} failed even after retries (first 10):",
                                  retry_results.permanent_failures.len(), prefix);
                            for (uri, error) in retry_results.permanent_failures.iter().take(10) {
                                warn!("  - {}: {}", uri, error);
                            }
                        }
                    }
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

