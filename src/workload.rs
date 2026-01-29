// src/workload.rs
//
use anyhow::{anyhow, bail, Context, Result};
use hdrhistogram::Histogram;
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::Semaphore;
use tracing::{debug, info, warn, error, trace};

use rand::{rng, Rng};
//use rand::rngs::SmallRng;
use rand::distr::weighted::WeightedIndex;
use rand_distr::Distribution;

use std::time::{Duration, Instant};
use crate::config::{Config, OpSpec, PathSelectionStrategy};
use crate::directory_tree::TreeManifest;

use std::collections::HashMap;
use crate::bucket_index;

use s3dlio::object_store::{
    store_for_uri, store_for_uri_with_logger, ObjectStore,
    GcsConfig, GcsObjectStore,
};
use s3dlio::file_store::{FileSystemObjectStore, FileSystemConfig};
use s3dlio::PageCacheMode as S3dlioPageCacheMode;
use s3dlio::{init_op_logger, finalize_op_logger, global_logger};

// Re-export prepare module functions for convenience
pub use crate::prepare::{
    prepare_objects, verify_prepared_objects,
    generate_cleanup_objects,
    PathSelector, PreparedObject, PrepareMetrics,
};

// Re-export cleanup module functions
pub use crate::cleanup::cleanup_prepared_objects;

// -----------------------------------------------------------------------------
// Import chunked read constants from constants module
use crate::constants::{DIRECT_IO_CHUNK_SIZE, CHUNKED_READ_THRESHOLD};

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

/// Create ObjectStore instance from Config, respecting multi-endpoint configuration
/// 
/// Priority order:
/// 1. Per-agent multi_endpoint (if agent_config provided and has multi_endpoint)
/// 2. Global multi_endpoint (from config)
/// 3. Per-agent target_override (if agent_config provided)
/// 4. Global target (from config)
/// 
/// This enables:
/// - Static per-agent endpoint mapping (Agent 1 -> [IP1, IP2], Agent 2 -> [IP3, IP4])
/// - Global endpoint pool shared by all agents
/// - Traditional single-target mode (backward compatible)
pub fn create_store_from_config(
    config: &Config,
    agent_config: Option<&crate::config::AgentConfig>,
) -> anyhow::Result<Box<dyn ObjectStore>> {
    // Priority 1: Per-agent multi-endpoint override
    if let Some(agent) = agent_config {
        if let Some(ref multi_ep) = agent.multi_endpoint {
            debug!("Using per-agent multi-endpoint config for agent: {:?} ({} endpoints, strategy: {})",
                   agent.id, multi_ep.endpoints.len(), multi_ep.strategy);
            return create_multi_endpoint_store(multi_ep, config.range_engine.as_ref(), config.page_cache_mode);
        }
    }
    
    // Priority 2: Global multi-endpoint config
    if let Some(ref multi_ep) = config.multi_endpoint {
        debug!("Using global multi-endpoint config ({} endpoints, strategy: {})",
               multi_ep.endpoints.len(), multi_ep.strategy);
        return create_multi_endpoint_store(multi_ep, config.range_engine.as_ref(), config.page_cache_mode);
    }
    
    // Priority 3: Per-agent target override
    if let Some(agent) = agent_config {
        if let Some(ref target_override) = agent.target_override {
            debug!("Using per-agent target override: {}", target_override);
            return create_store_for_uri_with_config(
                target_override,
                config.range_engine.as_ref(),
                config.page_cache_mode,
            );
        }
    }
    
    // Priority 4: Global target
    if let Some(ref target) = config.target {
        debug!("Using global target: {}", target);
        return create_store_for_uri_with_config(
            target,
            config.range_engine.as_ref(),
            config.page_cache_mode,
        );
    }
    
    bail!("No target configuration specified: must provide 'target', 'multi_endpoint', or per-agent override")
}

/// Create MultiEndpointStore from configuration
/// 
/// Converts load balance strategy string to enum and creates s3dlio MultiEndpointStore.
/// Each endpoint is created with appropriate RangeEngine and PageCache config.
pub fn create_multi_endpoint_store(
    multi_ep_config: &crate::config::MultiEndpointConfig,
    _range_config: Option<&crate::config::RangeEngineConfig>,
    _page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<Box<dyn ObjectStore>> {
    use s3dlio::multi_endpoint::{MultiEndpointStore, LoadBalanceStrategy};
    
    if multi_ep_config.endpoints.is_empty() {
        bail!("multi_endpoint.endpoints cannot be empty");
    }
    
    // Convert strategy string to enum
    let strategy = match multi_ep_config.strategy.to_lowercase().as_str() {
        "round_robin" | "roundrobin" => LoadBalanceStrategy::RoundRobin,
        "least_connections" | "leastconnections" => LoadBalanceStrategy::LeastConnections,
        _ => bail!("Invalid load_balance_strategy '{}': must be 'round_robin' or 'least_connections'",
                   multi_ep_config.strategy),
    };
    
    info!("Creating MultiEndpointStore with {} endpoints, strategy: {:?}",
          multi_ep_config.endpoints.len(), strategy);
    
    for (i, endpoint) in multi_ep_config.endpoints.iter().enumerate() {
        debug!("  Endpoint {}: {}", i + 1, endpoint);
    }
    
    // Create MultiEndpointStore - it will create individual stores for each endpoint
    // Note: s3dlio's MultiEndpointStore handles per-endpoint ObjectStore creation internally
    // TODO: Pass range_config and page_cache_mode to individual endpoint stores (future enhancement)
    let store = MultiEndpointStore::new(
        multi_ep_config.endpoints.clone(),
        strategy,
        None, // thread_count_per_endpoint - let s3dlio decide based on hardware
    ).context("Failed to create MultiEndpointStore")?;
    
    Ok(Box::new(store))
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
            let path = if let Some(stripped) = base_uri.strip_prefix("file://") {
                stripped
            } else if let Some(stripped) = base_uri.strip_prefix("direct://") {
                stripped
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
pub async fn get_object_multi_backend(uri: &str) -> anyhow::Result<bytes::Bytes> {
    trace!("GET operation starting for URI: {}", uri);
    
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
                trace!("direct:// file size: {} bytes", file_size);
                
                // Use chunked reads for files larger than threshold
                if file_size > CHUNKED_READ_THRESHOLD {
                    trace!("Using chunked reads (4 MiB chunks) for {} byte file", file_size);
                    return get_object_chunked(uri, file_size).await;
                }
            }
            Err(e) => {
                // If metadata fetch fails, fall back to whole-file read
                trace!("Failed to get file size for {}: {}, using whole-file read", uri, e);
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
        trace!("Using get_optimized() for cloud storage URI: {} (should use size cache if available)", uri);
        let start = std::time::Instant::now();
        let result = store.get_optimized(uri).await
            .with_context(|| format!("Failed to get object from URI: {}", uri))?;
        let elapsed = start.elapsed();
        trace!("get_optimized() completed for {}: {} bytes in {:?}", uri, result.len(), elapsed);
        result
    } else {
        // Local storage: use regular get()
        trace!("Using regular get() for local storage URI: {}", uri);
        store.get(uri).await
            .with_context(|| format!("Failed to get object from URI: {}", uri))?
    };
    
    trace!("GET operation completed successfully for URI: {}, {} bytes retrieved", uri, bytes.len());
    Ok(bytes)  // Zero-copy: return Bytes directly
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
async fn get_object_chunked(uri: &str, file_size: u64) -> anyhow::Result<bytes::Bytes> {
    // Safety check: Ensure this is only called for direct:// URIs
    if !uri.starts_with("direct://") {
        bail!("INTERNAL ERROR: get_object_chunked called for non-direct:// URI: {}", uri);
    }
    
    let store = create_store_with_logger(uri)?;
    trace!("Chunked read starting for URI: {}, size: {} bytes", uri, file_size);
    
    // Use BytesMut for zero-copy assembly
    let mut result = bytes::BytesMut::with_capacity(file_size as usize);
    let mut offset = 0u64;
    let chunk_size = DIRECT_IO_CHUNK_SIZE as u64;
    
    while offset < file_size {
        let remaining = file_size - offset;
        let chunk_len = remaining.min(chunk_size);
        
        trace!("Fetching chunk at offset {} (length {})", offset, chunk_len);
        
        // get_range(uri, offset, length)
        let chunk_bytes = store.get_range(uri, offset, Some(chunk_len)).await
            .with_context(|| format!("Failed to read chunk at offset {} from {}", offset, uri))?;
        
        result.extend_from_slice(&chunk_bytes);
        offset += chunk_len;
    }
    
    trace!("Chunked read completed: {} bytes in {} chunks", result.len(), 
           file_size.div_ceil(chunk_size));
    Ok(result.freeze())  // Zero-copy: BytesMut→Bytes

}

/// Multi-backend PUT operation using ObjectStore trait (zero-copy with Bytes)
pub async fn put_object_multi_backend(uri: &str, data: bytes::Bytes) -> anyhow::Result<()> {
    trace!("PUT operation starting for URI: {}, {} bytes", uri, data.len());
    let store = create_store_with_logger(uri)?;
    trace!("ObjectStore created successfully for URI: {}", uri);
    
    // Use ObjectStore put method with full URI (s3dlio handles URI parsing)
    // Zero-copy: Bytes is Arc-like, clone() just increments refcount
    store.put(uri, data).await
        .with_context(|| format!("Failed to put object to URI: {}", uri))?;
    
    trace!("PUT operation completed successfully for URI: {}", uri);
    Ok(())
}

/// Multi-backend LIST operation using ObjectStore trait
pub async fn list_objects_multi_backend(uri: &str) -> anyhow::Result<Vec<String>> {
    trace!("LIST operation starting for URI: {}", uri);
    let store = create_store_with_logger(uri)?;
    trace!("ObjectStore created successfully for URI: {}", uri);
    
    // Use ObjectStore list method with full URI and recursive=true (s3dlio handles URI parsing)
    let keys = store.list(uri, true).await
        .with_context(|| format!("Failed to list objects from URI: {}", uri))?;
    
    trace!("LIST operation completed successfully for URI: {}, {} objects found", uri, keys.len());
    Ok(keys)
}

/// Multi-backend STAT operation using ObjectStore trait
/// Uses s3dlio's native stat() method (v0.8.8+)
pub async fn stat_object_multi_backend(uri: &str) -> anyhow::Result<u64> {
    trace!("STAT operation starting for URI: {}", uri);
    let store = create_store_with_logger(uri)?;
    trace!("ObjectStore created successfully for URI: {}", uri);
    
    // Use s3dlio's native stat() method for proper HEAD/metadata operations
    let metadata = store.stat(uri).await
        .with_context(|| format!("Failed to stat object from URI: {}", uri))?;
    
    let size = metadata.size;
    trace!("STAT operation completed successfully for URI: {}, size: {} bytes", uri, size);
    Ok(size)
}

/// Multi-backend DELETE operation using ObjectStore trait
pub async fn delete_object_multi_backend(uri: &str) -> anyhow::Result<()> {
    trace!("DELETE operation starting for URI: {}", uri);
    let store = create_store_with_logger(uri)?;
    trace!("ObjectStore created successfully for URI: {}", uri);
    
    // Use ObjectStore delete method with full URI (s3dlio handles URI parsing)
    store.delete(uri).await
        .with_context(|| format!("Failed to delete object from URI: {}", uri))?;
    
    trace!("DELETE operation completed successfully for URI: {}", uri);
    Ok(())
}

// -----------------------------------------------------------------------------
// ObjectStore caching for connection pool reuse (v0.7.3+)
// -----------------------------------------------------------------------------

/// Store cache type for efficient connection pooling across operations.
/// Uses base URI as key to reuse HTTP clients and connection pools.
pub type StoreCache = Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<Box<dyn ObjectStore>>>>>;

/// Get or create an ObjectStore from cache
/// Cache key is based on base URI + config settings to ensure correct store for each combination
fn get_cached_store(
    uri: &str,
    cache: &StoreCache,
    config: &Config,
    agent_config: Option<&crate::config::AgentConfig>,
) -> anyhow::Result<Arc<Box<dyn ObjectStore>>> {
    // v0.8.22: Multi-endpoint support - create store based on configuration priority
    // If multi-endpoint is configured (global or per-agent), we use a special cache key
    // and create a MultiEndpointStore that handles load balancing across multiple endpoints
    
    // Check if multi-endpoint is configured (per-agent takes priority over global)
    let has_multi_endpoint = agent_config
        .and_then(|a| a.multi_endpoint.as_ref())
        .is_some() || config.multi_endpoint.is_some();
    
    if has_multi_endpoint {
        // For multi-endpoint, create a cache key that represents the entire endpoint set
        // This ensures we reuse the same MultiEndpointStore across all operations
        let multi_ep = agent_config
            .and_then(|a| a.multi_endpoint.as_ref())
            .or(config.multi_endpoint.as_ref())
            .unwrap();
        
        let cache_key = format!("multi_ep:{}:{}:{:?}:{:?}",
            multi_ep.strategy,
            multi_ep.endpoints.join(","),
            config.range_engine.as_ref().map(|c| c.enabled),
            config.page_cache_mode
        );
        
        // Check cache first
        {
            let cache_lock = cache.lock().unwrap();
            if let Some(store) = cache_lock.get(&cache_key) {
                return Ok(Arc::clone(store));
            }
        }
        
        // Not in cache - create new MultiEndpointStore
        let store = create_store_from_config(config, agent_config)?;
        let arc_store = Arc::new(store);
        
        // Add to cache
        {
            let mut cache_lock = cache.lock().unwrap();
            cache_lock.insert(cache_key, Arc::clone(&arc_store));
        }
        
        return Ok(arc_store);
    }
    
    // Traditional single-endpoint path (existing behavior)
    // Extract base URI (protocol + bucket/container)
    let base_uri = if let Some(idx) = uri.find("://") {
        let after_proto = &uri[idx+3..];
        if let Some(slash_idx) = after_proto.find('/') {
            &uri[..idx+3+slash_idx]
        } else {
            uri
        }
    } else {
        uri
    };
    
    // Create cache key including config settings
    let cache_key = format!("{}_range:{:?}_cache:{:?}", 
        base_uri,
        config.range_engine.as_ref().map(|c| c.enabled),
        config.page_cache_mode
    );
    
    // Check cache first
    {
        let cache_lock = cache.lock().unwrap();
        if let Some(store) = cache_lock.get(&cache_key) {
            return Ok(Arc::clone(store));
        }
    }
    
    // Not in cache - create new store
    let store = create_store_with_logger_and_config(uri, config.range_engine.as_ref(), config.page_cache_mode)?;
    let arc_store = Arc::new(store);
    
    // Add to cache
    {
        let mut cache_lock = cache.lock().unwrap();
        cache_lock.insert(cache_key, Arc::clone(&arc_store));
    }
    
    Ok(arc_store)
}

// -----------------------------------------------------------------------------
// Internal config-aware variants (used by workload::run with YAML configs)
// -----------------------------------------------------------------------------

/// Internal GET operation with cached store (for performance-critical workloads)
async fn get_object_cached(
    uri: &str,
    cache: &StoreCache,
    config: &Config,
    agent_config: Option<&crate::config::AgentConfig>,
) -> anyhow::Result<bytes::Bytes> {
    trace!("GET operation (cached store) starting for URI: {}", uri);
    let store = get_cached_store(uri, cache, config, agent_config)?;
    
    let bytes = store.get(uri).await
        .with_context(|| format!("Failed to get object from URI: {}", uri))?;
    
    trace!("GET operation completed successfully for URI: {}, {} bytes retrieved", uri, bytes.len());
    Ok(bytes)  // Zero-copy: return Bytes directly
}

/// Internal PUT operation with cached store (for performance-critical workloads)
async fn put_object_cached(
    uri: &str,
    data: bytes::Bytes,  // Zero-copy: Bytes instead of &[u8]
    cache: &StoreCache,
    config: &Config,
    agent_config: Option<&crate::config::AgentConfig>,
) -> anyhow::Result<()> {
    trace!("PUT operation (cached store) starting for URI: {}, {} bytes", uri, data.len());
    let store = get_cached_store(uri, cache, config, agent_config)?;
    
    store.put(uri, data).await  // Zero-copy: Bytes passed directly
        .with_context(|| format!("Failed to put object to URI: {}", uri))?;
    
    trace!("PUT operation completed successfully for URI: {}", uri);
    Ok(())
}

/// Internal DELETE operation with cached store (for performance-critical workloads)
async fn delete_object_cached(
    uri: &str,
    cache: &StoreCache,
    config: &Config,
    agent_config: Option<&crate::config::AgentConfig>,
) -> anyhow::Result<()> {
    trace!("DELETE operation (cached store) starting for URI: {}", uri);
    let store = get_cached_store(uri, cache, config, agent_config)?;
    
    store.delete(uri).await
        .with_context(|| format!("Failed to delete object from URI: {}", uri))?;
    
    trace!("DELETE operation completed successfully for URI: {}", uri);
    Ok(())
}

/// Internal LIST operation with cached store (for performance-critical workloads)
async fn list_objects_cached(
    uri: &str,
    cache: &StoreCache,
    config: &Config,
    agent_config: Option<&crate::config::AgentConfig>,
) -> anyhow::Result<Vec<String>> {
    trace!("LIST operation (cached store) starting for URI: {}", uri);
    let store = get_cached_store(uri, cache, config, agent_config)?;
    
    let keys = store.list(uri, true).await
        .with_context(|| format!("Failed to list objects from URI: {}", uri))?;
    
    trace!("LIST operation completed successfully for URI: {}, {} objects found", uri, keys.len());
    Ok(keys)
}

/// Internal STAT operation with cached store (for performance-critical workloads)
async fn stat_object_cached(
    uri: &str,
    cache: &StoreCache,
    config: &Config,
    agent_config: Option<&crate::config::AgentConfig>,
) -> anyhow::Result<u64> {
    trace!("STAT operation (cached store) starting for URI: {}", uri);
    let store = get_cached_store(uri, cache, config, agent_config)?;
    
    let metadata = store.stat(uri).await
        .with_context(|| format!("Failed to stat object from URI: {}", uri))?;
    
    let size = metadata.size;
    trace!("STAT operation completed successfully for URI: {}, size: {} bytes", uri, size);
    Ok(size)
}

// -----------------------------------------------------------------------------
// Internal config-aware variants (used by workload::run with YAML configs)
// -----------------------------------------------------------------------------

/// Internal MKDIR operation with config support (for filesystem backends)
async fn mkdir_with_config(
    uri: &str,
    range_config: Option<&crate::config::RangeEngineConfig>,
    page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<()> {
    trace!("MKDIR operation (with config) starting for URI: {}", uri);
    let store = create_store_with_logger_and_config(uri, range_config, page_cache_mode)?;
    
    store.mkdir(uri).await
        .with_context(|| format!("Failed to create directory at URI: {}", uri))?;
    
    trace!("MKDIR operation completed successfully for URI: {}", uri);
    Ok(())
}

/// Internal RMDIR operation with config support (for filesystem backends)
async fn rmdir_with_config(
    uri: &str,
    recursive: bool,
    range_config: Option<&crate::config::RangeEngineConfig>,
    page_cache_mode: Option<crate::config::PageCacheMode>,
) -> anyhow::Result<()> {
    trace!("RMDIR operation (with config, recursive={}) starting for URI: {}", recursive, uri);
    let store = create_store_with_logger_and_config(uri, range_config, page_cache_mode)?;
    
    store.rmdir(uri, recursive).await
        .with_context(|| format!("Failed to remove directory at URI: {}", uri))?;
    
    trace!("RMDIR operation completed successfully for URI: {}", uri);
    Ok(())
}

// -----------------------------------------------------------------------------
// NON-LOGGING variants for replay (avoid logging replay operations)
// -----------------------------------------------------------------------------

/// GET operation WITHOUT logging (for replay)
pub async fn get_object_no_log(uri: &str) -> anyhow::Result<bytes::Bytes> {
    let store = create_store_for_uri(uri)?;
    let bytes = store.get(uri).await
        .with_context(|| format!("Failed to get object from URI: {}", uri))?;
    // Zero-copy: return Bytes directly
    Ok(bytes)
}

/// PUT operation WITHOUT logging (for replay)
pub async fn put_object_no_log(uri: &str, data: bytes::Bytes) -> anyhow::Result<()> {
    let store = create_store_for_uri(uri)?;
    store.put(uri, data).await  // Zero-copy: Bytes passed directly
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
// Cached operations for replay (v0.8.9+) - efficient store reuse
// -----------------------------------------------------------------------------

/// Get or create an ObjectStore from cache (simple version for replay).
/// Uses base URI as cache key for connection pool reuse.
pub fn get_or_create_store(
    uri: &str,
    cache: &StoreCache,
) -> anyhow::Result<Arc<Box<dyn ObjectStore>>> {
    // Extract base URI (protocol + bucket/container)
    let base_uri = if let Some(idx) = uri.find("://") {
        let after_proto = &uri[idx+3..];
        if let Some(slash_idx) = after_proto.find('/') {
            &uri[..idx+3+slash_idx]
        } else {
            uri
        }
    } else {
        uri
    };
    
    // Check cache first
    {
        let cache_lock = cache.lock().unwrap();
        if let Some(store) = cache_lock.get(base_uri) {
            return Ok(Arc::clone(store));
        }
    }
    
    // Not in cache - create new store
    let store = create_store_for_uri(uri)?;
    let arc_store = Arc::new(store);
    
    // Add to cache
    {
        let mut cache_lock = cache.lock().unwrap();
        cache_lock.insert(base_uri.to_string(), Arc::clone(&arc_store));
    }
    
    Ok(arc_store)
}

/// GET operation with cached store (for replay - no per-call store creation)
pub async fn get_object_cached_simple(uri: &str, cache: &StoreCache) -> anyhow::Result<bytes::Bytes> {
    let store = get_or_create_store(uri, cache)?;
    let bytes = store.get(uri).await
        .with_context(|| format!("Failed to get object from URI: {}", uri))?;
    Ok(bytes)  // Zero-copy: return Bytes directly
}

/// PUT operation with cached store (for replay - no per-call store creation)
pub async fn put_object_cached_simple(uri: &str, data: bytes::Bytes, cache: &StoreCache) -> anyhow::Result<()> {
    let store = get_or_create_store(uri, cache)?;
    store.put(uri, data).await  // Zero-copy: Bytes passed directly
        .with_context(|| format!("Failed to put object to URI: {}", uri))
}

/// DELETE operation with cached store (for replay - no per-call store creation)
pub async fn delete_object_cached_simple(uri: &str, cache: &StoreCache) -> anyhow::Result<()> {
    let store = get_or_create_store(uri, cache)?;
    store.delete(uri).await
        .with_context(|| format!("Failed to delete object from URI: {}", uri))
}

/// LIST operation with cached store (for replay - no per-call store creation)
pub async fn list_objects_cached_simple(uri: &str, cache: &StoreCache) -> anyhow::Result<Vec<String>> {
    let store = get_or_create_store(uri, cache)?;
    store.list(uri, true).await
        .with_context(|| format!("Failed to list objects from URI: {}", uri))
}

/// STAT operation with cached store (for replay - no per-call store creation)
pub async fn stat_object_cached_simple(uri: &str, cache: &StoreCache) -> anyhow::Result<u64> {
    let store = get_or_create_store(uri, cache)?;
    let metadata = store.stat(uri).await
        .with_context(|| format!("Failed to stat object from URI: {}", uri))?;
    Ok(metadata.size)
}

// -----------------------------------------------------------------------------
// New summary/aggregation types
// -----------------------------------------------------------------------------
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpAgg {
    pub bytes: u64,
    pub ops: u64,
    pub mean_us: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SizeBins {
    // bucket_index -> (ops, bytes)
    pub by_bucket: HashMap<usize, (u64, u64)>,
}

impl SizeBins {
    pub fn add(&mut self, size_bytes: u64) {
        let b = bucket_index(size_bytes as usize);
        let e = self.by_bucket.entry(b).or_insert((0, 0));
        e.0 += 1;
        e.1 += size_bytes;
    }
    
    pub fn merge_from(&mut self, other: &SizeBins) {
        for (k, (ops, bytes)) in &other.by_bucket {
            let e = self.by_bucket.entry(*k).or_insert((0, 0));
            e.0 += ops;
            e.1 += bytes;
        }
    }
}

// Serializable summary for IPC between processes (v0.7.3+)
// Uses base64-encoded histogram serialization for lossless merging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcSummary {
    pub wall_seconds: f64,
    pub total_bytes: u64,
    pub total_ops: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub get: OpAgg,
    pub put: OpAgg,
    pub meta: OpAgg,
    pub get_bins: SizeBins,
    pub put_bins: SizeBins,
    pub meta_bins: SizeBins,
    
    // Serialized histograms (base64-encoded v2 format)
    // Each is Vec<String> with one entry per bucket
    pub get_hists_serialized: Vec<String>,
    pub put_hists_serialized: Vec<String>,
    pub meta_hists_serialized: Vec<String>,
    
    // v0.7.13: Error statistics
    pub total_errors: u64,
    pub error_rate: f64,
}

impl IpcSummary {
    /// Create IpcSummary from Summary, serializing histograms
    pub fn from_summary(s: &Summary) -> Result<Self> {
        use hdrhistogram::serialization::Serializer;
        use hdrhistogram::serialization::V2Serializer;
        use base64::Engine;
        
        let serialize_ophists = |ophists: &crate::metrics::OpHists| -> Result<Vec<String>> {
            let mut serialized = Vec::new();
            for i in 0..ophists.buckets.len() {
                let hist = ophists.buckets[i].lock().unwrap();
                let mut buf = Vec::new();
                V2Serializer::new()
                    .serialize(&*hist, &mut buf)
                    .context("Failed to serialize histogram")?;
                let encoded = base64::engine::general_purpose::STANDARD.encode(&buf);
                serialized.push(encoded);
            }
            Ok(serialized)
        };
        
        Ok(IpcSummary {
            wall_seconds: s.wall_seconds,
            total_bytes: s.total_bytes,
            total_ops: s.total_ops,
            p50_us: s.p50_us,
            p95_us: s.p95_us,
            p99_us: s.p99_us,
            get: s.get.clone(),
            put: s.put.clone(),
            meta: s.meta.clone(),
            get_bins: s.get_bins.clone(),
            put_bins: s.put_bins.clone(),
            meta_bins: s.meta_bins.clone(),
            get_hists_serialized: serialize_ophists(&s.get_hists)?,
            put_hists_serialized: serialize_ophists(&s.put_hists)?,
            meta_hists_serialized: serialize_ophists(&s.meta_hists)?,
            total_errors: s.total_errors,
            error_rate: s.error_rate,
        })
    }
}

impl From<&Summary> for IpcSummary {
    fn from(s: &Summary) -> Self {
        // Fallback implementation - panics if serialization fails
        // Use from_summary() for proper error handling
        Self::from_summary(s).expect("Failed to serialize summary for IPC")
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
    
    // v0.7.13: Error statistics
    pub total_errors: u64,
    pub error_rate: f64,  // Errors per second at end of workload
}

// -----------------------------------------------------------------------------
// Worker stats merged at the end
// -----------------------------------------------------------------------------
#[derive(Default)]
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


/// Error tracking for workload resilience (v0.7.13+)
/// 
/// Tracks errors across all worker tasks to implement configurable error thresholds:
/// - Total error count (abort if exceeded)
/// - Error rate (errors/second - trigger backoff if exceeded)
/// - Recent error timestamps for rate calculation
#[derive(Clone)]
struct ErrorTracker {
    total_errors: Arc<AtomicU64>,
    recent_errors: Arc<Mutex<Vec<Instant>>>,
    config: crate::config::ErrorHandlingConfig,
}

impl ErrorTracker {
    fn new(config: crate::config::ErrorHandlingConfig) -> Self {
        Self {
            total_errors: Arc::new(AtomicU64::new(0)),
            recent_errors: Arc::new(Mutex::new(Vec::new())),
            config,
        }
    }
    
    /// Record an error and check if thresholds are exceeded
    /// Returns: (should_backoff, should_abort, total_errors, error_rate)
    fn record_error(&self) -> (bool, bool, u64, f64) {
        let now = Instant::now();
        let total = self.total_errors.fetch_add(1, Ordering::Relaxed) + 1;
        
        // Add to recent errors
        let mut recent = self.recent_errors.lock().unwrap();
        recent.push(now);
        
        // Clean up old errors outside the rate window
        let window_start = now - Duration::from_secs_f64(self.config.error_rate_window);
        recent.retain(|&t| t >= window_start);
        
        let errors_in_window = recent.len();
        let error_rate = errors_in_window as f64 / self.config.error_rate_window;
        
        let should_backoff = error_rate >= self.config.error_rate_threshold;
        let should_abort = total >= self.config.max_total_errors;
        
        (should_backoff, should_abort, total, error_rate)
    }
    
    fn get_stats(&self) -> (u64, f64) {
        let total = self.total_errors.load(Ordering::Relaxed);
        let now = Instant::now();
        let mut recent = self.recent_errors.lock().unwrap();
        
        // Clean up old errors
        let window_start = now - Duration::from_secs_f64(self.config.error_rate_window);
        recent.retain(|&t| t >= window_start);
        
        let error_rate = recent.len() as f64 / self.config.error_rate_window;
        (total, error_rate)
    }
}

/// Configuration for retry with exponential backoff (v0.8.13)
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_multiplier: f64,
    pub jitter_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: crate::constants::DEFAULT_MAX_RETRIES,
            initial_delay_ms: crate::constants::DEFAULT_INITIAL_RETRY_DELAY_MS,
            max_delay_ms: crate::constants::DEFAULT_MAX_RETRY_DELAY_MS,
            backoff_multiplier: crate::constants::DEFAULT_RETRY_BACKOFF_MULTIPLIER,
            jitter_factor: crate::constants::DEFAULT_RETRY_JITTER_FACTOR,
        }
    }
}

impl From<&crate::config::ErrorHandlingConfig> for RetryConfig {
    fn from(cfg: &crate::config::ErrorHandlingConfig) -> Self {
        Self {
            max_retries: cfg.max_retries,
            initial_delay_ms: cfg.initial_retry_delay_ms,
            max_delay_ms: cfg.max_retry_delay_ms,
            backoff_multiplier: cfg.retry_backoff_multiplier,
            jitter_factor: cfg.retry_jitter_factor,
        }
    }
}

/// Result of a retry operation
pub enum RetryResult<T> {
    /// Operation succeeded
    Success(T),
    /// All retries failed - contains the last error
    Failed(anyhow::Error),
}

/// Execute an async operation with retry and exponential backoff (v0.8.13)
/// 
/// This is a reusable utility for both workload and prepare phases.
/// 
/// # Arguments
/// * `operation_name` - Name for logging purposes
/// * `config` - Retry configuration (delays, multiplier, jitter)
/// * `op_fn` - The async operation to execute
/// 
/// # Returns
/// * `RetryResult::Success(T)` - Operation succeeded (possibly after retries)
/// * `RetryResult::Failed(Error)` - All retries exhausted
pub async fn retry_with_backoff<F, Fut, T>(
    operation_name: &str,
    config: &RetryConfig,
    op_fn: F,
) -> RetryResult<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let max_attempts = config.max_retries + 1;  // Initial attempt + retries
    let mut current_delay_ms = config.initial_delay_ms as f64;
    let mut last_error: Option<anyhow::Error> = None;
    
    for attempt in 1..=max_attempts {
        match op_fn().await {
            Ok(result) => return RetryResult::Success(result),
            Err(e) => {
                last_error = Some(e);
                
                let is_last_attempt = attempt == max_attempts;
                
                if is_last_attempt {
                    debug!("❌ {} failed after {} attempts", operation_name, max_attempts);
                } else {
                    // Calculate delay with jitter
                    let jitter = if config.jitter_factor > 0.0 {
                        let mut rng = rand::rng();
                        let jitter_range = 1.0 - config.jitter_factor 
                            ..= 1.0 + config.jitter_factor;
                        rng.random_range(jitter_range)
                    } else {
                        1.0
                    };
                    
                    let delay_with_jitter_ms = (current_delay_ms * jitter).min(config.max_delay_ms as f64);
                    let delay = Duration::from_millis(delay_with_jitter_ms as u64);
                    
                    trace!("🔄 Retrying {} (attempt {}/{}) in {:?}", 
                        operation_name, attempt + 1, max_attempts, delay);
                    
                    tokio::time::sleep(delay).await;
                    
                    // Exponential backoff for next iteration
                    current_delay_ms = (current_delay_ms * config.backoff_multiplier)
                        .min(config.max_delay_ms as f64);
                }
            }
        }
    }
    
    RetryResult::Failed(last_error.unwrap_or_else(|| anyhow!("Unknown error in retry")))
}

/// Helper to execute an operation with error handling and retry logic (v0.7.13+)
/// 
/// v0.8.13: Added exponential backoff with jitter between retries
/// 
/// Returns: Ok(Some(result)) on success, Ok(None) on error (skip), Err on abort
async fn execute_with_error_handling<F, Fut, T>(
    operation_name: &str,
    error_tracker: &ErrorTracker,
    op_fn: F,
) -> Result<Option<T>>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let config = &error_tracker.config;
    let max_attempts = if config.retry_on_error {
        config.max_retries + 1  // Initial attempt + retries
    } else {
        1  // Single attempt, skip on error
    };
    
    // Track current backoff delay for exponential growth
    let mut current_delay_ms = config.initial_retry_delay_ms as f64;
    
    for attempt in 1..=max_attempts {
        match op_fn().await {
            Ok(result) => return Ok(Some(result)),
            Err(e) => {
                // Record error and check thresholds
                let (should_backoff, should_abort, total_errors, error_rate) = error_tracker.record_error();
                
                // Log individual error (visible with -vv debug level)
                debug!("❌ {} error (attempt {}/{}): {} [total_errors: {}, rate: {:.2}/sec]",
                    operation_name, attempt, max_attempts, e, total_errors, error_rate);
                
                if should_abort {
                    error!("❌ ERROR THRESHOLD EXCEEDED: {} total errors (max: {})",
                        total_errors, config.max_total_errors);
                    error!("   Aborting workload to prevent further failures");
                    return Err(anyhow!(
                        "Aborting workload: {} total errors exceeded threshold of {}",
                        total_errors, config.max_total_errors
                    ));
                }
                
                // Check if we should apply high-error-rate backoff (separate from retry backoff)
                if should_backoff {
                    warn!("⚠️  HIGH ERROR RATE: {:.2} errors/sec (threshold: {:.2})", 
                        error_rate, config.error_rate_threshold);
                    warn!("   Backing off for {:?} to allow transient issues to clear", 
                        config.backoff_duration);
                    tokio::time::sleep(config.backoff_duration).await;
                }
                
                let is_last_attempt = attempt == max_attempts;
                
                if is_last_attempt {
                    // Last attempt failed - skip this operation and continue
                    // Log at warn level (visible with -v) - user should know operations are failing
                    warn!("⚠️  {} FAILED after {} attempts - SKIPPING (total errors: {})",
                        operation_name, max_attempts, total_errors);
                    warn!("   Last error: {}", e);
                    return Ok(None);  // Skip operation, don't abort workload
                } else {
                    // v0.8.13: Apply exponential backoff with jitter before retry
                    let jitter = if config.retry_jitter_factor > 0.0 {
                        use rand::Rng;
                        let mut rng = rand::rng();
                        // Jitter range: [1 - jitter_factor, 1 + jitter_factor]
                        let jitter_range = 1.0 - config.retry_jitter_factor 
                            ..= 1.0 + config.retry_jitter_factor;
                        rng.random_range(jitter_range)
                    } else {
                        1.0
                    };
                    
                    let delay_with_jitter_ms = (current_delay_ms * jitter).min(config.max_retry_delay_ms as f64);
                    let delay = Duration::from_millis(delay_with_jitter_ms as u64);
                    
                    info!("🔄 Retrying {} (attempt {}/{}) in {:?} after error: {}", 
                        operation_name, attempt + 1, max_attempts, delay, e);
                    
                    tokio::time::sleep(delay).await;
                    
                    // Exponential backoff: multiply delay for next iteration
                    current_delay_ms = (current_delay_ms * config.retry_backoff_multiplier)
                        .min(config.max_retry_delay_ms as f64);
                }
            }
        }
    }
    
    // Should never reach here, but return None as fallback
    Ok(None)
}

/// Public entry: run a config and print a summary-like struct back.
pub async fn run(cfg: &Config, tree_manifest: Option<TreeManifest>) -> Result<Summary> {
    info!("Starting workload execution: duration={:?}, concurrency={}", cfg.duration, cfg.concurrency);
    
    // CRITICAL: Initialize RNG seed for THIS RUN
    // Combines PID + nanosecond timestamp to ensure each run generates different data
    // This prevents successive runs from generating identical data patterns
    crate::data_gen_pool::set_global_rng_seed(None);
    
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
    
    // Create shared atomic counters for live progress stats
    let live_ops = Arc::new(AtomicU64::new(0));
    let live_bytes = Arc::new(AtomicU64::new(0));
    
    // Create progress bar for time-based execution
    let pb = ProgressBar::new(cfg.duration.as_secs());
    pb.set_style(ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len}s ({eta_precise}) {msg}"
    )?);
    pb.set_message(format!("running with {} workers", cfg.concurrency));
    
    // Spawn progress monitoring task with live stats
    let pb_clone = pb.clone();
    let duration = cfg.duration;
    let concurrency = cfg.concurrency;  // Copy value for closure
    let ops_clone = live_ops.clone();
    let bytes_clone = live_bytes.clone();
    
    // v0.7.13: Add cancellation channel for progress task
    let (tx_cancel_progress, mut rx_cancel_progress) = tokio::sync::oneshot::channel::<()>();
    
    let progress_handle = tokio::spawn(async move {
        let progress_start = Instant::now();
        let update_interval = Duration::from_millis(100); // Update every 100ms
        let mut last_ops = 0u64;
        let mut last_bytes = 0u64;
        let mut last_update = progress_start;
        
        loop {
            let elapsed = progress_start.elapsed();
            if elapsed >= duration {
                pb_clone.set_position(duration.as_secs());
                break;
            }
            
            pb_clone.set_position(elapsed.as_secs());
            
            // Calculate live stats
            let now = Instant::now();
            let current_ops = ops_clone.load(Ordering::Relaxed);
            let current_bytes = bytes_clone.load(Ordering::Relaxed);
            let time_delta = now.duration_since(last_update).as_secs_f64();
            
            if time_delta >= 0.5 {  // Update stats display every 0.5s
                let ops_delta = current_ops.saturating_sub(last_ops);
                let bytes_delta = current_bytes.saturating_sub(last_bytes);
                
                let ops_per_sec = ops_delta as f64 / time_delta;
                let mib_per_sec = (bytes_delta as f64 / 1_048_576.0) / time_delta;
                
                // Calculate average latency (very rough estimate)
                let avg_latency_ms = if ops_per_sec > 0.0 {
                    (time_delta * 1000.0 * concurrency as f64) / ops_delta as f64
                } else {
                    0.0
                };
                
                pb_clone.set_message(format!(
                    "{} workers | {:.0} ops/s | {:.1} MiB/s | avg {:.2}ms",
                    concurrency, ops_per_sec, mib_per_sec, avg_latency_ms
                ));
                
                last_ops = current_ops;
                last_bytes = current_bytes;
                last_update = now;
            }
            
            // v0.7.13: Check for cancellation signal (workload error or completion)
            tokio::select! {
                _ = tokio::time::sleep(update_interval) => {}
                _ = &mut rx_cancel_progress => {
                    info!("Progress task cancelled - workload ending early");
                    break;
                }
            }
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
    
    // Create ObjectStore pool for connection reuse (v0.7.3+)
    // Instead of creating a new store for every operation (wasteful!),
    // we create stores once and reuse them across all operations
    let store_cache: StoreCache = Arc::new(std::sync::Mutex::new(HashMap::new()));
    
    // v0.7.13: Create error tracker for resilient error handling
    let error_tracker = ErrorTracker::new(cfg.error_handling.clone());
    
    // v0.8.19: Early error detection for skip_verification issues
    // Monitor error rate in first 5-10 seconds and warn if very high (indicates config problem)
    if let Some(ref prepare) = cfg.prepare {
        if prepare.skip_verification {
            let has_get_ops = cfg.workload.iter().any(|op| matches!(op.spec, OpSpec::Get { .. }));
            
            if has_get_ops {
                let error_tracker_clone = error_tracker.clone();
                let ops_clone_for_early = live_ops.clone();
                
                tokio::spawn(async move {
                    // Wait 5 seconds for workload warmup
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    
                    let (errors, _rate) = error_tracker_clone.get_stats();
                    let ops = ops_clone_for_early.load(Ordering::Relaxed);
                    
                    // Check if error rate > 90% (very high, likely config issue)
                    if ops > 10 && errors > 0 {
                        let error_rate_pct = (errors as f64 / ops as f64) * 100.0;
                        
                        if error_rate_pct > 90.0 {
                            eprintln!("\n{}", "=".repeat(80));
                            eprintln!("⚠️  WARNING: Very high error rate detected!");
                            eprintln!("{}", "=".repeat(80));
                            eprintln!("After 5 seconds: {} errors out of {} operations ({:.1}% error rate)",
                                errors, ops, error_rate_pct);
                            eprintln!();
                            eprintln!("This usually means:");
                            eprintln!("  • skip_verification=true is set, but objects don't exist");
                            eprintln!("  • GET operations are trying to read non-existent objects");
                            eprintln!();
                            eprintln!("Recommended action:");
                            eprintln!("  1. Stop this workload (Ctrl-C)");
                            eprintln!("  2. Set skip_verification=false in your config");
                            eprintln!("  3. Re-run to create objects during prepare phase");
                            eprintln!("{}", "=".repeat(80));
                            eprintln!();
                        }
                    }
                });
            }
        }
    }
    
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
        let ops_counter = live_ops.clone();  // Clone atomic counter for live stats
        let bytes_counter = live_bytes.clone();  // Clone atomic counter for live stats
        let store_cache = store_cache.clone();  // Clone store cache for this worker
        let error_tracker = error_tracker.clone();  // Clone error tracker for this worker

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
                        // v0.7.13: Wrap GET operation with error handling
                        // URI resolution needs to happen outside the retry closure
                        let full_uri = if let Some(ref selector) = path_selector {
                            let file_path = selector.select_file();
                            let base_uri = cfg.target.as_ref()
                                .ok_or_else(|| anyhow!("target required in tree mode"))?;
                            if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, file_path)
                            } else {
                                format!("{}/{}", base_uri, file_path)
                            }
                        } else {
                            let original_uri = cfg.get_uri(op);
                            let uri = rewrite_pattern_for_pool(&original_uri, false, separate_pools);
                            let src = pre.get_for_uri(&uri).unwrap();
                            let mut r = rng();
                            let uri_idx = r.random_range(0..src.full_uris.len());
                            src.full_uris[uri_idx].clone()
                        };

                        let store_cache_get = store_cache.clone();
                        let cfg_for_get = cfg.clone();
                        let uri_for_closure = full_uri.clone();
                        
                        let result = execute_with_error_handling(
                            "GET",
                            &error_tracker,
                            || async {
                                let t0 = Instant::now();
                                let bytes = get_object_cached(
                                    &uri_for_closure,
                                    &store_cache_get,
                                    &cfg_for_get,
                                    None,  // agent_config: None for standalone mode
                                ).await?;
                                let duration = t0.elapsed();
                                Ok((bytes, duration))
                            }
                        ).await;
                        
                        match result {
                            Ok(Some((bytes, duration))) => {
                                let bucket = crate::metrics::bucket_index(bytes.len());
                                ws.hist_get.record(bucket, duration);
                                ws.get_ops += 1;
                                ws.get_bytes += bytes.len() as u64;
                                ws.get_bins.add(bytes.len() as u64);
                                
                                if let Some(ref tracker) = cfg.live_stats_tracker {
                                    tracker.record_get(bytes.len(), duration);
                                }
                                
                                ops_counter.fetch_add(1, Ordering::Relaxed);
                                bytes_counter.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                            }
                            Ok(None) => {
                                // Operation skipped due to error - continue to next operation
                            }
                            Err(e) => {
                                // Error threshold exceeded - abort workload
                                return Err(e);
                            }
                        }
                    }
                    OpSpec::Put { dedup_factor, compress_factor, .. } => {
                        // v0.7.13: Wrap PUT operation with error handling
                        let (full_uri, sz) = if let Some(ref selector) = path_selector {
                            let file_path = selector.select_file();
                            let (_base_uri, size_spec) = cfg.get_put_size_spec(op);
                            use crate::size_generator::SizeGenerator;
                            let mut size_generator = SizeGenerator::new(&size_spec)?;
                            let sz = size_generator.generate();
                            let base_uri = cfg.target.as_ref()
                                .ok_or_else(|| anyhow!("target required in tree mode"))?;
                            let full_uri = if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, file_path)
                            } else {
                                format!("{}/{}", base_uri, file_path)
                            };
                            (full_uri, sz)
                        } else {
                            let (base_uri, size_spec) = cfg.get_put_size_spec(op);
                            use crate::size_generator::SizeGenerator;
                            let mut size_generator = SizeGenerator::new(&size_spec)?;
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
                        
                        // OPTIMIZED: Use cached data generator to reuse thread pool (50+ GB/s)
                        // Previously: s3dlio::generate_controlled_data() created new pool each call (~1-2 GB/s)
                        let buf = crate::data_gen_pool::generate_data_optimized(
                            sz as usize,
                            *dedup_factor,
                            *compress_factor
                        );

                        let store_cache_put = store_cache.clone();
                        let cfg_for_put = cfg.clone();
                        let uri_for_closure = full_uri.clone();
                        
                        let result = execute_with_error_handling(
                            "PUT",
                            &error_tracker,
                            || async {
                                let t0 = Instant::now();
                                put_object_cached(
                                    &uri_for_closure,
                                    buf.clone(),  // Clone is cheap: Bytes is Arc-like
                                    &store_cache_put,
                                    &cfg_for_put,
                                    None,  // agent_config: None for standalone mode
                                ).await?;
                                let duration = t0.elapsed();
                                Ok(duration)
                            }
                        ).await;
                        
                        match result {
                            Ok(Some(duration)) => {
                                let bucket = crate::metrics::bucket_index(buf.len());
                                ws.hist_put.record(bucket, duration);
                                ws.put_ops += 1;
                                ws.put_bytes += sz;
                                ws.put_bins.add(sz);
                                
                                if let Some(ref tracker) = cfg.live_stats_tracker {
                                    tracker.record_put(sz as usize, duration);
                                }
                                
                                ops_counter.fetch_add(1, Ordering::Relaxed);
                                bytes_counter.fetch_add(sz, Ordering::Relaxed);
                            }
                            Ok(None) => {
                                // Operation skipped due to error
                            }
                            Err(e) => {
                                return Err(e);
                            }
                        }
                    }
                    OpSpec::List { .. } => {
                        // v0.7.13: Wrap LIST operation with error handling
                        let uri = cfg.get_meta_uri(op);
                        let store_cache_list = store_cache.clone();
                        let cfg_for_list = cfg.clone();
                        
                        let result = execute_with_error_handling(
                            "LIST",
                            &error_tracker,
                            || async {
                                let t0 = Instant::now();
                                let _keys = list_objects_cached(
                                    &uri,
                                    &store_cache_list,
                                    &cfg_for_list,
                                    None,  // agent_config: None for standalone mode
                                ).await?;
                                let duration = t0.elapsed();
                                Ok(duration)
                            }
                        ).await;
                        
                        match result {
                            Ok(Some(duration)) => {
                                ws.hist_meta.record(0, duration);
                                ws.meta_ops += 1;
                                ws.meta_bins.add(0);
                                
                                if let Some(ref tracker) = cfg.live_stats_tracker {
                                    tracker.record_meta(duration);
                                }
                                
                                ops_counter.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(None) => {
                                // Operation skipped due to error
                            }
                            Err(e) => {
                                return Err(e);
                            }
                        }
                    }
                    OpSpec::Stat { .. } => {
                        // v0.7.13: Wrap STAT operation with error handling
                        let full_uri = if let Some(ref selector) = path_selector {
                            let file_path = selector.select_file();
                            let base_uri = cfg.target.as_ref()
                                .ok_or_else(|| anyhow!("target required in tree mode"))?;
                            if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, file_path)
                            } else {
                                format!("{}/{}", base_uri, file_path)
                            }
                        } else {
                            let original_pattern = cfg.get_meta_uri(op);
                            let pattern = rewrite_pattern_for_pool(&original_pattern, false, separate_pools);
                            let src = pre.stat_for_uri(&pattern)
                                .ok_or_else(|| anyhow!("No pre-resolved URIs for STAT pattern: {}", pattern))?;
                            let mut r = rng();
                            let uri_idx = r.random_range(0..src.full_uris.len());
                            src.full_uris[uri_idx].clone()
                        };

                        let store_cache_stat = store_cache.clone();
                        let cfg_for_stat = cfg.clone();
                        let uri_for_closure = full_uri.clone();
                        
                        let result = execute_with_error_handling(
                            "STAT",
                            &error_tracker,
                            || async {
                                let t0 = Instant::now();
                                let _size = stat_object_cached(
                                    &uri_for_closure,
                                    &store_cache_stat,
                                    &cfg_for_stat,
                                    None,  // agent_config: None for standalone mode
                                ).await?;
                                let duration = t0.elapsed();
                                Ok(duration)
                            }
                        ).await;
                        
                        match result {
                            Ok(Some(duration)) => {
                                ws.hist_meta.record(0, duration);
                                ws.meta_ops += 1;
                                ws.meta_bins.add(0);
                                
                                if let Some(ref tracker) = cfg.live_stats_tracker {
                                    tracker.record_meta(duration);
                                }
                                
                                ops_counter.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(None) => {
                                // Operation skipped due to error
                            }
                            Err(e) => {
                                return Err(e);
                            }
                        }
                    }
                    OpSpec::Delete { .. } => {
                        // v0.7.13: Wrap DELETE operation with error handling
                        let full_uri = if let Some(ref selector) = path_selector {
                            let file_path = selector.select_file();
                            let base_uri = cfg.target.as_ref()
                                .ok_or_else(|| anyhow!("target required in tree mode"))?;
                            if base_uri.ends_with('/') {
                                format!("{}{}", base_uri, file_path)
                            } else {
                                format!("{}/{}", base_uri, file_path)
                            }
                        } else {
                            let original_pattern = cfg.get_meta_uri(op);
                            let pattern = rewrite_pattern_for_pool(&original_pattern, true, separate_pools);
                            let src = pre.delete_for_uri(&pattern)
                                .ok_or_else(|| anyhow!("No pre-resolved URIs for DELETE pattern: {}", pattern))?;
                            let mut r = rng();
                            let uri_idx = r.random_range(0..src.full_uris.len());
                            src.full_uris[uri_idx].clone()
                        };

                        let store_cache_delete = store_cache.clone();
                        let cfg_for_delete = cfg.clone();
                        let uri_for_closure = full_uri.clone();
                        
                        let result = execute_with_error_handling(
                            "DELETE",
                            &error_tracker,
                            || async {
                                let t0 = Instant::now();
                                delete_object_cached(
                                    &uri_for_closure,
                                    &store_cache_delete,
                                    &cfg_for_delete,
                                    None,  // agent_config: None for standalone mode
                                ).await?;
                                let duration = t0.elapsed();
                                Ok(duration)
                            }
                        ).await;
                        
                        match result {
                            Ok(Some(duration)) => {
                                ws.hist_meta.record(0, duration);
                                ws.meta_ops += 1;
                                ws.meta_bins.add(0);
                                
                                if let Some(ref tracker) = cfg.live_stats_tracker {
                                    tracker.record_meta(duration);
                                }
                                
                                ops_counter.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(None) => {
                                // Operation skipped due to error
                            }
                            Err(e) => {
                                return Err(e);
                            }
                        }
                    }
                    OpSpec::Mkdir { .. } => {
                        // v0.7.13: Wrap MKDIR operation with error handling
                        let dir_name = if let Some(ref selector) = path_selector {
                            selector.select_directory()
                        } else {
                            return Err(anyhow!(
                                "MKDIR operation requires directory_structure in prepare config. \
                                 MKDIR/RMDIR are only for testing structured directory trees. \
                                 Configure prepare.directory_structure with width/depth/files_per_dir."
                            ));
                        };
                        
                        let base_uri = cfg.target.as_ref()
                            .ok_or_else(|| anyhow!("target required in tree mode"))?;
                        let full_uri = if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, dir_name)
                        } else {
                            format!("{}/{}", base_uri, dir_name)
                        };

                        let range_engine = cfg.range_engine.clone();
                        let page_cache = cfg.page_cache_mode;
                        let uri_for_closure = full_uri.clone();
                        
                        let result = execute_with_error_handling(
                            "MKDIR",
                            &error_tracker,
                            || async {
                                let t0 = Instant::now();
                                mkdir_with_config(
                                    &uri_for_closure,
                                    range_engine.as_ref(),
                                    page_cache,
                                ).await?;
                                let duration = t0.elapsed();
                                Ok(duration)
                            }
                        ).await;
                        
                        match result {
                            Ok(Some(duration)) => {
                                ws.hist_meta.record(0, duration);
                                ws.meta_ops += 1;
                                ws.meta_bins.add(0);
                                
                                if let Some(ref tracker) = cfg.live_stats_tracker {
                                    tracker.record_meta(duration);
                                }
                                
                                ops_counter.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(None) => {
                                // Operation skipped due to error
                            }
                            Err(e) => {
                                return Err(e);
                            }
                        }
                    }
                    OpSpec::Rmdir { recursive, .. } => {
                        // v0.7.13: Wrap RMDIR operation with error handling
                        let dir_name = if let Some(ref selector) = path_selector {
                            selector.select_directory()
                        } else {
                            return Err(anyhow!(
                                "RMDIR operation requires directory_structure in prepare config. \
                                 MKDIR/RMDIR are only for testing structured directory trees. \
                                 Configure prepare.directory_structure with width/depth/files_per_dir."
                            ));
                        };
                        
                        let base_uri = cfg.target.as_ref()
                            .ok_or_else(|| anyhow!("target required in tree mode"))?;
                        let full_uri = if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, dir_name)
                        } else {
                            format!("{}/{}", base_uri, dir_name)
                        };

                        let range_engine = cfg.range_engine.clone();
                        let page_cache = cfg.page_cache_mode;
                        let uri_for_closure = full_uri.clone();
                        let is_recursive = *recursive;
                        
                        let result = execute_with_error_handling(
                            "RMDIR",
                            &error_tracker,
                            || async {
                                let t0 = Instant::now();
                                rmdir_with_config(
                                    &uri_for_closure,
                                    is_recursive,
                                    range_engine.as_ref(),
                                    page_cache,
                                ).await?;
                                let duration = t0.elapsed();
                                Ok(duration)
                            }
                        ).await;
                        
                        match result {
                            Ok(Some(duration)) => {
                                ws.hist_meta.record(0, duration);
                                ws.meta_ops += 1;
                                ws.meta_bins.add(0);
                                
                                if let Some(ref tracker) = cfg.live_stats_tracker {
                                    tracker.record_meta(duration);
                                }
                                
                                ops_counter.fetch_add(1, Ordering::Relaxed);
                            }
                            Ok(None) => {
                                // Operation skipped due to error
                            }
                            Err(e) => {
                                return Err(e);
                            }
                        }
                    }
                }
            }

            Ok::<WorkerStats, anyhow::Error>(ws)
        }));
    }

    // v0.7.13: Merge results from all workers with error handling
    // If any worker hit error threshold, cancel progress bar and return error
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

    let mut workload_error: Option<anyhow::Error> = None;
    
    for h in handles {
        match h.await {
            Ok(Ok(ws)) => {
                // Worker completed successfully
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
            Ok(Err(e)) => {
                // Worker hit error threshold
                error!("Worker task failed: {}", e);
                workload_error = Some(e);
                break;  // Stop processing remaining workers
            }
            Err(e) => {
                // Worker panicked
                error!("Worker task panicked: {}", e);
                workload_error = Some(anyhow!("Worker task panicked: {}", e));
                break;
            }
        }
    }
    
    // v0.7.13: If error occurred, cancel progress bar before returning
    if let Some(err) = workload_error {
        let _ = tx_cancel_progress.send(());
        let _ = progress_handle.await;  // Wait for progress to exit
        
        let (total_errors, error_rate) = error_tracker.get_stats();
        error!("❌ Workload aborted: {} total errors, {:.2} errors/sec",
            total_errors, error_rate);
        
        return Err(err);
    }

    // Complete progress bar and wait for progress task
    let _ = tx_cancel_progress.send(());  // Signal normal completion
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
    
    // v0.7.13: Get final error statistics
    let (final_total_errors, final_error_rate) = error_tracker.get_stats();
    if final_total_errors > 0 {
        warn!("Workload completed with {} errors ({:.2} errors/sec)", final_total_errors, final_error_rate);
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
        total_errors: final_total_errors,
        error_rate: final_error_rate,
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

#[cfg(test)]
mod error_handling_tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    
    // =========================================================================
    // RetryConfig Tests
    // =========================================================================
    
    #[test]
    fn test_retry_config_default() {
        let config = RetryConfig::default();
        
        assert_eq!(config.max_retries, crate::constants::DEFAULT_MAX_RETRIES);
        assert_eq!(config.initial_delay_ms, crate::constants::DEFAULT_INITIAL_RETRY_DELAY_MS);
        assert_eq!(config.max_delay_ms, crate::constants::DEFAULT_MAX_RETRY_DELAY_MS);
        assert!((config.backoff_multiplier - crate::constants::DEFAULT_RETRY_BACKOFF_MULTIPLIER).abs() < 0.001);
        assert!((config.jitter_factor - crate::constants::DEFAULT_RETRY_JITTER_FACTOR).abs() < 0.001);
    }
    
    #[test]
    fn test_retry_config_from_error_handling_config() {
        let mut error_cfg = crate::config::ErrorHandlingConfig::default();
        error_cfg.max_retries = 5;
        error_cfg.initial_retry_delay_ms = 200;
        error_cfg.max_retry_delay_ms = 10000;
        error_cfg.retry_backoff_multiplier = 3.0;
        error_cfg.retry_jitter_factor = 0.5;
        
        let retry_config = RetryConfig::from(&error_cfg);
        
        assert_eq!(retry_config.max_retries, 5);
        assert_eq!(retry_config.initial_delay_ms, 200);
        assert_eq!(retry_config.max_delay_ms, 10000);
        assert!((retry_config.backoff_multiplier - 3.0).abs() < 0.001);
        assert!((retry_config.jitter_factor - 0.5).abs() < 0.001);
    }
    
    // =========================================================================
    // retry_with_backoff Tests
    // =========================================================================
    
    #[tokio::test]
    async fn test_retry_success_on_first_attempt() {
        let config = RetryConfig::default();
        let attempt_count = Arc::new(AtomicU32::new(0));
        let attempt_count_clone = attempt_count.clone();
        
        let result = retry_with_backoff(
            "test_op",
            &config,
            || {
                let count = attempt_count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    Ok::<i32, anyhow::Error>(42)
                }
            }
        ).await;
        
        match result {
            RetryResult::Success(val) => assert_eq!(val, 42),
            RetryResult::Failed(_) => panic!("Expected success"),
        }
        
        // Should only attempt once on success
        assert_eq!(attempt_count.load(Ordering::Relaxed), 1);
    }
    
    #[tokio::test]
    async fn test_retry_success_after_failures() {
        let config = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 1,  // Very short for testing
            max_delay_ms: 10,
            backoff_multiplier: 2.0,
            jitter_factor: 0.0,  // No jitter for predictable testing
        };
        
        let attempt_count = Arc::new(AtomicU32::new(0));
        let attempt_count_clone = attempt_count.clone();
        
        // Fail first 2 times, succeed on 3rd
        let result = retry_with_backoff(
            "test_op",
            &config,
            || {
                let count = attempt_count_clone.clone();
                async move {
                    let attempt = count.fetch_add(1, Ordering::Relaxed) + 1;
                    if attempt < 3 {
                        Err(anyhow::anyhow!("Simulated failure {}", attempt))
                    } else {
                        Ok::<i32, anyhow::Error>(42)
                    }
                }
            }
        ).await;
        
        match result {
            RetryResult::Success(val) => assert_eq!(val, 42),
            RetryResult::Failed(_) => panic!("Expected success after retries"),
        }
        
        // Should have attempted 3 times
        assert_eq!(attempt_count.load(Ordering::Relaxed), 3);
    }
    
    #[tokio::test]
    async fn test_retry_all_attempts_fail() {
        let config = RetryConfig {
            max_retries: 2,  // Initial + 2 retries = 3 attempts
            initial_delay_ms: 1,
            max_delay_ms: 10,
            backoff_multiplier: 2.0,
            jitter_factor: 0.0,
        };
        
        let attempt_count = Arc::new(AtomicU32::new(0));
        let attempt_count_clone = attempt_count.clone();
        
        let result = retry_with_backoff(
            "test_op",
            &config,
            || {
                let count = attempt_count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    Err::<i32, anyhow::Error>(anyhow::anyhow!("Always fails"))
                }
            }
        ).await;
        
        match result {
            RetryResult::Success(_) => panic!("Expected failure"),
            RetryResult::Failed(e) => {
                assert!(e.to_string().contains("Always fails"));
            }
        }
        
        // max_retries=2 means 3 total attempts (initial + 2 retries)
        assert_eq!(attempt_count.load(Ordering::Relaxed), 3);
    }
    
    #[tokio::test]
    async fn test_retry_delay_increases_exponentially() {
        let config = RetryConfig {
            max_retries: 4,
            initial_delay_ms: 10,
            max_delay_ms: 1000,
            backoff_multiplier: 2.0,
            jitter_factor: 0.0,  // No jitter for predictable timing
        };
        
        let timestamps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let timestamps_clone = timestamps.clone();
        
        let result = retry_with_backoff(
            "test_op",
            &config,
            || {
                let ts = timestamps_clone.clone();
                async move {
                    ts.lock().unwrap().push(Instant::now());
                    Err::<i32, anyhow::Error>(anyhow::anyhow!("Always fails"))
                }
            }
        ).await;
        
        assert!(matches!(result, RetryResult::Failed(_)));
        
        let times = timestamps.lock().unwrap();
        assert_eq!(times.len(), 5);  // Initial + 4 retries
        
        // Verify delays are approximately doubling (with some tolerance for execution time)
        // Delay 1: ~10ms, Delay 2: ~20ms, Delay 3: ~40ms, Delay 4: ~80ms
        let delays: Vec<u128> = times.windows(2)
            .map(|w| w[1].duration_since(w[0]).as_millis())
            .collect();
        
        // Each delay should be roughly double the previous (within 50% tolerance for test stability)
        for i in 1..delays.len() {
            let ratio = delays[i] as f64 / delays[i-1] as f64;
            assert!(ratio > 1.5 && ratio < 2.5, 
                "Delay ratio {} at index {} not within expected range (delays: {:?})", 
                ratio, i, delays);
        }
    }
    
    #[tokio::test]
    async fn test_retry_delay_capped_at_max() {
        let config = RetryConfig {
            max_retries: 5,
            initial_delay_ms: 100,
            max_delay_ms: 150,  // Cap at 150ms - should be hit after first retry
            backoff_multiplier: 2.0,
            jitter_factor: 0.0,
        };
        
        let timestamps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let timestamps_clone = timestamps.clone();
        
        let _ = retry_with_backoff(
            "test_op",
            &config,
            || {
                let ts = timestamps_clone.clone();
                async move {
                    ts.lock().unwrap().push(Instant::now());
                    Err::<i32, anyhow::Error>(anyhow::anyhow!("Always fails"))
                }
            }
        ).await;
        
        let times = timestamps.lock().unwrap();
        let delays: Vec<u128> = times.windows(2)
            .map(|w| w[1].duration_since(w[0]).as_millis())
            .collect();
        
        // After first delay (100ms), subsequent should be capped at 150ms
        for delay in delays.iter().skip(1) {
            assert!(*delay <= 200, "Delay {} exceeded max", delay);  // Some tolerance for timing
        }
    }
    
    #[tokio::test]
    async fn test_retry_with_jitter() {
        let config = RetryConfig {
            max_retries: 10,
            initial_delay_ms: 50,
            max_delay_ms: 1000,
            backoff_multiplier: 1.0,  // No growth, just jitter
            jitter_factor: 0.5,  // 50% jitter
        };
        
        let timestamps = Arc::new(std::sync::Mutex::new(Vec::new()));
        let timestamps_clone = timestamps.clone();
        
        let _ = retry_with_backoff(
            "test_op",
            &config,
            || {
                let ts = timestamps_clone.clone();
                async move {
                    ts.lock().unwrap().push(Instant::now());
                    Err::<i32, anyhow::Error>(anyhow::anyhow!("Always fails"))
                }
            }
        ).await;
        
        let times = timestamps.lock().unwrap();
        let delays: Vec<u128> = times.windows(2)
            .map(|w| w[1].duration_since(w[0]).as_millis())
            .collect();
        
        // With 50% jitter on 50ms base, delays should be in range [25ms, 75ms]
        // Allow some tolerance for test stability
        for delay in &delays {
            assert!(*delay >= 15 && *delay <= 100, 
                "Delay {} outside jitter range", delay);
        }
        
        // With jitter, delays should vary (not all identical)
        let unique_delays: std::collections::HashSet<u128> = delays.iter().cloned().collect();
        assert!(unique_delays.len() > 1, "Jitter should produce varying delays");
    }
    
    // =========================================================================
    // ErrorTracker Tests
    // =========================================================================
    
    #[test]
    fn test_error_tracker_record_error() {
        let config = crate::config::ErrorHandlingConfig::default();
        let tracker = ErrorTracker::new(config);
        
        let (should_backoff, should_abort, total, rate) = tracker.record_error();
        
        assert!(!should_backoff);  // Single error shouldn't trigger backoff
        assert!(!should_abort);    // Far below threshold
        assert_eq!(total, 1);
        assert!(rate > 0.0);
    }
    
    #[test]
    fn test_error_tracker_abort_threshold() {
        let mut config = crate::config::ErrorHandlingConfig::default();
        config.max_total_errors = 5;  // Low threshold for testing
        let tracker = ErrorTracker::new(config);
        
        // Record 4 errors - should not abort
        for _ in 0..4 {
            let (_, should_abort, _, _) = tracker.record_error();
            assert!(!should_abort);
        }
        
        // 5th error should trigger abort
        let (_, should_abort, total, _) = tracker.record_error();
        assert!(should_abort);
        assert_eq!(total, 5);
    }
    
    #[test]
    fn test_error_tracker_get_stats() {
        let config = crate::config::ErrorHandlingConfig::default();
        let tracker = ErrorTracker::new(config);
        
        // Record some errors
        for _ in 0..3 {
            tracker.record_error();
        }
        
        let (total, rate) = tracker.get_stats();
        assert_eq!(total, 3);
        assert!(rate > 0.0);
    }
    
    #[test]
    fn test_error_tracker_time_window_clears() {
        let mut config = crate::config::ErrorHandlingConfig::default();
        config.error_rate_window = 0.01;  // 10ms window for fast test
        let tracker = ErrorTracker::new(config);
        
        // Record error
        tracker.record_error();
        let (_, rate1) = tracker.get_stats();
        assert!(rate1 > 0.0);
        
        // Wait for window to expire
        std::thread::sleep(Duration::from_millis(20));
        
        // Rate should have dropped (errors aged out)
        let (total, rate2) = tracker.get_stats();
        assert_eq!(total, 1);  // Total is cumulative
        assert!(rate2 < rate1, "Rate should decrease as errors age out of window");
    }
    
    // =========================================================================
    // Backoff State Recovery Tests
    // =========================================================================
    
    #[test]
    fn test_error_rate_recovers_after_time() {
        let mut config = crate::config::ErrorHandlingConfig::default();
        config.error_rate_window = 0.05;  // 50ms window
        config.error_rate_threshold = 10.0;  // 10 errors/sec to trigger backoff
        let tracker = ErrorTracker::new(config);
        
        // Rapid errors to trigger backoff state
        for _ in 0..5 {
            tracker.record_error();
        }
        
        // Should be in high error rate state
        let (_, rate_before) = tracker.get_stats();
        assert!(rate_before > 10.0, "Should have high error rate");
        
        // Wait for window to expire
        std::thread::sleep(Duration::from_millis(100));
        
        // Error rate should have recovered
        let (_, rate_after) = tracker.get_stats();
        assert!(rate_after < rate_before, "Error rate should decrease over time");
    }
}



