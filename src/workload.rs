// src/workload.rs
//
use anyhow::{anyhow, Context, Result};
use hdrhistogram::Histogram;
use std::sync::Arc;
use tokio::sync::Semaphore;

use rand::{rng, Rng};
//use rand::rngs::SmallRng;
use rand::distr::weighted::WeightedIndex;
use rand_distr::Distribution;

use std::time::Instant;
use crate::config::{Config, OpSpec};

use std::collections::HashMap;
use crate::bucket_index;

use aws_config::{self, BehaviorVersion, Region};
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_s3 as s3;
use s3dlio::object_store::{store_for_uri, ObjectStore};
// TODO: Remove legacy s3_utils imports as we migrate operations
use s3dlio::s3_utils::{get_object, parse_s3_uri, put_object_async};

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

// -----------------------------------------------------------------------------
// New summary/aggregation types
// -----------------------------------------------------------------------------
#[derive(Debug, Clone, Default)]
pub struct OpAgg {
    pub bytes: u64,
    pub ops: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
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
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,

    // New: per-op aggregates and size bins
    pub get: OpAgg,
    pub put: OpAgg,
    pub get_bins: SizeBins,
    pub put_bins: SizeBins,
}

// -----------------------------------------------------------------------------
// Worker stats merged at the end
// -----------------------------------------------------------------------------
struct WorkerStats {
    hist_get: Histogram<u64>,
    hist_put: Histogram<u64>,
    get_bytes: u64,
    get_ops: u64,
    put_bytes: u64,
    put_ops: u64,
    get_bins: SizeBins,
    put_bins: SizeBins,
}

impl Default for WorkerStats {
    fn default() -> Self {
        Self {
            hist_get: hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000, 3).unwrap(),
            hist_put: hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000, 3).unwrap(),
            get_bytes: 0,
            get_ops: 0,
            put_bytes: 0,
            put_ops: 0,
            get_bins: SizeBins::default(),
            put_bins: SizeBins::default(),
        }
    }
}

/// Public entry: run a config and print a summary-like struct back.
pub async fn run(cfg: &Config) -> Result<Summary> {
    let start = Instant::now();
    let deadline = start + cfg.duration;

    // Pre-resolve any GET sources once.
    let mut pre = PreResolved::default();
    for wo in &cfg.workload {
        if let OpSpec::Get { uri } = &wo.spec {
            let backend_type = BackendType::from_uri(uri);
            
            // Try new multi-backend approach first
            match prefetch_uris_multi_backend(uri).await {
                Ok(full_uris) if !full_uris.is_empty() => {
                    // Create legacy bucket/pattern for backward compatibility
                    let (bucket, _pattern) = if backend_type == BackendType::S3 {
                        parse_s3_uri(uri).unwrap_or_else(|_| ("".to_string(), "".to_string()))
                    } else {
                        ("".to_string(), "".to_string())
                    };
                    
                    pre.get_lists.push(GetSource {
                        uri: uri.clone(),
                        bucket,
                        keys: full_uris.iter().map(|u| {
                            // Extract key part for legacy compatibility
                            if backend_type == BackendType::S3 {
                                u.split('/').last().unwrap_or("").to_string()
                            } else {
                                u.clone()
                            }
                        }).collect(),
                    });
                }
                _ => {
                    // Fall back to legacy S3-only approach for compatibility
                    if backend_type == BackendType::S3 {
                        let (bucket, pat) = parse_s3_uri(uri).context("parse get uri")?;
                        let keys = prefetch_keys(&bucket, &pat).await?;
                        if keys.is_empty() {
                            return Err(anyhow!("no keys found for GET uri: {}", uri));
                        }
                        
                        pre.get_lists.push(GetSource {
                            uri: uri.clone(),
                            bucket,
                            keys,
                        });
                    } else {
                        return Err(anyhow!("no keys found for non-S3 uri: {}", uri));
                    }
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
    let mut handles = Vec::with_capacity(cfg.concurrency);
    for _ in 0..cfg.concurrency {
        let sem = sem.clone();
        let workload = cfg.workload.clone();
        let chooser = chooser.clone();
        let pre = pre.clone();

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
                    OpSpec::Get { uri } => {
                        // Pick key before await
                        let (bucket, key) = {
                            let src = pre.get_for_uri(&uri).unwrap();
                            let mut r = rng();
                            let k = &src.keys[r.random_range(0..src.keys.len())];
                            (src.bucket.clone(), k.to_string())
                        };

                        let t0 = Instant::now();
                        let bytes = get_object(&bucket, &key).await?;
                        let ms = t0.elapsed().as_millis() as u64;

                        ws.hist_get.record(ms.max(1)).ok();
                        ws.get_ops += 1;
                        ws.get_bytes += bytes.len() as u64;
                        ws.get_bins.add(bytes.len() as u64);
                    }
                    OpSpec::Put {
                        bucket,
                        prefix,
                        object_size,
                    } => {
                        let sz = *object_size as u64; // adjust if your type is plain u64: `let sz = *object_size as u64` -> `let sz = *object_size` or `let sz = object_size as u64`
                        let key = {
                            let mut r = rng();
                            format!("{}obj_{}", prefix, r.random::<u64>())
                        };
                        let buf = vec![0u8; sz as usize];

                        let t0 = Instant::now();
                        put_object_async(bucket, &key, &buf).await?;
                        let ms = t0.elapsed().as_millis() as u64;

                        ws.hist_put.record(ms.max(1)).ok();
                        ws.put_ops += 1;
                        ws.put_bytes += sz;
                        ws.put_bins.add(sz);
                    }
                }
            }

            Ok::<WorkerStats, anyhow::Error>(ws)
        }));
    }

    // Merge results
    let mut merged_get = Histogram::<u64>::new_with_bounds(1, 60_000, 3).unwrap();
    let mut merged_put = Histogram::<u64>::new_with_bounds(1, 60_000, 3).unwrap();
    let mut get_bytes = 0u64;
    let mut get_ops = 0u64;
    let mut put_bytes = 0u64;
    let mut put_ops = 0u64;
    let mut get_bins = SizeBins::default();
    let mut put_bins = SizeBins::default();

    for h in handles {
        let ws = h.await??;
        merged_get.add(&ws.hist_get).ok();
        merged_put.add(&ws.hist_put).ok();
        get_bytes += ws.get_bytes;
        get_ops += ws.get_ops;
        put_bytes += ws.put_bytes;
        put_ops += ws.put_ops;
        get_bins.merge_from(&ws.get_bins);
        put_bins.merge_from(&ws.put_bins);
    }

    let wall = start.elapsed().as_secs_f64();

    let get = OpAgg {
        bytes: get_bytes,
        ops: get_ops,
        p50_ms: merged_get.value_at_quantile(0.50),
        p95_ms: merged_get.value_at_quantile(0.95),
        p99_ms: merged_get.value_at_quantile(0.99),
    };
    let put = OpAgg {
        bytes: put_bytes,
        ops: put_ops,
        p50_ms: merged_put.value_at_quantile(0.50),
        p95_ms: merged_put.value_at_quantile(0.95),
        p99_ms: merged_put.value_at_quantile(0.99),
    };

    // Preserve combined line for compatibility
    let total_bytes = get_bytes + put_bytes;
    let total_ops = get_ops + put_ops;

    // Build a combined histogram just for the old p50/95/99
    let mut combined = Histogram::<u64>::new_with_bounds(1, 60_000, 3).unwrap();
    combined.add(&merged_get).ok();
    combined.add(&merged_put).ok();

    Ok(Summary {
        wall_seconds: wall,
        total_bytes,
        total_ops,
        p50_ms: combined.value_at_quantile(0.50),
        p95_ms: combined.value_at_quantile(0.95),
        p99_ms: combined.value_at_quantile(0.99),
        get,
        put,
        get_bins,
        put_bins,
    })
}

/// Pre-resolved GET lists so workers can sample keys cheaply.
#[derive(Default, Clone)]
struct PreResolved {
    get_lists: Vec<GetSource>,
}
#[derive(Clone)]
struct GetSource {
    // Legacy S3-specific fields still in use
    uri: String,
    bucket: String,
    keys: Vec<String>,
}
impl PreResolved {
    fn get_for_uri(&self, uri: &str) -> Option<&GetSource> {
        self.get_lists.iter().find(|g| g.uri == uri)
    }
}


/// Expand keys for a GET uri: supports exact key, prefix, or glob '*'.
async fn prefetch_keys(bucket: &str, pat: &str) -> Result<Vec<String>> {
    if pat.contains('*') {
        // base = path up to and including the last '/'
        let base_end = pat.rfind('/').map(|i| i + 1).unwrap_or(0);
        let base = &pat[..base_end];

        // list returns keys relative to `base`; make them absolute, then filter by the full glob
        let rels = list_keys_async(bucket, base).await?;
        let re = glob_to_regex(pat)?;
        let full: Vec<String> = rels.into_iter().map(|r| format!("{base}{r}")).collect();
        Ok(full.into_iter().filter(|k| re.is_match(k)).collect())
    } else if pat.ends_with('/') || pat.is_empty() {
        // prefix-only: list relative, then make absolute
        let rels = list_keys_async(bucket, pat).await?;
        Ok(rels.into_iter().map(|r| format!("{pat}{r}")).collect())
    } else {
        // exact key
        Ok(vec![pat.to_string()])
    }
}

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

/// Async S3 list using AWS SDK (avoids nested runtimes)
async fn list_keys_async(bucket: &str, prefix: &str) -> Result<Vec<String>> {
    let region_provider = RegionProviderChain::first_try(
        std::env::var("AWS_REGION").ok().map(Region::new)
    )
    .or_default_provider()
    .or_else(Region::new("us-east-1"));

    let cfg = aws_config::defaults(BehaviorVersion::latest())
        .region(region_provider)
        .load()
        .await;

    let client = s3::Client::new(&cfg);

    let mut out = Vec::new();
    let mut cont: Option<String> = None;
    loop {
        let mut req = client.list_objects_v2().bucket(bucket).prefix(prefix);
        if let Some(c) = cont.as_deref() {
            req = req.continuation_token(c);
        }
        let resp = req.send().await?;
        for obj in resp.contents() {
            if let Some(k) = obj.key() {
                // store the key relative to prefix for matching
                let rel = k.strip_prefix(prefix).unwrap_or(k);
                out.push(rel.to_string());
            }
        }
        match resp.next_continuation_token() {
            Some(tok) if !tok.is_empty() => cont = Some(tok.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

