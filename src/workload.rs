// src/workload.rs
//
use anyhow::{anyhow, Context, Result};
use hdrhistogram::Histogram;
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, info};

use rand::{rng, Rng};
//use rand::rngs::SmallRng;
use rand::distr::weighted::WeightedIndex;
use rand_distr::Distribution;

use std::time::{Duration, Instant};
use crate::config::{Config, OpSpec};

use std::collections::HashMap;
use crate::bucket_index;

use s3dlio::object_store::{store_for_uri, store_for_uri_with_logger, ObjectStore};
use s3dlio::{init_op_logger, finalize_op_logger, global_logger};

// -----------------------------------------------------------------------------
// Multi-backend support infrastructure
// -----------------------------------------------------------------------------

/// Backend types supported by sai3-bench
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    S3,
    Azure,
    Gcs,
    File,
    DirectIO,
}

impl BackendType {
    /// Detect backend type from URI scheme
    pub fn from_uri(uri: &str) -> Self {
        if uri.starts_with("s3://") {
            BackendType::S3
        } else if uri.starts_with("az://") || uri.starts_with("azure://") {
            BackendType::Azure
        } else if uri.starts_with("gs://") || uri.starts_with("gcs://") {
            BackendType::Gcs
        } else if uri.starts_with("file://") {
            BackendType::File
        } else if uri.starts_with("direct://") {
            BackendType::DirectIO
        } else {
            // Default to File for unrecognized schemes or bare paths
            BackendType::File
        }
    }
    
    /// Get human-readable name for backend
    pub fn name(&self) -> &'static str {
        match self {
            BackendType::S3 => "S3",
            BackendType::Azure => "Azure Blob",
            BackendType::Gcs => "Google Cloud Storage",
            BackendType::File => "Local File",
            BackendType::DirectIO => "Direct I/O",
        }
    }
}

/// Create ObjectStore instance for given URI
pub fn create_store_for_uri(uri: &str) -> anyhow::Result<Box<dyn ObjectStore>> {
    store_for_uri(uri).context("Failed to create object store")
}

/// Create ObjectStore instance with op-logger if available
pub fn create_store_with_logger(uri: &str) -> anyhow::Result<Box<dyn ObjectStore>> {
    let logger = global_logger();
    if logger.is_some() {
        store_for_uri_with_logger(uri, logger).context("Failed to create object store with logger")
    } else {
        create_store_for_uri(uri)
    }
}

/// Initialize operation logger for performance analysis and replay
pub fn init_operation_logger(path: &std::path::Path) -> anyhow::Result<()> {
    let path_str = path.to_str()
        .ok_or_else(|| anyhow!("Invalid path for op-log"))?;
    init_op_logger(path_str).context("Failed to initialize operation logger")
}

/// Finalize and flush operation logger
pub fn finalize_operation_logger() -> anyhow::Result<()> {
    finalize_op_logger();
    Ok(())
}

// -----------------------------------------------------------------------------
// Prepare/Pre-population Support (Warp Parity - Phase 1)
// -----------------------------------------------------------------------------

/// Information about a prepared object
#[derive(Debug, Clone)]
pub struct PreparedObject {
    pub uri: String,
    pub size: u64,
    pub created: bool,  // True if we created it, false if it already existed
}

/// Execute prepare step: ensure objects exist for testing
pub async fn prepare_objects(config: &crate::config::PrepareConfig) -> anyhow::Result<Vec<PreparedObject>> {
    use crate::config::FillPattern;
    use crate::size_generator::SizeGenerator;
    
    let mut all_prepared = Vec::new();
    
    for spec in &config.ensure_objects {
        info!("Preparing objects: {} at {}", spec.count, spec.base_uri);
        
        let store = create_store_for_uri(&spec.base_uri)?;
        
        // 1. List existing objects
        let existing = store.list(&spec.base_uri, true).await
            .context("Failed to list existing objects")?;
        
        let existing_count = existing.len() as u64;
        info!("  Found {} existing objects", existing_count);
        
        // 2. Calculate how many to create
        let to_create = if existing_count >= spec.count {
            info!("  Sufficient objects already exist");
            0
        } else {
            spec.count - existing_count
        };
        
        // 3. Create missing objects
        if to_create > 0 {
            use tokio::sync::Semaphore;
            use futures::stream::{FuturesUnordered, StreamExt};
            use std::sync::Arc;
            
            // Create size generator from spec
            let size_spec = spec.get_size_spec();
            let size_generator = SizeGenerator::new(&size_spec)
                .context("Failed to create size generator")?;
            
            // Determine concurrency for prepare (use 32 like default workload concurrency)
            let concurrency = 32;
            
            info!("  Creating {} additional objects with {} workers (sizes: {}, fill: {:?}, dedup: {}, compress: {})", 
                to_create, concurrency, size_generator.description(), spec.fill, 
                spec.dedup_factor, spec.compress_factor);
            
            // Create progress bar for preparation
            let pb = ProgressBar::new(to_create);
            pb.set_style(ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects ({per_sec}) {msg}"
            )?);
            pb.set_message(format!("preparing with {} workers", concurrency));
            
            // Pre-generate all URIs and sizes for parallel execution
            let mut tasks: Vec<(String, u64)> = Vec::with_capacity(to_create as usize);
            for i in 0..to_create {
                let key = format!("prepared-{:08}.dat", existing_count + i);
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
                    
                    // PUT object
                    store.put(&uri, &data).await
                        .with_context(|| format!("Failed to PUT {}", uri))?;
                    
                    pb2.inc(1);
                    
                    Ok::<(String, u64), anyhow::Error>((uri, size))
                }));
            }
            
            // Collect results as they complete
            let mut created_objects = Vec::with_capacity(to_create as usize);
            while let Some(result) = futs.next().await {
                let (uri, size) = result
                    .context("Task join error")??;
                
                created_objects.push(PreparedObject {
                    uri,
                    size,
                    created: true,
                });
            }
            
            pb.finish_with_message(format!("created {} objects", to_create));
            
            // Add created objects to all_prepared
            all_prepared.extend(created_objects);
        }
    }
    
    info!("Prepare complete: {} objects ready", all_prepared.len());
    Ok(all_prepared)
}

/// Cleanup prepared objects
pub async fn cleanup_prepared_objects(objects: &[PreparedObject]) -> anyhow::Result<()> {
    if objects.is_empty() {
        return Ok(());
    }
    
    use tokio::sync::Semaphore;
    use futures::stream::{FuturesUnordered, StreamExt};
    use std::sync::Arc;
    
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

/// Helper to build full URI from components for different backends
pub fn build_full_uri(backend: BackendType, base_uri: &str, key: &str) -> String {
    match backend {
        BackendType::S3 | BackendType::Gcs => {
            if base_uri.ends_with('/') {
                format!("{}{}", base_uri, key)
            } else {
                format!("{}/{}", base_uri, key)
            }
        }
        BackendType::Azure => {
            if base_uri.ends_with('/') {
                format!("{}{}", base_uri, key)
            } else {
                format!("{}/{}", base_uri, key)
            }
        }
        BackendType::File | BackendType::DirectIO => {
            let path = if base_uri.starts_with("file://") {
                &base_uri[7..]
            } else if base_uri.starts_with("direct://") {
                &base_uri[9..]
            } else {
                base_uri
            };
            
            let scheme = match backend {
                BackendType::DirectIO => "direct://",
                _ => "file://",
            };
            
            if path.ends_with('/') {
                format!("{}{}{}", scheme, path, key)
            } else {
                format!("{}{}/{}", scheme, path, key)
            }
        }
    }
}

/// Multi-backend GET operation using ObjectStore trait
pub async fn get_object_multi_backend(uri: &str) -> anyhow::Result<Vec<u8>> {
    debug!("GET operation starting for URI: {}", uri);
    let store = create_store_with_logger(uri)?;
    debug!("ObjectStore created successfully for URI: {}", uri);
    
    // Use ObjectStore get method with full URI (s3dlio handles URI parsing)
    let bytes = store.get(uri).await
        .with_context(|| format!("Failed to get object from URI: {}", uri))?;
    
    debug!("GET operation completed successfully for URI: {}, {} bytes retrieved", uri, bytes.len());
    Ok(bytes)
}

/// Multi-backend PUT operation using ObjectStore trait
pub async fn put_object_multi_backend(uri: &str, data: &[u8]) -> anyhow::Result<()> {
    debug!("PUT operation starting for URI: {}, {} bytes", uri, data.len());
    let store = create_store_with_logger(uri)?;
    debug!("ObjectStore created successfully for URI: {}", uri);
    
    // Use ObjectStore put method with full URI (s3dlio handles URI parsing)
    store.put(uri, data).await
        .with_context(|| format!("Failed to put object to URI: {}", uri))?;
    
    debug!("PUT operation completed successfully for URI: {}", uri);
    Ok(())
}

/// Multi-backend LIST operation using ObjectStore trait
pub async fn list_objects_multi_backend(uri: &str) -> anyhow::Result<Vec<String>> {
    debug!("LIST operation starting for URI: {}", uri);
    let store = create_store_with_logger(uri)?;
    debug!("ObjectStore created successfully for URI: {}", uri);
    
    // Use ObjectStore list method with full URI and recursive=true (s3dlio handles URI parsing)
    let keys = store.list(uri, true).await
        .with_context(|| format!("Failed to list objects from URI: {}", uri))?;
    
    debug!("LIST operation completed successfully for URI: {}, {} objects found", uri, keys.len());
    Ok(keys)
}

/// Multi-backend STAT operation using ObjectStore trait
/// Uses s3dlio's native stat() method (v0.8.8+)
pub async fn stat_object_multi_backend(uri: &str) -> anyhow::Result<u64> {
    debug!("STAT operation starting for URI: {}", uri);
    let store = create_store_with_logger(uri)?;
    debug!("ObjectStore created successfully for URI: {}", uri);
    
    // Use s3dlio's native stat() method for proper HEAD/metadata operations
    let metadata = store.stat(uri).await
        .with_context(|| format!("Failed to stat object from URI: {}", uri))?;
    
    let size = metadata.size;
    debug!("STAT operation completed successfully for URI: {}, size: {} bytes", uri, size);
    Ok(size)
}

/// Multi-backend DELETE operation using ObjectStore trait
pub async fn delete_object_multi_backend(uri: &str) -> anyhow::Result<()> {
    debug!("DELETE operation starting for URI: {}", uri);
    let store = create_store_with_logger(uri)?;
    debug!("ObjectStore created successfully for URI: {}", uri);
    
    // Use ObjectStore delete method with full URI (s3dlio handles URI parsing)
    store.delete(uri).await
        .with_context(|| format!("Failed to delete object from URI: {}", uri))?;
    
    debug!("DELETE operation completed successfully for URI: {}", uri);
    Ok(())
}

// -----------------------------------------------------------------------------
// NON-LOGGING variants for replay (avoid logging replay operations)
// -----------------------------------------------------------------------------

/// GET operation WITHOUT logging (for replay)
pub async fn get_object_no_log(uri: &str) -> anyhow::Result<Vec<u8>> {
    let store = create_store_for_uri(uri)?;
    store.get(uri).await
        .with_context(|| format!("Failed to get object from URI: {}", uri))
}

/// PUT operation WITHOUT logging (for replay)
pub async fn put_object_no_log(uri: &str, data: &[u8]) -> anyhow::Result<()> {
    let store = create_store_for_uri(uri)?;
    store.put(uri, data).await
        .with_context(|| format!("Failed to put object to URI: {}", uri))
}

/// LIST operation WITHOUT logging (for replay)
pub async fn list_objects_no_log(uri: &str) -> anyhow::Result<Vec<String>> {
    let store = create_store_for_uri(uri)?;
    store.list(uri, true).await
        .with_context(|| format!("Failed to list objects from URI: {}", uri))
}

/// STAT operation WITHOUT logging (for replay)
pub async fn stat_object_no_log(uri: &str) -> anyhow::Result<u64> {
    let store = create_store_for_uri(uri)?;
    let metadata = store.stat(uri).await
        .with_context(|| format!("Failed to stat object at URI: {}", uri))?;
    Ok(metadata.size)
}

/// DELETE operation WITHOUT logging (for replay)
pub async fn delete_object_no_log(uri: &str) -> anyhow::Result<()> {
    let store = create_store_for_uri(uri)?;
    store.delete(uri).await
        .with_context(|| format!("Failed to delete object from URI: {}", uri))
}

// -----------------------------------------------------------------------------
// New summary/aggregation types
// -----------------------------------------------------------------------------
#[derive(Debug, Clone, Default)]
pub struct OpAgg {
    pub bytes: u64,
    pub ops: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SizeBins {
    // bucket_index -> (ops, bytes)
    pub by_bucket: HashMap<usize, (u64, u64)>,
}

impl SizeBins {
    fn add(&mut self, size_bytes: u64) {
        let b = bucket_index(size_bytes as usize);
        let e = self.by_bucket.entry(b).or_insert((0, 0));
        e.0 += 1;
        e.1 += size_bytes;
    }
    fn merge_from(&mut self, other: &SizeBins) {
        for (k, (ops, bytes)) in &other.by_bucket {
            let e = self.by_bucket.entry(*k).or_insert((0, 0));
            e.0 += ops;
            e.1 += bytes;
        }
    }
}

// Old combined fields are kept for backward compatibility.
#[derive(Debug, Clone)]
pub struct Summary {
    pub wall_seconds: f64,
    pub total_bytes: u64,
    pub total_ops: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,

    // New: per-op aggregates and size bins
    pub get: OpAgg,
    pub put: OpAgg,
    pub meta: OpAgg,
    pub get_bins: SizeBins,
    pub put_bins: SizeBins,
    pub meta_bins: SizeBins,
    
    // v0.5.1: Expose histograms for TSV export
    pub get_hists: crate::metrics::OpHists,
    pub put_hists: crate::metrics::OpHists,
    pub meta_hists: crate::metrics::OpHists,
}

// -----------------------------------------------------------------------------
// Worker stats merged at the end
// -----------------------------------------------------------------------------
struct WorkerStats {
    hist_get: crate::metrics::OpHists,
    hist_put: crate::metrics::OpHists,
    hist_meta: crate::metrics::OpHists,
    get_bytes: u64,
    get_ops: u64,
    put_bytes: u64,
    put_ops: u64,
    meta_bytes: u64,
    meta_ops: u64,
    get_bins: SizeBins,
    put_bins: SizeBins,
    meta_bins: SizeBins,
}

impl Default for WorkerStats {
    fn default() -> Self {
        Self {
            hist_get: crate::metrics::OpHists::new(),
            hist_put: crate::metrics::OpHists::new(),
            hist_meta: crate::metrics::OpHists::new(),
            get_bytes: 0,
            get_ops: 0,
            put_bytes: 0,
            put_ops: 0,
            meta_bytes: 0,
            meta_ops: 0,
            get_bins: SizeBins::default(),
            put_bins: SizeBins::default(),
            meta_bins: SizeBins::default(),
        }
    }
}

/// Public entry: run a config and print a summary-like struct back.
pub async fn run(cfg: &Config) -> Result<Summary> {
    info!("Starting workload execution: duration={:?}, concurrency={}", cfg.duration, cfg.concurrency);
    
    let start = Instant::now();
    let deadline = start + cfg.duration;

    // Pre-resolve GET, DELETE, and STAT sources once
    info!("Pre-resolving operation patterns for {} operations", cfg.workload.len());
    
    // Count operations that need resolution
    let mut get_count = 0;
    let mut delete_count = 0;
    let mut stat_count = 0;
    for wo in &cfg.workload {
        match &wo.spec {
            OpSpec::Get { .. } => get_count += 1,
            OpSpec::Delete { .. } => delete_count += 1,
            OpSpec::Stat { .. } => stat_count += 1,
            _ => {}
        }
    }
    
    let total_patterns = get_count + delete_count + stat_count;
    if total_patterns > 0 {
        println!("Resolving {} operation patterns ({} GET, {} DELETE, {} STAT)...", 
                 total_patterns, get_count, delete_count, stat_count);
    }
    
    let mut pre = PreResolved::default();
    for wo in &cfg.workload {
        match &wo.spec {
            OpSpec::Get { .. } => {
                let uri = cfg.get_uri(&wo.spec);
                info!("Resolving GET pattern: {}", uri);
                
                match prefetch_uris_multi_backend(&uri).await {
                    Ok(full_uris) if !full_uris.is_empty() => {
                        info!("Found {} objects for GET pattern: {}", full_uris.len(), uri);
                        pre.get_lists.push(UriSource {
                            full_uris: full_uris.clone(),
                            uri: uri.clone(),
                        });
                    }
                    Ok(_) => {
                        return Err(anyhow!("No URIs found for GET pattern: {}", uri));
                    }
                    Err(e) => {
                        return Err(anyhow!("Failed to resolve GET pattern {}: {}", uri, e));
                    }
                }
            }
            OpSpec::Delete { .. } => {
                let uri = cfg.get_meta_uri(&wo.spec);
                info!("Resolving DELETE pattern: {}", uri);
                
                match prefetch_uris_multi_backend(&uri).await {
                    Ok(full_uris) if !full_uris.is_empty() => {
                        info!("Found {} objects for DELETE pattern: {}", full_uris.len(), uri);
                        pre.delete_lists.push(UriSource {
                            full_uris: full_uris.clone(),
                            uri: uri.clone(),
                        });
                    }
                    Ok(_) => {
                        return Err(anyhow!("No URIs found for DELETE pattern: {}", uri));
                    }
                    Err(e) => {
                        return Err(anyhow!("Failed to resolve DELETE pattern {}: {}", uri, e));
                    }
                }
            }
            OpSpec::Stat { .. } => {
                let uri = cfg.get_meta_uri(&wo.spec);
                info!("Resolving STAT pattern: {}", uri);
                
                match prefetch_uris_multi_backend(&uri).await {
                    Ok(full_uris) if !full_uris.is_empty() => {
                        info!("Found {} objects for STAT pattern: {}", full_uris.len(), uri);
                        pre.stat_lists.push(UriSource {
                            full_uris: full_uris.clone(),
                            uri: uri.clone(),
                        });
                    }
                    Ok(_) => {
                        return Err(anyhow!("No URIs found for STAT pattern: {}", uri));
                    }
                    Err(e) => {
                        return Err(anyhow!("Failed to resolve STAT pattern {}: {}", uri, e));
                    }
                }
            }
            _ => {} // PUT and LIST don't need pre-resolution
        }
    }

    // Weighted chooser
    let weights: Vec<u32> = cfg.workload.iter().map(|w| w.weight).collect();
    let chooser = WeightedIndex::new(weights).context("invalid weights")?;

    // Concurrency: Create per-operation semaphores
    // If an operation has a concurrency override, use that; otherwise use global
    let mut op_semaphores: Vec<Arc<Semaphore>> = Vec::new();
    for wo in &cfg.workload {
        let concurrency = wo.concurrency.unwrap_or(cfg.concurrency);
        op_semaphores.push(Arc::new(Semaphore::new(concurrency)));
    }
    
    // Log per-op concurrency settings
    for (idx, wo) in cfg.workload.iter().enumerate() {
        if let Some(conc) = wo.concurrency {
            info!("Operation {} has custom concurrency: {}", idx, conc);
        }
    }

    // Spawn workers
    info!("Spawning {} worker tasks", cfg.concurrency);
    println!("Starting execution ({}s duration, {} concurrent workers)...", cfg.duration.as_secs(), cfg.concurrency);
    
    // Create progress bar for time-based execution
    let pb = ProgressBar::new(cfg.duration.as_secs());
    pb.set_style(ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len}s ({eta_precise}) {msg}"
    )?);
    pb.set_message(format!("running with {} workers", cfg.concurrency));
    
    // Spawn progress monitoring task
    let pb_clone = pb.clone();
    let duration = cfg.duration;
    let progress_handle = tokio::spawn(async move {
        let progress_start = Instant::now();
        let update_interval = Duration::from_millis(100); // Update every 100ms
        
        loop {
            let elapsed = progress_start.elapsed();
            if elapsed >= duration {
                pb_clone.set_position(duration.as_secs());
                break;
            }
            
            pb_clone.set_position(elapsed.as_secs());
            tokio::time::sleep(update_interval).await;
        }
    });
    
    let mut handles = Vec::with_capacity(cfg.concurrency);
    for _ in 0..cfg.concurrency {
        let op_sems = op_semaphores.clone();
        let workload = cfg.workload.clone();
        let chooser = chooser.clone();
        let pre = pre.clone();
        let cfg = cfg.clone();

        handles.push(tokio::spawn(async move {
            //let mut ws = WorkerStats::new();
            let mut ws: WorkerStats = Default::default();

            loop {
                if Instant::now() >= deadline {
                    break;
                }

                // Sample op index FIRST (before acquiring permit)
                let idx = {
                    let mut r = rng();
                    chooser.sample(&mut r)
                };

                // Acquire permit for this specific operation
                let _p = op_sems[idx].acquire().await.unwrap();
                
                let op = &workload[idx].spec;

                match op {
                    OpSpec::Get { .. } => {
                        // Get resolved URI from config
                        let uri = cfg.get_uri(op);
                        
                        // Pick full URI from pre-resolved list
                        let full_uri = {
                            let src = pre.get_for_uri(&uri).unwrap();
                            let mut r = rng();
                            let uri_idx = r.random_range(0..src.full_uris.len());
                            src.full_uris[uri_idx].clone()
                        };

                        let t0 = Instant::now();
                        let bytes = get_object_multi_backend(&full_uri).await?;
                        let duration = t0.elapsed();

                        let bucket = crate::metrics::bucket_index(bytes.len());
                        ws.hist_get.record(bucket, duration);
                        ws.get_ops += 1;
                        ws.get_bytes += bytes.len() as u64;
                        ws.get_bins.add(bytes.len() as u64);
                    }
                    OpSpec::Put { dedup_factor, compress_factor, .. } => {
                        // Get base URI and size spec from config
                        let (base_uri, size_spec) = cfg.get_put_size_spec(op);
                        
                        // Generate object size using size generator
                        use crate::size_generator::SizeGenerator;
                        let size_generator = SizeGenerator::new(&size_spec)?;
                        let sz = size_generator.generate();
                        
                        let key = {
                            let mut r = rng();
                            format!("obj_{}", r.random::<u64>())
                        };
                        
                        // Generate data using s3dlio's controlled data generation
                        let buf = s3dlio::generate_controlled_data(
                            sz as usize,
                            *dedup_factor,
                            *compress_factor
                        );

                        // Build full URI for the specific object
                        let full_uri = if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, key)
                        } else {
                            format!("{}/{}", base_uri, key)
                        };

                        let t0 = Instant::now();
                        put_object_multi_backend(&full_uri, &buf).await?;
                        let duration = t0.elapsed();

                        let bucket = crate::metrics::bucket_index(buf.len());
                        ws.hist_put.record(bucket, duration);
                        ws.put_ops += 1;
                        ws.put_bytes += sz;
                        ws.put_bins.add(sz);
                    }
                    OpSpec::List { .. } => {
                        // Get resolved URI from config
                        let uri = cfg.get_meta_uri(op);

                        let t0 = Instant::now();
                        let _keys = list_objects_multi_backend(&uri).await?;
                        let duration = t0.elapsed();

                        ws.hist_meta.record(0, duration); // Bucket 0 for metadata ops
                        ws.meta_ops += 1;
                        // List operations don't transfer data, just metadata
                        ws.meta_bins.add(0);
                    }
                    OpSpec::Stat { .. } => {
                        // Get pattern URI and pick random resolved URI
                        let pattern = cfg.get_meta_uri(op);
                        let full_uri = {
                            let src = pre.stat_for_uri(&pattern)
                                .ok_or_else(|| anyhow!("No pre-resolved URIs for STAT pattern: {}", pattern))?;
                            let mut r = rng();
                            let uri_idx = r.random_range(0..src.full_uris.len());
                            src.full_uris[uri_idx].clone()
                        };

                        let t0 = Instant::now();
                        let _size = stat_object_multi_backend(&full_uri).await?;
                        let duration = t0.elapsed();

                        ws.hist_meta.record(0, duration); // Bucket 0 for metadata ops
                        ws.meta_ops += 1;
                        // Stat operations don't transfer data, just metadata about the size
                        ws.meta_bins.add(0);
                    }
                    OpSpec::Delete { .. } => {
                        // Get pattern URI and pick random resolved URI
                        let pattern = cfg.get_meta_uri(op);
                        let full_uri = {
                            let src = pre.delete_for_uri(&pattern)
                                .ok_or_else(|| anyhow!("No pre-resolved URIs for DELETE pattern: {}", pattern))?;
                            let mut r = rng();
                            let uri_idx = r.random_range(0..src.full_uris.len());
                            src.full_uris[uri_idx].clone()
                        };

                        let t0 = Instant::now();
                        delete_object_multi_backend(&full_uri).await?;
                        let duration = t0.elapsed();

                        ws.hist_meta.record(0, duration); // Bucket 0 for metadata ops
                        ws.meta_ops += 1;
                        // Delete operations don't transfer data
                        ws.meta_bins.add(0);
                    }
                }
            }

            Ok::<WorkerStats, anyhow::Error>(ws)
        }));
    }

    // Merge results from all workers
    let mut merged_get = crate::metrics::OpHists::new();
    let mut merged_put = crate::metrics::OpHists::new();
    let mut merged_meta = crate::metrics::OpHists::new();
    let mut get_bytes = 0u64;
    let mut get_ops = 0u64;
    let mut put_bytes = 0u64;
    let mut put_ops = 0u64;
    let mut meta_bytes = 0u64;
    let mut meta_ops = 0u64;
    let mut get_bins = SizeBins::default();
    let mut put_bins = SizeBins::default();
    let mut meta_bins = SizeBins::default();

    for h in handles {
        let ws = h.await??;
        merged_get.merge(&ws.hist_get);
        merged_put.merge(&ws.hist_put);
        merged_meta.merge(&ws.hist_meta);
        get_bytes += ws.get_bytes;
        get_ops += ws.get_ops;
        put_bytes += ws.put_bytes;
        put_ops += ws.put_ops;
        meta_bytes += ws.meta_bytes;
        meta_ops += ws.meta_ops;
        get_bins.merge_from(&ws.get_bins);
        put_bins.merge_from(&ws.put_bins);
        meta_bins.merge_from(&ws.meta_bins);
    }

    // Complete progress bar and wait for progress task
    progress_handle.await?;
    
    let wall = start.elapsed().as_secs_f64();
    pb.finish_with_message(format!("completed in {:.2}s", wall));
    
    // Always show completion status
    println!("Execution completed in {:.2}s", wall);

    // Get combined percentiles for OpAgg structures
    let get_combined = merged_get.combined_histogram();
    let put_combined = merged_put.combined_histogram();
    let meta_combined = merged_meta.combined_histogram();

    let get = OpAgg {
        bytes: get_bytes,
        ops: get_ops,
        p50_us: get_combined.value_at_quantile(0.50),
        p95_us: get_combined.value_at_quantile(0.95),
        p99_us: get_combined.value_at_quantile(0.99),
    };
    let put = OpAgg {
        bytes: put_bytes,
        ops: put_ops,
        p50_us: put_combined.value_at_quantile(0.50),
        p95_us: put_combined.value_at_quantile(0.95),
        p99_us: put_combined.value_at_quantile(0.99),
    };
    let meta = OpAgg {
        bytes: meta_bytes,
        ops: meta_ops,
        p50_us: meta_combined.value_at_quantile(0.50),
        p95_us: meta_combined.value_at_quantile(0.95),
        p99_us: meta_combined.value_at_quantile(0.99),
    };

    // Preserve combined line for compatibility
    let total_bytes = get_bytes + put_bytes + meta_bytes;
    let total_ops = get_ops + put_ops + meta_ops;

    // Build a combined histogram across all operations for overall p50/95/99
    let mut combined = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    combined.add(&get_combined).ok();
    combined.add(&put_combined).ok();
    combined.add(&meta_combined).ok();

    info!("Workload execution completed: {:.2}s wall time, {} total ops ({} GET, {} PUT, {} META), {:.2} MB total ({:.2} MB GET, {:.2} MB PUT, {:.2} MB META)", 
          wall, 
          total_ops, get_ops, put_ops, meta_ops,
          total_bytes as f64 / 1_048_576.0,
          get_bytes as f64 / 1_048_576.0,
          put_bytes as f64 / 1_048_576.0,
          meta_bytes as f64 / 1_048_576.0);

    // Print detailed size-bucketed histograms
    if get_ops > 0 {
        merged_get.print_summary("GET");
    }
    if put_ops > 0 {
        merged_put.print_summary("PUT");
    }
    if meta_ops > 0 {
        merged_meta.print_summary("META");
    }

    Ok(Summary {
        wall_seconds: wall,
        total_bytes,
        total_ops,
        p50_us: combined.value_at_quantile(0.50),
        p95_us: combined.value_at_quantile(0.95),
        p99_us: combined.value_at_quantile(0.99),
        get,
        put,
        meta,
        get_bins,
        put_bins,
        meta_bins,
        get_hists: merged_get,
        put_hists: merged_put,
        meta_hists: merged_meta,
    })
}

/// Pre-resolved URI lists so workers can sample keys cheaply.
/// Handles GET, DELETE, and STAT operations with glob patterns.
#[derive(Default, Clone)]
struct PreResolved {
    get_lists: Vec<UriSource>,
    delete_lists: Vec<UriSource>,
    stat_lists: Vec<UriSource>,
}
#[derive(Clone)]
struct UriSource {
    full_uris: Vec<String>,     // Pre-resolved full URIs for random selection
    uri: String,                // Original pattern for lookup compatibility
}
impl PreResolved {
    fn get_for_uri(&self, uri: &str) -> Option<&UriSource> {
        self.get_lists.iter().find(|g| g.uri == uri)
    }
    fn delete_for_uri(&self, uri: &str) -> Option<&UriSource> {
        self.delete_lists.iter().find(|d| d.uri == uri)
    }
    fn stat_for_uri(&self, uri: &str) -> Option<&UriSource> {
        self.stat_lists.iter().find(|s| s.uri == uri)
    }
}


/// Expand keys for a GET uri: supports exact key, prefix, or glob '*'.
/// 
/// NOTE: s3dlio's ObjectStore trait does not provide pattern matching in list().
/// We implement glob pattern matching at the sai3-bench level by:
/// 1. Listing all objects in the directory
/// 2. Applying regex-based filtering to match the pattern
/// 
/// This is consistent with how object stores work - they don't have native glob support.
/// For local file operations, s3dlio has generic_upload_files() with glob crate support,
/// but ObjectStore operations work with URIs, not file paths.
async fn prefetch_uris_multi_backend(base_uri: &str) -> Result<Vec<String>> {
    let store = create_store_with_logger(base_uri)?;
    
    if base_uri.contains('*') {
        // Glob pattern: list directory and filter with regex
        let base_end = base_uri.rfind('/').map(|i| i + 1).unwrap_or(0);
        let list_prefix = &base_uri[..base_end];
        
        let uris = store.list(list_prefix, false).await?;
        
        // Handle URI scheme normalization: s3dlio may return different schemes than input
        // For pattern matching, normalize both pattern and results to the same scheme
        let normalized_pattern = normalize_scheme_for_matching(base_uri);
        let re = glob_to_regex(&normalized_pattern)?;
        
        let matched_uris: Vec<String> = uris.into_iter()
            .filter(|uri| {
                let normalized_uri = normalize_scheme_for_matching(uri);
                re.is_match(&normalized_uri)
            })
            .collect();
            
        Ok(matched_uris)
    } else if base_uri.ends_with('/') {
        // Directory listing
        store.list(base_uri, false).await
    } else {
        // Exact URI
        Ok(vec![base_uri.to_string()])
    }
}


/// Convert glob pattern to regex
/// Escapes all special regex chars except *, which becomes .*
fn glob_to_regex(glob: &str) -> Result<regex::Regex> {
    let s = regex::escape(glob).replace(r"\*", ".*");
    let re = format!("^{}$", s);
    Ok(regex::Regex::new(&re)?)
}

/// Normalize URI scheme for glob pattern matching
/// s3dlio may return different schemes than input (e.g., direct:// -> file://)
/// This function normalizes URIs to a consistent scheme for pattern matching
fn normalize_scheme_for_matching(uri: &str) -> String {
    if let Some(scheme_end) = uri.find("://") {
        let path_part = &uri[scheme_end + 3..];
        // Normalize to file:// scheme for consistent pattern matching
        format!("file://{}", path_part)
    } else {
        // No scheme, treat as file path
        format!("file://{}", uri)
    }
}



