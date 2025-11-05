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
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::config::{FillPattern, PrepareConfig, PrepareStrategy};
use crate::directory_tree::TreeManifest;
use crate::size_generator::SizeGenerator;

// Re-export for backward compatibility (so workload.rs can use via workload::PreparedObject)
pub use crate::workload::{create_store_for_uri, detect_pool_requirements};

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
pub async fn prepare_objects(
    config: &PrepareConfig,
    workload: Option<&[crate::config::WeightedOp]>
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
    
    // Choose execution strategy based on config
    let all_prepared = match config.prepare_strategy {
        PrepareStrategy::Sequential => {
            info!("Using sequential prepare strategy (one size group at a time)");
            prepare_sequential(config, needs_separate_pools, &mut metrics).await?
        }
        PrepareStrategy::Parallel => {
            info!("Using parallel prepare strategy (all sizes interleaved)");
            prepare_parallel(config, needs_separate_pools, &mut metrics).await?
        }
    };
    
    // Create directory tree if configured and collect mkdir metrics
    let tree_manifest = finalize_prepare_with_tree(config, &mut metrics).await?;
    
    // Finalize metrics
    metrics.wall_seconds = prepare_start.elapsed().as_secs_f64();
    metrics.objects_created = all_prepared.iter().filter(|obj| obj.created).count() as u64;
    metrics.objects_existed = all_prepared.iter().filter(|obj| !obj.created).count() as u64;
    
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
async fn prepare_sequential(
    config: &PrepareConfig,
    needs_separate_pools: bool,
    metrics: &mut PrepareMetrics,
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
            
            let store = create_store_for_uri(&spec.base_uri)?;
            
            // 1. List existing objects with this prefix
            let list_pattern = if spec.base_uri.ends_with('/') {
                format!("{}{}-", spec.base_uri, prefix)
            } else {
                format!("{}/{}-", spec.base_uri, prefix)
            };
            
            let existing = store.list(&list_pattern, true).await
                .context("Failed to list existing objects")?;
            
            let existing_count = existing.len() as u64;
            info!("  Found {} existing {} objects", existing_count, prefix);
            
            // 2. Calculate how many to create
            let to_create = if existing_count >= spec.count {
                info!("  Sufficient {} objects already exist", prefix);
                0
            } else {
                spec.count - existing_count
            };
            
            // 3. Create missing objects
            if to_create > 0 {
                use futures::stream::{FuturesUnordered, StreamExt};
                
                // Create size generator from spec
                let size_spec = spec.get_size_spec();
                let size_generator = SizeGenerator::new(&size_spec)
                    .context("Failed to create size generator")?;
                
                // Determine concurrency for prepare (use 32 like default workload concurrency)
                let concurrency = 32;
                
                info!("  Creating {} additional {} objects with {} workers (sizes: {}, fill: {:?}, dedup: {}, compress: {})", 
                    to_create, prefix, concurrency, size_generator.description(), spec.fill, 
                    spec.dedup_factor, spec.compress_factor);
                
                // Create atomic counters for live stats
                let live_ops = Arc::new(AtomicU64::new(0));
                let live_bytes = Arc::new(AtomicU64::new(0));
                
                // Create progress bar for preparation
                let pb = ProgressBar::new(to_create);
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
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        
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
                
                // Pre-generate all URIs and sizes for parallel execution
                let mut tasks: Vec<(String, u64)> = Vec::with_capacity(to_create as usize);
                for i in 0..to_create {
                    let key = format!("{}-{:08}.dat", prefix, existing_count + i);
                    let uri = if spec.base_uri.ends_with('/') {
                        format!("{}{}", spec.base_uri, key)
                    } else {
                        format!("{}/{}", spec.base_uri, key)
                    };
                    let size = size_generator.generate();
                    tasks.push((uri, size));
                }
                
                // Execute PUT operations in parallel with semaphore-controlled concurrency
                let sem = Arc::new(Semaphore::new(concurrency));
                let mut futs = FuturesUnordered::new();
                let pb_clone = pb.clone();
                
                for (uri, size) in tasks {
                    let sem2 = sem.clone();
                    let store_uri = spec.base_uri.clone();
                    let fill = spec.fill;
                    let dedup = spec.dedup_factor;
                    let compress = spec.compress_factor;
                    let pb2 = pb_clone.clone();
                    let ops_counter = live_ops.clone();
                    let bytes_counter = live_bytes.clone();
                    
                    futs.push(tokio::spawn(async move {
                        let _permit = sem2.acquire_owned().await.unwrap();
                        
                        // Generate data using s3dlio's controlled data generation
                        let data = match fill {
                            FillPattern::Zero => vec![0u8; size as usize],
                            FillPattern::Random => {
                                s3dlio::generate_controlled_data(size as usize, dedup, compress)
                            }
                        };
                        
                        // Create store instance for this task
                        let store = create_store_for_uri(&store_uri)?;
                        
                        // PUT object with timing
                        let put_start = Instant::now();
                        store.put(&uri, &data).await
                            .with_context(|| format!("Failed to PUT {}", uri))?;
                        let latency_us = put_start.elapsed().as_micros() as u64;
                        
                        // Update live counters
                        ops_counter.fetch_add(1, Ordering::Relaxed);
                        bytes_counter.fetch_add(size, Ordering::Relaxed);
                        
                        pb2.inc(1);
                        
                        Ok::<(String, u64, u64), anyhow::Error>((uri, size, latency_us))
                    }));
                }
                
                // Collect results as they complete
                let mut created_objects = Vec::with_capacity(to_create as usize);
                while let Some(result) = futs.next().await {
                    let (uri, size, latency_us) = result
                        .context("Task join error")??;
                    
                    // Update metrics
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
                
                // Wait for monitoring task to complete cleanly
                monitor_handle.await.ok();
                
                pb.finish_with_message(format!("created {} {} objects", to_create, prefix));
                
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
async fn prepare_parallel(
    config: &PrepareConfig,
    needs_separate_pools: bool,
    metrics: &mut PrepareMetrics,
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
            
            let store = create_store_for_uri(&spec.base_uri)?;
            
            // List existing objects with this prefix
            let list_pattern = if spec.base_uri.ends_with('/') {
                format!("{}{}-", spec.base_uri, prefix)
            } else {
                format!("{}/{}-", spec.base_uri, prefix)
            };
            
            let existing = store.list(&list_pattern, true).await
                .context("Failed to list existing objects")?;
            
            let existing_count = existing.len() as u64;
            info!("  Found {} existing {} objects", existing_count, prefix);
            
            // Store existing count for this pool
            let pool_key = (spec.base_uri.clone(), prefix.to_string());
            existing_count_per_pool.insert(pool_key.clone(), existing_count);
            
            // Calculate how many to create
            let to_create = if existing_count >= spec.count {
                info!("  Sufficient {} objects already exist", prefix);
                0
            } else {
                spec.count - existing_count
            };
            
            // Generate task specs (sizes but no URIs yet) for missing objects
            if to_create > 0 {
                let size_spec = spec.get_size_spec();
                let size_generator = SizeGenerator::new(&size_spec)
                    .context("Failed to create size generator")?;
                
                info!("  Will create {} additional {} objects (sizes: {}, fill: {:?}, dedup: {}, compress: {})", 
                    to_create, prefix, size_generator.description(), spec.fill, 
                    spec.dedup_factor, spec.compress_factor);
                
                // Generate all sizes for this spec
                for _ in 0..to_create {
                    let size = size_generator.generate();
                    
                    task_specs.push(TaskSpec {
                        size,
                        store_uri: spec.base_uri.clone(),
                        fill: spec.fill,
                        dedup: spec.dedup_factor,
                        compress: spec.compress_factor,
                        prefix: prefix.to_string(),
                    });
                }
                
                total_to_create += to_create;
            }
        }
    }
    
    if task_specs.is_empty() {
        info!("All objects already exist, no preparation needed");
        return Ok(Vec::new());
    }
    
    // Phase 2: Shuffle task specs to mix sizes across directories
    // Use StdRng which is Send-safe for async contexts
    info!("Shuffling {} tasks to distribute sizes evenly across directories", task_specs.len());
    let mut rng = rand::rngs::StdRng::seed_from_u64(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs());
    task_specs.shuffle(&mut rng);
    
    // Phase 3: Assign sequential URIs to shuffled tasks
    // Track next index per (base_uri, prefix) combination
    let mut next_index_per_pool: std::collections::HashMap<(String, String), u64> = std::collections::HashMap::new();
    for (key, existing_count) in &existing_count_per_pool {
        next_index_per_pool.insert(key.clone(), *existing_count);
    }
    
    let mut all_tasks: Vec<PrepareTask> = Vec::with_capacity(task_specs.len());
    for spec in task_specs {
        let pool_key = (spec.store_uri.clone(), spec.prefix.clone());
        let idx = next_index_per_pool.get_mut(&pool_key).unwrap();
        
        let key = format!("{}-{:08}.dat", spec.prefix, *idx);
        let uri = if spec.store_uri.ends_with('/') {
            format!("{}{}", spec.store_uri, key)
        } else {
            format!("{}/{}", spec.store_uri, key)
        };
        
        all_tasks.push(PrepareTask {
            uri,
            size: spec.size,
            store_uri: spec.store_uri,
            fill: spec.fill,
            dedup: spec.dedup,
            compress: spec.compress,
        });
        
        *idx += 1;
    }
    
    // Phase 4: Execute all tasks in parallel with unified progress bar
    info!("Creating {} total objects in parallel (sizes shuffled for even distribution)", total_to_create);
    
    let concurrency = 32; // Match sequential strategy
    
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
            tokio::time::sleep(Duration::from_millis(100)).await;
            
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
    
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut futs = FuturesUnordered::new();
    let pb_clone = pb.clone();
    
    for task in all_tasks {
        let sem2 = sem.clone();
        let pb2 = pb_clone.clone();
        let ops_counter = live_ops.clone();
        let bytes_counter = live_bytes.clone();
        
        futs.push(tokio::spawn(async move {
            let _permit = sem2.acquire_owned().await.unwrap();
            
            // Generate data using s3dlio's controlled data generation
            let data = match task.fill {
                FillPattern::Zero => vec![0u8; task.size as usize],
                FillPattern::Random => {
                    s3dlio::generate_controlled_data(task.size as usize, task.dedup, task.compress)
                }
            };
            
            // Create store instance for this task
            let store = create_store_for_uri(&task.store_uri)?;
            
            // PUT object with timing
            let put_start = Instant::now();
            store.put(&task.uri, &data).await
                .with_context(|| format!("Failed to PUT {}", task.uri))?;
            let latency_us = put_start.elapsed().as_micros() as u64;
            
            // Update live counters
            ops_counter.fetch_add(1, Ordering::Relaxed);
            bytes_counter.fetch_add(task.size, Ordering::Relaxed);
            
            pb2.inc(1);
            
            Ok::<(String, u64, u64), anyhow::Error>((task.uri, task.size, latency_us))
        }));
    }
    
    // Collect results as they complete
    let mut all_prepared = Vec::with_capacity(total_to_create as usize);
    while let Some(result) = futs.next().await {
        let (uri, size, latency_us) = result
            .context("Task join error")??;
        
        // Update metrics
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
    
    // Wait for monitoring task to complete cleanly
    monitor_handle.await.ok();
    
    pb.finish_with_message(format!("created {} objects (all sizes)", total_to_create));
    
    Ok(all_prepared)
}

/// Continue with directory tree creation after prepare strategy completes
async fn finalize_prepare_with_tree(
    config: &PrepareConfig,
    metrics: &mut PrepareMetrics,
) -> Result<Option<TreeManifest>> {
    // v0.7.0: Create directory tree if configured
    let tree_manifest = if config.directory_structure.is_some() {
        info!("Creating directory tree structure...");
        // For now, use agent_id=0, num_agents=1 (distributed config integration is TODO)
        // This will be properly extracted from distributed config in a future update
        let agent_id = 0;
        let num_agents = 1;
        
        // Need to extract base_uri from ensure_objects (first entry)
        let base_uri = config.ensure_objects.first()
            .map(|spec| spec.base_uri.as_str())
            .ok_or_else(|| anyhow!("directory_structure requires at least one ensure_objects entry for base_uri"))?;
        
        let manifest = create_directory_tree(
            config,
            agent_id,
            num_agents,
            base_uri,
            metrics,  // Pass metrics to collect mkdir latencies
        ).await?;
        
        info!("Directory tree created: {} total directories",
            manifest.all_directories.len()
        );
        
        Some(manifest)
    } else {
        None
    };
    
    Ok(tree_manifest)
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
        
        info!("Creating {} files across {} directories...", 
            total_files, dirs_with_files.len());
        
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
        let size_generator = SizeGenerator::new(&size_spec)
            .context("Failed to create size generator for tree files")?;
        
        info!("File size: {}, fill: {:?}, dedup: {}, compress: {}", 
            size_generator.description(), fill_pattern, dedup_factor, compress_factor);
        
        let pb = ProgressBar::new(total_files as u64);
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
                // CORRECT PATTERN: Check if this global file index belongs to this agent
                let assigned_agent = global_file_idx % num_agents;
                
                if assigned_agent == agent_id {
                    // This file belongs to us - create it
                    let file_name = manifest.get_file_name(global_file_idx);
                    let file_uri = format!("{}/{}", dir_uri, file_name);
                    
                    // Generate file data using EXACT same pattern as regular prepare_objects
                    let size = size_generator.generate();
                    let data = match fill_pattern {
                        FillPattern::Zero => vec![0u8; size as usize],
                        FillPattern::Random => {
                            s3dlio::generate_controlled_data(size as usize, dedup_factor, compress_factor)
                        }
                    };
                    
                    store.put(&file_uri, &data).await
                        .with_context(|| format!("Failed to create file: {}", file_uri))?;
                    
                    pb.inc(1);
                }
                
                // CRITICAL: Always increment global index, even if we skip this file
                global_file_idx += 1;
            }
        }
        
        pb.finish_with_message(format!("files created (agent {}/{})", agent_id, num_agents));
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
pub async fn cleanup_prepared_objects(objects: &[PreparedObject]) -> Result<()> {
    if objects.is_empty() {
        return Ok(());
    }
    
    use futures::stream::{FuturesUnordered, StreamExt};
    
    let to_delete: Vec<_> = objects.iter()
        .filter(|obj| obj.created)
        .collect();
    
    if to_delete.is_empty() {
        info!("No objects to clean up");
        return Ok(());
    }
    
    let delete_count = to_delete.len();
    
    // Use same concurrency as prepare stage (32 workers)
    let concurrency = 32;
    info!("Cleaning up {} prepared objects with {} workers", delete_count, concurrency);
    
    // Create progress bar for cleanup
    let pb = ProgressBar::new(delete_count as u64);
    pb.set_style(ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects ({per_sec}) {msg}"
    )?);
    pb.set_message(format!("cleaning up with {} workers", concurrency));
    
    // Execute DELETE operations in parallel with semaphore-controlled concurrency
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut futs = FuturesUnordered::new();
    let pb_clone = pb.clone();
    
    for obj in to_delete {
        let sem2 = sem.clone();
        let pb2 = pb_clone.clone();
        let uri = obj.uri.clone();
        
        futs.push(tokio::spawn(async move {
            let _permit = sem2.acquire_owned().await.unwrap();
            
            // Create store and delete object
            // Note: We intentionally don't fail the entire cleanup if a single delete fails
            // Instead, we log warnings and continue with remaining deletions
            match create_store_for_uri(&uri) {
                Ok(store) => {
                    if let Err(e) = store.delete(&uri).await {
                        tracing::warn!("Failed to delete {}: {}", uri, e);
                    }
                }
                Err(e) => {
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
    
    pb.finish_with_message(format!("deleted {} objects", delete_count));
    Ok(())
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
