// src/workload.rs
//
use anyhow::{anyhow, Context, Result};
use hdrhistogram::Histogram;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{debug, info};

use rand::{rng, Rng};
//use rand::rngs::SmallRng;
use rand::distr::weighted::WeightedIndex;
use rand_distr::Distribution;

use std::time::Instant;
use crate::config::{Config, OpSpec};

use std::collections::HashMap;
use crate::bucket_index;

use s3dlio::object_store::{store_for_uri, ObjectStore};

// -----------------------------------------------------------------------------
// Multi-backend support infrastructure
// -----------------------------------------------------------------------------

/// Backend types supported by s3-bench
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    S3,
    Azure,
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
            BackendType::File => "Local File",
            BackendType::DirectIO => "Direct I/O",
        }
    }
}

/// Create ObjectStore instance for given URI
pub fn create_store_for_uri(uri: &str) -> anyhow::Result<Box<dyn ObjectStore>> {
    store_for_uri(uri).context("Failed to create object store")
}

/// Helper to build full URI from components for different backends
pub fn build_full_uri(backend: BackendType, base_uri: &str, key: &str) -> String {
    match backend {
        BackendType::S3 => {
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
    let store = create_store_for_uri(uri)?;
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
    let store = create_store_for_uri(uri)?;
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
    let store = create_store_for_uri(uri)?;
    debug!("ObjectStore created successfully for URI: {}", uri);
    
    // Use ObjectStore list method with full URI and recursive=true (s3dlio handles URI parsing)
    let keys = store.list(uri, true).await
        .with_context(|| format!("Failed to list objects from URI: {}", uri))?;
    
    debug!("LIST operation completed successfully for URI: {}, {} objects found", uri, keys.len());
    Ok(keys)
}

/// Multi-backend STAT operation using ObjectStore trait
/// Since ObjectStore doesn't have a dedicated stat method, we use a minimal get operation
pub async fn stat_object_multi_backend(uri: &str) -> anyhow::Result<u64> {
    debug!("STAT operation starting for URI: {}", uri);
    let store = create_store_for_uri(uri)?;
    debug!("ObjectStore created successfully for URI: {}", uri);
    
    // For stat operation, we'll use a minimal get operation to get the size
    // This simulates HEAD/stat behavior by getting the data and measuring size
    let bytes = store.get(uri).await
        .with_context(|| format!("Failed to stat object from URI: {}", uri))?;
    
    let size = bytes.len() as u64;
    debug!("STAT operation completed successfully for URI: {}, size: {} bytes", uri, size);
    Ok(size)
}

/// Multi-backend DELETE operation using ObjectStore trait
pub async fn delete_object_multi_backend(uri: &str) -> anyhow::Result<()> {
    debug!("DELETE operation starting for URI: {}", uri);
    let store = create_store_for_uri(uri)?;
    debug!("ObjectStore created successfully for URI: {}", uri);
    
    // Use ObjectStore delete method with full URI (s3dlio handles URI parsing)
    store.delete(uri).await
        .with_context(|| format!("Failed to delete object from URI: {}", uri))?;
    
    debug!("DELETE operation completed successfully for URI: {}", uri);
    Ok(())
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
}

// -----------------------------------------------------------------------------
// Worker stats merged at the end
// -----------------------------------------------------------------------------
struct WorkerStats {
    hist_get: Histogram<u64>,
    hist_put: Histogram<u64>,
    hist_meta: Histogram<u64>,
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
            hist_get: hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap(),
            hist_put: hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap(),
            hist_meta: hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap(),
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

    // Pre-resolve any GET sources once.
    info!("Pre-resolving GET sources for {} operations", cfg.workload.len());
    let mut pre = PreResolved::default();
    for wo in &cfg.workload {
        if let OpSpec::Get { .. } = &wo.spec {
            let uri = cfg.get_uri(&wo.spec);
            
            info!("Resolving GET source for URI: {}", uri);
            
            // Try new multi-backend approach first
            match prefetch_uris_multi_backend(&uri).await {
                Ok(full_uris) if !full_uris.is_empty() => {
                    info!("Found {} objects for GET pattern: {}", full_uris.len(), uri);
                    pre.get_lists.push(GetSource {
                        full_uris: full_uris.clone(),
                        uri: uri.clone(),
                    });
                }
                Ok(_) => {
                    return Err(anyhow!("No URIs found for GET uri: {}", uri));
                }
                Err(e) => {
                    return Err(anyhow!("Failed to prefetch URIs for {}: {}", uri, e));
                }
            }
        }
    }

    // Weighted chooser
    let weights: Vec<u32> = cfg.workload.iter().map(|w| w.weight).collect();
    let chooser = WeightedIndex::new(weights).context("invalid weights")?;

    // concurrency
    let sem = Arc::new(Semaphore::new(cfg.concurrency));

    // Spawn workers
    info!("Spawning {} worker tasks", cfg.concurrency);
    let mut handles = Vec::with_capacity(cfg.concurrency);
    for _ in 0..cfg.concurrency {
        let sem = sem.clone();
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

                // Acquire permit first (await happens before we create RNGs)
                let _p = sem.acquire().await.unwrap();

                // Sample op index
                let idx = {
                    let mut r = rng();
                    chooser.sample(&mut r)
                };
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
                        let micros = t0.elapsed().as_micros() as u64;

                        ws.hist_get.record(micros.max(1)).ok();
                        ws.get_ops += 1;
                        ws.get_bytes += bytes.len() as u64;
                        ws.get_bins.add(bytes.len() as u64);
                    }
                    OpSpec::Put { .. } => {
                        // Get base URI from config  
                        let (base_uri, sz) = cfg.get_put_info(op);
                        
                        let key = {
                            let mut r = rng();
                            format!("obj_{}", r.random::<u64>())
                        };
                        let buf = vec![0u8; sz as usize];

                        // Build full URI for the specific object
                        let full_uri = if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, key)
                        } else {
                            format!("{}/{}", base_uri, key)
                        };

                        let t0 = Instant::now();
                        put_object_multi_backend(&full_uri, &buf).await?;
                        let micros = t0.elapsed().as_micros() as u64;

                        ws.hist_put.record(micros.max(1)).ok();
                        ws.put_ops += 1;
                        ws.put_bytes += sz;
                        ws.put_bins.add(sz);
                    }
                    OpSpec::List { .. } => {
                        // Get resolved URI from config
                        let uri = cfg.get_meta_uri(op);

                        let t0 = Instant::now();
                        let _keys = list_objects_multi_backend(&uri).await?;
                        let micros = t0.elapsed().as_micros() as u64;

                        ws.hist_meta.record(micros.max(1)).ok();
                        ws.meta_ops += 1;
                        // List operations don't transfer data, just metadata
                        ws.meta_bins.add(0);
                    }
                    OpSpec::Stat { .. } => {
                        // Get resolved URI from config
                        let uri = cfg.get_meta_uri(op);

                        let t0 = Instant::now();
                        let _size = stat_object_multi_backend(&uri).await?;
                        let micros = t0.elapsed().as_micros() as u64;

                        ws.hist_meta.record(micros.max(1)).ok();
                        ws.meta_ops += 1;
                        // Stat operations don't transfer data, just metadata about the size
                        ws.meta_bins.add(0);
                    }
                    OpSpec::Delete { .. } => {
                        // Get resolved URI from config
                        let uri = cfg.get_meta_uri(op);

                        let t0 = Instant::now();
                        delete_object_multi_backend(&uri).await?;
                        let micros = t0.elapsed().as_micros() as u64;

                        ws.hist_meta.record(micros.max(1)).ok();
                        ws.meta_ops += 1;
                        // Delete operations don't transfer data
                        ws.meta_bins.add(0);
                    }
                }
            }

            Ok::<WorkerStats, anyhow::Error>(ws)
        }));
    }

    // Merge results
    let mut merged_get = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    let mut merged_put = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    let mut merged_meta = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
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
        merged_get.add(&ws.hist_get).ok();
        merged_put.add(&ws.hist_put).ok();
        merged_meta.add(&ws.hist_meta).ok();
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

    let wall = start.elapsed().as_secs_f64();

    let get = OpAgg {
        bytes: get_bytes,
        ops: get_ops,
        p50_us: merged_get.value_at_quantile(0.50),
        p95_us: merged_get.value_at_quantile(0.95),
        p99_us: merged_get.value_at_quantile(0.99),
    };
    let put = OpAgg {
        bytes: put_bytes,
        ops: put_ops,
        p50_us: merged_put.value_at_quantile(0.50),
        p95_us: merged_put.value_at_quantile(0.95),
        p99_us: merged_put.value_at_quantile(0.99),
    };
    let meta = OpAgg {
        bytes: meta_bytes,
        ops: meta_ops,
        p50_us: merged_meta.value_at_quantile(0.50),
        p95_us: merged_meta.value_at_quantile(0.95),
        p99_us: merged_meta.value_at_quantile(0.99),
    };

    // Preserve combined line for compatibility
    let total_bytes = get_bytes + put_bytes + meta_bytes;
    let total_ops = get_ops + put_ops + meta_ops;

    // Build a combined histogram just for the old p50/95/99
    let mut combined = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    combined.add(&merged_get).ok();
    combined.add(&merged_put).ok();
    combined.add(&merged_meta).ok();

    info!("Workload execution completed: {:.2}s wall time, {} total ops ({} GET, {} PUT, {} META), {:.2} MB total ({:.2} MB GET, {:.2} MB PUT, {:.2} MB META)", 
          wall, 
          total_ops, get_ops, put_ops, meta_ops,
          total_bytes as f64 / 1_048_576.0,
          get_bytes as f64 / 1_048_576.0,
          put_bytes as f64 / 1_048_576.0,
          meta_bytes as f64 / 1_048_576.0);

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
    })
}

/// Pre-resolved GET lists so workers can sample keys cheaply.
#[derive(Default, Clone)]
struct PreResolved {
    get_lists: Vec<GetSource>,
}
#[derive(Clone)]
struct GetSource {
    full_uris: Vec<String>,     // Pre-resolved full URIs for random selection
    uri: String,                // For lookup compatibility
}
impl PreResolved {
    fn get_for_uri(&self, uri: &str) -> Option<&GetSource> {
        self.get_lists.iter().find(|g| g.uri == uri)
    }
}


/// Expand keys for a GET uri: supports exact key, prefix, or glob '*'.


/// Multi-backend version: expand URIs for GET operations using ObjectStore trait
async fn prefetch_uris_multi_backend(base_uri: &str) -> Result<Vec<String>> {
    let store = create_store_for_uri(base_uri)?;
    
    if base_uri.contains('*') {
        // Glob pattern: list and filter
        let base_end = base_uri.rfind('/').map(|i| i + 1).unwrap_or(0);
        let list_prefix = &base_uri[..base_end];
        
        let uris = store.list(list_prefix, false).await?;
        let re = glob_to_regex(base_uri)?;
        Ok(uris.into_iter().filter(|uri| re.is_match(uri)).collect())
    } else if base_uri.ends_with('/') {
        // Directory listing
        store.list(base_uri, false).await
    } else {
        // Exact URI
        Ok(vec![base_uri.to_string()])
    }
}


fn glob_to_regex(glob: &str) -> Result<regex::Regex> {
    let s = regex::escape(glob).replace(r"\*", ".*");
    let re = format!("^{}$", s);
    Ok(regex::Regex::new(&re)?)
}



