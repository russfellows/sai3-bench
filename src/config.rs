// src/config.rs
use crate::size_generator::SizeSpec;
use serde::{Deserialize, Serialize};

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

    /// v0.8.60: KV cache checkpoint interval in seconds (optional)
    /// Controls periodic persistence of metadata cache to storage under test
    /// Works for BOTH standalone (sai3-bench run) and distributed (sai3bench-ctl) modes
    /// 0 = Disabled (only checkpoint at end of prepare)
    /// Default: 300 seconds (5 minutes) for long-running prepares
    ///
    /// **Why periodic checkpointing?**
    /// - Protects against data loss if prepare crashes after hours of work
    /// - For cloud storage (s3://, az://, gs://), creates .tar.zst archive and uploads via ObjectStore
    /// - For filesystem (file://, direct://), creates .tar.zst archive on disk
    ///
    /// Example: checkpoint every 10 minutes during 12-hour prepare:
    /// ```yaml
    /// cache_checkpoint_interval_secs: 600
    /// ```
    #[serde(default = "default_cache_checkpoint_interval")]
    pub cache_checkpoint_interval_secs: u64,

    /// v0.8.89: Enable/disable the internal Fjall KV metadata cache for prepare operations.
    /// The metadata cache tracks which objects have been successfully created, enabling
    /// crash-resume for long-running prepare workloads.
    ///
    /// **When to disable:**
    /// - Per-batch object count exceeds ~1 Billion: above this threshold the KV store
    ///   becomes too large to be practical (~3.4 GB per 50M objects, ~15s scan on resume,
    ///   ~2 GB RAM spike during checkpoint upload).
    /// - Idempotent workloads where re-running from zero is acceptable
    /// - Benchmarks measuring raw write throughput without metadata overhead
    ///
    /// **When to keep enabled (default) — sweet spot: 1M to 1B objects/run:**
    /// - Below 1 Million objects: the cache helps but listing is fast enough to verify
    ///   without it (~200s at 5,000 LIST/s); still useful for crash-resume.
    /// - 1 Million to 1 Billion objects: ideal range — crash-resume value exceeds overhead.
    /// - Above 1 Billion: disable — overhead is impractical.
    ///
    /// Default: true (enabled — preserves existing behavior)
    ///
    /// Example — disable for trillion-object populate (5,000 batches × 200M objects/batch):
    /// ```yaml
    /// enable_metadata_cache: false
    /// ```
    #[serde(default = "default_enable_metadata_cache")]
    pub enable_metadata_cache: bool,

    /// Optional s3dlio optimization configuration (v0.8.63+, requires s3dlio v0.9.50+)
    /// Controls advanced s3dlio features via environment variables.
    /// These settings are applied automatically when the config is loaded.
    ///
    /// **Key Features:**
    /// - Range download optimization: 76% faster for large objects (≥64 MB)
    /// - Multipart upload improvements: Automatic zero-copy optimizations
    ///
    /// Default: None (s3dlio defaults apply - range optimization DISABLED)
    ///
    /// Example:
    /// ```yaml
    /// s3dlio_optimization:
    ///   enable_range_downloads: true
    ///   range_threshold_mb: 64
    /// ```
    #[serde(default)]
    pub s3dlio_optimization: Option<S3dlioOptimizationConfig>,

    /// Credentials to forward to agent nodes (v0.8.92+).
    ///
    /// When `sai3bench-ctl` is invoked with `--env-file <path>` (or `--forward-env`),
    /// it reads the allow-listed credential environment variables from that file and
    /// embeds them here before serialising the config to send to each agent.  The agent
    /// applies these key-value pairs to its own process environment before running the
    /// workload or pre-flight validation.
    ///
    /// **Allowed key prefixes (hard-coded allow-list):**
    /// - `AWS_*` — AWS SDK credentials and endpoint overrides
    /// - `GOOGLE_APPLICATION_CREDENTIALS` — GCP service-account key path
    /// - `AZURE_STORAGE_*` — Azure Blob Storage credentials
    ///
    /// **Security notes:**
    /// - This field is **skipped when empty** (`skip_serializing_if`), so it never
    ///   appears in user-written YAML config files.
    /// - Credentials travel inside the gRPC `config_yaml` field.  Enable TLS
    ///   (`--tls`) when operating over untrusted networks.
    /// - Key names (never values) are logged at `info` level for audit purposes.
    ///
    /// Users should NOT set this field manually in their YAML files; use
    /// `--env-file` on the controller command line instead.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub distributed_env: std::collections::HashMap<String, String>,

    /// When true, newly PUT objects are added to the GET selection pool during the
    /// execute phase, so GET workers can immediately access them (issue #82).
    /// This matches warp's mixed-mode behavior where mixed PUT+GET workloads
    /// eventually converge on steady state with a growing object population.
    ///
    /// Default: false — the GET pool stays frozen (more reproducible: every run
    /// GETs from the same fixed set regardless of PUT throughput).
    ///
    /// Example — enable for warp-style mixed workloads:
    /// ```yaml
    /// dynamic_put_pool: true
    /// ```
    #[serde(default)]
    pub dynamic_put_pool: bool,
}

fn default_duration() -> std::time::Duration {
    std::time::Duration::from_secs(crate::constants::DEFAULT_DURATION_SECS)
}

fn default_concurrency() -> usize {
    crate::constants::DEFAULT_CONCURRENCY
}

pub fn default_cache_checkpoint_interval() -> u64 {
    300 // 5 minutes - protects long-running prepares from data loss
}

fn default_enable_metadata_cache() -> bool {
    true // Enabled by default — preserves backward compatibility
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

    /// List of endpoint URIs to load balance across.
    ///
    /// Must have between 1 and `s3dlio::constants::MAX_ENDPOINTS` entries
    /// (currently 32).  A single-element list works exactly like a direct
    /// target URI with no load balancing overhead beyond one extra indirection.
    ///
    /// Examples:
    ///   - S3: ["s3://192.168.1.10:9000/bucket/", "s3://192.168.1.11:9000/bucket/"]
    ///   - NFS: ["file:///mnt/nfs1/data/", "file:///mnt/nfs2/data/"]
    ///   - Azure: ["az://account/container/", "az://192.168.1.10:10000/container/"]
    pub endpoints: Vec<String>,
}

impl MultiEndpointConfig {
    /// Validate that the endpoint list is within acceptable bounds.
    ///
    /// The maximum is defined once in s3dlio and queried here so that both
    /// libraries always agree on the limit without duplicating the constant.
    pub fn validate(&self) -> anyhow::Result<()> {
        let max = s3dlio::constants::max_endpoints();
        if self.endpoints.is_empty() {
            anyhow::bail!("multi_endpoint.endpoints must contain at least 1 entry");
        }
        if self.endpoints.len() > max {
            anyhow::bail!(
                "multi_endpoint.endpoints has {} entries; maximum allowed is {} \
                 (s3dlio::constants::MAX_ENDPOINTS)",
                self.endpoints.len(),
                max
            );
        }
        Ok(())
    }
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
            Some(s) => humantime::parse_duration(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
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
    Get { path: String },

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
        /// Target path/prefix for PUT objects.
        /// May be an absolute S3 URI ("s3://host:port/bucket/prefix/"),
        /// a relative path (resolved against `target:`), or empty string
        /// when a `multi_endpoint:` block is present — in which case the base URI
        /// is taken from `multi_endpoint.endpoints[0]`.
        #[serde(default)]
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
    List { path: String },

    /// STAT/HEAD a single object to get metadata.
    /// Uses 'path' relative to target, or absolute URI.
    Stat { path: String },

    /// DELETE objects (single or glob pattern).
    /// Uses 'path' relative to target, or absolute URI.
    Delete { path: String },

    /// MKDIR - Create a directory (filesystem backends only).
    /// Uses 'path' relative to target, or absolute URI (file:// or direct://).
    /// Creates parent directories as needed (like mkdir -p).
    Mkdir { path: String },

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
    /// Supports time units: "5s", "2m", "1h" (or plain seconds: 300)
    #[serde(
        default,
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
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

    /// Force overwrite all files regardless of existence (v0.8.24+)
    /// When true, creates all files even if skip_verification=true
    /// Use this to regenerate all data, overwriting partial/corrupted datasets
    /// Recommended: skip_verification=true + force_overwrite=true for fastest full recreation
    /// Default: false
    #[serde(default)]
    pub force_overwrite: bool,

    /// Cleanup error handling mode (v0.8.7+)
    /// Controls how cleanup handles objects that are already deleted or missing
    /// Default: tolerant (allows resuming interrupted cleanup operations)
    #[serde(default)]
    pub cleanup_mode: CleanupMode,

    /// Hex prefix shards for load distribution across hash-routed storage (v0.8.97+).
    ///
    /// When > 0, each flat-mode object key is prefixed with `format!("{:02x}/", idx % key_prefix_shards)`.
    /// Recommended value: `256` — gives a 2-char hex prefix (`00`–`ff`), creating 256 distinct
    /// shard directories that distribute load uniformly across all storage nodes.
    ///
    /// **Use case**: Storage systems like VAST that hash object key prefixes for routing.
    /// sai3-bench's default `prepared-NNNNNNNN.dat` naming has zero prefix entropy (all keys
    /// start with `prepared-`), concentrating all metadata on one server node. With
    /// `key_prefix_shards: 256`, keys become e.g. `2a/prepared-00000042.dat`, spreading
    /// load uniformly.
    ///
    /// Default: 0 (disabled — keys unchanged, existing configs unaffected).
    #[serde(default)]
    pub key_prefix_shards: u32,
}

/// Specification for ensuring objects exist
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct EnsureSpec {
    /// Base URI for object creation (e.g., "s3://bucket/prefix/")
    ///
    /// **Optional when `multi_endpoint:` block is present**: If omitted, the first URI
    /// from the `multi_endpoint` configuration is used for prepare/listing operations.
    /// In distributed isolated-mode runs each agent uses the first URI from its own
    /// `multi_endpoint` block.
    #[serde(default)]
    pub base_uri: Option<String>,

    /// Target number of objects to ensure exist
    #[serde(deserialize_with = "crate::serde_helpers::deserialize_u64_with_separators")]
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
    /// Get the effective base URI for this prepare spec.
    ///
    /// Priority:
    /// 1. `base_uri` if explicitly set
    /// 2. First entry of `multi_endpoint_uris` when no `base_uri` was provided
    /// 3. Error if neither is available
    pub fn get_base_uri(&self, multi_endpoint_uris: Option<&[String]>) -> anyhow::Result<String> {
        if let Some(ref uri) = self.base_uri {
            Ok(uri.clone())
        } else if let Some(uris) = multi_endpoint_uris {
            if let Some(first) = uris.first() {
                Ok(first.clone())
            } else {
                anyhow::bail!("multi_endpoint list is empty — cannot determine base_uri")
            }
        } else {
            anyhow::bail!("base_uri is required when no multi_endpoint configuration is provided")
        }
    }

    /// Get the size specification, converting legacy min/max_size if needed
    pub fn get_size_spec(&self) -> SizeSpec {
        use crate::size_generator::{DistributionParams, DistributionType, SizeDistribution};

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
    Prand, // Pseudo-random (alias for Random; formerly used s3dlio::fill_controlled_data)
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
            OpSpec::Put {
                path,
                object_size,
                size_spec,
                ..
            } => {
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
    ///
    /// When a `multi_endpoint:` block is present and `path` is empty, the base
    /// URI is taken from `endpoints[0]` so that the constructed object URI always
    /// matches one of the endpoint prefixes that MultiEndpointStore can rewrite.
    pub fn get_put_size_spec(&self, put_op: &OpSpec) -> (String, SizeSpec) {
        match put_op {
            OpSpec::Put {
                path,
                object_size,
                size_spec,
                ..
            } => {
                // When a multi_endpoint block is present and path is empty, anchor
                // to endpoints[0] so MultiEndpointStore rewrite logic can route it.
                let base_uri = if path.is_empty() && self.multi_endpoint.is_some() {
                    self.multi_endpoint
                        .as_ref()
                        .and_then(|me| me.endpoints.first())
                        .cloned()
                        .unwrap_or_else(|| self.resolve_uri(path))
                } else {
                    self.resolve_uri(path)
                };

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
            | OpSpec::Rmdir { path, .. } => self.resolve_uri(path),
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
    pub fn apply_agent_prefix(
        &mut self,
        _agent_id: &str,
        prefix: &str,
        shared_storage: bool,
    ) -> anyhow::Result<()> {
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
                if let (Some(ref new_target), Some(ref orig_target)) =
                    (&self.target, &original_target)
                {
                    for ensure_spec in &mut prepare.ensure_objects {
                        // If base_uri is None, skip modification (it will use first multi_endpoint)
                        if let Some(ref base_uri_str) = ensure_spec.base_uri {
                            // If base_uri starts with the original target, replace it with the new target
                            if base_uri_str.starts_with(orig_target) {
                                let relative_path = &base_uri_str[orig_target.len()..];
                                ensure_spec.base_uri =
                                    Some(format!("{}{}", new_target, relative_path));
                            } else {
                                // Otherwise, just append the prefix (shouldn't happen in normal configs)
                                ensure_spec.base_uri = Some(join_uri_path(base_uri_str, prefix)?);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Apply GCS-specific smart defaults and optional concurrency scaling.
    ///
    /// Call this on every config-parsing path (standalone `main.rs` and agent
    /// `execute_workload`), immediately after `s3dlio_opt.apply()`.
    ///
    /// **Behaviour for `gs://` / `gcs://` targets:**
    ///
    /// * **Explicit `gcs_channel_count = N`** (already forwarded to s3dlio via
    ///   `apply()`): treats YAML `concurrency` as *per-channel* parallelism and
    ///   scales the total Tokio task count to `concurrency × N`, capped at the
    ///   number of logical CPUs.  A `WARN` line is emitted whenever scaling
    ///   occurs (both the scaled-up case and the CPU-capped case).
    ///
    /// * **No `gcs_channel_count`** (smart default): calls
    ///   `s3dlio::set_gcs_channel_count(concurrency)` — one subchannel per
    ///   concurrent task, no concurrency change.  Emits `INFO`.
    ///
    /// Non-GCS targets (`s3://`, `az://`, `file://`, …): no-op.
    pub fn apply_gcs_defaults(&mut self) {
        let target_is_gcs = self
            .target
            .as_deref()
            .map(|t| t.starts_with("gs://") || t.starts_with("gcs://"))
            .unwrap_or(false)
            || self
                .multi_endpoint
                .as_ref()
                .map(|me| {
                    me.endpoints
                        .iter()
                        .any(|e| e.starts_with("gs://") || e.starts_with("gcs://"))
                })
                .unwrap_or(false);

        if !target_is_gcs {
            return;
        }

        let explicit_channels = self
            .s3dlio_optimization
            .as_ref()
            .and_then(|o| o.gcs_channel_count);

        if explicit_channels.is_none() {
            // Smart default: one gRPC subchannel per concurrent task.
            // If gcs_channel_count was explicitly set it was already forwarded
            // to s3dlio in apply(), so nothing to do in that case.
            s3dlio::set_gcs_channel_count(self.concurrency);
            tracing::info!(
                "GCS smart default: gRPC channel count = concurrency ({})",
                self.concurrency
            );
        }
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

        Ok(format!(
            "{}{}{}",
            scheme, normalized_path, normalized_suffix
        ))
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
    false // Disabled by default - avoids 20-25% regression on typical workloads
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

/// s3dlio optimization configuration (v0.8.63+, requires s3dlio v0.9.50+)
///
/// Controls advanced s3dlio performance features via environment variables.
/// These settings are converted to environment variables (S3DLIO_*) when the config is loaded,
/// allowing you to configure s3dlio optimizations directly in your YAML files.
///
/// **Key Features**:
/// - **Range downloads**: Parallel range requests for large objects (76% faster for ≥64 MB)
/// - **Multipart uploads**: Zero-copy chunking (automatic, no configuration needed)
///
/// **Performance Benchmark** (16× 148 MB objects, MinIO):
/// - Without optimization: 429 MB/s (5.52s)
/// - With optimization (64 MB threshold): 755 MB/s (3.14s) - **76% faster**
///
/// **When to Enable Range Downloads**:
/// - ✅ Large objects (≥64 MB typical, ≥128 MB for very large)
/// - ✅ Cross-region downloads (high latency)
/// - ✅ Throttled connections
/// - ✅ Workloads dominated by large file GET operations
///
/// **When to Disable**:
/// - ❌ Small files (< 64 MB) - adds HEAD request overhead
/// - ❌ Same-region high-bandwidth (coordination overhead may dominate)
/// - ❌ PUT-heavy workloads
/// - ❌ HEAD requests are rate-limited
///
/// **Example YAML**:
/// ```yaml
/// target: "s3://my-bucket/large-files/"
/// duration: 300s
/// concurrency: 16
///
/// # Enable s3dlio optimizations
/// s3dlio_optimization:
///   enable_range_downloads: true   # Enable parallel range requests
///   range_threshold_mb: 64          # Only for objects ≥64 MB (recommended)
///   # range_concurrency: 16         # Optional: Override auto-calculated concurrency
///   # chunk_size_mb: 4              # Optional: Override auto-calculated chunk size
///
/// workload:
///   - op: get
///     path: "data/*"
///     weight: 80
/// ```
///
/// **Environment Variables Set**:
/// - `S3DLIO_ENABLE_RANGE_OPTIMIZATION`: "1" if enable_range_downloads is true
/// - `S3DLIO_RANGE_THRESHOLD_MB`: Threshold in MB (default: 64)
/// - `S3DLIO_RANGE_CONCURRENCY`: Number of parallel requests (optional, auto-calculated by default)
/// - `S3DLIO_CHUNK_SIZE`: Chunk size in bytes (optional, auto-calculated by default)
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct S3dlioOptimizationConfig {
    /// Enable parallel range downloads for large objects (v0.9.50+)
    ///
    /// When enabled, objects ≥ range_threshold_mb are downloaded using concurrent range requests
    /// instead of a single sequential GET. This provides significant speedups (50-76%) for
    /// large objects, especially in high-latency or cross-region scenarios.
    ///
    /// **Performance Impact**:
    /// - 64 MB threshold: +76% throughput (429 → 755 MB/s)
    /// - 32 MB threshold: +71% throughput
    /// - 16 MB threshold: +69% throughput
    ///
    /// **Overhead**: Adds one HEAD request per object to determine size.
    /// Only enable if your workload includes many objects ≥ threshold size.
    ///
    /// Default: false (disabled to avoid HEAD overhead on typical workloads)
    #[serde(default)]
    pub enable_range_downloads: bool,

    /// Threshold in MB for when to use parallel range downloads
    /// Objects smaller than this use simple sequential downloads.
    ///
    /// **Recommended values**:
    /// - 64 MB: Best balance (76% improvement, low overhead risk)
    /// - 128 MB: Very conservative (only huge files)
    /// - 32 MB: Moderate (71% improvement, some overhead for 32-64 MB files)
    /// - 16 MB: Aggressive (69% improvement, higher overhead for 16-32 MB files)
    ///
    /// Default: 64 MB (optimal for most workloads)
    #[serde(default = "default_s3dlio_range_threshold_mb")]
    pub range_threshold_mb: u64,

    /// Optional: Number of concurrent range requests
    /// If not specified, s3dlio auto-calculates based on object size (8-32 typical).
    ///
    /// **Tuning**:
    /// - Higher values (32+): Better for high-bandwidth, low-latency networks
    /// - Lower values (8-16): Better for bandwidth-limited or high-latency scenarios
    ///
    /// Default: None (auto-calculated by s3dlio)
    #[serde(default)]
    pub range_concurrency: Option<usize>,

    /// Optional: Chunk size in MB for each range request
    /// If not specified, s3dlio auto-calculates based on object size (1-8 MB typical).
    ///
    /// **Tuning**:
    /// - Larger chunks (8+ MB): Fewer requests, less coordination overhead
    /// - Smaller chunks (1-2 MB): More parallelism, better for very high latency
    ///
    /// Default: None (auto-calculated by s3dlio)
    #[serde(default)]
    pub chunk_size_mb: Option<u64>,

    // ── GCS-specific settings ─────────────────────────────────────────────────
    /// GCS: gRPC subchannel count override (v0.9.65+, requires s3dlio ≥ 0.9.65)
    ///
    /// Sets the number of gRPC subchannels (independent HTTP/2 connections) used
    /// by the GCS client.  Each subchannel can carry multiple concurrent streams;
    /// matching this to concurrency gives each task an uncontested flow-control
    /// window.
    ///
    /// **Concurrency scaling**: When set to N, sai3-bench treats the YAML
    /// `concurrency` field as *per-channel* parallelism and scales the total
    /// Tokio task count to `concurrency × N`, capped at the number of logical
    /// CPUs.  A `WARN` line is emitted whenever the total is adjusted.
    ///
    /// If absent, channel count is auto-set to `concurrency` for `gs://` targets
    /// (smart default — one channel per task, no concurrency scaling).
    ///
    /// Only meaningful for `gs://` / `gcs://` targets; silently ignored otherwise.
    #[serde(default)]
    pub gcs_channel_count: Option<usize>,

    /// GCS: RAPID (Hyperdisk ML / zonal GCS) mode override (v0.9.65+)
    ///
    /// Controls whether GCS I/O uses the appendable-object / bidi-read RAPID
    /// path.  By default s3dlio auto-detects per bucket via `GetStorageLayout`.
    ///
    /// - `true`  — force RAPID on (skips detection RPC; use for known RAPID buckets)
    /// - `false` — force RAPID off (standard reads, no appendable writes)
    /// - absent  — auto-detect per bucket (recommended default)
    ///
    /// The `S3DLIO_GCS_RAPID` environment variable still takes precedence if set.
    /// Only meaningful for `gs://` targets; silently ignored for S3/Azure/file.
    #[serde(default)]
    pub gcs_rapid_mode: Option<bool>,

    /// GCS: gRPC write chunk size in bytes (v0.9.65+, requires s3dlio ≥ 0.9.65)
    ///
    /// Sets `S3DLIO_GRPC_WRITE_CHUNK_SIZE`. Controls the payload size of each
    /// gRPC write frame for GCS bidi-streaming uploads (RAPID write path).
    ///
    /// Valid range: 256 KiB – 4 128 768 bytes (4 MiB − 64 KiB, the gRPC frame
    /// limit).
    ///
    /// | Value   | Notes                                              |
    /// |---------|----------------------------------------------------|
    /// | 2097152 | Default (2 MiB) — safe for all workloads           |
    /// | 4128768 | Maximum safe value — best throughput on RAPID      |
    ///
    /// Only meaningful for `gs://` write operations; ignored for reads.
    #[serde(default)]
    pub gcs_write_chunk_size_bytes: Option<u64>,

    /// GCS: Google Cloud project ID (v0.9.70+)
    ///
    /// Sets the GCS project ID used for listing buckets. When set, takes
    /// precedence over the `GOOGLE_CLOUD_PROJECT`, `GCLOUD_PROJECT`, and
    /// `GCS_PROJECT` environment variables.
    ///
    /// Set this in YAML so you don't have to export the variable on every
    /// agent host separately.  Only needed for `sai3bench-ctl list-buckets`
    /// and bucket-creation operations; individual object reads/writes resolve
    /// the project from the bucket's own metadata.
    ///
    /// If absent, falls back to `GOOGLE_CLOUD_PROJECT` → `GCLOUD_PROJECT` →
    /// `GCS_PROJECT` environment variables (existing behaviour).
    #[serde(default)]
    pub gcs_project: Option<String>,

    // ── HTTP/2 (h2c) settings ─────────────────────────────────────────────────
    /// Enable HTTP/2 cleartext (h2c) for `http://` endpoints (v0.9.90+)
    ///
    /// Sets `S3DLIO_H2C`.  Only meaningful for storage endpoints accessed via
    /// plain `http://`; `https://` endpoints negotiate HTTP/2 automatically via
    /// TLS ALPN and are unaffected.
    ///
    /// - `true`  — force h2c prior-knowledge (no HTTP/1.1 fallback)
    /// - `false` — always HTTP/1.1, skip the auto-probe
    /// - absent  — auto-probe: try h2c on first connection, fall back if refused
    ///
    /// Required before the h2_* window settings below have any effect.
    #[serde(default)]
    pub h2c: Option<bool>,

    /// HTTP/2 adaptive flow-control window (BDP estimator) (v0.9.90+)
    ///
    /// Sets `S3DLIO_H2_ADAPTIVE_WINDOW`.  Only active when `h2c: true`.
    ///
    /// When `true` (default), hyper sends H2 PING frames, measures RTT, computes
    /// `BDP = throughput × RTT`, and issues WINDOW_UPDATE to keep the window ≥ BDP.
    /// This self-tunes from 64 KB up to hundreds of MiB with no manual input.
    ///
    /// **When `true`, the two static window fields below are ignored entirely.**
    ///
    /// Set to `false` for reproducible benchmarks where you want fixed window sizes.
    #[serde(default)]
    pub h2_adaptive_window: Option<bool>,

    /// HTTP/2 per-stream flow-control window in MiB (v0.9.90+)
    ///
    /// Sets `S3DLIO_H2_STREAM_WINDOW_MB`.  Static mode only (`h2_adaptive_window: false`).
    ///
    /// Controls how much response data the server may send on a single H2 stream before
    /// we must issue WINDOW_UPDATE.  The H2 spec default is 64 KB — far too small for
    /// high-throughput storage I/O.  Clamped to 256 MiB maximum.
    ///
    /// When absent, defaults to the s3dlio default (4 MiB).
    #[serde(default)]
    pub h2_stream_window_mb: Option<u32>,

    /// HTTP/2 connection-level flow-control window in MiB (v0.9.90+)
    ///
    /// Sets `S3DLIO_H2_CONN_WINDOW_MB`.  Static mode only (`h2_adaptive_window: false`).
    ///
    /// Aggregate receive budget across all concurrent H2 streams on one TCP connection.
    /// Must be ≥ `h2_stream_window_mb` or the connection becomes the bottleneck.
    /// Defaults to 4× `h2_stream_window_mb` when absent.  Clamped to 256 MiB maximum.
    #[serde(default)]
    pub h2_conn_window_mb: Option<u32>,

    // ── Runtime thread counts ─────────────────────────────────────────────────
    /// Thread count for the s3dlio global async runtime (v0.8.92)
    ///
    /// Sets `S3DLIO_RT_THREADS`.  The s3dlio library maintains an internal Tokio
    /// runtime separate from the sai3-bench agent runtime.  By default it caps at
    /// `min(num_cpus × 2, 32)`, which is far too low for high-concurrency S3 workloads.
    ///
    /// **Rule of thumb**: set this to at least your `concurrency` value.
    /// For a 384-concurrency workload across 16 endpoints, 384 is a safe minimum.
    ///
    /// If absent and `auto_threads` (below) is `true`, this is derived automatically
    /// from the `concurrency` field in the parent `Config` at apply time.
    #[serde(default)]
    pub s3dlio_rt_threads: Option<usize>,

    /// Thread count for the sai3-bench agent's own Tokio runtime (v0.8.92)
    ///
    /// Corresponds to the `--worker-threads` CLI flag on `sai3bench-agent`.
    /// **This field is informational only when sent via gRPC** — the agent Tokio
    /// runtime is already running by the time the YAML config arrives.  Set the
    /// `--worker-threads N` flag when starting the agent, or set the
    /// `TOKIO_WORKER_THREADS` environment variable on each agent host.
    ///
    /// When this field is set in the YAML and the agent has not already set a thread
    /// count, it is recorded in the perf-log header for observability.
    ///
    /// **Rule of thumb**: set to the same value as `s3dlio_rt_threads` (or
    /// `concurrency`).  Default Tokio behaviour is `num_cpus` which on most servers
    /// is 32–128 — usually sufficient for pure-async I/O but explicit is better.
    #[serde(default)]
    pub tokio_worker_threads: Option<usize>,

    /// Auto-derive `s3dlio_rt_threads` from the parent `concurrency` if not set (v0.8.92)
    ///
    /// Default: `true`.  When `true` and `s3dlio_rt_threads` is absent, the apply()
    /// method sets `S3DLIO_RT_THREADS = max(concurrency, current_s3dlio_default)`.
    /// Set to `false` to disable auto-derivation and rely solely on the explicit
    /// `s3dlio_rt_threads` field or the `S3DLIO_RT_THREADS` environment variable.
    #[serde(default = "default_auto_threads")]
    pub auto_threads: bool,
}

fn default_s3dlio_range_threshold_mb() -> u64 {
    64 // 64 MB - optimal balance per benchmarks (76% improvement)
}

fn default_auto_threads() -> bool {
    true
}

impl S3dlioOptimizationConfig {
    /// Apply this configuration by setting corresponding environment variables
    /// Called automatically when config is loaded
    pub fn apply(&self) {
        // Set range optimization enable flag
        if self.enable_range_downloads {
            std::env::set_var("S3DLIO_ENABLE_RANGE_OPTIMIZATION", "1");
            tracing::info!(
                "Enabled s3dlio range download optimization (S3DLIO_ENABLE_RANGE_OPTIMIZATION=1)"
            );
        } else {
            std::env::set_var("S3DLIO_ENABLE_RANGE_OPTIMIZATION", "0");
            tracing::debug!("s3dlio range download optimization disabled (default)");
        }

        // Set threshold
        std::env::set_var(
            "S3DLIO_RANGE_THRESHOLD_MB",
            self.range_threshold_mb.to_string(),
        );
        tracing::info!(
            "Set s3dlio range threshold: {} MB (S3DLIO_RANGE_THRESHOLD_MB={})",
            self.range_threshold_mb,
            self.range_threshold_mb
        );

        // Set optional concurrency
        if let Some(concurrency) = self.range_concurrency {
            std::env::set_var("S3DLIO_RANGE_CONCURRENCY", concurrency.to_string());
            tracing::info!(
                "Set s3dlio range concurrency: {} (S3DLIO_RANGE_CONCURRENCY={})",
                concurrency,
                concurrency
            );
        } else {
            tracing::debug!("Using s3dlio auto-calculated range concurrency");
        }

        // Set optional chunk size (convert MB to bytes)
        if let Some(chunk_mb) = self.chunk_size_mb {
            let chunk_bytes = chunk_mb * 1024 * 1024;
            std::env::set_var("S3DLIO_CHUNK_SIZE", chunk_bytes.to_string());
            tracing::info!(
                "Set s3dlio chunk size: {} MB ({} bytes) (S3DLIO_CHUNK_SIZE={})",
                chunk_mb,
                chunk_bytes,
                chunk_bytes
            );
        } else {
            tracing::debug!("Using s3dlio auto-calculated chunk size");
        }

        // GCS: subchannel count — only if explicitly set in YAML.
        // Smart default (channel count = concurrency) is applied later via
        // Config::apply_gcs_defaults(), which also has access to concurrency.
        if let Some(n) = self.gcs_channel_count {
            s3dlio::set_gcs_channel_count(n);
            tracing::info!(
                "Set GCS gRPC channel count: {} (via set_gcs_channel_count)",
                n
            );
        }

        // GCS: RAPID mode override
        if let Some(rapid) = self.gcs_rapid_mode {
            // Mirror YAML choice to env so inherited shell environment cannot
            // override explicit config intent (s3dlio resolves env first).
            std::env::set_var("S3DLIO_GCS_RAPID", if rapid { "true" } else { "false" });
            s3dlio::set_gcs_rapid_mode(Some(rapid));
            tracing::info!(
                "Set GCS RAPID mode: {} (via YAML -> S3DLIO_GCS_RAPID + set_gcs_rapid_mode)",
                rapid
            );
        } else {
            tracing::debug!("GCS RAPID mode: auto-detect per bucket (default)");
        }

        // GCS: write chunk size in bytes
        if let Some(bytes) = self.gcs_write_chunk_size_bytes {
            std::env::set_var("S3DLIO_GRPC_WRITE_CHUNK_SIZE", bytes.to_string());
            tracing::info!(
                "Set GCS write chunk size: {} bytes (S3DLIO_GRPC_WRITE_CHUNK_SIZE={})",
                bytes,
                bytes
            );
        }

        // GCS: project ID — YAML takes precedence over environment variable
        if let Some(ref project) = self.gcs_project {
            std::env::set_var("GOOGLE_CLOUD_PROJECT", project);
            tracing::info!(
                "Set GCS project: {} (via YAML -> GOOGLE_CLOUD_PROJECT)",
                project
            );
        } else {
            tracing::debug!("GCS project: using GOOGLE_CLOUD_PROJECT env var fallback");
        }

        // ── HTTP/2 (h2c) settings ─────────────────────────────────────────────

        if let Some(h2c) = self.h2c {
            std::env::set_var("S3DLIO_H2C", if h2c { "1" } else { "0" });
            tracing::info!(
                "Set h2c mode: {} (S3DLIO_H2C={})",
                h2c,
                if h2c { "1" } else { "0" }
            );
        }

        if let Some(adaptive) = self.h2_adaptive_window {
            std::env::set_var(
                "S3DLIO_H2_ADAPTIVE_WINDOW",
                if adaptive { "1" } else { "0" },
            );
            tracing::info!(
                "Set H2 adaptive window: {} (S3DLIO_H2_ADAPTIVE_WINDOW={})",
                adaptive,
                if adaptive { "1" } else { "0" }
            );
        }

        if let Some(stream_mb) = self.h2_stream_window_mb {
            std::env::set_var("S3DLIO_H2_STREAM_WINDOW_MB", stream_mb.to_string());
            tracing::info!(
                "Set H2 stream window: {} MiB (S3DLIO_H2_STREAM_WINDOW_MB={})",
                stream_mb,
                stream_mb
            );
        }

        if let Some(conn_mb) = self.h2_conn_window_mb {
            std::env::set_var("S3DLIO_H2_CONN_WINDOW_MB", conn_mb.to_string());
            tracing::info!(
                "Set H2 connection window: {} MiB (S3DLIO_H2_CONN_WINDOW_MB={})",
                conn_mb,
                conn_mb
            );
        }
    }

    /// Apply s3dlio runtime thread count, optionally auto-deriving from concurrency.
    ///
    /// Call this **before** any s3dlio operation so the global runtime is
    /// initialised with the correct thread count.  The s3dlio global runtime is lazy
    /// (initialised on first use), so setting `S3DLIO_RT_THREADS` here is effective
    /// as long as no s3dlio call has been made yet.
    ///
    /// `concurrency` is the `Config.concurrency` value.
    pub fn apply_thread_counts(&self, concurrency: usize) {
        // Explicit YAML value takes precedence over auto-derivation and over the
        // existing environment variable (YAML config is intentional, env var may
        // be a stale leftover from a previous test session).
        if let Some(explicit) = self.s3dlio_rt_threads {
            std::env::set_var("S3DLIO_RT_THREADS", explicit.to_string());
            tracing::info!(
                "Set s3dlio runtime threads: {} (S3DLIO_RT_THREADS={}) [explicit YAML]",
                explicit,
                explicit
            );
            return;
        }

        // Auto-derive: if the env var is already set (e.g., by the operator on the
        // agent host), respect it.
        if std::env::var("S3DLIO_RT_THREADS").is_ok() {
            tracing::debug!(
                "S3DLIO_RT_THREADS already set in environment — skipping auto-derivation"
            );
            return;
        }

        if self.auto_threads && concurrency > 0 {
            // The s3dlio global RT default caps at min(num_cpus*2, 32).
            // For high-concurrency S3 workloads this is far too low.  Set it to
            // `concurrency` so there is at least one RT thread per in-flight task.
            // Add a small overhead factor (1.5×) to absorb connection management and
            // housekeeping tasks that run alongside the PUT/GET tasks.
            let derived = std::cmp::max(concurrency + concurrency / 2, 32);
            std::env::set_var("S3DLIO_RT_THREADS", derived.to_string());
            tracing::info!(
                "Auto-set s3dlio runtime threads: {} (concurrency={}) \
                 — set s3dlio_optimization.s3dlio_rt_threads or S3DLIO_RT_THREADS to override",
                derived,
                concurrency
            );
        }
    }
}

/// Auto-derive and apply S3DLIO_RT_THREADS from concurrency when no explicit config is available.
///
/// Convenience helper for code paths where the YAML has no `s3dlio_optimization` block.
/// Equivalent to calling `S3dlioOptimizationConfig { auto_threads: true, ..zero }.apply_thread_counts(concurrency)`.
pub fn auto_apply_rt_threads(concurrency: usize) {
    if std::env::var("S3DLIO_RT_THREADS").is_ok() {
        return; // respect operator override
    }
    if concurrency > 0 {
        let derived = std::cmp::max(concurrency + concurrency / 2, 32);
        std::env::set_var("S3DLIO_RT_THREADS", derived.to_string());
        tracing::info!(
            "Auto-set s3dlio runtime threads: {} (concurrency={}) \
             — set s3dlio_optimization.s3dlio_rt_threads or S3DLIO_RT_THREADS to override",
            derived,
            concurrency
        );
    }
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
    /// Supports time units: "30s", "5m", "1h" (or plain seconds: 300)
    #[serde(
        default = "default_start_delay",
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
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

    /// gRPC keep-alive interval in seconds (default: 30)
    /// How often PING frames are sent to detect dead connections
    /// For very slow operations (>30s), increase to 60+ to avoid premature disconnects
    /// Supports time units: "30s", "5m", "1h" (or plain seconds: 300)
    #[serde(
        default = "default_grpc_keepalive_interval",
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
    pub grpc_keepalive_interval: u64,

    /// gRPC keep-alive timeout in seconds (default: 10)
    /// How long to wait for PONG response before declaring connection dead
    /// Supports time units: "10s", "30s", "1m" (or plain seconds: 60)
    #[serde(
        default = "default_grpc_keepalive_timeout",
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
    pub grpc_keepalive_timeout: u64,

    /// v0.8.51: Timeout for agents to send initial READY signal after receiving config (default: 120)
    /// Agents perform config validation (including glob pattern expansion) before sending READY.
    /// Large directory structures (>100K files) require longer timeouts.
    /// Recommendations: Small (<10K files): 60s, Medium (10K-100K): 120s, Large (>100K): 300-600s
    /// Supports time units: "2m", "5m", "10m" (or plain seconds: 300)
    #[serde(
        default = "default_agent_ready_timeout",
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
    pub agent_ready_timeout: u64,

    /// v0.8.25: Barrier synchronization for phase coordination
    /// Disabled by default for backward compatibility
    #[serde(default)]
    pub barrier_sync: BarrierSyncConfig,

    /// v0.8.61: YAML-driven stage orchestration (REQUIRED for distributed mode)
    /// Explicit stage ordering with flexible completion criteria
    /// Stages MUST be defined when using distributed testing
    /// Stages are sorted by 'order' field, not YAML position
    /// Legacy auto-generation removed in v0.8.61 - stages are mandatory
    /// Note: default allows parsing old configs for conversion
    #[serde(default)]
    pub stages: Vec<StageConfig>,

    /// v0.8.60: Default KV cache directory for metadata caching (optional)
    /// Used to isolate LSM operations (journals, compaction, version files) from test storage
    /// Default: System temp dir (/tmp on Linux, %TEMP% on Windows)
    /// This prevents random small-block I/O from contaminating workload measurements
    /// Example: "/mnt/local-ssd/kv-cache" for fast local storage
    /// Per-agent overrides: Use agents[].kv_cache_dir to specify different paths per agent
    #[serde(default)]
    pub kv_cache_dir: Option<std::path::PathBuf>,
}

/// Individual agent configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AgentConfig {
    /// Agent address (host:port) or hostname (for SSH deployment)
    /// Examples: "node1.example.com:7167", "10.0.1.50:7167", "node1" (SSH mode)
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

    /// Listen port for agent (SSH mode only, default: 7167)
    #[serde(default = "default_agent_port")]
    pub listen_port: u16,

    /// Per-agent multi-endpoint override (v0.8.22+)
    /// If specified, this agent uses these endpoints instead of global multi_endpoint config
    /// Enables static endpoint mapping: assign specific endpoints to specific agents
    /// Example: Agent 1 -> [IP1, IP2], Agent 2 -> [IP3, IP4], etc.
    #[serde(default)]
    pub multi_endpoint: Option<MultiEndpointConfig>,

    /// Per-agent KV cache directory override (v0.8.60+)
    /// Overrides distributed.kv_cache_dir for this specific agent
    /// Useful for heterogeneous storage: fast local SSDs on some nodes, slower on others
    /// Example: "/mnt/agent1-local-ssd/kv-cache"
    #[serde(default)]
    pub kv_cache_dir: Option<std::path::PathBuf>,
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
    /// Supports time units: "10s", "30s", "1m" (or plain seconds: 60)
    #[serde(
        default = "default_ssh_timeout",
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
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
    7167
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

fn default_grpc_keepalive_interval() -> u64 {
    30 // 30 seconds - send PING every 30s
}

fn default_grpc_keepalive_timeout() -> u64 {
    10 // 10 seconds - wait for PONG
}

fn default_agent_ready_timeout() -> u64 {
    120 // 120 seconds (2 minutes) - increased from 30s in v0.8.51 for large-scale deployments
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
                    .map(|n| {
                        if n == 0 {
                            IopsTarget::Max
                        } else {
                            IopsTarget::Fixed(n)
                        }
                    })
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
    #[serde(
        default = "default_replay_recovery_threshold",
        with = "humantime_serde"
    )]
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

// ============================================================================
// v0.8.25: Barrier Synchronization Configuration
// ============================================================================

/// Barrier type determines strictness of synchronization
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarrierType {
    /// All agents must reach barrier (hard requirement)
    /// Missing agents cause entire workload to abort
    AllOrNothing,

    /// Majority (>50%) must reach barrier
    /// Stragglers are marked failed and excluded from next phase
    Majority,

    /// Best effort - proceed when liveness check fails on stragglers
    /// Stragglers continue independently (out of sync OK)
    BestEffort,
}

/// Per-phase barrier configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseBarrierConfig {
    /// Barrier type for this phase
    #[serde(rename = "type")]
    pub barrier_type: BarrierType,

    /// How often agents report progress (default: 30s)
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u64,

    /// How many missed heartbeats before query (default: 3 = 90s with 30s interval)
    #[serde(default = "default_missed_threshold")]
    pub missed_threshold: u32,

    /// Timeout for explicit agent query (default: 10s)
    /// Supports time units: "10s", "30s", "1m" (or plain seconds: 60)
    #[serde(
        default = "default_query_timeout",
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
    pub query_timeout: u64,

    /// Retries for agent query (default: 2)
    #[serde(default = "default_query_retries")]
    pub query_retries: u32,

    /// Agent-side barrier wait timeout (default: 120s)
    /// How long each agent waits for controller to release barrier
    /// Must be > (heartbeat_interval * missed_threshold + query_timeout * query_retries)
    /// For very large scales (300k+ dirs), use 600s+
    /// Supports time units: "2m", "5m", "10m" (or plain seconds: 600)
    #[serde(
        default = "default_agent_barrier_timeout",
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
    pub agent_barrier_timeout: u64,
}

/// Completion criteria for stage execution (how stage knows it's done)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum CompletionCriteria {
    /// Complete after specified duration (time-based)
    /// Used by: execute stages, custom stages with fixed runtime
    #[default]
    Duration,

    /// Complete when all tasks finished (task-based)
    /// Used by: prepare (objects created), cleanup (objects deleted)
    TasksDone,

    /// Complete when script exits (script-based)
    /// Used by: custom stages running external tools
    ScriptExit,

    /// Complete when validation checks pass (validation-based)
    /// Used by: preflight/validation stages
    ValidationPassed,

    /// Complete when either duration OR tasks finish (hybrid)
    /// Whichever criterion met first
    /// Used by: stages with both time limit and task count
    DurationOrTasks,
}

/// Stage-specific configuration based on stage type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StageSpecificConfig {
    /// Execute stage configuration (read/write workload)
    Execute {
        /// Duration to run workload (required for execute stages)
        #[serde(with = "humantime_serde")]
        duration: std::time::Duration,
    },

    /// Prepare stage configuration (object creation)
    Prepare {
        /// Expected number of objects to create (optional, for progress tracking)
        #[serde(default)]
        expected_objects: Option<usize>,
    },

    /// Cleanup stage configuration (object deletion)
    Cleanup {
        /// Expected number of objects to delete (optional, for progress tracking)
        #[serde(default)]
        expected_objects: Option<usize>,
    },

    /// Custom stage configuration (user-defined script/command)
    Custom {
        /// Command to execute
        command: String,

        /// Optional arguments to command
        #[serde(default)]
        args: Vec<String>,
    },

    /// Hybrid stage with both duration and task limits
    Hybrid {
        /// Maximum duration (optional)
        #[serde(default, with = "humantime_serde_opt")]
        max_duration: Option<std::time::Duration>,

        /// Expected number of tasks (optional)
        #[serde(default)]
        expected_tasks: Option<usize>,
    },

    /// Validation stage (pre-flight checks, no workload execution)
    /// Completes immediately - validation happens at RPC level before stages run
    /// Use stage.timeout_secs for timeout configuration (not nested here to avoid duplication)
    Validation,
}

/// Stage configuration for YAML-driven stage orchestration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageConfig {
    /// Stage name (e.g., "preflight", "prepare", "epoch-1", "cleanup")
    pub name: String,

    /// Explicit execution order (1, 2, 3, etc.)
    /// Stages sorted by this field, not YAML position
    pub order: usize,

    /// Completion criteria (how to determine stage is done)
    #[serde(default)]
    pub completion: CompletionCriteria,

    /// Barrier synchronization for this stage (optional)
    /// If not specified, uses barrier_sync.enabled from DistributedConfig
    #[serde(default)]
    pub barrier: Option<PhaseBarrierConfig>,

    /// Maximum time to wait for stage completion before timeout (seconds)
    /// Default: no timeout (wait indefinitely)
    #[serde(default)]
    pub timeout_secs: Option<u64>,

    /// Whether this stage is optional (can be skipped on failure)
    /// Default: false (all stages required)
    #[serde(default)]
    pub optional: bool,

    /// Stage-specific configuration (flattened into struct)
    #[serde(flatten)]
    pub config: StageSpecificConfig,
}

/// Top-level barrier synchronization configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BarrierSyncConfig {
    /// Enable barrier synchronization (default: false for backward compat)
    #[serde(default)]
    pub enabled: bool,

    /// Default heartbeat settings (apply to all phases unless overridden)
    /// Supports time units: "30s", "1m", "5m" (or plain seconds: 300)
    #[serde(
        default = "default_heartbeat_interval",
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
    pub default_heartbeat_interval: u64,

    #[serde(default = "default_missed_threshold")]
    pub default_missed_threshold: u32,

    /// Supports time units: "10s", "30s", "1m" (or plain seconds: 60)
    #[serde(
        default = "default_query_timeout",
        deserialize_with = "crate::serde_helpers::deserialize_duration_seconds"
    )]
    pub default_query_timeout: u64,

    #[serde(default = "default_query_retries")]
    pub default_query_retries: u32,

    /// Per-phase configuration
    #[serde(default)]
    pub validation: Option<PhaseBarrierConfig>,

    #[serde(default)]
    pub prepare: Option<PhaseBarrierConfig>,

    #[serde(default)]
    pub execute: Option<PhaseBarrierConfig>,

    #[serde(default)]
    pub cleanup: Option<PhaseBarrierConfig>,
}

// Default values for barrier configuration
fn default_heartbeat_interval() -> u64 {
    30
}
fn default_missed_threshold() -> u32 {
    3
}
fn default_query_timeout() -> u64 {
    10
}
fn default_query_retries() -> u32 {
    2
}
fn default_agent_barrier_timeout() -> u64 {
    120
} // 2 minutes default (was hardcoded 30s)

impl Default for BarrierSyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_heartbeat_interval: default_heartbeat_interval(),
            default_missed_threshold: default_missed_threshold(),
            default_query_timeout: default_query_timeout(),
            default_query_retries: default_query_retries(),
            validation: None,
            prepare: None,
            execute: None,
            cleanup: None,
        }
    }
}

impl BarrierSyncConfig {
    /// Get effective configuration for a phase (uses defaults if not overridden)
    pub fn get_phase_config(&self, phase: &str) -> PhaseBarrierConfig {
        let config = match phase {
            "validation" => self.validation.as_ref(),
            "prepare" => self.prepare.as_ref(),
            "execute" => self.execute.as_ref(),
            "cleanup" => self.cleanup.as_ref(),
            _ => None,
        };

        config.cloned().unwrap_or_else(|| PhaseBarrierConfig {
            barrier_type: BarrierType::AllOrNothing,
            heartbeat_interval: self.default_heartbeat_interval,
            missed_threshold: self.default_missed_threshold,
            query_timeout: self.default_query_timeout,
            query_retries: self.default_query_retries,
            agent_barrier_timeout: default_agent_barrier_timeout(),
        })
    }
}

// ============================================================================
// End Barrier Synchronization Configuration
// ============================================================================

impl DistributedConfig {
    /// Get sorted stages (by order field), generating defaults if not specified
    ///
    /// Get sorted stages from configuration (stages are now MANDATORY)
    pub fn get_sorted_stages(&self) -> Result<Vec<StageConfig>, String> {
        let stages = &self.stages;

        // Validate user-provided stages
        self.validate_stages(stages)?;

        // Sort by order field
        let mut sorted = stages.clone();
        sorted.sort_by_key(|s| s.order);

        // Apply barrier defaults from barrier_sync when stages omit per-stage config
        if self.barrier_sync.enabled {
            for stage in sorted.iter_mut() {
                if stage.barrier.is_none() {
                    let phase_name = match stage.config {
                        StageSpecificConfig::Validation => "validation",
                        StageSpecificConfig::Prepare { .. } => "prepare",
                        StageSpecificConfig::Cleanup { .. } => "cleanup",
                        StageSpecificConfig::Execute { .. } => "execute",
                        StageSpecificConfig::Custom { .. } | StageSpecificConfig::Hybrid { .. } => {
                            "execute"
                        }
                    };
                    stage.barrier = Some(self.barrier_sync.get_phase_config(phase_name));
                }
            }
        }

        Ok(sorted)
    }

    /// Validate stage configuration
    fn validate_stages(&self, stages: &[StageConfig]) -> Result<(), String> {
        if stages.is_empty() {
            return Err("stages list cannot be empty".to_string());
        }

        // Check for unique stage names
        let mut names = std::collections::HashSet::new();
        for stage in stages {
            if !names.insert(&stage.name) {
                return Err(format!("duplicate stage name: {}", stage.name));
            }
        }

        // Check for unique order values (no duplicates)
        let mut orders = std::collections::HashSet::new();
        for stage in stages {
            if !orders.insert(stage.order) {
                return Err(format!("duplicate stage order: {}", stage.order));
            }
        }

        // Warn about gaps in ordering (not an error, but suspicious)
        let mut sorted_orders: Vec<_> = orders.iter().copied().collect();
        sorted_orders.sort();
        for window in sorted_orders.windows(2) {
            if window[1] - window[0] > 1 {
                eprintln!(
                    "Warning: gap in stage ordering: {} -> {} (missing {})",
                    window[0],
                    window[1],
                    window[0] + 1
                );
            }
        }

        // Validate completion criteria matches stage type
        for stage in stages {
            match &stage.config {
                StageSpecificConfig::Execute { duration: _ } => {
                    if !matches!(
                        stage.completion,
                        CompletionCriteria::Duration | CompletionCriteria::DurationOrTasks
                    ) {
                        return Err(format!(
                            "Execute stage '{}' should use Duration or DurationOrTasks completion",
                            stage.name
                        ));
                    }
                }
                StageSpecificConfig::Prepare { .. } | StageSpecificConfig::Cleanup { .. } => {
                    if !matches!(
                        stage.completion,
                        CompletionCriteria::TasksDone | CompletionCriteria::DurationOrTasks
                    ) {
                        return Err(format!("Prepare/Cleanup stage '{}' should use TasksDone or DurationOrTasks completion", stage.name));
                    }
                }
                StageSpecificConfig::Custom { .. } => {
                    if !matches!(
                        stage.completion,
                        CompletionCriteria::ScriptExit | CompletionCriteria::DurationOrTasks
                    ) {
                        return Err(format!(
                            "Custom stage '{}' should use ScriptExit or DurationOrTasks completion",
                            stage.name
                        ));
                    }
                }
                StageSpecificConfig::Hybrid { .. } => {
                    if !matches!(stage.completion, CompletionCriteria::DurationOrTasks) {
                        return Err(format!(
                            "Hybrid stage '{}' must use DurationOrTasks completion",
                            stage.name
                        ));
                    }
                }
                StageSpecificConfig::Validation => {
                    if !matches!(stage.completion, CompletionCriteria::ValidationPassed) {
                        return Err(format!(
                            "Validation stage '{}' should use ValidationPassed completion",
                            stage.name
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

/// Multi-process scaling configuration (v0.7.3+)
/// Controls how many processes to spawn per endpoint for parallel execution
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
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
                    _ => match value.parse::<usize>() {
                        Ok(1) => Ok(ProcessScaling::Single),
                        Ok(n) if n > 1 => Ok(ProcessScaling::Manual(n)),
                        Ok(_) => Err(E::custom("processes must be >= 1")),
                        Err(_) => Err(E::custom(format!(
                            "invalid process scaling value: {}",
                            value
                        ))),
                    },
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
