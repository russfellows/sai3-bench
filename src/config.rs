// src/config.rs
use serde::{Deserialize, Serialize};
use crate::size_generator::SizeSpec;

#[derive(Debug, Deserialize, Serialize, Clone)]
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
    
    /// Optional page cache behavior hint for file system operations (v0.6.8+)
    /// Only applies to file:// and direct:// backends on Linux/Unix systems
    /// Maps to posix_fadvise() system call for optimizing kernel page cache behavior
    /// Default: None (uses Auto mode - Sequential for large files, Random for small)
    #[serde(default)]
    pub page_cache_mode: Option<PageCacheMode>,
    
    /// Optional distributed testing configuration (v0.6.11+)
    /// Enables automated SSH deployment, per-agent customization, and coordinated execution
    #[serde(default)]
    pub distributed: Option<DistributedConfig>,
    
    /// Optional I/O rate control (v0.7.1+)
    /// Controls the rate at which operations are issued (not their completion rate)
    /// Default: None (unlimited throughput - current behavior)
    #[serde(default)]
    pub io_rate: Option<IoRateConfig>,
    
    /// Optional multi-process scaling configuration (v0.7.3+)
    /// Controls how many processes to spawn per endpoint for parallel execution
    /// Default: None (single process - current behavior)
    #[serde(default)]
    pub processes: Option<ProcessScaling>,
    
    /// Processing mode for multi-core execution (v0.7.3+)
    /// Default: MultiRuntime (stays in single process, multiple tokio runtimes)
    #[serde(default)]
    pub processing_mode: ProcessingMode,
    
    /// Optional live stats tracker for distributed execution (v0.7.5+)
    /// Used by agent to report real-time operation statistics to controller
    /// This is runtime state and should not be serialized to YAML config files
    #[serde(skip)]
    pub live_stats_tracker: Option<std::sync::Arc<crate::live_stats::LiveStatsTracker>>,
    
    /// Optional error handling configuration (v0.8.0+)
    /// Controls how workload handles I/O errors (retry, skip, or abort)
    /// Default: ErrorHandlingConfig::default() (100 max errors, 5 errors/sec threshold)
    #[serde(default)]
    pub error_handling: ErrorHandlingConfig,
    
    /// Optional operation log path for s3dlio oplog tracing (v0.8.2+)
    /// Captures all storage operations (GET/PUT/DELETE/LIST/etc.) with timestamps and latencies
    /// Output format: TSV (optionally zstd-compressed if path ends with .zst)
    /// Supports environment variables: S3DLIO_OPLOG_BUF=8192 (buffer size)
    /// Note: Oplogs are NOT sorted during capture. Use 'sai3-bench sort' for post-processing.
    /// In distributed mode, overrides agent CLI --op-log flag if specified
    /// Default: None (no operation logging)
    #[serde(default)]
    pub op_log_path: Option<std::path::PathBuf>,
    
    /// Optional warmup period to exclude from statistics analysis (v0.8.15+)
    /// Time at the beginning of workload execution to ignore for metrics.
    /// Data is still captured but flagged as warmup (is_warmup=1 in perf-log).
    /// Useful for excluding cache warming, JIT compilation, connection establishment, etc.
    /// Format: humantime duration (e.g., "30s", "2m", "90s")
    /// Default: None (no warmup period)
    #[serde(default, with = "humantime_serde_opt")]
    pub warmup_period: Option<std::time::Duration>,
    
    /// Optional performance log configuration for time-series metrics (v0.8.15+)
    /// Captures aggregate metrics at regular intervals for performance analysis over time.
    /// Unlike op-log (per-operation), perf-log captures rates/latencies per interval.
    /// Default: None (no performance logging)
    #[serde(default)]
    pub perf_log: Option<PerfLogConfig>,
    
    /// Optional multi-endpoint configuration (v0.8.22+)
    /// Enables load balancing across multiple storage endpoints (S3, Azure, GCS, file://)
    /// All agents use these endpoints unless per-agent override is specified in distributed.agents
    /// Default: None (single target URI)
    #[serde(default)]
    pub multi_endpoint: Option<MultiEndpointConfig>,
}

fn default_duration() -> std::time::Duration {
    std::time::Duration::from_secs(crate::constants::DEFAULT_DURATION_SECS)
}

fn default_concurrency() -> usize {
    crate::constants::DEFAULT_CONCURRENCY
}

/// Error handling configuration (v0.8.0+)
/// 
/// I/O errors are expected during benchmarking (network failures, timeouts, etc.)
/// This configuration controls when the workload should abort vs. continue.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ErrorHandlingConfig {
    /// Maximum total errors before aborting workload
    /// Default: 100
    #[serde(default = "default_max_errors")]
    pub max_total_errors: u64,
    
    /// Error rate threshold (errors per second) to trigger backoff
    /// When this rate is exceeded, workload backs off for backoff_duration
    /// Default: 5.0 (5 errors/second)
    #[serde(default = "default_error_rate_threshold")]
    pub error_rate_threshold: f64,
    
    /// Duration to back off when error rate threshold is hit
    /// Default: 5 seconds
    #[serde(default = "default_backoff_duration", with = "humantime_serde")]
    pub backoff_duration: std::time::Duration,
    
    /// Window for calculating error rate (seconds)
    /// Default: 1 second
    #[serde(default = "default_error_rate_window")]
    pub error_rate_window: f64,
    
    /// Whether to retry failed operations or skip them
    /// Default: false (skip and continue)
    #[serde(default)]
    pub retry_on_error: bool,
    
    /// Maximum retries per operation (only used if retry_on_error=true)
    /// Default: 3
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    
    // v0.8.13: Exponential backoff configuration for retries
    
    /// Initial delay before first retry (milliseconds)
    /// Default: 100ms
    #[serde(default = "default_initial_retry_delay_ms")]
    pub initial_retry_delay_ms: u64,
    
    /// Maximum delay between retries (milliseconds)
    /// Caps exponential growth to prevent excessive waits
    /// Default: 5000ms (5 seconds)
    #[serde(default = "default_max_retry_delay_ms")]
    pub max_retry_delay_ms: u64,
    
    /// Multiplier for exponential backoff (delay * multiplier each retry)
    /// 2.0 means delay doubles: 100ms -> 200ms -> 400ms -> 800ms
    /// Default: 2.0
    #[serde(default = "default_retry_backoff_multiplier")]
    pub retry_backoff_multiplier: f64,
    
    /// Jitter factor for retry delays (0.0 = no jitter, 1.0 = full jitter)
    /// Adds randomness to prevent thundering herd on retries
    /// Default: 0.25 (25% jitter)
    #[serde(default = "default_retry_jitter_factor")]
    pub retry_jitter_factor: f64,
}

impl Default for ErrorHandlingConfig {
    fn default() -> Self {
        Self {
            max_total_errors: crate::constants::DEFAULT_MAX_ERRORS,
            error_rate_threshold: crate::constants::DEFAULT_ERROR_RATE_THRESHOLD,
            backoff_duration: crate::constants::DEFAULT_BACKOFF_DURATION,
            error_rate_window: crate::constants::DEFAULT_ERROR_RATE_WINDOW,
            retry_on_error: crate::constants::DEFAULT_RETRY_ON_ERROR,
            max_retries: crate::constants::DEFAULT_MAX_RETRIES,
            initial_retry_delay_ms: crate::constants::DEFAULT_INITIAL_RETRY_DELAY_MS,
            max_retry_delay_ms: crate::constants::DEFAULT_MAX_RETRY_DELAY_MS,
            retry_backoff_multiplier: crate::constants::DEFAULT_RETRY_BACKOFF_MULTIPLIER,
            retry_jitter_factor: crate::constants::DEFAULT_RETRY_JITTER_FACTOR,
        }
    }
}

fn default_max_errors() -> u64 {
    crate::constants::DEFAULT_MAX_ERRORS
}

fn default_error_rate_threshold() -> f64 {
    crate::constants::DEFAULT_ERROR_RATE_THRESHOLD
}

fn default_backoff_duration() -> std::time::Duration {
    crate::constants::DEFAULT_BACKOFF_DURATION
}

fn default_error_rate_window() -> f64 {
    crate::constants::DEFAULT_ERROR_RATE_WINDOW
}

fn default_max_retries() -> u32 {
    crate::constants::DEFAULT_MAX_RETRIES
}

fn default_initial_retry_delay_ms() -> u64 {
    crate::constants::DEFAULT_INITIAL_RETRY_DELAY_MS
}

fn default_max_retry_delay_ms() -> u64 {
    crate::constants::DEFAULT_MAX_RETRY_DELAY_MS
}

fn default_retry_backoff_multiplier() -> f64 {
    crate::constants::DEFAULT_RETRY_BACKOFF_MULTIPLIER
}

fn default_retry_jitter_factor() -> f64 {
    crate::constants::DEFAULT_RETRY_JITTER_FACTOR
}

/// Performance log configuration for time-series metrics capture (v0.8.15+)
/// 
/// Captures aggregate performance metrics at regular intervals for analysis
/// of how performance changes over the duration of a workload.
/// 
/// In distributed mode, perf_log.tsv is always written to the results directory,
/// so the path field is optional and ignored.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PerfLogConfig {
    /// Path to performance log file (OPTIONAL - ignored in distributed mode)
    /// In distributed mode, perf_log.tsv is always written to results directory.
    /// For single-node mode, specify path like "/tmp/perf.tsv" or "/tmp/perf.tsv.zst"
    /// If path ends with .zst, output will be zstd-compressed
    #[serde(default)]
    pub path: Option<std::path::PathBuf>,
    
    /// Sampling interval for metrics capture (e.g., "1s", "5s", "10s")
    /// Default: 1 second
    #[serde(default = "default_perf_log_interval", with = "humantime_serde")]
    pub interval: std::time::Duration,
}

fn default_perf_log_interval() -> std::time::Duration {
    std::time::Duration::from_secs(crate::constants::DEFAULT_PERF_LOG_INTERVAL_SECS)
}

/// Multi-endpoint configuration for load balancing across multiple storage endpoints (v0.8.22+)
/// 
/// Enables distributing I/O operations across multiple storage endpoints for improved
/// performance and bandwidth utilization. Particularly useful for:
/// - Multi-NIC storage systems (VAST, Weka, etc.)
/// - Distributed object storage (multiple S3 endpoints, MinIO clusters)
/// - Multi-mount NFS with identical namespaces
/// 
/// Example use case: 4 test hosts, each targeting 2 of 8 storage IPs
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MultiEndpointConfig {
    /// Load balancing strategy (default: round_robin)
    /// - round_robin: Cycle through endpoints sequentially (simple, predictable)
    /// - least_connections: Route to endpoint with fewest active requests (adaptive)
    #[serde(default = "default_load_balance_strategy")]
    pub strategy: String,
    
    /// List of endpoint URIs to load balance across
    /// All endpoints must present identical namespace (same files accessible from each)
    /// Examples:
    ///   - S3: ["s3://192.168.1.10:9000/bucket/", "s3://192.168.1.11:9000/bucket/"]
    ///   - NFS: ["file:///mnt/nfs1/data/", "file:///mnt/nfs2/data/"]
    ///   - Azure: ["az://account/container/", "az://192.168.1.10:10000/container/"]
    pub endpoints: Vec<String>,
}

fn default_load_balance_strategy() -> String {
    "round_robin".to_string()
}

/// Custom serde module for Option<Duration> with humantime parsing
/// 
/// This allows YAML like:
///   warmup_period: 30s
///   warmup_period: 2m
/// while still supporting None for unset values.
mod humantime_serde_opt {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;
    
    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(d) => serializer.serialize_str(&humantime::format_duration(*d).to_string()),
            None => serializer.serialize_none(),
        }
    }
    
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            Some(s) => {
                humantime::parse_duration(&s)
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
            None => Ok(None),
        }
    }
}

/// Processing mode for parallel execution (v0.7.3+)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingMode {
    /// Multiple OS processes (true isolation, pipes for IPC)
    /// Better for: kernel contention measurement, production simulation
    MultiProcess,
    
    /// Multiple tokio runtimes in single process (cleaner, native channels)
    /// Better for: simplicity, debugging, when OS-level isolation not needed
    #[default]
    MultiRuntime,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WeightedOp {
    pub weight: u32,
    
    /// Optional per-operation concurrency override (v0.5.3+)
    #[serde(default)]
    pub concurrency: Option<usize>,
    
    #[serde(flatten)]
    pub spec: OpSpec,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum OpSpec {
    /// GET with a single key, a prefix (ending in '/'), or a glob with '*'.
    /// Can be absolute URI (s3://bucket/key) or relative path (data/file.txt) when target is set.
    Get { 
        path: String,
        
        /// Use multi-endpoint configuration for load balancing (v0.8.22+)
        /// When true, operations are distributed across all configured endpoints
        /// Default: false (use path directly as single endpoint)
        #[serde(default)]
        use_multi_endpoint: bool,
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
        
        /// Use multi-endpoint configuration for load balancing (v0.8.22+)
        /// When true, operations are distributed across all configured endpoints
        /// Default: false (use path directly as single endpoint)
        #[serde(default)]
        use_multi_endpoint: bool,
    },

    /// LIST objects under a path/prefix.
    /// Uses 'path' relative to target, or absolute URI.
    List {
        path: String,
        
        /// Use multi-endpoint configuration for load balancing (v0.8.22+)
        /// When true, operations are distributed across all configured endpoints
        /// Default: false (use path directly as single endpoint)
        #[serde(default)]
        use_multi_endpoint: bool,
    },

    /// STAT/HEAD a single object to get metadata.
    /// Uses 'path' relative to target, or absolute URI.
    Stat {
        path: String,
        
        /// Use multi-endpoint configuration for load balancing (v0.8.22+)
        /// When true, operations are distributed across all configured endpoints
        /// Default: false (use path directly as single endpoint)
        #[serde(default)]
        use_multi_endpoint: bool,
    },

    /// DELETE objects (single or glob pattern).
    /// Uses 'path' relative to target, or absolute URI.
    Delete {
        path: String,
        
        /// Use multi-endpoint configuration for load balancing (v0.8.22+)
        /// When true, operations are distributed across all configured endpoints
        /// Default: false (use path directly as single endpoint)
        #[serde(default)]
        use_multi_endpoint: bool,
    },

    /// MKDIR - Create a directory (filesystem backends only).
    /// Uses 'path' relative to target, or absolute URI (file:// or direct://).
    /// Creates parent directories as needed (like mkdir -p).
    Mkdir {
        path: String,
    },

    /// RMDIR - Remove a directory (filesystem backends only).
    /// Uses 'path' relative to target, or absolute URI (file:// or direct://).
    /// Set recursive=true to remove non-empty directories (like rm -r).
    Rmdir {
        path: String,
        #[serde(default)]
        recursive: bool,
    },
}

/// Strategy for executing prepare phase with multiple ensure_objects entries
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum PrepareStrategy {
    /// Process each ensure_objects entry sequentially (one size at a time)
    /// Default behavior: predictable, separate progress bars per size
    #[default]
    Sequential,
    
    /// Process all ensure_objects entries in parallel (all sizes interleaved)
    /// Better throughput: unified progress bar, better storage pipeline utilization
    Parallel,
}


/// Cleanup error handling mode (v0.8.7+)
/// 
/// Controls how cleanup handles objects that are already deleted or missing.
/// This is important for resuming interrupted cleanup operations.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum CleanupMode {
    /// Strict mode: Report errors for any failed deletion
    /// Best for: First-time cleanup where all objects should exist
    Strict,
    
    /// Tolerant mode: Ignore "not found" errors, report other errors
    /// Best for: Resuming interrupted cleanup operations
    /// Objects that were already deleted will be silently skipped
    #[default]
    Tolerant,
    
    /// Best-effort mode: Ignore all deletion errors
    /// Best for: Cleanup after test failures where object state is uncertain
    /// All errors are logged as warnings but don't affect cleanup success
    BestEffort,
}


/// Prepare configuration for pre-populating objects before testing
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct PrepareConfig {
    /// Objects to ensure exist before test
    #[serde(default)]
    pub ensure_objects: Vec<EnsureSpec>,
    
    /// Whether to cleanup prepared objects after test
    #[serde(default)]
    pub cleanup: bool,
    
    /// Skip workload and only run cleanup (v0.8.7+)
    /// When true, skips prepare and workload phases, only performs cleanup
    /// Used for resuming interrupted cleanup operations or cleaning up after manual testing
    /// Default: false (run normal prepare -> workload -> cleanup sequence)
    #[serde(default)]
    pub cleanup_only: Option<bool>,
    
    /// Delay in seconds after prepare phase completes (for cloud storage eventual consistency)
    /// Default: 0 (no delay). Recommended: 2-5 seconds for cloud storage (GCS, S3, Azure)
    #[serde(default)]
    pub post_prepare_delay: u64,
    
    /// Optional directory tree structure (rdf-bench style width/depth model)
    /// When specified, creates deterministic hierarchical directory structure before workload
    #[serde(default)]
    pub directory_structure: Option<crate::directory_tree::DirectoryStructureConfig>,
    
    /// Strategy for executing prepare phase (v0.7.2+)
    /// - sequential: Process each ensure_objects entry one at a time (default, backward compatible)
    /// - parallel: Interleave all ensure_objects entries for maximum throughput
    #[serde(default)]
    pub prepare_strategy: PrepareStrategy,
    
    /// Skip verification of directory tree structure (v0.7.8+)
    /// When true, assumes all files exist and skips the listing verification step
    /// Use this for faster startup when you know the directory structure is complete
    /// Default: false (always verify and create missing files)
    #[serde(default)]
    pub skip_verification: bool,
    
    /// Cleanup error handling mode (v0.8.7+)
    /// Controls how cleanup handles objects that are already deleted or missing
    /// Default: tolerant (allows resuming interrupted cleanup operations)
    #[serde(default)]
    pub cleanup_mode: CleanupMode,
}

/// Specification for ensuring objects exist
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnsureSpec {
    /// Base URI for object creation (e.g., "s3://bucket/prefix/")
    /// When use_multi_endpoint=true, this provides the path component only (after the endpoint)
    pub base_uri: String,
    
    /// Use multi-endpoint configuration for object creation (v0.8.22+)
    /// When true, objects are distributed across all configured endpoints
    /// This is CRITICAL for performance when endpoints represent different network paths
    /// Default: false (use base_uri directly)
    #[serde(default)]
    pub use_multi_endpoint: bool,
    
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
    
    /// Fill pattern: "zero", "random", or "prand" (pseudo-random, faster)
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

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FillPattern {
    Zero,
    Random,
    Prand,  // Pseudo-random (faster, uses old BASE_BLOCK algorithm)
}

fn default_min_size() -> u64 { 
    crate::constants::DEFAULT_MIN_SIZE
}

fn default_max_size() -> u64 { 
    crate::constants::DEFAULT_MAX_SIZE
}

fn default_fill() -> FillPattern { 
    FillPattern::Random 
}

fn default_dedup_factor() -> usize {
    crate::constants::DEFAULT_DEDUP_FACTOR
}

fn default_compress_factor() -> usize {
    crate::constants::DEFAULT_COMPRESS_FACTOR
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
            OpSpec::Get { path, .. } => self.resolve_uri(path),
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

    /// Get the resolved URI for a metadata operation (List, Stat, Delete, Mkdir, Rmdir)
    pub fn get_meta_uri(&self, meta_op: &OpSpec) -> String {
        match meta_op {
            OpSpec::List { path, .. } 
            | OpSpec::Stat { path, .. } 
            | OpSpec::Delete { path, .. }
            | OpSpec::Mkdir { path }
            | OpSpec::Rmdir { path, .. } => {
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
    /// # use sai3_bench::config::Config;
    /// # use serde_yaml;
    /// 
    /// // Shared storage example:
    /// let yaml = r#"
    /// target: "s3://bucket/bench/"
    /// duration: 5s
    /// workload:
    ///   - op: get
    ///     path: "*"
    ///     weight: 100
    /// "#;
    /// let mut config: Config = serde_yaml::from_str(yaml).unwrap();
    /// config.apply_agent_prefix("agent-1", "agent-1/", true).unwrap();
    /// assert_eq!(config.target.unwrap(), "s3://bucket/bench/agent-1/");
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
        // Shared storage: all agents use same prepared data
        // Per-agent storage: each agent prepares its own data
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
/// hiding network latency through parallelism. However, it adds HEAD request
/// overhead to check file sizes on every GET operation.
/// 
/// **Performance Impact:**
/// - Small files (<16 MiB): HEAD overhead outweighs benefits - net SLOWER
/// - Medium files (16-64 MiB): Marginal benefit, often still slower
/// - Large files (>64 MiB): 30-50% faster on high-latency networks
/// 
/// **Default: DISABLED** (false) based on production benchmarks showing 20-25%
/// regression on typical workloads (1 MiB objects on GCS). Only enable if:
/// - Your workload has primarily large files (>64 MiB)
/// - Network latency is high (>100ms)
/// - High bandwidth available (>1 Gbps)
/// 
/// When enabled, set min_split_size to at least 16 MiB to avoid overhead.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RangeEngineConfig {
    /// Enable or disable RangeEngine
    /// Default: false (disabled) - avoids HEAD overhead on typical workloads
    #[serde(default = "default_range_engine_enabled")]
    pub enabled: bool,
    
    /// Size of each concurrent range request in bytes
    /// Default: 67108864 (64 MiB)
    /// - Larger chunks: Fewer requests, less overhead, but less parallelism
    /// - Smaller chunks: More parallelism, but more overhead
    ///
    /// Recommended: 32-128 MiB depending on network speed
    #[serde(default = "default_chunk_size")]
    pub chunk_size: u64,
    
    /// Maximum number of concurrent range requests
    /// Default: 16
    /// - Higher values: More parallelism for high-bandwidth networks (>1 Gbps)
    /// - Lower values: Reduce load on slow networks (<100 Mbps)
    ///
    /// Recommended: 8-32 depending on network capacity
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_ranges: usize,
    
    /// Minimum file size to trigger RangeEngine (in bytes)
    /// Default: 16777216 (16 MiB) - matches s3dlio library default
    /// Files smaller than this use simple sequential downloads
    /// 
    /// **Performance Note**: Production benchmarks show HEAD overhead makes
    /// RangeEngine slower for files <16 MiB. Only lower this if you have:
    /// - Verified performance benefit on your specific workload
    /// - Very high network latency (>200ms) where parallelism helps
    /// 
    /// Recommended: 16-64 MiB for typical cloud storage workloads
    #[serde(default = "default_min_split_size")]
    pub min_split_size: u64,
    
    /// Timeout for each range request in seconds
    /// Default: 30 seconds
    /// Increase for slow or unstable networks
    #[serde(default = "default_range_timeout_secs")]
    pub range_timeout_secs: u64,
}

fn default_range_engine_enabled() -> bool {
    false  // Disabled by default - avoids 20-25% regression on typical workloads
}

fn default_chunk_size() -> u64 {
    crate::constants::DEFAULT_CHUNK_SIZE
}

fn default_max_concurrent() -> usize {
    crate::constants::DEFAULT_MAX_CONCURRENT
}

fn default_min_split_size() -> u64 {
    crate::constants::DEFAULT_MIN_SPLIT_SIZE
}

fn default_range_timeout_secs() -> u64 {
    crate::constants::DEFAULT_RANGE_TIMEOUT_SECS
}

/// Page cache behavior modes for file system operations (Linux/Unix posix_fadvise hints)
/// Only applies to file:// and direct:// backends
/// 
/// This controls kernel page cache behavior via posix_fadvise() system calls,
/// optimizing memory usage and I/O patterns for different access scenarios.
/// 
/// **Performance Impact**:
/// - Proper mode selection can provide 2-3x performance improvement
/// - Wrong mode can cause cache thrashing and memory pressure
/// - No effect on non-Linux/Unix systems (silently ignored)
/// 
/// **Mode Selection Guide**:
/// - **Sequential**: Large files read once from start to finish (e.g., video streaming)
/// - **Random**: Many small seeks within files (e.g., database workloads)
/// - **DontNeed**: Read-once data that shouldn't evict other cache (e.g., backup/archival)
/// - **Normal**: Default kernel heuristics (safe but not optimized)
/// - **Auto**: Smart default - Sequential for full GETs, Random for range GETs
/// 
/// **Example YAML**:
/// ```yaml
/// target: "file:///data/test/"
/// page_cache_mode: sequential  # or: random, dontneed, normal, auto
/// duration: 60
/// ```
#[derive(Debug, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum PageCacheMode {
    /// Smart default: Sequential for large files, Random for small/range requests
    Auto,
    /// Read ahead aggressively, pages not needed after reading (streaming workloads)
    Sequential,
    /// Don't read ahead, pages needed again soon (database/random access)
    Random,
    /// Pages will not be accessed again, free immediately (backup/archival)
    #[serde(rename = "dontneed")]
    DontNeed,
    /// Use default kernel heuristics (safe baseline)
    Normal,
}

/// Distributed testing configuration (v0.6.11+)
/// Enables automated multi-host deployment with per-agent customization
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DistributedConfig {
    /// List of agent configurations
    pub agents: Vec<AgentConfig>,
    
    /// SSH deployment configuration (optional)
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    
    /// Docker deployment configuration (optional)
    #[serde(default)]
    pub deployment: Option<DeploymentConfig>,
    
    /// Delay in seconds before coordinated start (default: 2)
    /// Allows time for all agents to receive config and prepare
    #[serde(default = "default_start_delay")]
    pub start_delay: u64,
    
    /// Path prefix template for agent isolation (default: "agent-{id}/")
    /// Use {id} as placeholder for agent identifier
    #[serde(default = "default_path_template")]
    pub path_template: String,
    
    /// Whether target storage is shared across agents (REQUIRED - no default)
    /// Must be explicitly set to true or false based on your actual deployment
    /// - true: Shared storage - agents access same data (e.g., NFS, Lustre, shared S3 bucket)
    /// - false: Per-agent storage - each agent has isolated namespace (e.g., local disks, agent-specific S3 prefixes)
    ///
    /// Note: This is independent of storage TYPE - file://, s3://, az://, gs:// can all be shared OR per-agent
    pub shared_filesystem: bool,
    
    /// How agents create directory tree structure (REQUIRED - no default)
    /// - isolated: Each agent creates separate tree under agent-{id}/ prefix
    /// - coordinator: Controller creates tree once, agents skip prepare phase
    /// - concurrent: All agents create same tree (idempotent mkdir handles races)
    pub tree_creation_mode: TreeCreationMode,
    
    /// How agents select paths during workload (REQUIRED - affects contention level)
    /// - random: All agents pick any directory - maximum contention
    /// - partitioned: Agents prefer hash(path) % agent_id directories - reduced contention
    /// - exclusive: Each agent only uses assigned directories - minimal contention
    /// - weighted: Probabilistic mix controlled by partition_overlap
    pub path_selection: PathSelectionStrategy,
    
    /// For partitioned/weighted modes: probability of accessing other agents' partitions (0.0-1.0)
    /// Default: 0.3 (30% chance to access another agent's partition)
    /// Only used when path_selection is "partitioned" or "weighted"
    #[serde(default = "default_partition_overlap")]
    pub partition_overlap: f64,
}

/// Individual agent configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AgentConfig {
    /// Agent address (host:port) or hostname (for SSH deployment)
    /// Examples: "node1.example.com:7761", "10.0.1.50:7761", "node1" (SSH mode)
    pub address: String,
    
    /// Optional friendly identifier (default: derived from address)
    /// Used in results directory naming and logging
    #[serde(default)]
    pub id: Option<String>,
    
    /// Override base target URI for this agent
    /// Example: "s3://bucket-2/" to use different bucket
    #[serde(default)]
    pub target_override: Option<String>,
    
    /// Override concurrency for this agent
    /// Useful for heterogeneous hardware (different CPU counts)
    #[serde(default)]
    pub concurrency_override: Option<usize>,
    
    /// Environment variables to inject into agent container/process
    /// Example: {"AWS_PROFILE": "benchmark-reader", "RUST_LOG": "debug"}
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    
    /// Docker volume mounts for this agent (format: "host:container" or "host:container:mode")
    /// Example: ["/mnt/nvme:/data", "/tmp/results:/results:ro"]
    #[serde(default)]
    pub volumes: Vec<String>,
    
    /// Custom path template override for this agent (default: uses distributed.path_template)
    #[serde(default)]
    pub path_template: Option<String>,
    
    /// Listen port for agent (SSH mode only, default: 7761)
    #[serde(default = "default_agent_port")]
    pub listen_port: u16,
    
    /// Per-agent multi-endpoint override (v0.8.22+)
    /// If specified, this agent uses these endpoints instead of global multi_endpoint config
    /// Enables static endpoint mapping: assign specific endpoints to specific agents
    /// Example: Agent 1 -> [IP1, IP2], Agent 2 -> [IP3, IP4], etc.
    #[serde(default)]
    pub multi_endpoint: Option<MultiEndpointConfig>,
}

/// SSH configuration for automated deployment
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SshConfig {
    /// Enable SSH automation (default: false)
    #[serde(default)]
    pub enabled: bool,
    
    /// SSH username (default: current user)
    #[serde(default)]
    pub user: Option<String>,
    
    /// Path to SSH private key (default: ~/.ssh/id_rsa)
    #[serde(default)]
    pub key_path: Option<String>,
    
    /// SSH connection timeout in seconds (default: 10)
    #[serde(default = "default_ssh_timeout")]
    pub timeout: u64,
    
    /// Known hosts file path (default: ~/.ssh/known_hosts)
    /// Set to empty string to disable host key checking (insecure!)
    #[serde(default)]
    pub known_hosts: Option<String>,
}

/// Container deployment configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DeploymentConfig {
    /// Deployment type: "docker" or "binary"
    /// - docker: Use container runtime with specified image
    /// - binary: Execute sai3bench-agent directly on host
    #[serde(default = "default_deployment_type")]
    pub deploy_type: String,
    
    /// Container runtime command: "docker" or "podman" (default: "docker")
    /// Used for: container_runtime run, container_runtime pull, container_runtime ps, etc.
    #[serde(default = "default_container_runtime")]
    pub container_runtime: String,
    
    /// Container image to use (e.g., "sai3bench:v0.6.11")
    #[serde(default = "default_docker_image")]
    pub image: String,
    
    /// Container network mode (default: "host")
    /// For gRPC agent communication, "host" is recommended
    #[serde(default = "default_network_mode")]
    pub network_mode: String,
    
    /// Image pull policy: "always", "if_not_present", "never"
    #[serde(default = "default_pull_policy")]
    pub pull_policy: String,
    
    /// Path to sai3bench-agent binary on remote hosts (binary mode)
    #[serde(default = "default_binary_path")]
    pub binary_path: String,
    
    /// Additional docker run arguments
    #[serde(default)]
    pub docker_args: Vec<String>,
}

fn default_start_delay() -> u64 {
    2
}

fn default_path_template() -> String {
    "agent-{id}/".to_string()
}

fn default_agent_port() -> u16 {
    7761
}

fn default_ssh_timeout() -> u64 {
    10
}

fn default_deployment_type() -> String {
    "docker".to_string()
}

fn default_docker_image() -> String {
    "sai3bench:latest".to_string()
}

fn default_container_runtime() -> String {
    "docker".to_string()
}

fn default_network_mode() -> String {
    "host".to_string()
}

fn default_pull_policy() -> String {
    "if_not_present".to_string()
}

fn default_binary_path() -> String {
    "/usr/local/bin/sai3bench-agent".to_string()
}

fn default_partition_overlap() -> f64 {
    crate::constants::DEFAULT_PARTITION_OVERLAP
}

/// Directory tree creation mode for distributed testing
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TreeCreationMode {
    /// Each agent creates separate tree under agent-{id}/ prefix
    Isolated,
    /// Controller creates tree once, agents skip prepare phase
    Coordinator,
    /// All agents create same tree (idempotent mkdir handles races)
    Concurrent,
}

/// Path selection strategy for workload operations (affects contention level)
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PathSelectionStrategy {
    /// All agents pick any directory - maximum contention
    Random,
    /// Agents prefer hash(path) % agent_id directories - reduced contention
    Partitioned,
    /// Each agent only uses assigned directories - minimal contention
    Exclusive,
    /// Probabilistic mix controlled by partition_overlap parameter
    Weighted,
}

// ============================================================================
// I/O Rate Control Configuration (v0.7.1+)
// ============================================================================

/// I/O rate control configuration (v0.7.1)
/// 
/// Controls the rate at which operations are issued to storage backends.
/// Inspired by rdf-bench's iorate= parameter but adapted for sai3-bench's
/// async architecture.
/// 
/// # Example
/// ```yaml
/// io_rate:
///   iops: 1000                 # Target 1000 IOPS total
///   distribution: exponential  # Realistic Poisson arrivals
/// ```
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct IoRateConfig {
    /// Target operations per second (IOPS)
    /// 
    /// Special values:
    /// - 0 or "max": No rate limiting (maximum throughput)
    /// - Numeric value: Target IOPS (e.g., 1000, 5000, 10000)
    /// 
    /// The target rate is distributed evenly across all concurrent workers.
    #[serde(deserialize_with = "deserialize_iops")]
    pub iops: IopsTarget,
    
    /// Inter-arrival time distribution (default: exponential)
    /// 
    /// - exponential: Poisson arrivals (realistic, default)
    /// - uniform: Fixed intervals between operations
    /// - deterministic: Precise timing (testing/debugging only)
    #[serde(default = "default_distribution")]
    pub distribution: ArrivalDistribution,
}

/// IOPS target specification
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum IopsTarget {
    /// No rate limiting - run at maximum throughput
    Max,
    /// Fixed IOPS target (distributed across workers)
    Fixed(u64),
}

/// Inter-arrival time distribution type
#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArrivalDistribution {
    /// Exponential distribution (Poisson arrivals) - realistic
    Exponential,
    /// Uniform distribution (fixed intervals) - synthetic
    Uniform,
    /// Deterministic timing (precise intervals) - testing only
    Deterministic,
}

fn default_distribution() -> ArrivalDistribution {
    ArrivalDistribution::Exponential
}

/// Custom deserializer for IOPS target
/// Accepts:
/// - "max" (string) -> IopsTarget::Max
/// - 0 (number) -> IopsTarget::Max
/// - positive integer -> IopsTarget::Fixed(n)
fn deserialize_iops<'de, D>(deserializer: D) -> Result<IopsTarget, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct IopsVisitor;

    impl<'de> Visitor<'de> for IopsVisitor {
        type Value = IopsTarget;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a positive integer, 0, or \"max\"")
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value == 0 {
                Ok(IopsTarget::Max)
            } else {
                Ok(IopsTarget::Fixed(value))
            }
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value <= 0 {
                Ok(IopsTarget::Max)
            } else {
                Ok(IopsTarget::Fixed(value as u64))
            }
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            if value == "max" || value == "MAX" {
                Ok(IopsTarget::Max)
            } else {
                value
                    .parse::<u64>()
                    .map(|n| if n == 0 { IopsTarget::Max } else { IopsTarget::Fixed(n) })
                    .map_err(|_| E::custom(format!("invalid IOPS value: {}", value)))
            }
        }
    }

    deserializer.deserialize_any(IopsVisitor)
}

// =============================================================================
// Replay Backpressure Configuration (v0.8.9+)
// =============================================================================

/// Replay backpressure configuration (v0.8.9+)
/// 
/// Controls how replay mode handles situations where the target system
/// cannot sustain the recorded I/O rate. When lag exceeds the threshold,
/// replay switches to best-effort mode (issuing ops as fast as possible
/// without timing). If mode flapping is detected (oscillating between
/// normal and best-effort), replay exits gracefully.
/// 
/// # Example
/// ```yaml
/// replay:
///   lag_threshold: 5s      # Switch to best-effort when 5s behind
///   recovery_threshold: 1s # Switch back to normal when caught up to 1s
///   max_flaps_per_minute: 3 # Exit if mode changes 3+ times per minute
///   drain_timeout: 10s     # Wait 10s for in-flight ops on exit
///   max_concurrent: 1000   # Maximum concurrent operations
/// ```
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ReplayConfig {
    /// Lag threshold before switching to best-effort mode
    /// When the gap between scheduled time and wall clock exceeds this,
    /// timing constraints are abandoned and ops are issued immediately.
    /// Default: 5 seconds
    #[serde(default = "default_replay_lag_threshold", with = "humantime_serde")]
    pub lag_threshold: std::time::Duration,
    
    /// Recovery threshold for switching back to normal mode
    /// When lag drops below this threshold, normal timing resumes.
    /// Must be less than lag_threshold to prevent oscillation (hysteresis).
    /// Default: 1 second
    #[serde(default = "default_replay_recovery_threshold", with = "humantime_serde")]
    pub recovery_threshold: std::time::Duration,
    
    /// Maximum mode transitions (flaps) per minute before graceful exit
    /// Prevents endless oscillation between normal and best-effort modes.
    /// When exceeded, replay drains in-flight ops and exits with warning.
    /// Default: 3
    #[serde(default = "default_replay_max_flaps")]
    pub max_flaps_per_minute: u32,
    
    /// Timeout for draining in-flight operations on flap-exit
    /// After flap limit is hit, wait this long for pending ops to complete.
    /// Default: 10 seconds
    #[serde(default = "default_replay_drain_timeout", with = "humantime_serde")]
    pub drain_timeout: std::time::Duration,
    
    /// Maximum concurrent operations during replay
    /// Limits how many operations can be in-flight simultaneously.
    /// Default: 1000
    #[serde(default = "default_replay_max_concurrent")]
    pub max_concurrent: usize,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            lag_threshold: crate::constants::DEFAULT_REPLAY_LAG_THRESHOLD,
            recovery_threshold: crate::constants::DEFAULT_REPLAY_RECOVERY_THRESHOLD,
            max_flaps_per_minute: crate::constants::DEFAULT_REPLAY_MAX_FLAPS_PER_MINUTE,
            drain_timeout: crate::constants::DEFAULT_REPLAY_DRAIN_TIMEOUT,
            max_concurrent: crate::constants::DEFAULT_REPLAY_MAX_CONCURRENT,
        }
    }
}

fn default_replay_lag_threshold() -> std::time::Duration {
    crate::constants::DEFAULT_REPLAY_LAG_THRESHOLD
}

fn default_replay_recovery_threshold() -> std::time::Duration {
    crate::constants::DEFAULT_REPLAY_RECOVERY_THRESHOLD
}

fn default_replay_max_flaps() -> u32 {
    crate::constants::DEFAULT_REPLAY_MAX_FLAPS_PER_MINUTE
}

fn default_replay_drain_timeout() -> std::time::Duration {
    crate::constants::DEFAULT_REPLAY_DRAIN_TIMEOUT
}

fn default_replay_max_concurrent() -> usize {
    crate::constants::DEFAULT_REPLAY_MAX_CONCURRENT
}

/// Multi-process scaling configuration (v0.7.3+)
/// Controls how many processes to spawn per endpoint for parallel execution
#[derive(Debug, Clone, PartialEq, Serialize)]
#[derive(Default)]
pub enum ProcessScaling {
    /// Single process mode (default, current behavior)
    #[default]
    Single,
    
    /// Auto-scale to physical CPU cores (not hyperthreads)
    /// Best for cloud storage (S3/Azure/GCS) with single endpoint
    Auto,
    
    /// Explicit process count
    /// Useful for manual tuning or on-premises multi-endpoint setups
    Manual(usize),
}


/// Serde deserialization for ProcessScaling
/// Accepts: "auto", "single", 1, or explicit integer
impl<'de> Deserialize<'de> for ProcessScaling {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ProcessScalingVisitor;

        impl<'de> serde::de::Visitor<'de> for ProcessScalingVisitor {
            type Value = ProcessScaling;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("\"auto\", \"single\", 1, or a positive integer")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value == 1 {
                    Ok(ProcessScaling::Single)
                } else if value > 1 {
                    Ok(ProcessScaling::Manual(value as usize))
                } else {
                    Err(E::custom("processes must be >= 1"))
                }
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value == 1 {
                    Ok(ProcessScaling::Single)
                } else if value > 1 {
                    Ok(ProcessScaling::Manual(value as usize))
                } else {
                    Err(E::custom("processes must be >= 1"))
                }
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match value {
                    "auto" | "Auto" | "AUTO" => Ok(ProcessScaling::Auto),
                    "single" | "Single" | "SINGLE" | "1" => Ok(ProcessScaling::Single),
                    _ => {
                        match value.parse::<usize>() {
                            Ok(1) => Ok(ProcessScaling::Single),
                            Ok(n) if n > 1 => Ok(ProcessScaling::Manual(n)),
                            Ok(_) => Err(E::custom("processes must be >= 1")),
                            Err(_) => Err(E::custom(format!("invalid process scaling value: {}", value))),
                        }
                    }
                }
            }
        }

        deserializer.deserialize_any(ProcessScalingVisitor)
    }
}

impl ProcessScaling {
    /// Resolve the actual number of processes to spawn
    /// For Auto mode, detects physical CPU cores
    pub fn resolve(&self) -> usize {
        match self {
            ProcessScaling::Single => 1,
            ProcessScaling::Auto => detect_physical_cores(),
            ProcessScaling::Manual(n) => *n,
        }
    }
}

/// Detect number of physical CPU cores (not hyperthreads)
/// Returns 1 if detection fails
fn detect_physical_cores() -> usize {
    num_cpus::get_physical().max(1)
}
