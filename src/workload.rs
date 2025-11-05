// src/workload.rs
//
use anyhow::{anyhow, bail, Context, Result};
use hdrhistogram::Histogram;
use indicatif::{ProgressBar, ProgressStyle};
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use rand::{rng, Rng};
//use rand::rngs::SmallRng;
use rand::distr::weighted::WeightedIndex;
use rand_distr::Distribution;

use std::time::{Duration, Instant};
use crate::config::{Config, OpSpec};

use std::collections::HashMap;
use crate::bucket_index;

use s3dlio::object_store::{
    store_for_uri, store_for_uri_with_logger, ObjectStore,
    GcsConfig, GcsObjectStore,
};
use s3dlio::file_store::{FileSystemObjectStore, FileSystemConfig};
use s3dlio::PageCacheMode as S3dlioPageCacheMode;
use s3dlio::{init_op_logger, finalize_op_logger, global_logger};

// -----------------------------------------------------------------------------
// Chunked read configuration for optimal direct:// performance
// -----------------------------------------------------------------------------

/// Optimal chunk size for direct:// reads (4 MiB)
/// Based on testing: 4M chunks achieve 1.73 GiB/s vs 0.01 GiB/s for whole-file
const DIRECT_IO_CHUNK_SIZE: usize = 4 * 1024 * 1024;  // 4 MiB

/// Threshold for using chunked reads vs whole-file reads
/// Files larger than this will use chunked reads for direct://
const CHUNKED_READ_THRESHOLD: u64 = 8 * 1024 * 1024;  // 8 MiB

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

/// Create ObjectStore instance for given URI with optional RangeEngine and PageCache configuration
/// 
/// If range_config is provided and enabled=true for GCS/Azure/S3 backends, creates
/// store with custom RangeEngine settings. Otherwise uses defaults (RangeEngine disabled).
/// 
/// If page_cache_mode is provided for file:// or direct:// backends, configures
/// posix_fadvise hints for kernel page cache optimization (Linux/Unix only).
pub fn create_store_for_uri_with_config(
    uri: &str, 
    range_config: Option<&crate::config::RangeEngineConfig>,
    page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<Box<dyn ObjectStore>> {
    // For file:// and direct:// URIs, apply FileSystemConfig with page_cache_mode
    if uri.starts_with("file://") || uri.starts_with("direct://") {
        if let Some(mode) = page_cache_mode {
            // Convert config::PageCacheMode to s3dlio::PageCacheMode
            let s3dlio_mode = match mode {
                crate::config::PageCacheMode::Auto => S3dlioPageCacheMode::Auto,
                crate::config::PageCacheMode::Sequential => S3dlioPageCacheMode::Sequential,
                crate::config::PageCacheMode::Random => S3dlioPageCacheMode::Random,
                crate::config::PageCacheMode::DontNeed => S3dlioPageCacheMode::DontNeed,
                crate::config::PageCacheMode::Normal => S3dlioPageCacheMode::Normal,
            };
            
            debug!("File system URI detected - page_cache_mode: {:?}", mode);
            
            let config = FileSystemConfig {
                enable_range_engine: false,  // Local files rarely benefit from range parallelism
                range_engine: Default::default(),
                page_cache_mode: Some(s3dlio_mode),
            };
            
            let store = FileSystemObjectStore::with_config(config);
            return Ok(Box::new(store));
        } else {
            // No page cache mode specified - use store_for_uri which handles direct://
            return store_for_uri(uri).context("Failed to create object store");
        }
    }
    
    // For GCS URIs, apply RangeEngine configuration
    if uri.starts_with("gs://") || uri.starts_with("gcs://") {
        let enabled = range_config.map(|c| c.enabled).unwrap_or(false);
        debug!("GCS URI detected - RangeEngine {}", if enabled { "enabled" } else { "disabled" });
        
        let config = if let Some(cfg) = range_config {
            GcsConfig {
                enable_range_engine: cfg.enabled,
                range_engine: s3dlio::range_engine_generic::RangeEngineConfig {
                    chunk_size: cfg.chunk_size as usize,
                    max_concurrent_ranges: cfg.max_concurrent_ranges,
                    min_split_size: cfg.min_split_size,
                    range_timeout: std::time::Duration::from_secs(cfg.range_timeout_secs),
                },
                size_cache_ttl_secs: 60,  // v0.9.10: Enable size cache with 60s TTL
            }
        } else {
            // No config provided - use disabled default
            GcsConfig {
                enable_range_engine: false,
                size_cache_ttl_secs: 60,  // v0.9.10: Enable size cache with 60s TTL
                ..Default::default()
            }
        };
        
        let store = GcsObjectStore::with_config(config);
        return Ok(Box::new(store));
    }
    
    // For other backends, use standard creation
    // TODO: Add Azure/S3 config support here when needed
    store_for_uri(uri).context("Failed to create object store")
}

/// Create ObjectStore instance for given URI (backward compatible - no config)
/// Uses default RangeEngine settings (disabled) and no page cache hints
pub fn create_store_for_uri(uri: &str) -> anyhow::Result<Box<dyn ObjectStore>> {
    create_store_for_uri_with_config(uri, None, None)
}

/// Create ObjectStore instance with op-logger and optional RangeEngine/PageCache configuration
pub fn create_store_with_logger_and_config(
    uri: &str,
    range_config: Option<&crate::config::RangeEngineConfig>,
    page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<Box<dyn ObjectStore>> {
    // For GCS URIs, apply RangeEngine configuration
    if uri.starts_with("gs://") || uri.starts_with("gcs://") {
        let enabled = range_config.map(|c| c.enabled).unwrap_or(false);
        debug!("GCS URI detected - RangeEngine {}", if enabled { "enabled" } else { "disabled" });
        
        let config = if let Some(cfg) = range_config {
            GcsConfig {
                enable_range_engine: cfg.enabled,
                range_engine: s3dlio::range_engine_generic::RangeEngineConfig {
                    chunk_size: cfg.chunk_size as usize,
                    max_concurrent_ranges: cfg.max_concurrent_ranges,
                    min_split_size: cfg.min_split_size,
                    range_timeout: std::time::Duration::from_secs(cfg.range_timeout_secs),
                },
                size_cache_ttl_secs: 60,  // v0.9.10: Enable size cache with 60s TTL
            }
        } else {
            GcsConfig {
                enable_range_engine: false,
                size_cache_ttl_secs: 60,  // v0.9.10: Enable size cache with 60s TTL
                ..Default::default()
            }
        };
        
        let store = GcsObjectStore::with_config(config);
        return Ok(Box::new(store));
    }
    
    // For other backends, use logger if available
    let logger = global_logger();
    if logger.is_some() {
        store_for_uri_with_logger(uri, logger).context("Failed to create object store with logger")
    } else {
        create_store_for_uri_with_config(uri, range_config, page_cache_mode)
    }
}

/// Create ObjectStore instance with op-logger if available (backward compatible)
pub fn create_store_with_logger(uri: &str) -> anyhow::Result<Box<dyn ObjectStore>> {
    create_store_with_logger_and_config(uri, None, None)
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

/// Detect if workload requires separate readonly and deletable object pools
/// Returns (has_delete, has_readonly) where readonly = GET or STAT operations
pub fn detect_pool_requirements(workload: &[crate::config::WeightedOp]) -> (bool, bool) {
    let mut has_delete = false;
    let mut has_readonly = false;
    
    for wo in workload {
        match &wo.spec {
            OpSpec::Delete { .. } | OpSpec::Rmdir { .. } => has_delete = true,
            OpSpec::Get { .. } | OpSpec::Stat { .. } => has_readonly = true,
            _ => {}
        }
    }
    
    (has_delete, has_readonly)
}

/// Rewrite pattern to use the correct object pool for mixed workloads
/// 
/// When mixed workload is detected (DELETE + GET/STAT), automatically rewrites patterns:
/// - GET/STAT: prepared-*.dat → prepared-*.dat (readonly pool)
/// - DELETE: prepared-*.dat → deletable-*.dat (consumable pool)
pub fn rewrite_pattern_for_pool(pattern: &str, is_delete: bool, needs_separate_pools: bool) -> String {
    if !needs_separate_pools {
        // Not a mixed workload, use pattern as-is
        return pattern.to_string();
    }
    
    // For mixed workloads: rewrite "prepared-" to appropriate pool prefix
    if pattern.contains("prepared-") {
        if is_delete {
            // DELETE operations use deletable pool
            pattern.replace("prepared-", "deletable-")
        } else {
            // GET/STAT keep readonly pool (prepared-)
            pattern.to_string()
        }
    } else {
        // Pattern doesn't use prepared- prefix, use as-is
        // (user may be using custom prefixes)
        pattern.to_string()
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
pub async fn prepare_objects(
    config: &crate::config::PrepareConfig,
    workload: Option<&[crate::config::WeightedOp]>
) -> anyhow::Result<(Vec<PreparedObject>, Option<TreeManifest>)> {
    use crate::config::PrepareStrategy;
    
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
    
    // Choose execution strategy based on config
    let all_prepared = match config.prepare_strategy {
        PrepareStrategy::Sequential => {
            info!("Using sequential prepare strategy (one size group at a time)");
            prepare_sequential(config, needs_separate_pools).await?
        }
        PrepareStrategy::Parallel => {
            info!("Using parallel prepare strategy (all sizes interleaved)");
            prepare_parallel(config, needs_separate_pools).await?
        }
    };
    
    // Create directory tree if configured and return final result
    finalize_prepare_with_tree(config, all_prepared).await
}

/// Sequential prepare strategy: Process each ensure_objects entry one at a time
/// This is the original behavior - predictable, separate progress bars per size
async fn prepare_sequential(
    config: &crate::config::PrepareConfig,
    needs_separate_pools: bool
) -> anyhow::Result<Vec<PreparedObject>> {
    use crate::config::FillPattern;
    use crate::size_generator::SizeGenerator;
    
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
                use tokio::sync::Semaphore;
                use futures::stream::{FuturesUnordered, StreamExt};
                use std::sync::Arc;
                
                // Create size generator from spec
                let size_spec = spec.get_size_spec();
                let size_generator = SizeGenerator::new(&size_spec)
                    .context("Failed to create size generator")?;
                
                // Determine concurrency for prepare (use 32 like default workload concurrency)
                let concurrency = 32;
                
                info!("  Creating {} additional {} objects with {} workers (sizes: {}, fill: {:?}, dedup: {}, compress: {})", 
                    to_create, prefix, concurrency, size_generator.description(), spec.fill, 
                    spec.dedup_factor, spec.compress_factor);
                
                // Create progress bar for preparation
                let pb = ProgressBar::new(to_create);
                pb.set_style(ProgressStyle::with_template(
                    "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects ({per_sec}) {msg}"
                )?);
                pb.set_message(format!("preparing {} with {} workers", prefix, concurrency));
                
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
    config: &crate::config::PrepareConfig,
    needs_separate_pools: bool
) -> anyhow::Result<Vec<PreparedObject>> {
    use crate::config::FillPattern;
    use crate::size_generator::SizeGenerator;
    use tokio::sync::Semaphore;
    use futures::stream::{FuturesUnordered, StreamExt};
    use std::sync::Arc;
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
    let pb = ProgressBar::new(total_to_create);
    pb.set_style(ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects ({per_sec}) {msg}"
    )?);
    pb.set_message(format!("parallel prepare with {} workers", concurrency));
    
    let sem = Arc::new(Semaphore::new(concurrency));
    let mut futs = FuturesUnordered::new();
    let pb_clone = pb.clone();
    
    for task in all_tasks {
        let sem2 = sem.clone();
        let pb2 = pb_clone.clone();
        
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
            
            // PUT object
            store.put(&task.uri, &data).await
                .with_context(|| format!("Failed to PUT {}", task.uri))?;
            
            pb2.inc(1);
            
            Ok::<(String, u64), anyhow::Error>((task.uri, task.size))
        }));
    }
    
    // Collect results as they complete
    let mut all_prepared = Vec::with_capacity(total_to_create as usize);
    while let Some(result) = futs.next().await {
        let (uri, size) = result
            .context("Task join error")??;
        
        all_prepared.push(PreparedObject {
            uri,
            size,
            created: true,
        });
    }
    
    pb.finish_with_message(format!("created {} objects (all sizes)", total_to_create));
    
    Ok(all_prepared)
}

/// Continue with directory tree creation after prepare strategy completes
async fn finalize_prepare_with_tree(
    config: &crate::config::PrepareConfig,
    all_prepared: Vec<PreparedObject>
) -> anyhow::Result<(Vec<PreparedObject>, Option<TreeManifest>)> {
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
            base_uri
        ).await?;
        
        info!("Directory tree created: {} total directories",
            manifest.all_directories.len()
        );
        
        Some(manifest)
    } else {
        None
    };
    
    info!("Prepare complete: {} objects ready", all_prepared.len());
    Ok((all_prepared, tree_manifest))
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

/// Create directory tree structure from PrepareConfig
/// 
/// This function creates the hierarchical directory tree and populates it with files.
/// CRITICAL: Uses global file indexing to avoid rdf-bench collision bug (v2025.10.0 fix)
/// 
/// # Arguments
/// * `config` - PrepareConfig containing directory_structure
/// * `agent_id` - This agent's ID (0-indexed), used for modulo distribution
/// * `num_agents` - Total number of agents creating the tree
/// * `base_uri` - Base URI where tree will be created (e.g., "file:///tmp/test/")
/// 
/// # Returns
/// TreeManifest with all paths and agent assignments
pub async fn create_directory_tree(
    config: &crate::config::PrepareConfig,
    agent_id: usize,
    num_agents: usize,
    base_uri: &str,
) -> anyhow::Result<crate::directory_tree::TreeManifest> {
    use crate::directory_tree::{DirectoryTree, TreeManifest};
    
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
                
                store.mkdir(&full_uri).await
                    .with_context(|| format!("Failed to create directory: {}", full_uri))?;
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
                (SizeSpec::Fixed(1024), crate::config::FillPattern::Zero, 1, 1)
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
                        crate::config::FillPattern::Zero => vec![0u8; size as usize],
                        crate::config::FillPattern::Random => {
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
use crate::directory_tree::TreeManifest;

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
        let mut rng = rng();
        let idx = rng.random_range(0..self.manifest.all_directories.len());
        self.manifest.all_directories[idx].clone()
    }
    
    /// Partitioned: Prefer assigned directories, but can pick others
    /// Uses 70/30 split to reduce contention while allowing flexibility
    fn select_partitioned(&self) -> String {
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

/// Verify that prepared objects exist and are accessible
pub async fn verify_prepared_objects(config: &crate::config::PrepareConfig) -> anyhow::Result<()> {
    use indicatif::{ProgressBar, ProgressStyle};
    
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
            bail!("Verification failed: Found {} objects but expected {} at {}",
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
            match stat_object_multi_backend(uri).await {
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
            bail!("Verification failed: {}/{} objects accessible at {}",
                  accessible_count, expected_count, spec.base_uri);
        }
        
        println!("✓ {}/{} objects verified and accessible at {}", 
                 accessible_count, expected_count, spec.base_uri);
        info!("Verification successful: {}/{} objects", accessible_count, expected_count);
    }
    
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
/// 
/// **CRITICAL OPTIMIZATION FOR direct:// ONLY**
/// Automatically uses chunked reads for direct:// URIs on large files (>8 MiB)
/// to achieve optimal performance (173x faster than whole-file reads).
/// 
/// **Cloud storage backends (s3://, gs://, az://) always use whole-file reads**
/// to avoid multiple HTTP requests and network latency amplification.
/// 
/// **Local file:// URIs use whole-file reads** - acceptable performance (0.57 GiB/s)
/// and simpler implementation.
/// 
/// Performance characteristics (1 GiB test):
/// - s3://  whole-file:     OPTIMAL (ObjectStore handles efficiently)
/// - gs://  whole-file:     OPTIMAL (ObjectStore handles efficiently)  
/// - az://  whole-file:     OPTIMAL (ObjectStore handles efficiently)
/// - file:// whole-file:    0.57 GiB/s (acceptable, keep simple)
/// - direct:// whole-file:  0.01 GiB/s (CATASTROPHIC - 76 seconds!)
/// - direct:// 4M chunks:   1.73 GiB/s (OPTIMAL - 0.6 seconds, 173x faster!)
pub async fn get_object_multi_backend(uri: &str) -> anyhow::Result<Vec<u8>> {
    debug!("GET operation starting for URI: {}", uri);
    
    let is_direct_io = uri.starts_with("direct://");
    
    // ONLY use chunked reads for direct:// URIs
    // Cloud storage (s3://, gs://, az://) and file:// use whole-file reads
    if is_direct_io {
        // Extract file path from URI
        let path_str = uri.strip_prefix("direct://").unwrap_or(uri);
        
        // Get file size (async metadata fetch - negligible overhead for local files)
        match tokio::fs::metadata(path_str).await {
            Ok(meta) => {
                let file_size = meta.len();
                debug!("direct:// file size: {} bytes", file_size);
                
                // Use chunked reads for files larger than threshold
                if file_size > CHUNKED_READ_THRESHOLD {
                    debug!("Using chunked reads (4 MiB chunks) for {} byte file", file_size);
                    return get_object_chunked(uri, file_size).await;
                }
            }
            Err(e) => {
                // If metadata fetch fails, fall back to whole-file read
                debug!("Failed to get file size for {}: {}, using whole-file read", uri, e);
            }
        }
    }
    
    // Use optimized GET for cloud storage (leverages size cache and concurrent ranges)
    // For cloud storage backends (s3://, gs://, az://): get_optimized() uses:
    //   - Size cache from pre_stat_and_cache() to avoid HEAD requests (v0.9.10)
    //     NOTE: Testing shows this provides NO benefit for high-bandwidth same-region GCS
    //           Only run pre-stat when RangeEngine is enabled (when we actually need sizes)
    //   - Concurrent range requests for large objects (>32MB default threshold)
    //     NOTE: RangeEngine adds ~35% overhead for GCS same-region due to coordination costs
    //           Only enable for cross-region, high-latency, or throttled scenarios
    // For local file:// URIs: get_optimized() delegates to regular get()
    let store = create_store_with_logger(uri)?;
    debug!("ObjectStore created successfully for URI: {}", uri);
    
    let bytes = if uri.starts_with("s3://") || uri.starts_with("gs://") || uri.starts_with("gcs://") || uri.starts_with("az://") {
        // Cloud storage: use get_optimized() to benefit from size cache
        debug!("Using get_optimized() for cloud storage URI: {} (should use size cache if available)", uri);
        let start = std::time::Instant::now();
        let result = store.get_optimized(uri).await
            .with_context(|| format!("Failed to get object from URI: {}", uri))?;
        let elapsed = start.elapsed();
        debug!("get_optimized() completed for {}: {} bytes in {:?}", uri, result.len(), elapsed);
        result
    } else {
        // Local storage: use regular get()
        debug!("Using regular get() for local storage URI: {}", uri);
        store.get(uri).await
            .with_context(|| format!("Failed to get object from URI: {}", uri))?
    };
    
    debug!("GET operation completed successfully for URI: {}, {} bytes retrieved", uri, bytes.len());
    Ok(bytes.to_vec())
}

/// Chunked read implementation for optimal direct:// performance
/// 
/// **INTERNAL USE ONLY - Called only for direct:// URIs with files >8 MiB**
/// 
/// Uses get_range() with 4 MiB chunks to achieve 173x better performance
/// than whole-file reads for direct:// URIs. This optimization is NOT
/// beneficial for cloud storage backends (s3://, gs://, az://) where
/// multiple HTTP requests add latency.
/// 
/// # Safety
/// This function should NEVER be called for cloud storage URIs.
async fn get_object_chunked(uri: &str, file_size: u64) -> anyhow::Result<Vec<u8>> {
    // Safety check: Ensure this is only called for direct:// URIs
    if !uri.starts_with("direct://") {
        bail!("INTERNAL ERROR: get_object_chunked called for non-direct:// URI: {}", uri);
    }
    
    let store = create_store_with_logger(uri)?;
    debug!("Chunked read starting for URI: {}, size: {} bytes", uri, file_size);
    
    let mut result = Vec::with_capacity(file_size as usize);
    let mut offset = 0u64;
    let chunk_size = DIRECT_IO_CHUNK_SIZE as u64;
    
    while offset < file_size {
        let remaining = file_size - offset;
        let chunk_len = remaining.min(chunk_size);
        
        debug!("Fetching chunk at offset {} (length {})", offset, chunk_len);
        
        // get_range(uri, offset, length)
        let chunk_bytes = store.get_range(uri, offset, Some(chunk_len)).await
            .with_context(|| format!("Failed to read chunk at offset {} from {}", offset, uri))?;
        
        result.extend_from_slice(&chunk_bytes);
        offset += chunk_len;
    }
    
    debug!("Chunked read completed: {} bytes in {} chunks", result.len(), 
           (file_size + chunk_size - 1) / chunk_size);
    Ok(result)

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
// Internal config-aware variants (used by workload::run with YAML configs)
// -----------------------------------------------------------------------------

/// Internal GET operation with config support (for performance-critical workloads)
async fn get_object_with_config(
    uri: &str,
    range_config: Option<&crate::config::RangeEngineConfig>,
    page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<Vec<u8>> {
    debug!("GET operation (with config) starting for URI: {}", uri);
    let store = create_store_with_logger_and_config(uri, range_config, page_cache_mode)?;
    
    let bytes = store.get(uri).await
        .with_context(|| format!("Failed to get object from URI: {}", uri))?;
    
    debug!("GET operation completed successfully for URI: {}, {} bytes retrieved", uri, bytes.len());
    Ok(bytes.to_vec())
}

/// Internal PUT operation with config support (for performance-critical workloads)
async fn put_object_with_config(
    uri: &str,
    data: &[u8],
    range_config: Option<&crate::config::RangeEngineConfig>,
    page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<()> {
    debug!("PUT operation (with config) starting for URI: {}, {} bytes", uri, data.len());
    let store = create_store_with_logger_and_config(uri, range_config, page_cache_mode)?;
    
    store.put(uri, data).await
        .with_context(|| format!("Failed to put object to URI: {}", uri))?;
    
    debug!("PUT operation completed successfully for URI: {}", uri);
    Ok(())
}

/// Internal DELETE operation with config support (for performance-critical workloads)
async fn delete_object_with_config(
    uri: &str,
    range_config: Option<&crate::config::RangeEngineConfig>,
    page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<()> {
    debug!("DELETE operation (with config) starting for URI: {}", uri);
    let store = create_store_with_logger_and_config(uri, range_config, page_cache_mode)?;
    
    store.delete(uri).await
        .with_context(|| format!("Failed to delete object from URI: {}", uri))?;
    
    debug!("DELETE operation completed successfully for URI: {}", uri);
    Ok(())
}

/// Internal MKDIR operation with config support (for filesystem backends)
async fn mkdir_with_config(
    uri: &str,
    range_config: Option<&crate::config::RangeEngineConfig>,
    page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<()> {
    debug!("MKDIR operation (with config) starting for URI: {}", uri);
    let store = create_store_with_logger_and_config(uri, range_config, page_cache_mode)?;
    
    store.mkdir(uri).await
        .with_context(|| format!("Failed to create directory at URI: {}", uri))?;
    
    debug!("MKDIR operation completed successfully for URI: {}", uri);
    Ok(())
}

/// Internal RMDIR operation with config support (for filesystem backends)
async fn rmdir_with_config(
    uri: &str,
    recursive: bool,
    range_config: Option<&crate::config::RangeEngineConfig>,
    page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<()> {
    debug!("RMDIR operation (with config, recursive={}) starting for URI: {}", recursive, uri);
    let store = create_store_with_logger_and_config(uri, range_config, page_cache_mode)?;
    
    store.rmdir(uri, recursive).await
        .with_context(|| format!("Failed to remove directory at URI: {}", uri))?;
    
    debug!("RMDIR operation completed successfully for URI: {}", uri);
    Ok(())
}

// -----------------------------------------------------------------------------
// NON-LOGGING variants for replay (avoid logging replay operations)
// -----------------------------------------------------------------------------

/// GET operation WITHOUT logging (for replay)
pub async fn get_object_no_log(uri: &str) -> anyhow::Result<Vec<u8>> {
    let store = create_store_for_uri(uri)?;
    let bytes = store.get(uri).await
        .with_context(|| format!("Failed to get object from URI: {}", uri))?;
    // Convert Bytes to Vec<u8> for compatibility
    Ok(bytes.to_vec())
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
    pub mean_us: u64,
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
pub async fn run(cfg: &Config, tree_manifest: Option<TreeManifest>) -> Result<Summary> {
    info!("Starting workload execution: duration={:?}, concurrency={}", cfg.duration, cfg.concurrency);
    
    let start = Instant::now();
    let deadline = start + cfg.duration;

    // Detect if we need separate object pools for mixed DELETE + (GET|STAT) workloads
    let (has_delete, has_readonly) = detect_pool_requirements(&cfg.workload);
    let needs_separate_pools = has_delete && has_readonly;
    
    if needs_separate_pools {
        info!("Mixed workload detected: Using separate readonly and deletable object pools");
        println!("Mixed workload: Using separate object pools (readonly for GET/STAT, deletable for DELETE)");
    }

    // Pre-resolve GET, DELETE, and STAT sources once
    // SKIP pre-resolution when using directory tree mode (path_selector handles selection)
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
    
    // Skip URI pre-resolution when in directory tree mode
    // PathSelector will dynamically select files from the tree
    let mut pre = PreResolved::default();
    if tree_manifest.is_none() && total_patterns > 0 {
        println!("Resolving {} operation patterns ({} GET, {} DELETE, {} STAT)...", 
                 total_patterns, get_count, delete_count, stat_count);
                 
        for wo in &cfg.workload {
        match &wo.spec {
            OpSpec::Get { .. } => {
                let original_uri = cfg.get_uri(&wo.spec);
                let uri = rewrite_pattern_for_pool(&original_uri, false, needs_separate_pools);
                
                if needs_separate_pools && uri != original_uri {
                    info!("Rewriting GET pattern for readonly pool: {} -> {}", original_uri, uri);
                }
                info!("Resolving GET pattern: {}", uri);
                
                match prefetch_uris_multi_backend(&uri).await {
                    Ok(full_uris) if !full_uris.is_empty() => {
                        info!("Found {} objects for GET pattern: {}", full_uris.len(), uri);
                        
                        // v0.9.10: Pre-stat objects for size caching (cloud storage optimization)
                        // This eliminates HEAD overhead on subsequent get_optimized() calls
                        // ONLY run when RangeEngine is enabled, since that's when we need object sizes
                        let should_prestat = (uri.starts_with("s3://") || uri.starts_with("gs://") || 
                                             uri.starts_with("gcs://") || uri.starts_with("az://")) &&
                                            cfg.range_engine.as_ref().map(|re| re.enabled).unwrap_or(false);
                        
                        if should_prestat {
                            let store = create_store_for_uri(&uri)?;
                            let start = std::time::Instant::now();
                            match store.pre_stat_and_cache(&full_uris, 100).await {
                                Ok(cached_count) => {
                                    info!("Pre-statted {} objects in {:?} (size cache populated for RangeEngine)", 
                                          cached_count, start.elapsed());
                                }
                                Err(e) => {
                                    // Non-fatal: pre-stat optimization failed, but workload can continue
                                    warn!("Pre-stat failed (workload will continue): {}", e);
                                }
                            }
                        }
                        
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
                let original_uri = cfg.get_meta_uri(&wo.spec);
                let uri = rewrite_pattern_for_pool(&original_uri, true, needs_separate_pools);
                
                if needs_separate_pools && uri != original_uri {
                    info!("Rewriting DELETE pattern for deletable pool: {} -> {}", original_uri, uri);
                }
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
                let original_uri = cfg.get_meta_uri(&wo.spec);
                let uri = rewrite_pattern_for_pool(&original_uri, false, needs_separate_pools);
                
                if needs_separate_pools && uri != original_uri {
                    info!("Rewriting STAT pattern for readonly pool: {} -> {}", original_uri, uri);
                }
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
    } else if tree_manifest.is_some() {
        // Directory tree mode: No pre-resolution needed
        // PathSelector will dynamically select files from the tree at runtime
        println!("Directory tree mode: Using PathSelector for dynamic file selection");
        info!("Skipping URI pre-resolution (tree mode with {} patterns)", total_patterns);
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

    // Create rate controller (v0.7.1)
    use crate::rate_controller::OptionalRateController;
    let rate_controller = Arc::new(OptionalRateController::new(
        cfg.io_rate.clone(), 
        cfg.concurrency
    ));
    
    if rate_controller.is_enabled() {
        if let Some(ref rate_cfg) = cfg.io_rate {
            info!("Rate control enabled: target {:?} IOPS, distribution {:?}", 
                rate_cfg.iops, rate_cfg.distribution);
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
    
    // Create PathSelector for directory-based operations (v0.7.0)
    // MKDIR/RMDIR operations REQUIRE a TreeManifest - they only make sense with directory structure
    // If user wants to test mkdir/rmdir, they MUST configure directory_structure in prepare phase
    let path_selector: Option<Arc<PathSelector>> = if let Some(manifest) = tree_manifest {
        // Extract agent_id, num_agents, and path_selection strategy from distributed config
        // Default to single-agent mode with Random strategy if not configured
        let (agent_id, num_agents, strategy, partition_overlap) = if let Some(ref dist) = cfg.distributed {
            let num_agents = dist.agents.len();
            // TODO: Need mechanism to identify which agent this is (env var? command line flag?)
            // For now, default to agent 0 (coordinator) for single-process testing
            let agent_id = 0;
            let strategy = dist.path_selection.clone();
            let overlap = dist.partition_overlap;
            (agent_id, num_agents, strategy, overlap)
        } else {
            // Single-agent mode: use Random strategy (all directories available)
            (0, 1, PathSelectionStrategy::Random, 0.3)
        };
        
        info!("Creating PathSelector: strategy={:?}, agent_id={}, num_agents={}, {} total dirs",
            strategy, agent_id, num_agents, manifest.all_directories.len()
        );
        
        Some(Arc::new(PathSelector::new(manifest, agent_id, num_agents, strategy, partition_overlap)))
    } else {
        None
    };
    
    let mut handles = Vec::with_capacity(cfg.concurrency);
    for _ in 0..cfg.concurrency {
        let op_sems = op_semaphores.clone();
        let workload = cfg.workload.clone();
        let chooser = chooser.clone();
        let pre = pre.clone();
        let cfg = cfg.clone();
        let separate_pools = needs_separate_pools;  // Clone flag for workers
        let path_selector = path_selector.clone();  // Clone PathSelector for this worker
        let rate_controller = rate_controller.clone();  // Clone rate controller for this worker

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

                // Apply rate control throttling (v0.7.1)
                // This should happen BEFORE acquiring the operation-specific permit
                // to ensure we're controlling the rate of operation STARTS, not queued operations
                rate_controller.wait().await;

                // Acquire permit for this specific operation
                let _p = op_sems[idx].acquire().await.unwrap();
                
                let op = &workload[idx].spec;

                match op {
                    OpSpec::Get { .. } => {
                        // GET operation: Use PathSelector if available (directory tree mode),
                        // otherwise fall back to pre-resolved URIs (legacy mode)
                        let full_uri = if let Some(ref selector) = path_selector {
                            // Directory tree mode: Select file from tree
                            let file_path = selector.select_file();
                            // In tree mode, use target directly (path: "/" is ignored)
                            let base_uri = cfg.target.as_ref()
                                .ok_or_else(|| anyhow!("target required in tree mode"))?;
                            
                            // Build full URI for the specific file
                            if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, file_path)
                            } else {
                                format!("{}/{}", base_uri, file_path)
                            }
                        } else {
                            // Legacy mode: Pick from pre-resolved list
                            let original_uri = cfg.get_uri(op);
                            let uri = rewrite_pattern_for_pool(&original_uri, false, separate_pools);
                            
                            let src = pre.get_for_uri(&uri).unwrap();
                            let mut r = rng();
                            let uri_idx = r.random_range(0..src.full_uris.len());
                            src.full_uris[uri_idx].clone()
                        };

                        let t0 = Instant::now();
                        let bytes = get_object_with_config(
                            &full_uri,
                            cfg.range_engine.as_ref(),
                            cfg.page_cache_mode,
                        ).await?;
                        let duration = t0.elapsed();

                        let bucket = crate::metrics::bucket_index(bytes.len());
                        ws.hist_get.record(bucket, duration);
                        ws.get_ops += 1;
                        ws.get_bytes += bytes.len() as u64;
                        ws.get_bins.add(bytes.len() as u64);
                    }
                    OpSpec::Put { dedup_factor, compress_factor, .. } => {
                        // PUT operation: Use PathSelector if available (directory tree mode),
                        // otherwise use random key generation (legacy mode)
                        //
                        // Tree mode: Overwrite/update existing file in tree structure
                        // Legacy mode: Create new object with random name
                        let (full_uri, sz) = if let Some(ref selector) = path_selector {
                            // Directory tree mode: Select EXISTING file from tree to overwrite
                            let file_path = selector.select_file();
                            let (_base_uri, size_spec) = cfg.get_put_size_spec(op);
                            
                            // Generate size for this write
                            use crate::size_generator::SizeGenerator;
                            let size_generator = SizeGenerator::new(&size_spec)?;
                            let sz = size_generator.generate();
                            
                            // In tree mode, use target directly (path from op is ignored)
                            let base_uri = cfg.target.as_ref()
                                .ok_or_else(|| anyhow!("target required in tree mode"))?;
                            
                            // Build full URI for the existing file
                            let full_uri = if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, file_path)
                            } else {
                                format!("{}/{}", base_uri, file_path)
                            };
                            
                            (full_uri, sz)
                        } else {
                            // Legacy mode: Create new object with random name
                            let (base_uri, size_spec) = cfg.get_put_size_spec(op);
                            
                            use crate::size_generator::SizeGenerator;
                            let size_generator = SizeGenerator::new(&size_spec)?;
                            let sz = size_generator.generate();
                            
                            let key = {
                                let mut r = rng();
                                format!("obj_{}", r.random::<u64>())
                            };
                            
                            let full_uri = if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, key)
                            } else {
                                format!("{}/{}", base_uri, key)
                            };
                            
                            (full_uri, sz)
                        };
                        
                        // Generate data using s3dlio's controlled data generation
                        let buf = s3dlio::generate_controlled_data(
                            sz as usize,
                            *dedup_factor,
                            *compress_factor
                        );

                        let t0 = Instant::now();
                        put_object_with_config(
                            &full_uri,
                            &buf,
                            cfg.range_engine.as_ref(),
                            cfg.page_cache_mode,
                        ).await?;
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
                        // STAT operation: Use PathSelector if available (directory tree mode),
                        // otherwise fall back to pre-resolved URIs (legacy mode)
                        let full_uri = if let Some(ref selector) = path_selector {
                            // Directory tree mode: Select file from tree
                            let file_path = selector.select_file();
                            // In tree mode, use target directly (path from op is ignored)
                            let base_uri = cfg.target.as_ref()
                                .ok_or_else(|| anyhow!("target required in tree mode"))?;
                            
                            // Build full URI for the specific file
                            if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, file_path)
                            } else {
                                format!("{}/{}", base_uri, file_path)
                            }
                        } else {
                            // Legacy mode: Pick from pre-resolved list
                            let original_pattern = cfg.get_meta_uri(op);
                            let pattern = rewrite_pattern_for_pool(&original_pattern, false, separate_pools);
                            
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
                        // DELETE operation: Use PathSelector if available (directory tree mode),
                        // otherwise fall back to pre-resolved URIs (legacy mode)
                        let full_uri = if let Some(ref selector) = path_selector {
                            // Directory tree mode: Select file from tree
                            let file_path = selector.select_file();
                            // In tree mode, use target directly (path from op is ignored)
                            let base_uri = cfg.target.as_ref()
                                .ok_or_else(|| anyhow!("target required in tree mode"))?;
                            
                            // Build full URI for the specific file
                            if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, file_path)
                            } else {
                                format!("{}/{}", base_uri, file_path)
                            }
                        } else {
                            // Legacy mode: Pick from pre-resolved list
                            let original_pattern = cfg.get_meta_uri(op);
                            let pattern = rewrite_pattern_for_pool(&original_pattern, true, separate_pools);
                            
                            let src = pre.delete_for_uri(&pattern)
                                .ok_or_else(|| anyhow!("No pre-resolved URIs for DELETE pattern: {}", pattern))?;
                            let mut r = rng();
                            let uri_idx = r.random_range(0..src.full_uris.len());
                            src.full_uris[uri_idx].clone()
                        };

                        let t0 = Instant::now();
                        delete_object_with_config(
                            &full_uri,
                            cfg.range_engine.as_ref(),
                            cfg.page_cache_mode,
                        ).await?;
                        let duration = t0.elapsed();

                        ws.hist_meta.record(0, duration); // Bucket 0 for metadata ops
                        ws.meta_ops += 1;
                        // Delete operations don't transfer data
                        ws.meta_bins.add(0);
                    }
                    OpSpec::Mkdir { .. } => {
                        // MKDIR requires PathSelector with TreeManifest
                        // These operations only make sense with a structured directory tree
                        let dir_name = if let Some(ref selector) = path_selector {
                            selector.select_directory()
                        } else {
                            // No TreeManifest available - this is a configuration error
                            return Err(anyhow!(
                                "MKDIR operation requires directory_structure in prepare config. \
                                 MKDIR/RMDIR are only for testing structured directory trees. \
                                 Configure prepare.directory_structure with width/depth/files_per_dir."
                            ));
                        };
                        
                        // In tree mode, use target directly (path from op is ignored)
                        let base_uri = cfg.target.as_ref()
                            .ok_or_else(|| anyhow!("target required in tree mode"))?;
                        
                        // Build full URI for the specific directory
                        let full_uri = if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, dir_name)
                        } else {
                            format!("{}/{}", base_uri, dir_name)
                        };

                        let t0 = Instant::now();
                        mkdir_with_config(
                            &full_uri,
                            cfg.range_engine.as_ref(),
                            cfg.page_cache_mode,
                        ).await?;
                        let duration = t0.elapsed();

                        ws.hist_meta.record(0, duration); // Bucket 0 for metadata ops
                        ws.meta_ops += 1;
                        // Mkdir operations don't transfer data
                        ws.meta_bins.add(0);
                    }
                    OpSpec::Rmdir { recursive, .. } => {
                        // RMDIR requires PathSelector with TreeManifest
                        // These operations only make sense with a structured directory tree
                        let dir_name = if let Some(ref selector) = path_selector {
                            selector.select_directory()
                        } else {
                            // No TreeManifest available - this is a configuration error
                            return Err(anyhow!(
                                "RMDIR operation requires directory_structure in prepare config. \
                                 MKDIR/RMDIR are only for testing structured directory trees. \
                                 Configure prepare.directory_structure with width/depth/files_per_dir."
                            ));
                        };
                        
                        // In tree mode, use target directly (path from op is ignored)
                        let base_uri = cfg.target.as_ref()
                            .ok_or_else(|| anyhow!("target required in tree mode"))?;
                        
                        // Build full URI for the specific directory
                        let full_uri = if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, dir_name)
                        } else {
                            format!("{}/{}", base_uri, dir_name)
                        };

                        let t0 = Instant::now();
                        rmdir_with_config(
                            &full_uri,
                            *recursive,
                            cfg.range_engine.as_ref(),
                            cfg.page_cache_mode,
                        ).await?;
                        let duration = t0.elapsed();

                        ws.hist_meta.record(0, duration); // Bucket 0 for metadata ops
                        ws.meta_ops += 1;
                        // Rmdir operations don't transfer data
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
        mean_us: get_combined.mean() as u64,
        p50_us: get_combined.value_at_quantile(0.50),
        p95_us: get_combined.value_at_quantile(0.95),
        p99_us: get_combined.value_at_quantile(0.99),
    };
    let put = OpAgg {
        bytes: put_bytes,
        ops: put_ops,
        mean_us: put_combined.mean() as u64,
        p50_us: put_combined.value_at_quantile(0.50),
        p95_us: put_combined.value_at_quantile(0.95),
        p99_us: put_combined.value_at_quantile(0.99),
    };
    let meta = OpAgg {
        bytes: meta_bytes,
        ops: meta_ops,
        mean_us: meta_combined.mean() as u64,
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

    // Detailed size-bucketed histograms are now shown in the consolidated Results section
    // (removed duplicate output here - was confusing to show latency stats twice)

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



