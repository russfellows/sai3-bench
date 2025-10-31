// src/config.rs
use serde::{Deserialize, Serialize};
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
    
    /// Optional directory tree structure (rdf-bench style width/depth model)
    /// When specified, creates deterministic hierarchical directory structure before workload
    #[serde(default)]
    pub directory_structure: Option<crate::directory_tree::DirectoryStructureConfig>,
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
    FillPattern::Random 
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

    /// Get the resolved URI for a metadata operation (List, Stat, Delete, Mkdir, Rmdir)
    pub fn get_meta_uri(&self, meta_op: &OpSpec) -> String {
        match meta_op {
            OpSpec::List { path } 
            | OpSpec::Stat { path } 
            | OpSpec::Delete { path }
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
#[derive(Debug, Deserialize, Clone)]
pub struct RangeEngineConfig {
    /// Enable or disable RangeEngine
    /// Default: false (disabled) - avoids HEAD overhead on typical workloads
    #[serde(default = "default_range_engine_enabled")]
    pub enabled: bool,
    
    /// Size of each concurrent range request in bytes
    /// Default: 67108864 (64 MiB)
    /// - Larger chunks: Fewer requests, less overhead, but less parallelism
    /// - Smaller chunks: More parallelism, but more overhead
    /// Recommended: 32-128 MiB depending on network speed
    #[serde(default = "default_chunk_size")]
    pub chunk_size: u64,
    
    /// Maximum number of concurrent range requests
    /// Default: 16
    /// - Higher values: More parallelism for high-bandwidth networks (>1 Gbps)
    /// - Lower values: Reduce load on slow networks (<100 Mbps)
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
    64 * 1024 * 1024  // 64 MB
}

fn default_max_concurrent() -> usize {
    16  // Safe default for concurrent range requests
}

fn default_min_split_size() -> u64 {
    16 * 1024 * 1024  // 16 MiB - matches s3dlio library default
}

fn default_range_timeout_secs() -> u64 {
    30
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
#[derive(Debug, Deserialize, Clone, Copy)]
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
    
    /// Whether target filesystem is shared across agents (REQUIRED - no default)
    /// Must be explicitly set to true or false
    /// - true: Shared filesystem (NFS, Lustre, S3, GCS, Azure) - agents may access same paths
    /// - false: Local per-agent storage - each agent has independent namespace
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
    0.3  // 30% overlap
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
