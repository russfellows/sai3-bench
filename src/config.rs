// src/config.rs
use serde::Deserialize;
use crate::size_generator::SizeSpec;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// Total wall time to run (e.g. "60s", "5m"). Defaults to 60s if omitted.
    #[serde(default = "default_duration", with = "humantime_serde")]
    pub duration: std::time::Duration,

    /// Number of concurrent workers.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,

    /// Base URI for the target backend (e.g., "s3://bucket/path", "file:///tmp/test", "direct:///mnt/data")
    /// If specified, all operations will be relative to this base URI.
    pub target: Option<String>,

    /// Weighted list of operations to pick from.
    pub workload: Vec<WeightedOp>,

    /// Optional prepare step to ensure objects exist before testing (Warp parity)
    #[serde(default)]
    pub prepare: Option<PrepareConfig>,
    
    /// Optional RangeEngine configuration for controlling concurrent range downloads (v0.9.4+)
    /// Only applies to network backends (S3, Azure, GCS) with files >= min_split_size
    #[serde(default)]
    pub range_engine: Option<RangeEngineConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WeightedOp {
    pub weight: u32,
    
    /// Optional per-operation concurrency override (v0.5.3+)
    #[serde(default)]
    pub concurrency: Option<usize>,
    
    #[serde(flatten)]
    pub spec: OpSpec,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum OpSpec {
    /// GET with a single key, a prefix (ending in '/'), or a glob with '*'.
    /// Can be absolute URI (s3://bucket/key) or relative path (data/file.txt) when target is set.
    Get { 
        path: String 
    },

    /// PUT objects with configurable sizes.
    /// Uses 'path' relative to target, or absolute URI.
    /// 
    /// Supports two syntaxes:
    /// 1. Fixed size (backward compatible):
    ///    Put { path: "data/", object_size: 1048576 }
    /// 
    /// 2. Size distribution (v0.5.3+):
    ///    Put { path: "data/", size_distribution: { type: "lognormal", mean: 1048576, ... } }
    Put {
        path: String,
        
        /// Fixed object size in bytes (backward compatible)
        #[serde(default)]
        object_size: Option<u64>,
        
        /// Size distribution specification (v0.5.3+)
        #[serde(default, alias = "size_distribution")]
        size_spec: Option<SizeSpec>,
        
        /// Deduplication factor: 1 = all unique blocks, 2 = 1/2 unique, 3 = 1/3 unique, etc.
        /// Controls block-level deduplication ratio for generated data (v0.5.3+)
        #[serde(default = "default_dedup_factor")]
        dedup_factor: usize,
        
        /// Compression factor: 1 = random (uncompressible), 2 = 2:1 ratio, 3 = 3:1 ratio, etc.
        /// Controls compressibility of generated data (v0.5.3+)
        #[serde(default = "default_compress_factor")]
        compress_factor: usize,
    },

    /// LIST objects under a path/prefix.
    /// Uses 'path' relative to target, or absolute URI.
    List {
        path: String,
    },

    /// STAT/HEAD a single object to get metadata.
    /// Uses 'path' relative to target, or absolute URI.
    Stat {
        path: String,
    },

    /// DELETE objects (single or glob pattern).
    /// Uses 'path' relative to target, or absolute URI.
    Delete {
        path: String,
    },
}

fn default_duration() -> std::time::Duration {
    std::time::Duration::from_secs(60)
}

fn default_concurrency() -> usize {
    32  // Higher default for better throughput
}

/// Prepare configuration for pre-populating objects before testing
#[derive(Debug, Deserialize, Clone, Default)]
pub struct PrepareConfig {
    /// Objects to ensure exist before test
    #[serde(default)]
    pub ensure_objects: Vec<EnsureSpec>,
    
    /// Whether to cleanup prepared objects after test
    #[serde(default)]
    pub cleanup: bool,
    
    /// Delay in seconds after prepare phase completes (for cloud storage eventual consistency)
    /// Default: 0 (no delay). Recommended: 2-5 seconds for cloud storage (GCS, S3, Azure)
    #[serde(default)]
    pub post_prepare_delay: u64,
}

/// Specification for ensuring objects exist
#[derive(Debug, Deserialize, Clone)]
pub struct EnsureSpec {
    /// Base URI for object creation (e.g., "s3://bucket/prefix/")
    pub base_uri: String,
    
    /// Target number of objects to ensure exist
    pub count: u64,
    
    /// Minimum object size in bytes (deprecated - use size_distribution)
    #[serde(default)]
    pub min_size: Option<u64>,
    
    /// Maximum object size in bytes (deprecated - use size_distribution)
    #[serde(default)]
    pub max_size: Option<u64>,
    
    /// Size distribution specification (v0.5.3+, preferred)
    #[serde(default, alias = "size_distribution")]
    pub size_spec: Option<SizeSpec>,
    
    /// Fill pattern: "zero" or "random"
    #[serde(default = "default_fill")]
    pub fill: FillPattern,
    
    /// Deduplication factor (v0.5.3+): 1 = all unique, 2 = 1/2 unique, 3 = 1/3 unique, etc.
    /// Only applies when using s3dlio-controlled data generation
    #[serde(default = "default_dedup_factor")]
    pub dedup_factor: usize,
    
    /// Compression factor (v0.5.3+): 1 = uncompressible (random), 2 = 2:1, 3 = 3:1, etc.
    /// Controls the ratio of constant (zero) bytes to random bytes
    #[serde(default = "default_compress_factor")]
    pub compress_factor: usize,
}

impl EnsureSpec {
    /// Get the size specification, converting legacy min/max_size if needed
    pub fn get_size_spec(&self) -> SizeSpec {
        use crate::size_generator::{SizeDistribution, DistributionType, DistributionParams};
        
        if let Some(ref spec) = self.size_spec {
            // New syntax
            spec.clone()
        } else {
            // Backward compatibility: convert min_size/max_size to uniform distribution
            let min = self.min_size.unwrap_or(default_min_size());
            let max = self.max_size.unwrap_or(default_max_size());
            
            if min == max {
                // Fixed size
                SizeSpec::Fixed(min)
            } else {
                // Uniform distribution
                SizeSpec::Distribution(SizeDistribution {
                    dist_type: DistributionType::Uniform,
                    min: Some(min),
                    max: Some(max),
                    params: DistributionParams {
                        mean: None,
                        std_dev: None,
                    },
                })
            }
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FillPattern {
    Zero,
    Random,
}

fn default_min_size() -> u64 { 
    1024 * 1024  // 1 MiB
}

fn default_max_size() -> u64 { 
    1024 * 1024  // 1 MiB
}

fn default_fill() -> FillPattern { 
    FillPattern::Zero 
}

fn default_dedup_factor() -> usize {
    1  // All unique blocks by default
}

fn default_compress_factor() -> usize {
    1  // Uncompressible (random) by default
}

impl Config {
    /// Resolve a path to a full URI using the target base URI
    pub fn resolve_uri(&self, path: &str) -> String {
        match &self.target {
            Some(base) => {
                if path.contains("://") {
                    // Path is already an absolute URI
                    path.to_string()
                } else {
                    // Combine base URI with relative path
                    if base.ends_with('/') {
                        format!("{}{}", base, path)
                    } else {
                        format!("{}/{}", base, path)
                    }
                }
            }
            None => {
                // No target specified, path must be absolute URI
                path.to_string()
            }
        }
    }

    /// Get the resolved URI for a GET operation
    pub fn get_uri(&self, get_op: &OpSpec) -> String {
        match get_op {
            OpSpec::Get { path } => self.resolve_uri(path),
            _ => panic!("Expected GET operation"),
        }
    }

    /// Get the resolved PUT target information (legacy method - returns fixed size)
    /// For backward compatibility with code expecting a fixed size
    pub fn get_put_info(&self, put_op: &OpSpec) -> (String, u64) {
        match put_op {
            OpSpec::Put { path, object_size, size_spec, .. } => {
                let base_uri = self.resolve_uri(path);
                
                // Backward compatibility: prefer object_size if specified
                let size = if let Some(sz) = object_size {
                    *sz
                } else if let Some(spec) = size_spec {
                    // If it's a fixed size spec, return that
                    if let Some(fixed) = spec.as_fixed() {
                        fixed
                    } else {
                        // Distribution - can't return a single size, panic with helpful message
                        panic!("get_put_info called on PUT with distribution; use get_put_size_spec instead");
                    }
                } else {
                    panic!("PUT operation must specify either object_size or size_spec");
                };
                
                (base_uri, size)
            }
            _ => panic!("Expected PUT operation"),
        }
    }
    
    /// Get the resolved PUT target and size specification (v0.5.3+)
    /// Returns (base_uri, SizeSpec) for use with SizeGenerator
    pub fn get_put_size_spec(&self, put_op: &OpSpec) -> (String, SizeSpec) {
        match put_op {
            OpSpec::Put { path, object_size, size_spec, .. } => {
                let base_uri = self.resolve_uri(path);
                
                // Backward compatibility: convert object_size to SizeSpec::Fixed
                let spec = if let Some(spec) = size_spec {
                    spec.clone()
                } else if let Some(sz) = object_size {
                    SizeSpec::Fixed(*sz)
                } else {
                    panic!("PUT operation must specify either object_size or size_spec");
                };
                
                (base_uri, spec)
            }
            _ => panic!("Expected PUT operation"),
        }
    }

    /// Get the resolved URI for a metadata operation (List, Stat, Delete)
    pub fn get_meta_uri(&self, meta_op: &OpSpec) -> String {
        match meta_op {
            OpSpec::List { path } | OpSpec::Stat { path } | OpSpec::Delete { path } => {
                self.resolve_uri(path)
            }
            _ => panic!("Expected metadata operation"),
        }
    }

    /// Apply agent-specific path prefix to all operations for distributed execution.
    /// This enables per-agent path isolation to prevent conflicts between agents.
    /// 
    /// For SHARED storage (S3, GCS, Azure): Only applies prefix to target, NOT to prepare config.
    /// This allows all agents to use the same prepared dataset.
    /// 
    /// For LOCAL storage (file://, direct://): Applies prefix to both target AND prepare config.
    /// Each agent prepares and uses its own isolated dataset.
    /// 
    /// # Arguments
    /// * `agent_id` - Unique identifier for this agent (e.g., "agent-1")
    /// * `prefix` - Path prefix to apply (e.g., "agent-1/")
    /// * `shared_storage` - True if storage is shared (S3/GCS/Azure), false if local per-agent
    /// 
    /// # Example
    /// ```
    /// // Shared storage (S3):
    /// config.apply_agent_prefix("agent-1", "agent-1/", true)?;
    /// // Result: target = "s3://bucket/bench/agent-1/", prepare base_uri unchanged
    /// 
    /// // Local storage (file://):
    /// config.apply_agent_prefix("agent-1", "agent-1/", false)?;
    /// // Result: target = "file:///tmp/bench/agent-1/", prepare base_uri = "file:///tmp/bench/agent-1/data/"
    /// ```
    pub fn apply_agent_prefix(&mut self, _agent_id: &str, prefix: &str, shared_storage: bool) -> anyhow::Result<()> {
        // Store original target for prepare base_uri rewriting
        let original_target = self.target.clone();
        
        // Apply prefix to base target if set
        if let Some(ref target) = self.target {
            self.target = Some(join_uri_path(target, prefix)?);
        }

        // Do NOT apply prefix to operation paths - they are relative to target!
        // The workload execution will resolve them against the modified target.

        // Apply prefix to prepare config ONLY if storage is NOT shared
        // Shared storage (S3/GCS/Azure): all agents use same prepared data
        // Local storage (file://): each agent prepares its own data
        if !shared_storage {
            if let Some(ref mut prepare) = self.prepare {
                // For local storage, we need to update prepare base_uris to match the modified target
                // The prepare base_uri might be an absolute URI that extends the target path
                // We need to insert the prefix at the same location where we modified the target
                if let (Some(ref new_target), Some(ref orig_target)) = (&self.target, &original_target) {
                    for ensure_spec in &mut prepare.ensure_objects {
                        // If base_uri starts with the original target, replace it with the new target
                        if ensure_spec.base_uri.starts_with(orig_target) {
                            let relative_path = &ensure_spec.base_uri[orig_target.len()..];
                            ensure_spec.base_uri = format!("{}{}", new_target, relative_path);
                        } else {
                            // Otherwise, just append the prefix (shouldn't happen in normal configs)
                            ensure_spec.base_uri = join_uri_path(&ensure_spec.base_uri, prefix)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Join a base URI with a path suffix, handling different URI schemes properly.
/// 
/// # Examples
/// - `join_uri_path("s3://bucket/base/", "agent-1/")` → `"s3://bucket/base/agent-1/"`
/// - `join_uri_path("file:///tmp/test/", "agent-1/")` → `"file:///tmp/test/agent-1/"`
/// - `join_uri_path("data/", "agent-1/")` → `"data/agent-1/"` (relative path)
fn join_uri_path(base: &str, suffix: &str) -> anyhow::Result<String> {
    if suffix.is_empty() {
        return Ok(base.to_string());
    }

    // Handle URIs with schemes (s3://, file://, direct://, az://, gs://)
    if let Some(scheme_end) = base.find("://") {
        let scheme = &base[..scheme_end + 3]; // Include "://"
        let path = &base[scheme_end + 3..];
        
        // Ensure path ends with / before appending suffix
        let normalized_path = if path.is_empty() || path.ends_with('/') {
            path.to_string()
        } else {
            format!("{}/", path)
        };
        
        // Ensure suffix doesn't start with / (would create double slash)
        let normalized_suffix = suffix.trim_start_matches('/');
        
        Ok(format!("{}{}{}", scheme, normalized_path, normalized_suffix))
    } else {
        // Relative path (no scheme)
        let normalized_base = if base.is_empty() || base.ends_with('/') {
            base.to_string()
        } else {
            format!("{}/", base)
        };
        
        let normalized_suffix = suffix.trim_start_matches('/');
        Ok(format!("{}{}", normalized_base, normalized_suffix))
    }
}

/// RangeEngine configuration for controlling concurrent range downloads (v0.9.4+)
/// 
/// RangeEngine splits large file downloads into concurrent HTTP range requests,
/// hiding network latency through parallelism. This is most effective for:
/// - Network storage backends (S3, Azure, GCS)
/// - High-bandwidth networks (>100 Mbps)
/// - Large files (>= 4 MB default threshold)
/// 
/// Default settings are optimized for cloud storage with typical network latency.
/// Adjust based on your network characteristics and workload.
#[derive(Debug, Deserialize, Clone)]
pub struct RangeEngineConfig {
    /// Enable or disable RangeEngine
    /// Default: true (enabled)
    #[serde(default = "default_range_engine_enabled")]
    pub enabled: bool,
    
    /// Size of each concurrent range request in bytes
    /// Default: 67108864 (64 MB)
    /// - Larger chunks: Fewer requests, less overhead, but less parallelism
    /// - Smaller chunks: More parallelism, but more overhead
    /// Recommended: 32-128 MB depending on network speed
    #[serde(default = "default_chunk_size")]
    pub chunk_size: u64,
    
    /// Maximum number of concurrent range requests
    /// Default: 32
    /// - Higher values: More parallelism for high-bandwidth networks (>1 Gbps)
    /// - Lower values: Reduce load on slow networks (<100 Mbps)
    /// Recommended: 8-64 depending on network capacity
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_ranges: usize,
    
    /// Minimum file size to trigger RangeEngine (in bytes)
    /// Default: 4194304 (4 MB)
    /// Files smaller than this use simple sequential downloads
    /// - Raise threshold: Reduce overhead for workloads with many small files
    /// - Lower threshold: Enable RangeEngine for smaller files (may add overhead)
    /// Recommended: 4-16 MB
    #[serde(default = "default_min_split_size")]
    pub min_split_size: u64,
    
    /// Timeout for each range request in seconds
    /// Default: 30 seconds
    /// Increase for slow or unstable networks
    #[serde(default = "default_range_timeout_secs")]
    pub range_timeout_secs: u64,
}

fn default_range_engine_enabled() -> bool {
    true
}

fn default_chunk_size() -> u64 {
    64 * 1024 * 1024  // 64 MB
}

fn default_max_concurrent() -> usize {
    32
}

fn default_min_split_size() -> u64 {
    4 * 1024 * 1024  // 4 MB
}

fn default_range_timeout_secs() -> u64 {
    30
}
