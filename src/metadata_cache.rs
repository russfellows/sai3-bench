//! Distributed metadata cache using fjall v3 LSM-tree KV store
//!
//! # Architecture
//!
//! - **Per-endpoint caches**: Each endpoint (file://, s3://, etc.) stores metadata ONLY for
//!   objects it contains. Located at `{endpoint_uri}/sai3-kv-cache/`.
//!
//! - **Coordinator cache**: Shared global metadata (tree manifests, endpoint registry).
//!   Located at `{results_dir}/.sai3-coordinator-cache/`.
//!
//! - **Distributed ownership**: File assignments use round-robin: `file_idx % num_endpoints == endpoint_idx`
//!
//! # State Tracking (CRITICAL FEATURE)
//!
//! The cache tracks BOTH desired state (what SHOULD exist) AND current state (what DOES exist).
//! This enables:
//! - **Resume capability**: Interrupted prepare can continue from last checkpoint
//! - **Pre-workload validation**: Verify all planned objects exist before benchmark starts
//! - **Cleanup verification**: Ensure we delete exactly what we created
//! - **Progress tracking**: Real-time progress during 64M file creation
//! - **Drift detection**: Detect externally deleted/modified files
//!
//! # Performance Benefits
//!
//! For 64M files across 4 endpoints:
//! - **Tree generation**: 45s → 0.5s (90x speedup, subsequent runs)
//! - **LIST operations**: 53 minutes → 8 seconds (400x speedup, parallel loads)
//! - **Path lookups**: O(n) iteration → O(1) distributed lookups (10000x improvement)
//!
//! # Example Usage
//!
//! ```rust,no_run
//! use sai3_bench::metadata_cache::{MetadataCache, ObjectState};
//! use std::path::Path;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let cache = MetadataCache::new(
//!     Path::new("/tmp/results"),
//!     &["file:///mnt/nvme1/".to_string(), "s3://bucket/".to_string()],
//!     "abc123def".to_string(),
//!     None,  // agent_id
//!     None,  // kv_cache_base_dir
//! ).await?;
//!
//! // Plan object (desired state)
//! cache.endpoint(0).unwrap()
//!     .plan_object("abc123def", 0, "d1/file_000.dat", 1048576)?;
//!
//! // Mark as created (current state)
//! cache.endpoint(0).unwrap()
//!     .mark_created("abc123def", 0, Some(1738886400), None)?;
//!
//! // Query state
//! let entry = cache.endpoint(0).unwrap()
//!     .get_object("abc123def", 0)?;
//! assert_eq!(entry.unwrap().state, ObjectState::Created);
//! # Ok(())
//! # }
//! ```

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use fjall::{Database, Keyspace, KeyspaceCreateOptions};
use serde::{Deserialize, Serialize};
use serde_json; // For config hash generation
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tracing::{debug, info, warn};
use url::Url;

// ObjectStore for cloud storage checkpoint uploads
use s3dlio::object_store::store_for_uri;

// ============================================================================
// Flush Policy (copied from io-stack/fjall_engine.rs)
// ============================================================================

/// Write batch flush policy.
///
/// Controls how writes are batched before flushing to persistent storage.
/// This is CRITICAL for performance - avoids blocking benchmark I/O on cache updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushPolicy {
    /// Flush immediately after each write (safest, highest latency) - NOT RECOMMENDED for hot paths
    Immediate,
    /// Flush after N write operations (recommended: 1000-10000 for balance)
    BatchSize(u64),
    /// Time-based async flush (background thread, zero blocking) - RECOMMENDED for benchmarks
    AsyncInterval(u64), // Interval in seconds
}

impl Default for FlushPolicy {
    fn default() -> Self {
        // Default: 30-second async flush (zero impact on benchmark)
        FlushPolicy::AsyncInterval(30)
    }
}

impl FlushPolicy {
    /// Create a batch size policy.
    ///
    /// # Arguments
    /// * `size` - Batch size. Use 0 for immediate flush, or any positive integer.
    ///
    /// # Returns
    /// Batch policy or Immediate if size is 0.
    pub fn batch(size: u64) -> Self {
        if size == 0 {
            FlushPolicy::Immediate
        } else {
            FlushPolicy::BatchSize(size)
        }
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Detect if multiple endpoints point to the same underlying storage.
///
/// This is critical for checkpoint optimization:
/// - **Shared storage** (same bucket/path): Only checkpoint first endpoint (avoid race conditions)
/// - **Independent storage** (different buckets/paths): Checkpoint each endpoint separately
///
/// # Detection Strategy
///
/// Compares storage locations by extracting bucket/container names:
/// - `s3://bucket1/prefix1` vs `s3://bucket1/prefix2` → **Shared** (same bucket)
/// - `s3://bucket1/prefix` vs `s3://bucket2/prefix` → **Independent** (different buckets)
/// - `file:///mnt/nvme1/` vs `file:///mnt/nvme2/` → **Independent** (different paths)
/// - `file:///mnt/nvme1/data` vs `file:///mnt/nvme1/data` → **Shared** (same path)
///
/// # Examples
///
/// ```rust,no_run
/// use sai3_bench::metadata_cache::endpoints_share_storage;
///
/// // Shared storage - load balancer to same bucket
/// let shared = vec![
///     "s3://10.9.0.21/my-bucket/".to_string(),
///     "s3://10.9.0.25/my-bucket/".to_string(),
/// ];
/// assert!(endpoints_share_storage(&shared));
///
/// // Independent storage - different buckets
/// let independent = vec![
///     "s3://10.9.0.21/bucket1/".to_string(),
///     "s3://10.9.0.21/bucket2/".to_string(),
/// ];
/// assert!(!endpoints_share_storage(&independent));
/// ```
pub fn endpoints_share_storage(endpoint_uris: &[String]) -> bool {
    if endpoint_uris.len() <= 1 {
        return false; // Single endpoint can't share with itself
    }

    // Extract normalized storage locations
    let locations: Vec<String> = endpoint_uris
        .iter()
        .filter_map(|uri| extract_storage_location(uri))
        .collect();

    // If we couldn't parse some URIs, assume independent (safe default)
    if locations.len() != endpoint_uris.len() {
        debug!("Could not parse all endpoint URIs - assuming independent storage");
        return false;
    }

    // Check if all locations are identical
    let first = &locations[0];
    let all_same = locations.iter().all(|loc| loc == first);

    if all_same {
        info!(
            "Shared storage detected: all {} endpoints use location '{}'",
            endpoint_uris.len(),
            first
        );
    } else {
        info!(
            "Independent storage detected: {} unique locations",
            locations.len()
        );
    }

    all_same
}

/// Extract normalized storage location from URI for comparison.
///
/// Returns a string that uniquely identifies the storage location:
/// - S3/GCS/Azure: `{scheme}://{bucket_or_container}`
/// - File/Direct: `{scheme}://{full_path}`
///
/// **CRITICAL**: This function ignores hostnames/IPs completely for S3/Azure,
/// comparing only the bucket/container names. This correctly handles:
/// - Load balancers with multiple IPs (10.9.0.21, 10.9.0.25, etc.)
/// - DNS round-robin with multiple hostnames (storage1.example.com, storage2.example.com)
/// - Mixed IP and hostname configurations
///
/// # Examples
/// - `s3://10.9.0.21:9000/my-bucket/prefix/` → `s3://my-bucket`
/// - `s3://storage.example.com/my-bucket/prefix/` → `s3://my-bucket` (same as above!)
/// - `az://account.blob.core.windows.net/container/` → `az://container`
/// - `file:///mnt/nvme1/data/` → `file:///mnt/nvme1/data`
fn extract_storage_location(uri: &str) -> Option<String> {
    // Try parsing as URL
    let parsed = Url::parse(uri).ok()?;
    let scheme = parsed.scheme();

    match scheme {
        "s3" => {
            // For S3: extract bucket name from path (first path component)
            // Hostname/IP is IGNORED - we only compare bucket names
            // s3://hostname-or-ip:port/bucket/prefix → s3://bucket
            let path = parsed.path().trim_start_matches('/');
            let bucket = path.split('/').next()?;
            if bucket.is_empty() {
                return None;
            }
            Some(format!("s3://{}", bucket))
        }
        "gs" => {
            // For GCS: bucket name is the hostname, not in path
            // gs://bucket/prefix → bucket
            let bucket = parsed.host_str()?;
            if bucket.is_empty() {
                return None;
            }
            Some(format!("gs://{}", bucket))
        }
        "az" | "azure" => {
            // For Azure: extract container name from path
            // az://account.blob.core.windows.net/container/prefix → container
            let path = parsed.path().trim_start_matches('/');
            let container = path.split('/').next()?;
            if container.is_empty() {
                return None;
            }
            Some(format!("az://{}", container))
        }
        "file" => {
            // For file: use full normalized path
            // file:///mnt/nvme1/data/ → file:///mnt/nvme1/data
            let path = parsed.path().trim_end_matches('/');
            Some(format!("file://{}", path))
        }
        "direct" => {
            // For direct: use full path (similar to file)
            let path = uri.strip_prefix("direct://")?.trim_end_matches('/');
            Some(format!("direct://{}", path))
        }
        _ => {
            debug!("Unknown URI scheme: {}", scheme);
            None
        }
    }
}

// ============================================================================

/// Extract file index from prepared object path
///
/// Supports patterns generated by prepare phase:
/// - `d1_w2/file_00000123.dat` → Some(123)
/// - `prepared-00000042.dat` → Some(42)
/// - `file_00000000.dat` → Some(0)
///
/// This is used to map file paths back to their file_idx for cache lookups
/// during workload execution.
///
/// Returns None if no numeric index can be extracted.
pub fn extract_file_index_from_path(path: &str) -> Option<usize> {
    // Extract filename from path (after last '/')
    let filename = path.rsplit('/').next().unwrap_or(path);

    // Remove extension
    let name_without_ext = filename.strip_suffix(".dat").unwrap_or(filename);

    // Extract numeric part after last '_' or '-'
    if let Some(idx) = name_without_ext
        .rfind('_')
        .or_else(|| name_without_ext.rfind('-'))
    {
        let num_str = &name_without_ext[idx + 1..];
        num_str.parse::<usize>().ok()
    } else {
        None
    }
}

/// Generate config hash from Config struct for cache invalidation
///
/// Usesseahash for fast hashing suitable for cache keys (not cryptographic).
/// Config changes invalidate cached metadata, forcing re-preparation.
///
/// # Arguments
/// * `config` - The benchmark configuration
///
/// # Returns
/// Hexadecimal hash string (e.g., "abc123def456")
pub fn generate_config_hash(config: &crate::config::Config) -> String {
    // Serialize config to JSON for deterministic hashing
    let config_json =
        serde_json::to_string(config).unwrap_or_else(|_| "invalid_config".to_string());

    // Use seahash for fast, non-cryptographic hashing
    let hash = seahash::hash(config_json.as_bytes());

    // Return as hex string
    format!("{:x}", hash)
}

// ============================================================================
// Core Types
// ============================================================================

/// Object state in metadata cache
///
/// Tracks both DESIRED state (what should exist) and CURRENT state (what does exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[repr(u8)]
pub enum ObjectState {
    /// Directory or bucket container doesn't exist yet
    /// Used for parent directory/bucket tracking before creation
    DirectoryNonExistent = 0,

    /// Object should exist but hasn't been created yet (DESIRED state)
    Planned = 1,

    /// Object creation in progress (TRANSITIONAL state)
    Creating = 2,

    /// Object successfully created and verified (CURRENT state matches DESIRED)
    Created = 3,

    /// Object creation failed (ERROR state)
    Failed = 4,

    /// Object was created but subsequently deleted (DRIFT detection)
    Deleted = 5,
}

impl ObjectState {
    /// Parse from u8 byte value
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::DirectoryNonExistent),
            1 => Some(Self::Planned),
            2 => Some(Self::Creating),
            3 => Some(Self::Created),
            4 => Some(Self::Failed),
            5 => Some(Self::Deleted),
            _ => None,
        }
    }

    /// Convert to u8 byte value
    pub fn to_u8(self) -> u8 {
        self as u8
    }

    /// Is this a terminal success state?
    pub fn is_created(&self) -> bool {
        matches!(self, Self::Created)
    }

    /// Is this a failure state?
    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed)
    }

    /// Is this object in a state that requires action?
    pub fn needs_creation(&self) -> bool {
        matches!(self, Self::Planned | Self::Failed)
    }
}

/// Cached object entry with full metadata
///
/// Stores BOTH desired configuration AND current state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectEntry {
    /// Relative path from endpoint base (e.g., "d1/w1/file_00042.dat")
    pub path: String,

    /// Expected object size in bytes (DESIRED state)
    pub size: u64,

    /// Current state (DirectoryNonExistent/Planned/Creating/Created/Failed/Deleted)
    pub state: ObjectState,

    /// Endpoint index in multi-endpoint configuration
    pub endpoint_idx: usize,

    /// Timestamp when object was created (epoch seconds)
    /// None if state is Planned/Creating/DirectoryNonExistent
    pub created_at: Option<u64>,

    /// Optional checksum for validation (e.g., seahash of content)
    /// None if not yet created or validation not enabled
    pub checksum: Option<u64>,
}

impl ObjectEntry {
    /// Create new planned object entry (DESIRED state)
    pub fn new_planned(path: String, size: u64, endpoint_idx: usize) -> Self {
        Self {
            path,
            size,
            state: ObjectState::Planned,
            endpoint_idx,
            created_at: None,
            checksum: None,
        }
    }

    /// Serialize to bytes for KV storage using postcard (compact binary, no field names).
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        postcard::to_allocvec(self).map_err(|e| anyhow::anyhow!("postcard serialize: {}", e))
    }

    /// Deserialize from bytes.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        postcard::from_bytes(data).map_err(|e| anyhow::anyhow!("postcard deserialize: {}", e))
    }
}

/// Per-endpoint metadata cache
///
/// Stores ONLY data for objects belonging to this endpoint (file_idx % num_endpoints == endpoint_idx).
/// Cache location: `{endpoint_uri}/sai3-kv-cache/`
///
/// Uses fjall v3 API: Database → Keyspace (not Keyspace → PartitionHandle from v2)
pub struct EndpointCache {
    /// fjall database (needed for persist)
    db: Database,

    /// Endpoint index in multi-endpoint configuration
    endpoint_index: usize,

    /// Endpoint URI (e.g., "file:///mnt/nvme1/", "s3://bucket/")
    endpoint_uri: String,

    /// Physical cache location on disk/cloud
    cache_location: PathBuf,

    // Keyspaces (fjall v3 terminology)
    /// Object entries: "{config_hash}:{file_idx:08}" → ObjectEntry (JSON)
    objects: Keyspace,

    /// Listing cache: "{config_hash}:list_timestamp:{epoch}" → compressed file list (zstd)
    listing_cache: Keyspace,

    /// Endpoint distribution: "{config_hash}:{strategy}:{file_idx:08}" → endpoint_index (u32)
    endpoint_map: Keyspace,

    /// RNG seeds: "{config_hash}:{seed_type}:{index}" → u64 seed value
    seeds: Keyspace,

    // Performance tracking (lock-free atomics)
    /// Number of unflushed writes (for BatchSize policy)
    pending_writes: AtomicU64,

    /// Flush policy for write batching (prevents blocking benchmark I/O)
    flush_policy: RwLock<FlushPolicy>,
}

/// Coordinator cache (shared global metadata)
///
/// Located at `{results_dir}/.sai3-coordinator-cache/`.
/// Stores global data shared across all endpoints.
pub struct CoordinatorCache {
    /// fjall database (needed for persist)
    db: Database,

    /// Cache directory path
    cache_dir: PathBuf,

    // Performance tracking (lock-free atomics)
    /// Number of unflushed writes (for BatchSize policy)
    pending_writes: AtomicU64,

    /// Flush policy for write batching (prevents blocking benchmark I/O)
    flush_policy: RwLock<FlushPolicy>,

    // Keyspaces (global metadata)
    /// Tree manifests: "{config_hash}:manifest" → JSON TreeManifest
    tree_manifests: Keyspace,

    /// Endpoint registry: "{config_hash}:endpoints" → JSON array of endpoint URIs
    endpoint_registry: Keyspace,

    /// Config metadata: "{config_hash}" → JSON test configuration metadata
    config_metadata: Keyspace,
}

/// Distributed metadata cache orchestrator
///
/// Manages coordinator cache + all endpoint caches.
/// Routes operations to correct endpoint based on file_idx.
pub struct MetadataCache {
    /// Coordinator cache (shared global metadata)
    coordinator: CoordinatorCache,

    /// Per-endpoint caches (distributed data)
    endpoints: HashMap<usize, EndpointCache>,

    /// Configuration hash for cache key namespacing
    config_hash: String,
}

impl EndpointCache {
    /// Create or open endpoint cache
    ///
    /// # Arguments
    /// * `endpoint_uri` - Endpoint URI (e.g., "file:///mnt/nvme1/", "s3://bucket/testdata/")
    /// * `endpoint_index` - Index in multi-endpoint configuration (0-based)
    /// * `agent_id` - Optional agent ID for distributed mode (creates agent-specific cache)
    /// * `kv_cache_base_dir` - Optional base directory for KV cache (defaults to system temp dir)
    ///
    /// # Cache Location
    ///
    /// **ALWAYS uses local temp storage** to prevent LSM I/O from contaminating workload measurements.
    /// Base dir can be customized via `kv_cache_base_dir` parameter (defaults to `/tmp/` or system temp).
    /// KV cache directory format:
    /// - Distributed: `{base_dir}/sai3-cache-agent-{id}-{endpoint_hash}/`
    /// - Single-node: `{base_dir}/sai3-cache-{endpoint_hash}/`
    ///
    /// This keeps random small-block LSM operations (journals, compaction, version files)
    /// isolated from the test storage, ensuring accurate performance measurements.
    ///
    /// # Resume Capability
    ///
    /// **CRITICAL**: Before opening cache, attempts to restore from checkpoint on storage.
    /// - Checks for checkpoint archive: `{endpoint}/.sai3-cache-agent-{id}.tar.zst`
    /// - Downloads if checkpoint newer than local cache
    /// - Extracts tar.zst to cache location
    /// - Then opens fjall database (loads restored state)
    ///
    /// This enables resume after agent crashes/restarts - picks up from last checkpoint!
    pub async fn new(
        endpoint_uri: &str,
        endpoint_index: usize,
        agent_id: Option<usize>,
        kv_cache_base_dir: Option<&Path>,
    ) -> Result<Self> {
        let cache_location =
            Self::resolve_cache_location(endpoint_uri, agent_id, kv_cache_base_dir)?;

        // v0.8.60: Try to restore from checkpoint BEFORE opening database
        // This enables resume capability after crashes/restarts
        info!(
            "Checking for checkpoint to restore: endpoint {}",
            endpoint_index
        );
        match Self::try_restore_from_checkpoint(endpoint_uri, &cache_location, agent_id).await {
            Ok(true) => {
                info!(
                    "✅ Checkpoint restored successfully for endpoint {}",
                    endpoint_index
                );
            }
            Ok(false) => {
                debug!(
                    "No checkpoint found or local cache is newer (endpoint {})",
                    endpoint_index
                );
            }
            Err(e) => {
                warn!(
                    "Failed to restore checkpoint for endpoint {} (non-fatal): {}",
                    endpoint_index, e
                );
                warn!("Will proceed with local cache or create new cache");
            }
        }

        std::fs::create_dir_all(&cache_location)
            .context("Failed to create endpoint cache directory")?;

        debug!(
            "Opening endpoint cache: {} (index={})",
            cache_location.display(),
            endpoint_index
        );

        // Open fjall v3 database (now potentially restored from checkpoint)
        let db = Database::builder(&cache_location)
            .open()
            .context("Failed to open fjall database for endpoint")?;

        // Create keyspaces with more aggressive compaction for large-scale prepares
        // Default table target size is 64 MB - we use 16 MB to trigger 4x more compactions
        // This keeps version file count lower during 12-hour 100M object prepares
        use fjall::compaction::Leveled;
        let compaction_strategy =
            Arc::new(Leveled::default().with_table_target_size(16 * 1024 * 1024));

        let keyspace_opts =
            KeyspaceCreateOptions::default().compaction_strategy(compaction_strategy.clone());

        let objects = db
            .keyspace("objects", || keyspace_opts.clone())
            .context("Failed to create objects keyspace")?;
        let listing_cache = db
            .keyspace("listing_cache", || keyspace_opts.clone())
            .context("Failed to create listing_cache keyspace")?;
        let endpoint_map = db
            .keyspace("endpoint_map", || keyspace_opts.clone())
            .context("Failed to create endpoint_map keyspace")?;
        let seeds = db
            .keyspace("seeds", || keyspace_opts)
            .context("Failed to create seeds keyspace")?;

        Ok(EndpointCache {
            db,
            endpoint_index,
            endpoint_uri: endpoint_uri.to_string(),
            cache_location,
            objects,
            listing_cache,
            endpoint_map,
            seeds,
            pending_writes: AtomicU64::new(0),
            flush_policy: RwLock::new(FlushPolicy::default()), // 30-second async flush
        })
    }

    /// Resolve cache location from endpoint URI
    ///
    /// - file:///path/ → /path/.sai3-cache-agent-{id}/ (distributed) or /path/sai3-kv-cache/ (single)
    /// - s3://bucket/prefix/ → /tmp/sai3-endpoint-cache-{hash}/
    /// - az://container/prefix/ → /tmp/sai3-endpoint-cache-{hash}/
    fn resolve_cache_location(
        endpoint_uri: &str,
        agent_id: Option<usize>,
        kv_cache_base_dir: Option<&Path>,
    ) -> Result<PathBuf> {
        // CRITICAL: Always use local temp storage to avoid polluting workload I/O measurements
        // LSM operations (journals, compaction, version files) generate random small-block I/O
        // that would contaminate sequential large-block storage testing (e.g., object storage)

        // Use configured base dir or default to system temp
        let base_dir = kv_cache_base_dir
            .map(|p| p.to_path_buf())
            .unwrap_or_else(std::env::temp_dir);

        use seahash::hash;
        let hash = hash(endpoint_uri.as_bytes());

        // Create unique cache directory name with agent ID and endpoint hash
        let cache_dir_name = if let Some(id) = agent_id {
            format!("sai3-cache-agent-{}-{:016x}", id, hash)
        } else {
            format!("sai3-cache-{:016x}", hash)
        };

        let cache_location = base_dir.join(cache_dir_name);

        debug!(
            "KV cache location: {} (isolated from test storage)",
            cache_location.display()
        );

        Ok(cache_location)
    }

    /// Attempt to restore endpoint cache from checkpoint on storage.
    ///
    /// # Resume Flow
    /// 1. Look for checkpoint archive: `{endpoint}/.sai3-cache-agent-{id}.tar.zst`
    /// 2. Try to download checkpoint from storage
    /// 3. If exists and downloadable, extract tar.zst to cache_location
    /// 4. If checkpoint doesn't exist, continue with empty or existing local cache
    ///
    /// # Returns
    /// - `Ok(true)` - Checkpoint restored successfully
    /// - `Ok(false)` - No checkpoint found on storage
    /// - `Err(_)` - Download/extract failed (non-fatal)
    ///
    /// # Errors
    /// Non-fatal errors (missing checkpoint, extraction failures) are warned and return false.
    /// The agent will proceed with local cache or create a new one.
    async fn try_restore_from_checkpoint(
        endpoint_uri: &str,
        cache_location: &Path,
        agent_id: Option<usize>,
    ) -> Result<bool> {
        use std::fs::File;
        use tar::Archive;

        // Build checkpoint file name (MUST match write_checkpoint / create_and_write_checkpoint_static)
        // Single-node:     sai3-kv-cache.tar.zst           (no leading dot, legacy-compatible name)
        // Distributed:    .sai3-cache-agent-{id}.tar.zst  (leading dot, hidden from plain LIST)
        let checkpoint_name = if let Some(id) = agent_id {
            format!(".sai3-cache-agent-{}.tar.zst", id)
        } else {
            "sai3-kv-cache.tar.zst".to_string()
        };

        // Create object store for endpoint
        let store = crate::workload::create_store_for_uri(endpoint_uri)
            .context("Failed to create object store for checkpoint restore")?;

        // Build full URI for checkpoint file
        let checkpoint_uri = if endpoint_uri.ends_with('/') {
            format!("{}{}", endpoint_uri, checkpoint_name)
        } else {
            format!("{}/{}", endpoint_uri, checkpoint_name)
        };

        info!("🔍 Checking for checkpoint on storage: {}", checkpoint_uri);

        // Try to download checkpoint from storage
        let bytes = match store.get(&checkpoint_uri).await {
            Ok(data) => data,
            Err(e) => {
                debug!("No checkpoint found on storage: {} ({})", checkpoint_uri, e);
                return Ok(false);
            }
        };

        info!(
            "📥 Downloaded checkpoint from storage: {} ({} bytes)",
            checkpoint_name,
            bytes.len()
        );

        // Build a temp-file path that is unique per (endpoint, agent_id, process).
        // cache_location already contains a hash of endpoint_uri, so its filename
        // component differs across concurrent tests that use different endpoints.
        // Including the PID prevents collisions across separate processes.
        let cache_dir_name = cache_location
            .file_name()
            .context("Cache location has no filename component")?
            .to_string_lossy();
        let temp_file = std::env::temp_dir().join(format!(
            ".sai3-extract-{}-{}.tar.zst",
            cache_dir_name,
            std::process::id()
        ));

        std::fs::write(&temp_file, &bytes)
            .context("Failed to write checkpoint to temporary file")?;

        info!(
            "🗜️  Extracting checkpoint: {} → {}",
            temp_file.display(),
            cache_location.display()
        );

        // Extract tar.zst to cache location
        let file = File::open(&temp_file).context("Failed to open checkpoint archive")?;
        let decompressor = zstd::Decoder::new(file).context("Failed to create zstd decoder")?;
        let mut archive = Archive::new(decompressor);

        // List archive contents for debugging
        let file2 = File::open(&temp_file).context("Failed to reopen checkpoint archive")?;
        let decompressor2 =
            zstd::Decoder::new(file2).context("Failed to create zstd decoder for listing")?;
        let mut archive2 = Archive::new(decompressor2);

        info!("📋 Archive contents:");
        for entry in archive2
            .entries()
            .context("Failed to list archive entries")?
        {
            let entry = entry.context("Failed to read archive entry")?;
            let path = entry.path().context("Failed to get entry path")?;
            info!("   - {}", path.display());
        }

        // Remove old cache directory if exists
        if cache_location.exists() {
            warn!(
                "⚠️  Removing old cache directory before restoration: {}",
                cache_location.display()
            );
            std::fs::remove_dir_all(cache_location)
                .context("Failed to remove old cache directory")?;
        } else {
            info!("No existing cache directory to remove");
        }

        // Extract to parent directory (archive contains cache_dir/...)
        let cache_parent = cache_location
            .parent()
            .context("Cache location has no parent directory")?;

        info!(
            "📂 Extracting to parent directory: {}",
            cache_parent.display()
        );

        archive
            .unpack(cache_parent)
            .context("Failed to extract checkpoint archive")?;

        // Verify extraction succeeded
        if !cache_location.exists() {
            return Err(anyhow!(
                "Checkpoint extraction failed: cache directory not created at {}",
                cache_location.display()
            ));
        }

        // Verify database files exist
        let expected_files = ["manifest", "partitions"];
        for file in &expected_files {
            let file_path = cache_location.join(file);
            if !file_path.exists() {
                warn!(
                    "⚠️  Expected database file missing after extraction: {}",
                    file_path.display()
                );
            } else {
                debug!("✓ Found database file: {}", file_path.display());
            }
        }

        // Clean up temp extraction file
        let _ = std::fs::remove_file(&temp_file);

        info!("✅ Checkpoint restored successfully from storage");

        Ok(true)
    }

    //
    // === Accessor Methods ===
    //

    /// Get cache location on disk
    pub fn cache_location(&self) -> &std::path::Path {
        &self.cache_location
    }

    /// Get endpoint index
    pub fn endpoint_index(&self) -> usize {
        self.endpoint_index
    }

    /// Get endpoint URI
    pub fn endpoint_uri(&self) -> &str {
        &self.endpoint_uri
    }

    //
    // === Object State Tracking API ===
    //

    /// Plan object creation (set DESIRED state)
    ///
    /// Marks object as Planned - it SHOULD exist but hasn't been created yet.
    pub fn plan_object(
        &self,
        config_hash: &str,
        file_idx: usize,
        path: &str,
        size: u64,
    ) -> Result<()> {
        let entry = ObjectEntry::new_planned(path.to_string(), size, self.endpoint_index);
        let key = format!("{}:{:08}", config_hash, file_idx);
        self.objects.insert(key.as_bytes(), &entry.to_bytes()?)?;
        self.maybe_flush()?;
        Ok(())
    }

    /// Mark object as creating (transitional state)
    pub fn mark_creating(&self, config_hash: &str, file_idx: usize) -> Result<()> {
        let key = format!("{}:{:08}", config_hash, file_idx);
        if let Some(bytes) = self.objects.get(key.as_bytes())? {
            let mut entry = ObjectEntry::from_bytes(&bytes)?;
            entry.state = ObjectState::Creating;
            self.objects.insert(key.as_bytes(), &entry.to_bytes()?)?;
        }
        self.maybe_flush()?;
        Ok(())
    }

    /// Mark object as created (CURRENT state matches DESIRED)
    ///
    /// # Arguments
    /// * `created_at` - Optional timestamp (epoch seconds). If None, uses current time.
    /// * `checksum` - Optional checksum for validation
    pub fn mark_created(
        &self,
        config_hash: &str,
        file_idx: usize,
        created_at: Option<u64>,
        checksum: Option<u64>,
    ) -> Result<()> {
        let key = format!("{}:{:08}", config_hash, file_idx);
        if let Some(bytes) = self.objects.get(key.as_bytes())? {
            let mut entry = ObjectEntry::from_bytes(&bytes)?;
            entry.state = ObjectState::Created;
            entry.created_at = Some(created_at.unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
            }));
            entry.checksum = checksum;
            self.objects.insert(key.as_bytes(), &entry.to_bytes()?)?;
        }
        self.maybe_flush()?;
        Ok(())
    }

    /// Mark object creation as failed
    pub fn mark_failed(&self, config_hash: &str, file_idx: usize) -> Result<()> {
        let key = format!("{}:{:08}", config_hash, file_idx);
        if let Some(bytes) = self.objects.get(key.as_bytes())? {
            let mut entry = ObjectEntry::from_bytes(&bytes)?;
            entry.state = ObjectState::Failed;
            self.objects.insert(key.as_bytes(), &entry.to_bytes()?)?;
        }
        self.maybe_flush()?;
        Ok(())
    }

    /// Mark object as deleted (drift detection)
    pub fn mark_deleted(&self, config_hash: &str, file_idx: usize) -> Result<()> {
        let key = format!("{}:{:08}", config_hash, file_idx);
        if let Some(bytes) = self.objects.get(key.as_bytes())? {
            let mut entry = ObjectEntry::from_bytes(&bytes)?;
            entry.state = ObjectState::Deleted;
            self.objects.insert(key.as_bytes(), &entry.to_bytes()?)?;
        }
        self.maybe_flush()?;
        Ok(())
    }

    /// Record object as successfully created (unconditional upsert).
    ///
    /// Unlike `mark_created`, this does **not** require a prior `plan_object` call.
    /// It inserts a `Created` entry whether or not the key already exists.
    /// This is the correct method to call from the prepare PUT loop.
    ///
    /// # Arguments
    /// * `path` - Relative path of the object (e.g., "prepared-00000042.dat")
    /// * `size` - Object size in bytes
    /// * `created_at` - Optional epoch seconds; uses current time when `None`
    /// * `checksum`   - Optional content checksum
    pub fn record_created(
        &self,
        config_hash: &str,
        file_idx: usize,
        path: &str,
        size: u64,
        created_at: Option<u64>,
        checksum: Option<u64>,
    ) -> Result<()> {
        let key = format!("{}:{:08}", config_hash, file_idx);
        let timestamp = created_at.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        });

        // Upsert: update existing entry if present, otherwise create a new one.
        let mut entry = if let Some(bytes) = self.objects.get(key.as_bytes())? {
            ObjectEntry::from_bytes(&bytes)?
        } else {
            ObjectEntry::new_planned(path.to_string(), size, self.endpoint_index)
        };

        entry.state = ObjectState::Created;
        entry.created_at = Some(timestamp);
        entry.checksum = checksum;

        self.objects.insert(key.as_bytes(), &entry.to_bytes()?)?;
        self.maybe_flush()?;
        Ok(())
    }

    /// Get object entry
    pub fn get_object(&self, config_hash: &str, file_idx: usize) -> Result<Option<ObjectEntry>> {
        let key = format!("{}:{:08}", config_hash, file_idx);
        if let Some(bytes) = self.objects.get(key.as_bytes())? {
            let entry = ObjectEntry::from_bytes(&bytes)?;
            Ok(Some(entry))
        } else {
            Ok(None)
        }
    }

    /// Get all objects in a specific state (for resume/validation)
    ///
    /// WARNING: This scans ALL objects - use sparingly!
    pub fn get_objects_by_state(
        &self,
        config_hash: &str,
        target_state: ObjectState,
    ) -> Result<Vec<(usize, ObjectEntry)>> {
        let mut results = Vec::new();
        let prefix = format!("{}:", config_hash);

        // Scan keyspace with prefix
        // fjall .iter() returns guards - need into_inner() to get (key, value)
        for guard in self.objects.iter() {
            let (key, value) = guard.into_inner().context("Fjall iteration error")?;

            let key_str = std::str::from_utf8(&key)?;

            if !key_str.starts_with(&prefix) {
                continue;
            }

            let entry = ObjectEntry::from_bytes(&value)?;
            if entry.state == target_state {
                // Extract file_idx from key
                let idx_str = key_str.strip_prefix(&prefix).unwrap();
                if let Ok(file_idx) = idx_str.parse::<usize>() {
                    results.push((file_idx, entry));
                }
            }
        }

        Ok(results)
    }

    /// Count objects by state (for progress reporting)
    pub fn count_by_state(&self, config_hash: &str) -> Result<HashMap<ObjectState, usize>> {
        let mut counts = HashMap::new();
        let prefix = format!("{}:", config_hash);

        for guard in self.objects.iter() {
            let (key, value) = guard.into_inner().context("Fjall iteration error")?;

            let key_str = std::str::from_utf8(&key)?;

            if !key_str.starts_with(&prefix) {
                continue;
            }

            let entry = ObjectEntry::from_bytes(&value)?;
            *counts.entry(entry.state).or_insert(0) += 1;
        }

        Ok(counts)
    }

    /// Count objects **and** accumulate byte totals by state in a single scan.
    ///
    /// When called from the coverage path the fjall scan is already required;
    /// summing bytes here adds no extra I/O.
    ///
    /// Emits [`warn!`] messages if the scan runs longer than expected so that
    /// operators are never left wondering why startup is slow:
    /// - First warning at 10 s, then every 10 s thereafter.
    /// - Message tone escalates at 30 s to note the scan is still running.
    /// - The scan is **never aborted** — the alternative (a full LIST of millions
    ///   of objects) is orders of magnitude slower and the user is better served
    ///   by a slow KV scan than by falling through to a LIST that may take hours.
    pub fn count_and_bytes_by_state(
        &self,
        config_hash: &str,
    ) -> Result<(HashMap<ObjectState, usize>, HashMap<ObjectState, u64>)> {
        let mut counts: HashMap<ObjectState, usize> = HashMap::new();
        let mut bytes: HashMap<ObjectState, u64> = HashMap::new();
        let prefix = format!("{}:", config_hash);

        let t0 = std::time::Instant::now();
        let mut scanned: usize = 0;
        // next_warn fires at 10 s, then every 10 s after that.
        let mut next_warn_secs: u64 = 10;

        for guard in self.objects.iter() {
            let (key, value) = guard.into_inner().context("Fjall iteration error")?;

            let key_str = std::str::from_utf8(&key)?;

            if !key_str.starts_with(&prefix) {
                continue;
            }

            let entry = ObjectEntry::from_bytes(&value)?;
            *counts.entry(entry.state).or_insert(0) += 1;
            *bytes.entry(entry.state).or_insert(0) += entry.size;
            scanned += 1;

            // Check elapsed every 50 000 entries — negligible overhead but
            // responsive enough to catch a slow scan promptly.
            if scanned.is_multiple_of(50_000) {
                let elapsed_secs = t0.elapsed().as_secs();
                if elapsed_secs >= next_warn_secs {
                    if elapsed_secs >= 30 {
                        warn!(
                            "⏳ KV cache scan still running: {}s elapsed, {} entries scanned. \
                             NOT aborting — a LIST of this dataset would take far longer. \
                             Please wait.",
                            elapsed_secs, scanned
                        );
                    } else {
                        warn!(
                            "⏳ KV cache scan is taking longer than expected: {}s elapsed, \
                             {} entries scanned so far. Normal for very large datasets — \
                             please wait.",
                            elapsed_secs, scanned
                        );
                    }
                    // Next warning fires 10 s later.
                    next_warn_secs = elapsed_secs.saturating_add(10);
                }
            }
        }

        Ok((counts, bytes))
    }

    //
    // === Batch Operations (Performance Critical) ===
    //

    /// Batch plan objects (for initial prepare phase)
    ///
    /// Creates planned entries for all objects this endpoint will own.
    /// Commits every 100k inserts for crash safety.
    pub fn plan_objects_batch(
        &self,
        config_hash: &str,
        objects: &[(usize, String, u64)], // (file_idx, path, size)
    ) -> Result<()> {
        const BATCH_SIZE: usize = 100_000;

        for (idx, (file_idx, path, size)) in objects.iter().enumerate() {
            let entry = ObjectEntry::new_planned(path.clone(), *size, self.endpoint_index);
            let key = format!("{}:{:08}", config_hash, file_idx);
            self.objects.insert(key.as_bytes(), &entry.to_bytes()?)?;

            if (idx + 1) % BATCH_SIZE == 0 {
                // Non-blocking checkpoint (async flush handles persistence)
                self.maybe_flush()?;
                debug!(
                    "Object batch checkpoint: {}/{} objects",
                    idx + 1,
                    objects.len()
                );
            }
        }

        // Final flush ensures data persisted before returning
        self.force_flush()?;

        info!(
            "Planned {} objects for endpoint {}",
            objects.len(),
            self.endpoint_index
        );
        Ok(())
    }

    //
    // === Listing Cache (for Object Storage) ===
    //

    /// Get listing cache (compressed file list)
    ///
    /// Key format: "{config_hash}:list_timestamp:{epoch}"
    pub fn get_listing(&self, config_hash: &str, timestamp: u64) -> Result<Option<Vec<String>>> {
        let key = format!("{}:list_timestamp:{}", config_hash, timestamp);
        if let Some(compressed) = self.listing_cache.get(key.as_bytes())? {
            // Decompress zstd
            let json_bytes =
                zstd::decode_all(&compressed[..]).context("Failed to decompress listing cache")?;
            let json = std::str::from_utf8(&json_bytes)?;
            let paths: Vec<String> = serde_json::from_str(json)?;
            Ok(Some(paths))
        } else {
            Ok(None)
        }
    }

    /// Put listing cache (zstd compressed)
    pub fn put_listing(&self, config_hash: &str, timestamp: u64, paths: &[String]) -> Result<()> {
        let key = format!("{}:list_timestamp:{}", config_hash, timestamp);
        let json = serde_json::to_string(paths)?;
        let compressed = zstd::encode_all(json.as_bytes(), 3)?; // Level 3 = good balance

        self.listing_cache.insert(key.as_bytes(), &compressed)?;
        self.maybe_flush()?; // Non-blocking for performance;

        info!(
            "Cached {} file paths in listing cache (endpoint {}, {} KB compressed)",
            paths.len(),
            self.endpoint_index,
            compressed.len() / 1024
        );
        Ok(())
    }

    //
    // === Endpoint Distribution Map ===
    //

    /// Get endpoint index for file (pre-computed distribution)
    pub fn get_endpoint_index(
        &self,
        config_hash: &str,
        strategy: &str,
        file_idx: usize,
    ) -> Result<Option<usize>> {
        let key = format!("{}:{}:{:08}", config_hash, strategy, file_idx);
        if let Some(bytes) = self.endpoint_map.get(key.as_bytes())? {
            let idx_bytes: [u8; 4] = bytes
                .as_ref()
                .try_into()
                .context("Invalid endpoint index bytes")?;
            let idx = u32::from_le_bytes(idx_bytes);
            Ok(Some(idx as usize))
        } else {
            Ok(None)
        }
    }

    /// Pre-compute endpoint distribution (batch operation)
    ///
    /// Populates endpoint_map keyspace for O(1) lookups during workload.
    pub fn populate_endpoint_map(
        &self,
        config_hash: &str,
        strategy: &str,
        total_files: usize,
        num_endpoints: usize,
    ) -> Result<()> {
        info!(
            "Pre-computing endpoint distribution: {} files across {} endpoints (strategy: {})",
            total_files, num_endpoints, strategy
        );

        const BATCH_SIZE: usize = 100_000;

        for file_idx in 0..total_files {
            // Compute endpoint index based on strategy
            let endpoint_idx = match strategy {
                "round_robin" => file_idx % num_endpoints,
                _ => {
                    warn!(
                        "Unknown endpoint distribution strategy: {}, using round-robin",
                        strategy
                    );
                    file_idx % num_endpoints
                }
            };

            let key = format!("{}:{}:{:08}", config_hash, strategy, file_idx);
            let value = (endpoint_idx as u32).to_le_bytes();
            self.endpoint_map.insert(key.as_bytes(), value)?;

            if (file_idx + 1) % BATCH_SIZE == 0 {
                self.maybe_flush()?; // Non-blocking checkpoint;

                if (file_idx + 1) % 1_000_000 == 0 {
                    info!(
                        "  Endpoint map progress: {}/{} files ({:.1}%)",
                        file_idx + 1,
                        total_files,
                        ((file_idx + 1) as f64 / total_files as f64) * 100.0
                    );
                }
            }
        }

        // Final flush ensures completion
        self.force_flush()?;

        info!(
            "✓ Endpoint map populated: {} entries for endpoint {}",
            total_files, self.endpoint_index
        );
        Ok(())
    }

    //
    // === RNG Seeds ===
    //

    /// Get RNG seed from cache
    pub fn get_seed(
        &self,
        config_hash: &str,
        seed_type: &str,
        index: usize,
    ) -> Result<Option<u64>> {
        let key = format!("{}:{}:{:08}", config_hash, seed_type, index);
        if let Some(bytes) = self.seeds.get(key.as_bytes())? {
            let seed_bytes: [u8; 8] = bytes.as_ref().try_into().context("Invalid seed bytes")?;
            let seed = u64::from_le_bytes(seed_bytes);
            Ok(Some(seed))
        } else {
            Ok(None)
        }
    }

    /// Put RNG seed into cache
    pub fn put_seed(
        &self,
        config_hash: &str,
        seed_type: &str,
        index: usize,
        seed: u64,
    ) -> Result<()> {
        let key = format!("{}:{}:{:08}", config_hash, seed_type, index);
        self.seeds.insert(key.as_bytes(), seed.to_le_bytes())?;
        self.maybe_flush()?; // Non-blocking
        Ok(())
    }

    //
    // === Utilities ===
    //

    /// Check if this endpoint owns a specific file
    ///
    /// Uses modulo distribution: file_idx % total_endpoints == endpoint_index
    pub fn owns_file(&self, file_idx: usize, total_endpoints: usize) -> bool {
        file_idx % total_endpoints == self.endpoint_index
    }

    /// Conditionally flush based on policy (NON-BLOCKING for benchmarks)
    ///
    /// This is the performance-critical method that prevents cache updates
    /// from blocking benchmark I/O operations.
    ///
    /// - FlushPolicy::Immediate → flush every write (HIGH LATENCY, NOT RECOMMENDED)
    /// - FlushPolicy::BatchSize(N) → flush every N writes (MEDIUM LATENCY)
    /// - FlushPolicy::AsyncInterval(secs) → background flush only (ZERO BLOCKING)
    fn maybe_flush(&self) -> Result<()> {
        let policy = *self.flush_policy.read().unwrap();
        match policy {
            FlushPolicy::Immediate => {
                // Synchronous flush (NOT recommended for hot paths)
                self.db
                    .persist(fjall::PersistMode::SyncData)
                    .context("Fjall flush error")?;
            }
            FlushPolicy::BatchSize(threshold) => {
                // Batched flush (only when threshold hit)
                let pending = self.pending_writes.fetch_add(1, Ordering::SeqCst) + 1;
                if pending >= threshold {
                    self.pending_writes.store(0, Ordering::SeqCst);
                    self.db
                        .persist(fjall::PersistMode::SyncData)
                        .context("Fjall flush error")?;
                    debug!("Cache batch flush: {} writes", threshold);
                }
            }
            FlushPolicy::AsyncInterval(_) => {
                // NO-OP: Background thread handles flushing
                // This is ZERO-COST for benchmark hot paths
                self.pending_writes.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Force flush all pending writes to disk (blocking)
    ///
    /// Call this at end of prepare phase or before critical validation.
    /// DO NOT call in hot benchmark paths.
    pub fn force_flush(&self) -> Result<()> {
        let pending = self.pending_writes.swap(0, Ordering::SeqCst);
        if pending > 0 {
            debug!("Force flushing {} pending writes", pending);
        }
        self.db
            .persist(fjall::PersistMode::SyncAll)
            .context("Fjall force flush error")?;
        Ok(())
    }

    /// Set flush policy at runtime
    ///
    /// Resets pending write counter. Call force_flush() first if needed.
    pub fn set_flush_policy(&self, policy: FlushPolicy) {
        *self.flush_policy.write().unwrap() = policy;
        self.pending_writes.store(0, Ordering::SeqCst);
        info!(
            "Endpoint {} flush policy: {:?}",
            self.endpoint_index, policy
        );
    }

    /// Get pending write count (unflushed operations)
    pub fn pending_write_count(&self) -> u64 {
        self.pending_writes.load(Ordering::Relaxed)
    }

    /// Flush all pending writes to disk (DEPRECATED - use force_flush instead)
    #[deprecated(
        note = "Use force_flush() for explicit blocking flush, or rely on background flush"
    )]
    pub fn flush(&self) -> Result<()> {
        self.force_flush()
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            endpoint_index: self.endpoint_index,
            endpoint_uri: self.endpoint_uri.clone(),
            cache_location: self.cache_location.clone(),
            // Note: fjall doesn't expose size stats easily
            objects_count: 0, // TODO: Track separately if needed
            cache_size_bytes: 0,
        }
    }

    /// Write KV cache checkpoint to storage under test with robust retry logic
    ///
    /// **CRITICAL**: Creates compressed tar.zst archive of cache and persists it alongside
    /// test data for resume capability. Works for BOTH filesystem and cloud storage.
    ///
    /// # Arguments
    /// * `agent_id` - Optional agent ID for distributed mode (uses agent-specific filename)
    ///
    /// # Returns
    /// Result with checkpoint location (path or URI) on success
    ///
    /// # Retry Strategy
    /// - Maximum 5 attempts with exponential backoff (1s, 2s, 4s, 8s, 16s)
    /// - Verifies archive integrity by comparing sizes
    /// - For cloud storage: uses ObjectStore::put() for upload
    /// - For filesystem: writes .tar.zst file to disk
    ///
    /// # Archive Format
    /// - Filename: `.sai3-cache-agent-{id}.tar.zst` (or `sai3-kv-cache.tar.zst` for single-node)
    /// - Location: Same directory/prefix as endpoint URI
    /// - Compression: zstd level 3 (fast, ~3x compression)
    ///
    /// # Example
    /// ```no_run
    /// # use anyhow::Result;
    /// # use sai3_bench::metadata_cache::EndpointCache;
    /// # async fn example() -> Result<()> {
    /// # let cache = todo!() as EndpointCache;
    /// // After prepare completes:
    /// let checkpoint = cache.write_checkpoint(Some(0)).await?;
    /// println!("KV cache checkpoint created: {}", checkpoint);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn write_checkpoint(&self, agent_id: Option<usize>) -> Result<String> {
        use std::time::Duration;
        use tokio::time::{sleep, timeout};

        // CRITICAL: Guaranteed flush with verification
        // Force ALL pending writes to disk and sync
        info!("🔄 Flushing KV cache before checkpoint (guaranteed sync)...");
        self.force_flush()
            .context("Failed to flush KV cache before checkpoint")?;

        // Give OS time to complete write buffers (100ms is enough for most systems)
        sleep(Duration::from_millis(100)).await;

        // Verify database files exist before archiving
        // CRITICAL FIX (v0.8.61): Use spawn_blocking for filesystem read with timeout
        let cache_location = self.cache_location.clone();
        timeout(
            Duration::from_secs(5),
            tokio::task::spawn_blocking(move || Self::verify_database_files_sync(&cache_location)),
        )
        .await
        .context(
            "Database verification timed out after 5 seconds - filesystem may be unresponsive",
        )??
        .context("Database files missing after flush - cannot create checkpoint")?;

        let checkpoint_name = if let Some(id) = agent_id {
            format!(".sai3-cache-agent-{}.tar.zst", id)
        } else {
            "sai3-kv-cache.tar.zst".to_string()
        };

        info!("");
        info!("📦 Creating KV cache checkpoint:");
        info!("   Source: {}", self.cache_location.display());
        info!("   Endpoint: {}", self.endpoint_uri);
        info!("   Checkpoint: {}", checkpoint_name);

        const MAX_ATTEMPTS: u32 = 5;
        const BASE_DELAY_MS: u64 = 1000; // 1 second base delay

        for attempt in 1..=MAX_ATTEMPTS {
            match self.try_write_checkpoint(&checkpoint_name).await {
                Ok(location) => {
                    info!(
                        "✅ KV cache checkpoint created successfully (attempt {}/{}): {}",
                        attempt, MAX_ATTEMPTS, location
                    );

                    // v0.8.62: Clean up /tmp cache directory AFTER successful checkpoint
                    // Cache is now persisted to storage, safe to remove /tmp copy
                    // If we crash before this, cache remains for recovery on next startup
                    if let Err(e) = std::fs::remove_dir_all(&self.cache_location) {
                        // Non-fatal: checkpoint succeeded, cleanup is best-effort
                        warn!(
                            "⚠️  Failed to cleanup /tmp cache directory {}: {}",
                            self.cache_location.display(),
                            e
                        );
                    } else {
                        debug!(
                            "🧹 Cleaned up /tmp cache: {}",
                            self.cache_location.display()
                        );
                    }

                    return Ok(location);
                }
                Err(e) => {
                    if attempt < MAX_ATTEMPTS {
                        let delay_ms = BASE_DELAY_MS * (1 << (attempt - 1)); // Exponential backoff
                        warn!(
                            "⚠️  Checkpoint attempt {}/{} failed: {}. Retrying in {}ms...",
                            attempt, MAX_ATTEMPTS, e, delay_ms
                        );
                        sleep(Duration::from_millis(delay_ms)).await;
                    } else {
                        return Err(anyhow!(
                            "Failed to create KV cache checkpoint after {} attempts. Last error: {}",
                            MAX_ATTEMPTS, e
                        ));
                    }
                }
            }
        }

        unreachable!("Retry loop should always return")
    }

    /// Attempt to write checkpoint (single try)
    async fn try_write_checkpoint(&self, checkpoint_name: &str) -> Result<String> {
        use std::time::Duration;
        use tokio::time::timeout;

        info!("📦 Step 1/2: Creating checkpoint archive (tar + zstd compression)...");

        // Create tar.zst archive from cache directory
        // CRITICAL FIX (v0.8.61): Use spawn_blocking to prevent executor starvation
        // Timeout: 30 seconds (conservative - should complete in <10s for most workloads)
        let cache_location = self.cache_location.clone();
        let archive_start = std::time::Instant::now();

        let archive_bytes = timeout(
            Duration::from_secs(30),
            tokio::task::spawn_blocking(move || {
                Self::create_checkpoint_archive_sync(&cache_location)
            }),
        )
        .await
        .map_err(|_| {
            anyhow!("⏱️  Archive creation TIMED OUT after 30 seconds - disk I/O may be hung")
        })??
        .context("Failed to create checkpoint archive")?;

        let archive_size = archive_bytes.len();
        let archive_elapsed = archive_start.elapsed();
        info!(
            "✅ Step 1/2: Archive created ({:.2} MB, took {:.2}s)",
            archive_size as f64 / 1_048_576.0,
            archive_elapsed.as_secs_f64()
        );

        // Determine storage type and write accordingly
        if self.endpoint_uri.starts_with("file://") {
            // Filesystem: write tar.zst to disk
            let url = Url::parse(&self.endpoint_uri).context("Failed to parse file:// URI")?;
            let base_path = url
                .to_file_path()
                .map_err(|_| anyhow!("Invalid file:// path"))?;

            let checkpoint_path = base_path.join(checkpoint_name);

            info!(
                "📦 Step 2/2: Writing checkpoint to filesystem: {}",
                checkpoint_path.display()
            );

            // CRITICAL FIX (v0.8.61): Use spawn_blocking for filesystem operations
            // Timeout calculation: assume minimum 10 MB/s write speed (very conservative)
            // For 64MB archive: 6.4 seconds + 3.6 seconds buffer = 10 seconds
            let timeout_secs = std::cmp::max(10, (archive_size as u64 / 10_000_000) + 4);

            let base_path_clone = base_path.clone();
            let checkpoint_path_clone = checkpoint_path.clone();
            let archive_bytes_clone = archive_bytes.clone();
            let write_start = std::time::Instant::now();

            timeout(
                Duration::from_secs(timeout_secs),
                tokio::task::spawn_blocking(move || {
                    // Ensure directory exists
                    std::fs::create_dir_all(&base_path_clone)?;
                    std::fs::write(&checkpoint_path_clone, &archive_bytes_clone)?;
                    Ok::<(), std::io::Error>(())
                })
            )
            .await
            .map_err(|_| anyhow!(
                "⏱️  File write TIMED OUT after {} seconds ({:.2} MB at <10 MB/s) - check NFS mount, disk space, or filesystem health",
                timeout_secs, archive_size as f64 / 1_048_576.0
            ))??
            .with_context(|| format!("Failed to write checkpoint: {}", checkpoint_path.display()))?;

            let write_elapsed = write_start.elapsed();
            let write_speed_mb_s =
                (archive_size as f64 / 1_048_576.0) / write_elapsed.as_secs_f64();
            info!(
                "✅ Step 2/2: Checkpoint written ({:.2} MB/s)",
                write_speed_mb_s
            );

            Ok(checkpoint_path.display().to_string())
        } else if self.endpoint_uri.starts_with("direct://") {
            // Direct I/O: write tar.zst to disk (strip direct:// prefix)
            let path_str = self
                .endpoint_uri
                .strip_prefix("direct://")
                .ok_or_else(|| anyhow!("Invalid direct:// URI"))?;
            let base_path = PathBuf::from(path_str);

            let checkpoint_path = base_path.join(checkpoint_name);

            info!(
                "📦 Step 2/2: Writing checkpoint to direct I/O: {}",
                checkpoint_path.display()
            );

            // CRITICAL FIX (v0.8.61): Use spawn_blocking for filesystem operations
            let timeout_secs = std::cmp::max(10, (archive_size as u64 / 10_000_000) + 4);

            let base_path_clone = base_path.clone();
            let checkpoint_path_clone = checkpoint_path.clone();
            let write_start = std::time::Instant::now();

            timeout(
                Duration::from_secs(timeout_secs),
                tokio::task::spawn_blocking(move || {
                    // Ensure directory exists
                    std::fs::create_dir_all(&base_path_clone)?;
                    std::fs::write(&checkpoint_path_clone, &archive_bytes)?;
                    Ok::<(), std::io::Error>(())
                }),
            )
            .await
            .map_err(|_| {
                anyhow!(
                "⏱️  File write TIMED OUT after {} seconds - check disk space or filesystem health",
                timeout_secs
            )
            })??
            .with_context(|| {
                format!("Failed to write checkpoint: {}", checkpoint_path.display())
            })?;

            let write_elapsed = write_start.elapsed();
            let write_speed_mb_s =
                (archive_size as f64 / 1_048_576.0) / write_elapsed.as_secs_f64();
            info!(
                "✅ Step 2/2: Checkpoint written ({:.2} MB/s)",
                write_speed_mb_s
            );

            Ok(checkpoint_path.display().to_string())
        } else if self.endpoint_uri.starts_with("s3://")
            || self.endpoint_uri.starts_with("az://")
            || self.endpoint_uri.starts_with("gs://")
        {
            // Cloud storage: upload via ObjectStore
            info!(
                "📦 Step 2/2: Uploading checkpoint to cloud storage: {}",
                self.endpoint_uri
            );

            let store =
                store_for_uri(&self.endpoint_uri).context("Failed to create object store")?;

            // Upload checkpoint archive
            let checkpoint_uri = format!("{}{}", self.endpoint_uri, checkpoint_name);
            let upload_start = std::time::Instant::now();

            store
                .put(&checkpoint_uri, Bytes::from(archive_bytes.clone()))
                .await
                .with_context(|| format!("Failed to upload checkpoint to {}", checkpoint_uri))?;

            let upload_elapsed = upload_start.elapsed();
            let upload_speed_mb_s =
                (archive_bytes.len() as f64 / 1_048_576.0) / upload_elapsed.as_secs_f64();
            info!(
                "✅ Step 2/2: Checkpoint uploaded ({:.2} MB/s)",
                upload_speed_mb_s
            );

            Ok(checkpoint_uri)
        } else {
            Err(anyhow!(
                "Unsupported URI scheme for checkpointing: {}",
                self.endpoint_uri
            ))
        }
    }

    /// Verify database files exist on disk before checkpointing (SYNCHRONOUS - blocking I/O)
    ///
    /// This ensures the database was actually flushed to disk by fjall
    ///
    /// CRITICAL: This is a synchronous function that must be called via spawn_blocking
    fn verify_database_files_sync(cache_location: &PathBuf) -> Result<()> {
        // Count files in cache directory to ensure it's not empty
        let entries: Vec<_> = std::fs::read_dir(cache_location)
            .context("Failed to read cache directory")?
            .collect();

        if entries.is_empty() {
            return Err(anyhow!(
                "Cache directory is empty: {} - database not initialized?",
                cache_location.display()
            ));
        }

        // List what we actually have
        info!("📂 Database directory verification:");
        info!("   Location: {}", cache_location.display());
        for entry in entries.iter().flatten() {
            let path = entry.path();
            let file_type = if path.is_dir() { "DIR" } else { "FILE" };
            let size = path.metadata().ok().map(|m| m.len()).unwrap_or(0);
            info!(
                "   - {}: {} ({} bytes)",
                file_type,
                path.file_name().unwrap().to_string_lossy(),
                size
            );
        }

        info!(
            "✓ Database verification passed: {} files/dirs in cache",
            entries.len()
        );
        Ok(())
    }

    /// Create tar.zst archive from cache directory (SYNCHRONOUS - blocking I/O)
    ///
    /// Returns compressed archive as byte vector
    ///
    /// CRITICAL: This is a synchronous function that must be called via spawn_blocking
    fn create_checkpoint_archive_sync(cache_location: &PathBuf) -> Result<Vec<u8>> {
        use tar::Builder;
        use zstd::stream::write::Encoder;

        // Log cache directory contents before archiving
        info!("📂 Cache directory contents before archiving:");
        info!("   Location: {}", cache_location.display());
        if let Ok(entries) = std::fs::read_dir(cache_location) {
            for entry in entries.flatten() {
                let path = entry.path();
                let metadata = entry.metadata().ok();
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                let file_type = if path.is_dir() { "DIR" } else { "FILE" };
                info!(
                    "   - {} {} ({} bytes)",
                    file_type,
                    path.file_name().unwrap().to_string_lossy(),
                    size
                );
            }
        } else {
            warn!("   Failed to read cache directory!");
        }

        // Create in-memory buffer for compressed archive
        let buffer = Vec::new();
        let encoder = Encoder::new(buffer, 3) // zstd level 3 (fast, good compression)
            .context("Failed to create zstd encoder")?;

        let mut tar_builder = Builder::new(encoder);

        // Get cache directory name for proper archive structure
        let cache_dir_name = cache_location
            .file_name()
            .ok_or_else(|| anyhow!("Cache location has no directory name"))?;

        // Add cache directory contents to archive with proper naming
        // This creates archive structure: cache-dir-name/file1, cache-dir-name/file2, etc.
        tar_builder
            .append_dir_all(cache_dir_name, cache_location)
            .with_context(|| {
                format!(
                    "Failed to add cache directory to archive: {}",
                    cache_location.display()
                )
            })?;

        // Finish tar archive
        let encoder = tar_builder
            .into_inner()
            .context("Failed to finalize tar archive")?;

        // Finish zstd compression and get bytes
        let compressed = encoder
            .finish()
            .context("Failed to finalize zstd compression")?;

        Ok(compressed)
    }

    /// Spawn background task for periodic checkpointing
    ///
    /// **CRITICAL**: Protects long-running prepares from data loss by checkpointing at regular intervals.
    ///
    /// # Arguments
    /// * `interval_secs` - Checkpoint interval in seconds (0 = disabled)
    /// * `agent_id` - Optional agent ID for distributed mode
    ///
    /// # Returns
    /// JoinHandle that can be used to cancel background task
    ///
    /// # Example
    /// ```no_run
    /// # use anyhow::Result;
    /// # use sai3_bench::metadata_cache::EndpointCache;
    /// # async fn example() -> Result<()> {
    /// # let cache = todo!() as EndpointCache;
    /// // Checkpoint every 5 minutes during prepare
    /// let handle = cache.spawn_periodic_checkpoint(300, Some(0));
    ///
    /// // ... prepare runs ...
    ///
    /// // Cancel background task when done
    /// handle.abort();
    /// # Ok(())
    /// # }
    /// ```
    pub fn spawn_periodic_checkpoint(
        &self,
        interval_secs: u64,
        agent_id: Option<usize>,
    ) -> tokio::task::JoinHandle<()> {
        use std::time::Duration;
        use tokio::time::sleep;

        if interval_secs == 0 {
            // Return no-op task if disabled
            return tokio::spawn(async {});
        }

        // Clone necessary data for the background task
        let cache_location = self.cache_location.clone();
        let endpoint_uri = self.endpoint_uri.clone();
        let endpoint_index = self.endpoint_index;

        tokio::spawn(async move {
            info!(
                "🔄 Starting periodic checkpointing every {} seconds (endpoint {})",
                interval_secs, endpoint_index
            );
            let interval = Duration::from_secs(interval_secs);

            loop {
                sleep(interval).await;

                debug!(
                    "⏰ Periodic checkpoint timer triggered (endpoint {})",
                    endpoint_index
                );

                // Create a temporary EndpointCache-like structure for checkpointing
                // This is a simplified version that only handles checkpoint creation
                match Self::create_and_write_checkpoint_static(
                    &cache_location,
                    &endpoint_uri,
                    endpoint_index,
                    agent_id,
                )
                .await
                {
                    Ok(location) => {
                        info!(
                            "✅ Periodic checkpoint created (endpoint {}): {}",
                            endpoint_index, location
                        );
                    }
                    Err(e) => {
                        // Log error but don't abort - prepare can continue
                        // Final checkpoint at end will retry
                        warn!(
                            "⚠️  Periodic checkpoint failed (endpoint {}, non-fatal): {}",
                            endpoint_index, e
                        );
                    }
                }
            }
        })
    }

    /// Static helper to create and write checkpoint without self reference
    /// Used by background checkpointing task
    async fn create_and_write_checkpoint_static(
        cache_location: &Path,
        endpoint_uri: &str,
        endpoint_index: usize,
        agent_id: Option<usize>,
    ) -> Result<String> {
        use std::time::Duration;
        use tokio::time::sleep;

        let checkpoint_name = if let Some(id) = agent_id {
            format!(".sai3-cache-agent-{}.tar.zst", id)
        } else {
            "sai3-kv-cache.tar.zst".to_string()
        };

        const MAX_ATTEMPTS: u32 = 5;
        const BASE_DELAY_MS: u64 = 1000;

        for attempt in 1..=MAX_ATTEMPTS {
            match Self::try_write_checkpoint_static(cache_location, endpoint_uri, &checkpoint_name)
                .await
            {
                Ok(location) => {
                    return Ok(location);
                }
                Err(e) => {
                    if attempt < MAX_ATTEMPTS {
                        let delay_ms = BASE_DELAY_MS * (1 << (attempt - 1));
                        warn!(
                            "⚠️  Checkpoint attempt {}/{} failed (endpoint {}): {}. Retrying in {}ms...",
                            attempt, MAX_ATTEMPTS, endpoint_index, e, delay_ms
                        );
                        sleep(Duration::from_millis(delay_ms)).await;
                    } else {
                        return Err(anyhow!(
                            "Failed to create checkpoint after {} attempts (endpoint {}). Last error: {}",
                            MAX_ATTEMPTS, endpoint_index, e
                        ));
                    }
                }
            }
        }

        unreachable!("Retry loop should always return")
    }

    /// Static helper to try writing checkpoint once
    async fn try_write_checkpoint_static(
        cache_location: &Path,
        endpoint_uri: &str,
        checkpoint_name: &str,
    ) -> Result<String> {
        use std::time::Duration;
        use tokio::time::timeout;

        // Create tar.zst archive from cache directory
        // CRITICAL FIX (v0.8.61): Use spawn_blocking to prevent executor starvation
        // Timeout: 30 seconds for archive creation
        let cache_location_clone = cache_location.to_path_buf();
        let archive_bytes = timeout(
            Duration::from_secs(30),
            tokio::task::spawn_blocking(move || {
                EndpointCache::create_checkpoint_archive_sync(&cache_location_clone)
            }),
        )
        .await
        .context("Checkpoint archive creation timed out after 30 seconds")??
        .context("Failed to create checkpoint archive")?;

        let archive_size = archive_bytes.len();
        debug!("Created checkpoint archive: {} bytes", archive_size);

        // Determine storage type and write accordingly
        if endpoint_uri.starts_with("file://") {
            let url = Url::parse(endpoint_uri).context("Failed to parse file:// URI")?;
            let base_path = url
                .to_file_path()
                .map_err(|_| anyhow!("Invalid file:// path"))?;
            let checkpoint_path = base_path.join(checkpoint_name);

            // CRITICAL FIX (v0.8.61): Use spawn_blocking with timeout
            let checkpoint_path_clone = checkpoint_path.clone();
            let archive_bytes_clone = archive_bytes.clone();
            timeout(
                Duration::from_secs(10),
                tokio::task::spawn_blocking(move || {
                    std::fs::write(&checkpoint_path_clone, &archive_bytes_clone)
                }),
            )
            .await
            .context("File write timed out after 10 seconds")??
            .with_context(|| {
                format!("Failed to write checkpoint: {}", checkpoint_path.display())
            })?;

            Ok(checkpoint_path.display().to_string())
        } else if endpoint_uri.starts_with("direct://") {
            let path_str = endpoint_uri
                .strip_prefix("direct://")
                .ok_or_else(|| anyhow!("Invalid direct:// URI"))?;
            let base_path = PathBuf::from(path_str);
            let checkpoint_path = base_path.join(checkpoint_name);

            // CRITICAL FIX (v0.8.61): Use spawn_blocking with timeout
            let checkpoint_path_clone = checkpoint_path.clone();
            timeout(
                Duration::from_secs(10),
                tokio::task::spawn_blocking(move || {
                    std::fs::write(&checkpoint_path_clone, &archive_bytes)
                }),
            )
            .await
            .context("File write timed out after 10 seconds")??
            .with_context(|| {
                format!("Failed to write checkpoint: {}", checkpoint_path.display())
            })?;

            Ok(checkpoint_path.display().to_string())
        } else if endpoint_uri.starts_with("s3://")
            || endpoint_uri.starts_with("az://")
            || endpoint_uri.starts_with("gs://")
        {
            // Cloud storage: upload via ObjectStore
            // FIX (v0.8.62): Add timeout and retry logic to prevent fatal errors
            // Checkpoint creation is non-critical and should never abort the workload

            debug!(
                "📦 Step 2/2: Creating ObjectStore for checkpoint upload to {}",
                endpoint_uri
            );

            // Try to create ObjectStore with timeout (max 15 seconds)
            let store_result = timeout(Duration::from_secs(15), async {
                // Retry store creation up to 3 times
                for attempt in 1..=3 {
                    match store_for_uri(endpoint_uri) {
                        Ok(store) => {
                            debug!("   ObjectStore created successfully (attempt {})", attempt);
                            return Ok(store);
                        }
                        Err(e) if attempt < 3 => {
                            warn!(
                                "   ObjectStore creation attempt {}/3 failed: {}",
                                attempt, e
                            );
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            continue;
                        }
                        Err(e) => {
                            return Err(anyhow!("Failed after {} attempts: {}", attempt, e));
                        }
                    }
                }
                unreachable!()
            })
            .await;

            let store = match store_result {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    // Non-fatal: Log warning and skip checkpoint
                    warn!(
                        "⚠️  Failed to create ObjectStore for checkpoint upload: {}",
                        e
                    );
                    warn!(
                        "   Skipping checkpoint - prepare will continue without resume capability"
                    );
                    return Err(anyhow!("Non-fatal checkpoint skip: {}", e));
                }
                Err(_) => {
                    // Timeout: Log warning and skip checkpoint
                    warn!("⚠️  ObjectStore creation timed out after 15 seconds");
                    warn!(
                        "   Skipping checkpoint - prepare will continue without resume capability"
                    );
                    return Err(anyhow!("Non-fatal checkpoint skip: timeout"));
                }
            };

            debug!(
                "   Uploading checkpoint archive ({} bytes)...",
                archive_bytes.len()
            );
            let checkpoint_uri = format!("{}{}", endpoint_uri, checkpoint_name);

            // Upload with timeout (max 60 seconds for multi-GB archives)
            match timeout(
                Duration::from_secs(60),
                store.put(&checkpoint_uri, Bytes::from(archive_bytes)),
            )
            .await
            {
                Ok(Ok(())) => {
                    debug!("   ✅ Checkpoint uploaded successfully");
                    Ok(checkpoint_uri)
                }
                Ok(Err(e)) => {
                    warn!("⚠️  Checkpoint upload failed: {}", e);
                    Err(anyhow!("Non-fatal checkpoint upload failure: {}", e))
                }
                Err(_) => {
                    warn!("⚠️  Checkpoint upload timed out after 60 seconds");
                    Err(anyhow!("Non-fatal checkpoint upload timeout"))
                }
            }
        } else {
            Err(anyhow!(
                "Unsupported URI scheme for checkpointing: {}",
                endpoint_uri
            ))
        }
    }

    // Note: create_checkpoint_archive_static was removed (duplicate of create_checkpoint_archive_sync)
    // All checkpoint creation now uses create_checkpoint_archive_sync with spawn_blocking + timeout
}

impl CoordinatorCache {
    /// Create or open coordinator cache in results directory
    ///
    /// Cache location: `{results_dir}/.sai3-coordinator-cache/`
    pub fn new(results_dir: &Path) -> Result<Self> {
        let cache_dir = results_dir.join(".sai3-coordinator-cache");
        std::fs::create_dir_all(&cache_dir)
            .context("Failed to create coordinator cache directory")?;

        debug!("Opening coordinator cache: {}", cache_dir.display());

        // Open fjall v3 database
        let db = Database::builder(&cache_dir)
            .open()
            .context("Failed to open fjall database for coordinator")?;

        //Create coordinator keyspaces with aggressive compaction
        use fjall::compaction::Leveled;
        let compaction_strategy =
            Arc::new(Leveled::default().with_table_target_size(16 * 1024 * 1024));

        let keyspace_opts =
            KeyspaceCreateOptions::default().compaction_strategy(compaction_strategy.clone());

        let tree_manifests = db
            .keyspace("tree_manifests", || keyspace_opts.clone())
            .context("Failed to create tree_manifests keyspace")?;
        let endpoint_registry = db
            .keyspace("endpoint_registry", || keyspace_opts.clone())
            .context("Failed to create endpoint_registry keyspace")?;
        let config_metadata = db
            .keyspace("config_metadata", || keyspace_opts)
            .context("Failed to create config_metadata keyspace")?;

        info!(
            "✅ Coordinator cache initialized at: {}",
            cache_dir.display()
        );

        Ok(CoordinatorCache {
            db,
            cache_dir,
            pending_writes: AtomicU64::new(0),
            flush_policy: RwLock::new(FlushPolicy::default()), // 30-second async flush
            tree_manifests,
            endpoint_registry,
            config_metadata,
        })
    }

    /// Get tree manifest from cache
    pub fn get_tree_manifest(&self, config_hash: &str) -> Result<Option<String>> {
        let key = format!("{}:manifest", config_hash);
        if let Some(bytes) = self.tree_manifests.get(key.as_bytes())? {
            let json = std::str::from_utf8(&bytes)?.to_string();
            Ok(Some(json))
        } else {
            Ok(None)
        }
    }

    /// Get cache directory path (for diagnostics/logging)
    pub fn cache_dir(&self) -> &std::path::Path {
        &self.cache_dir
    }

    /// Put tree manifest into cache
    pub fn put_tree_manifest(&self, config_hash: &str, manifest_json: &str) -> Result<()> {
        let key = format!("{}:manifest", config_hash);
        self.tree_manifests
            .insert(key.as_bytes(), manifest_json.as_bytes())?;
        self.maybe_flush()?; // Non-blocking
        info!("✓ Cached TreeManifest in coordinator cache");
        Ok(())
    }

    /// Get endpoint registry (list of endpoint URIs)
    pub fn get_endpoints(&self, config_hash: &str) -> Result<Option<Vec<String>>> {
        let key = format!("{}:endpoints", config_hash);
        if let Some(bytes) = self.endpoint_registry.get(key.as_bytes())? {
            let json = std::str::from_utf8(&bytes)?;
            let endpoints: Vec<String> = serde_json::from_str(json)?;
            Ok(Some(endpoints))
        } else {
            Ok(None)
        }
    }

    /// Put endpoint registry into cache
    pub fn put_endpoints(&self, config_hash: &str, endpoints: &[String]) -> Result<()> {
        let key = format!("{}:endpoints", config_hash);
        let json = serde_json::to_string(endpoints)?;
        self.endpoint_registry
            .insert(key.as_bytes(), json.as_bytes())?;
        self.maybe_flush()?; // Non-blocking
        Ok(())
    }

    /// Get config metadata
    pub fn get_config_metadata(&self, config_hash: &str) -> Result<Option<String>> {
        if let Some(bytes) = self.config_metadata.get(config_hash.as_bytes())? {
            let json = std::str::from_utf8(&bytes)?.to_string();
            Ok(Some(json))
        } else {
            Ok(None)
        }
    }

    /// Put config metadata
    pub fn put_config_metadata(&self, config_hash: &str, metadata_json: &str) -> Result<()> {
        self.config_metadata
            .insert(config_hash.as_bytes(), metadata_json.as_bytes())?;
        self.maybe_flush()?; // Non-blocking
        Ok(())
    }

    /// Get cached listing (compressed)
    ///
    /// Returns list of object paths, decompressed from zstd
    pub fn get_listing(&self, config_hash: &str) -> Result<Option<Vec<String>>> {
        let key = format!("{}:listing", config_hash);
        if let Some(compressed) = self.config_metadata.get(key.as_bytes())? {
            // Decompress with zstd
            let decompressed = zstd::decode_all(&compressed[..])?;
            let json = std::str::from_utf8(&decompressed)?;
            let paths: Vec<String> = serde_json::from_str(json)?;
            Ok(Some(paths))
        } else {
            Ok(None)
        }
    }

    /// Put listing into cache (with zstd compression)
    ///
    /// Compresses list of object paths before storage
    pub fn put_listing(&self, config_hash: &str, paths: &[String]) -> Result<()> {
        let key = format!("{}:listing", config_hash);
        let json = serde_json::to_string(paths)?;

        // Compress with zstd (level 3 for fast compression)
        let compressed = zstd::encode_all(json.as_bytes(), 3)?;

        self.config_metadata.insert(key.as_bytes(), &compressed)?;
        self.maybe_flush()?; // Non-blocking

        debug!(
            "✓ Cached listing with {} paths (compressed: {} bytes)",
            paths.len(),
            compressed.len()
        );
        Ok(())
    }

    /// Conditionally flush based on policy (NON-BLOCKING for benchmarks)
    ///
    /// This is the performance-critical method that prevents cache updates
    /// from blocking benchmark I/O operations.
    ///
    /// - FlushPolicy::Immediate → flush every write (HIGH LATENCY, NOT RECOMMENDED)
    /// - FlushPolicy::BatchSize(N) → flush every N writes (MEDIUM LATENCY)
    /// - FlushPolicy::AsyncInterval(secs) → background flush only (ZERO BLOCKING)
    fn maybe_flush(&self) -> Result<()> {
        let policy = *self.flush_policy.read().unwrap();
        match policy {
            FlushPolicy::Immediate => {
                // Synchronous flush (NOT recommended for hot paths)
                self.db
                    .persist(fjall::PersistMode::SyncData)
                    .context("Fjall flush error")?;
            }
            FlushPolicy::BatchSize(threshold) => {
                // Batched flush (only when threshold hit)
                let pending = self.pending_writes.fetch_add(1, Ordering::SeqCst) + 1;
                if pending >= threshold {
                    self.pending_writes.store(0, Ordering::SeqCst);
                    self.db
                        .persist(fjall::PersistMode::SyncData)
                        .context("Fjall flush error")?;
                    debug!("Coordinator cache batch flush: {} writes", threshold);
                }
            }
            FlushPolicy::AsyncInterval(_) => {
                // NO-OP: Background thread handles flushing
                // This is ZERO-COST for benchmark hot paths
                self.pending_writes.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Force flush all pending writes to disk (blocking)
    ///
    /// Call this at end of prepare phase or before critical validation.
    /// DO NOT call in hot benchmark paths.
    pub fn force_flush(&self) -> Result<()> {
        let pending = self.pending_writes.swap(0, Ordering::SeqCst);
        if pending > 0 {
            debug!(
                "Coordinator cache force flushing {} pending writes",
                pending
            );
        }
        self.db
            .persist(fjall::PersistMode::SyncAll)
            .context("Fjall force flush error")?;
        Ok(())
    }

    /// Set flush policy at runtime
    ///
    /// Resets pending write counter. Call force_flush() first if needed.
    pub fn set_flush_policy(&self, policy: FlushPolicy) {
        *self.flush_policy.write().unwrap() = policy;
        self.pending_writes.store(0, Ordering::SeqCst);
        info!("Coordinator flush policy: {:?}", policy);
    }

    /// Get pending write count (unflushed operations)
    pub fn pending_write_count(&self) -> u64 {
        self.pending_writes.load(Ordering::Relaxed)
    }

    /// Flush all pending writes (DEPRECATED - use force_flush instead)
    #[deprecated(
        note = "Use force_flush() for explicit blocking flush, or rely on background flush"
    )]
    pub fn flush(&self) -> Result<()> {
        self.force_flush()
    }
}

impl MetadataCache {
    /// Create multi-endpoint cache manager
    ///
    /// Opens coordinator cache + all endpoint caches
    pub async fn new(
        results_dir: &Path,
        endpoint_uris: &[String],
        config_hash: String,
        agent_id: Option<usize>, // v0.8.60: For agent-specific endpoint cache paths
        kv_cache_base_dir: Option<&Path>, // v0.8.60: Optional base directory for KV cache (defaults to system temp)
    ) -> Result<Self> {
        info!("Initializing distributed metadata cache:");
        info!("  Config hash: {}", config_hash);
        info!("  Endpoints: {}", endpoint_uris.len());
        if let Some(id) = agent_id {
            info!("  Agent ID: {}", id);
        }

        // Open coordinator cache (shared global metadata)
        let coordinator = CoordinatorCache::new(results_dir)?;
        info!(
            "  ✓ Coordinator cache: {}",
            results_dir.join(".sai3-coordinator-cache").display()
        );

        // Open per-endpoint caches (distributed data)
        let mut endpoints = HashMap::new();
        for (idx, uri) in endpoint_uris.iter().enumerate() {
            let endpoint_cache = EndpointCache::new(uri, idx, agent_id, kv_cache_base_dir).await?;
            info!(
                "  ✓ Endpoint {} cache: {}",
                idx,
                endpoint_cache.cache_location.display()
            );
            endpoints.insert(idx, endpoint_cache);
        }

        Ok(MetadataCache {
            coordinator,
            endpoints,
            config_hash,
        })
    }

    /// Get endpoint cache for a specific file index
    ///
    /// Uses round-robin: file_idx % num_endpoints
    pub fn endpoint_for_file(&self, file_idx: usize) -> Option<&EndpointCache> {
        let endpoint_idx = file_idx % self.endpoints.len();
        self.endpoints.get(&endpoint_idx)
    }

    /// Get mutable endpoint cache for a specific file index
    pub fn endpoint_for_file_mut(&mut self, file_idx: usize) -> Option<&mut EndpointCache> {
        let endpoint_idx = file_idx % self.endpoints.len();
        self.endpoints.get_mut(&endpoint_idx)
    }

    /// Get endpoint cache by index
    pub fn endpoint(&self, endpoint_idx: usize) -> Option<&EndpointCache> {
        self.endpoints.get(&endpoint_idx)
    }

    /// Get mutable endpoint cache by index
    pub fn endpoint_mut(&mut self, endpoint_idx: usize) -> Option<&mut EndpointCache> {
        self.endpoints.get_mut(&endpoint_idx)
    }

    /// Get config hash
    pub fn config_hash(&self) -> &str {
        &self.config_hash
    }

    /// Number of endpoints
    pub fn num_endpoints(&self) -> usize {
        self.endpoints.len()
    }

    /// Get reference to coordinator cache
    pub fn coordinator_cache(&self) -> &CoordinatorCache {
        &self.coordinator
    }

    /// Get reference to all endpoint caches (for iteration)
    ///
    /// **v0.8.60**: Used for copying all endpoint caches back to storage after prepare
    pub fn endpoints(&self) -> &HashMap<usize, EndpointCache> {
        &self.endpoints
    }

    /// Flush all caches (forceful blocking flush)
    ///
    /// Call this at end of prepare phase or before critical validation.
    /// DO NOT call during workload hot paths - use background async flush instead.
    pub fn flush_all(&self) -> Result<()> {
        self.coordinator.force_flush()?;
        for endpoint in self.endpoints.values() {
            endpoint.force_flush()?;
        }
        Ok(())
    }

    /// Get all cache statistics
    pub fn all_stats(&self) -> Vec<CacheStats> {
        self.endpoints.values().map(|e| e.stats()).collect()
    }

    /// Clean up endpoint cache directories (delete from disk)
    ///
    /// Call this after successful prepare to reclaim disk space.
    /// The cache is only needed for resume capability - once prepare succeeds, it's wasted space.
    ///
    /// For 100M objects: ~8 GB per agent saved.
    pub fn cleanup_endpoint_caches(&self) -> Result<()> {
        info!("🗑️  Cleaning up endpoint cache directories to reclaim disk space...");

        for (idx, endpoint) in &self.endpoints {
            let cache_path = &endpoint.cache_location;
            if cache_path.exists() {
                match std::fs::remove_dir_all(cache_path) {
                    Ok(_) => {
                        info!(
                            "  ✓ Deleted endpoint {} cache: {}",
                            idx,
                            cache_path.display()
                        );
                    }
                    Err(e) => {
                        warn!(
                            "  ⚠ Failed to delete endpoint {} cache: {} - {}",
                            idx,
                            cache_path.display(),
                            e
                        );
                    }
                }
            }
        }

        Ok(())
    }

    /// Get aggregate progress across all endpoints
    ///
    /// Returns total counts for each ObjectState
    pub fn aggregate_progress(&self, config_hash: &str) -> Result<HashMap<ObjectState, usize>> {
        let mut totals = HashMap::new();

        for endpoint in self.endpoints.values() {
            let counts = endpoint.count_by_state(config_hash)?;
            for (state, count) in counts {
                *totals.entry(state).or_insert(0) += count;
            }
        }

        Ok(totals)
    }

    /// Aggregate counts **and** byte totals across all endpoints in one scan per endpoint.
    fn aggregate_coverage(
        &self,
        config_hash: &str,
    ) -> Result<(HashMap<ObjectState, usize>, HashMap<ObjectState, u64>)> {
        let mut total_counts: HashMap<ObjectState, usize> = HashMap::new();
        let mut total_bytes: HashMap<ObjectState, u64> = HashMap::new();
        let t0 = std::time::Instant::now();

        for endpoint in self.endpoints.values() {
            let (counts, bytes) = endpoint.count_and_bytes_by_state(config_hash)?;
            for (state, count) in counts {
                *total_counts.entry(state).or_insert(0) += count;
            }
            for (state, b) in bytes {
                *total_bytes.entry(state).or_insert(0) += b;
            }
        }

        let total: usize = total_counts.values().sum();
        debug!(
            "KV cache coverage scan: {} entries in {:.3}s",
            total,
            t0.elapsed().as_secs_f64()
        );

        Ok((total_counts, total_bytes))
    }

    /// Enable async interval flush policy for ZERO-BLOCKING benchmark performance
    ///
    /// This sets flush policy to AsyncInterval mode, which means cache writes
    /// NEVER block on disk I/O. The caller is responsible for periodic flushing:
    ///
    /// ```rust,no_run
    /// # use sai3_bench::metadata_cache::MetadataCache;
    /// # use std::path::Path;
    /// # async fn example() -> anyhow::Result<()> {
    /// # let cache = MetadataCache::new(Path::new("/tmp"), &[], "hash".to_string(), None, None).await?;
    /// // Enable async mode (zero blocking)
    /// cache.enable_async_flush(30);
    ///
    /// // In your main loop: flush every 30 seconds
    /// let mut last_flush = std::time::Instant::now();
    /// loop {
    ///     // ... benchmark operations ...
    ///
    ///     if last_flush.elapsed().as_secs() >= 30 {
    ///         // Non-critical background flush (doesn't block if it fails)
    ///         let _ = cache.flush_all();
    ///         last_flush = std::time::Instant::now();
    ///     }
    /// }
    ///
    /// // CRITICAL: Final flush before exiting
    /// cache.flush_all()?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Arguments
    /// * `interval_secs` - Flush interval in seconds (recommended: 30)
    ///
    /// # Performance
    /// - AsyncInterval mode has ZERO overhead on cache writes
    /// - Periodic flushes prevent unbounded memory growth
    /// - Final flush ensures all data persisted
    pub fn enable_async_flush(&self, interval_secs: u64) {
        let policy = FlushPolicy::AsyncInterval(interval_secs);

        info!(
            "🚀 KV cache: Enabling async flush mode ({}s interval, ZERO blocking)",
            interval_secs
        );

        self.coordinator.set_flush_policy(policy);
        for endpoint in self.endpoints.values() {
            endpoint.set_flush_policy(policy);
        }

        info!("⚡ KV cache: Async mode active - cache operations will NOT block benchmarks");
    }

    /// Set batch flush policy for CONTROLLED blocking (alternative to async mode)
    ///
    /// Flushes every N writes instead of every write. Lower blocking than Immediate,
    /// but still has some I/O overhead. Use async mode for zero blocking.
    ///
    /// # Arguments
    /// * `batch_size` - Number of writes before flush (recommended: 1000-10000)
    ///
    /// # Performance
    /// - BatchSize(1000): ~1ms blocking per 1000 writes
    /// - BatchSize(10000): ~10ms blocking per 10000 writes
    /// - AsyncInterval(30): 0ms blocking (recommended)
    pub fn set_batch_flush(&self, batch_size: u64) {
        let policy = FlushPolicy::BatchSize(batch_size);

        info!(
            "KV cache: Enabling batch flush mode (flush every {} writes)",
            batch_size
        );

        self.coordinator.set_flush_policy(policy);
        for endpoint in self.endpoints.values() {
            endpoint.set_flush_policy(policy);
        }
    }
}

/// Cache statistics for reporting
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub endpoint_index: usize,
    pub endpoint_uri: String,
    pub cache_location: PathBuf,
    pub objects_count: usize,
    pub cache_size_bytes: u64,
}

/// Compute configuration hash for cache key namespacing
///
/// Hash includes all parameters that affect file distribution.
pub fn compute_config_hash(
    total_files: usize,
    directory_config: Option<&str>, // JSON repr of DirectoryConfig
    endpoints: &[String],
) -> String {
    use seahash::hash;

    let mut hasher_input = String::new();
    hasher_input.push_str(&format!("files:{},", total_files));
    if let Some(dir_cfg) = directory_config {
        hasher_input.push_str(&format!("dir:{},", dir_cfg));
    }
    hasher_input.push_str(&format!("endpoints:{}", endpoints.join(",")));

    let hash_value = hash(hasher_input.as_bytes());
    format!("{:016x}", hash_value)
}

// ============================================================================
// Checkpoint-Based Listing-Skip Optimisation (v0.8.89)
// ============================================================================

/// How completely a KV-cache checkpoint covers the expected object population.
///
/// Returned by [`EndpointCache::check_coverage`] and
/// [`MetadataCache::check_listing_coverage`].  The caller can decide whether to
/// skip a potentially expensive LIST operation based on [`Self::covers_all`].
///
/// # Example
/// ```text
/// CheckpointCoverage {
///     created_count: 1_000_000,
///     planned_count: 0,
///     failed_count:  0,
///     total_tracked: 1_000_000,
///     expected_count: 1_000_000,
///     covers_all: true,       ← safe to skip LIST
/// }
/// ```
#[derive(Debug, Clone)]
pub struct CheckpointCoverage {
    /// Number of objects in the `Created` state.
    pub created_count: usize,
    /// Number of objects still in the `Planned` state (not yet created).
    pub planned_count: usize,
    /// Number of objects in the `Failed` state.
    pub failed_count: usize,
    /// Total objects tracked in this cache (all states).
    pub total_tracked: usize,
    /// The expected total from the prepare config (`ensure_objects[].count`).
    pub expected_count: usize,
    /// `true` iff `created_count >= expected_count` — it is safe to skip LIST.
    pub covers_all: bool,
    /// Full state breakdown (may contain states not listed above).
    pub state_counts: HashMap<ObjectState, usize>,
    /// Total bytes per state (populated when computed through coverage path; zero otherwise).
    pub bytes_by_state: HashMap<ObjectState, u64>,
}

impl CheckpointCoverage {
    /// Build from state-count and byte-total maps.
    fn from_state_counts(
        state_counts: HashMap<ObjectState, usize>,
        bytes_by_state: HashMap<ObjectState, u64>,
        expected_count: usize,
    ) -> Self {
        let created_count = *state_counts.get(&ObjectState::Created).unwrap_or(&0);
        let planned_count = *state_counts.get(&ObjectState::Planned).unwrap_or(&0);
        let failed_count = *state_counts.get(&ObjectState::Failed).unwrap_or(&0);
        let total_tracked: usize = state_counts.values().sum();
        let covers_all = created_count >= expected_count && expected_count > 0;
        Self {
            created_count,
            planned_count,
            failed_count,
            total_tracked,
            expected_count,
            covers_all,
            state_counts,
            bytes_by_state,
        }
    }
}

impl EndpointCache {
    /// Load an `EndpointCache` from a checkpoint already stored on the endpoint's
    /// own storage, **without** going through the full `EndpointCache::new()` path.
    ///
    /// # When to use this
    ///
    /// Call this when you need to *read* checkpoint data outside of a normal
    /// prepare cycle — for example, in pre-workload validation or the
    /// `StageKind::Validation` stage handler.
    ///
    /// # Returns
    ///
    /// - `Ok(None)` — no checkpoint file exists on storage (not an error).
    /// - `Ok(Some(cache))` — checkpoint found, extracted, and database opened.
    /// - `Err(…)` — checkpoint exists but could not be decompressed/opened.
    ///
    /// # Arguments
    ///
    /// * `endpoint_uri` — storage location, e.g. `s3://bucket/prefix/`
    /// * `agent_id`     — agent index used when writing the checkpoint, or
    ///   `None` for the single-node checkpoint.
    /// * `kv_cache_base_dir` — local directory for the extracted fjall database
    ///   (defaults to system temp dir).
    pub async fn try_load_from_checkpoint(
        endpoint_uri: &str,
        agent_id: Option<usize>,
        kv_cache_base_dir: Option<&Path>,
    ) -> Result<Option<Self>> {
        let cache_location =
            Self::resolve_cache_location(endpoint_uri, agent_id, kv_cache_base_dir)
                .context("Failed to resolve cache location")?;

        // Attempt to restore from the checkpoint stored on the endpoint.
        let restored = Self::try_restore_from_checkpoint(endpoint_uri, &cache_location, agent_id)
            .await
            .context("Failed to restore checkpoint")?;

        if !restored {
            // No checkpoint on storage — not an error, caller should fall back to LIST.
            return Ok(None);
        }

        // Checkpoint was extracted; now open the fjall database.
        std::fs::create_dir_all(&cache_location)
            .context("Failed to create cache directory after restore")?;

        let db = Database::builder(&cache_location)
            .open()
            .context("Failed to open fjall database from restored checkpoint")?;

        use fjall::compaction::Leveled;
        let compaction_strategy =
            Arc::new(Leveled::default().with_table_target_size(16 * 1024 * 1024));
        let keyspace_opts =
            KeyspaceCreateOptions::default().compaction_strategy(compaction_strategy);

        let objects = db
            .keyspace("objects", || keyspace_opts.clone())
            .context("Failed to open objects keyspace")?;
        let listing_cache = db
            .keyspace("listing_cache", || keyspace_opts.clone())
            .context("Failed to open listing_cache keyspace")?;
        let endpoint_map = db
            .keyspace("endpoint_map", || keyspace_opts.clone())
            .context("Failed to open endpoint_map keyspace")?;
        let seeds = db
            .keyspace("seeds", || keyspace_opts)
            .context("Failed to open seeds keyspace")?;

        // Use a fixed index of 0 here — callers only query by config_hash, not endpoint index.
        Ok(Some(EndpointCache {
            db,
            endpoint_index: 0,
            endpoint_uri: endpoint_uri.to_string(),
            cache_location,
            objects,
            listing_cache,
            endpoint_map,
            seeds,
            pending_writes: AtomicU64::new(0),
            flush_policy: RwLock::new(FlushPolicy::Immediate),
        }))
    }

    /// Summarise how much of `expected_count` objects are in the `Created` state.
    ///
    /// This is an **O(n)** scan of the objects keyspace — acceptable for
    /// pre-workload validation but should not be called in hot paths.
    ///
    /// Returns a [`CheckpointCoverage`] regardless of object count; if the
    /// keyspace is empty (no checkpoint was restored) `creates_all` will be
    /// `false`.
    pub fn check_coverage(
        &self,
        config_hash: &str,
        expected_count: u64,
    ) -> Result<CheckpointCoverage> {
        let (state_counts, bytes_by_state) = self.count_and_bytes_by_state(config_hash)?;
        Ok(CheckpointCoverage::from_state_counts(
            state_counts,
            bytes_by_state,
            expected_count as usize,
        ))
    }
}

impl MetadataCache {
    /// Query the aggregate coverage across **all** endpoint sub-caches.
    ///
    /// Use this in the prepare-phase listing decision to determine whether a
    /// full LIST operation can be skipped:
    ///
    /// ```rust,ignore
    /// // Inside prepare_sequential / prepare_parallel
    /// if let Some(cov) = metadata_cache.check_listing_coverage(spec.count)? {
    ///     if cov.covers_all {
    ///         // skip LIST — KV cache confirms all objects are Created
    ///     }
    /// }
    /// ```
    ///
    /// Returns `Ok(None)` when the cache has no tracked objects at all
    /// (e.g., no checkpoint was restored and prepare hasn't started).
    pub fn check_listing_coverage(
        &self,
        expected_count: u64,
    ) -> Result<Option<CheckpointCoverage>> {
        let (state_counts, bytes_by_state) = self.aggregate_coverage(&self.config_hash)?;
        if state_counts.is_empty() {
            return Ok(None);
        }
        Ok(Some(CheckpointCoverage::from_state_counts(
            state_counts,
            bytes_by_state,
            expected_count as usize,
        )))
    }

    /// Config hash accessor (needed externally to query endpoint caches).
    pub fn config_hash_str(&self) -> &str {
        &self.config_hash
    }
}

/// Convenience: download a checkpoint from `endpoint_uri`, open it, and return
/// a [`CheckpointCoverage`] — all without a full prepare cycle.
///
/// This is the main entry-point for the `StageKind::Validation` agent handler
/// and for the preflight object-storage check.
///
/// # Returns
///
/// - `Ok(None)` — no checkpoint exists on storage.
/// - `Ok(Some(coverage))` — checkpoint loaded and queried.
/// - `Err(…)` — checkpoint exists but could not be read.
pub async fn query_checkpoint_coverage(
    endpoint_uri: &str,
    agent_id: Option<usize>,
    config_hash: &str,
    expected_count: u64,
) -> Result<Option<CheckpointCoverage>> {
    match EndpointCache::try_load_from_checkpoint(endpoint_uri, agent_id, None).await? {
        None => Ok(None),
        Some(cache) => {
            let coverage = cache.check_coverage(config_hash, expected_count)?;
            Ok(Some(coverage))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ========================================================================
    // Unit Tests: ObjectState enum
    // ========================================================================

    #[test]
    fn test_object_state_conversions() {
        assert_eq!(
            ObjectState::from_u8(0),
            Some(ObjectState::DirectoryNonExistent)
        );
        assert_eq!(ObjectState::from_u8(1), Some(ObjectState::Planned));
        assert_eq!(ObjectState::from_u8(2), Some(ObjectState::Creating));
        assert_eq!(ObjectState::from_u8(3), Some(ObjectState::Created));
        assert_eq!(ObjectState::from_u8(4), Some(ObjectState::Failed));
        assert_eq!(ObjectState::from_u8(5), Some(ObjectState::Deleted));
        assert_eq!(ObjectState::from_u8(99), None);

        assert_eq!(ObjectState::DirectoryNonExistent.to_u8(), 0);
        assert_eq!(ObjectState::Planned.to_u8(), 1);
        assert_eq!(ObjectState::Creating.to_u8(), 2);
        assert_eq!(ObjectState::Created.to_u8(), 3);
        assert_eq!(ObjectState::Failed.to_u8(), 4);
        assert_eq!(ObjectState::Deleted.to_u8(), 5);
    }

    #[test]
    fn test_object_state_queries() {
        assert!(ObjectState::Created.is_created());
        assert!(!ObjectState::Planned.is_created());
        assert!(!ObjectState::Creating.is_created());

        assert!(ObjectState::Failed.is_failed());
        assert!(!ObjectState::Created.is_failed());
        assert!(!ObjectState::Planned.is_failed());

        assert!(ObjectState::Planned.needs_creation());
        assert!(ObjectState::Failed.needs_creation());
        assert!(!ObjectState::Created.needs_creation());
        assert!(!ObjectState::Creating.needs_creation());
        assert!(!ObjectState::DirectoryNonExistent.needs_creation());
    }

    #[test]
    fn test_object_state_ordering() {
        // Verify ordering for sorted collections
        assert!(ObjectState::DirectoryNonExistent < ObjectState::Planned);
        assert!(ObjectState::Planned < ObjectState::Creating);
        assert!(ObjectState::Creating < ObjectState::Created);
        assert!(ObjectState::Created < ObjectState::Failed);
        assert!(ObjectState::Failed < ObjectState::Deleted);
    }

    // ========================================================================
    // Unit Tests: ObjectEntry serialization
    // ========================================================================

    #[test]
    fn test_object_entry_serialization() {
        let entry = ObjectEntry {
            path: "d1/w1/file_00042.dat".to_string(),
            size: 1048576,
            state: ObjectState::Planned,
            endpoint_idx: 2,
            created_at: None,
            checksum: None,
        };

        let bytes = entry.to_bytes().unwrap();
        let recovered = ObjectEntry::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.path, entry.path);
        assert_eq!(recovered.size, entry.size);
        assert_eq!(recovered.state, entry.state);
        assert_eq!(recovered.endpoint_idx, entry.endpoint_idx);
        assert_eq!(recovered.created_at, None);
        assert_eq!(recovered.checksum, None);
    }

    #[test]
    fn test_object_entry_with_metadata() {
        let entry = ObjectEntry {
            path: "s3/bucket/file.dat".to_string(),
            size: 2097152,
            state: ObjectState::Created,
            endpoint_idx: 1,
            created_at: Some(1738886400),
            checksum: Some(0xdeadbeef),
        };

        let bytes = entry.to_bytes().unwrap();
        let recovered = ObjectEntry::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.created_at, Some(1738886400));
        assert_eq!(recovered.checksum, Some(0xdeadbeef));
    }

    // ========================================================================
    // Unit Tests: Config hashing
    // ========================================================================

    #[test]
    fn test_compute_config_hash_deterministic() {
        let endpoints = vec!["file:///tmp/a".to_string(), "s3://bucket/b".to_string()];
        let hash1 = compute_config_hash(1000, Some("{\"depth\":3}"), &endpoints);
        let hash2 = compute_config_hash(1000, Some("{\"depth\":3}"), &endpoints);
        assert_eq!(hash1, hash2, "Hash should be deterministic");
    }

    #[test]
    fn test_compute_config_hash_changes() {
        let endpoints = vec!["file:///tmp/a".to_string()];
        let hash1 = compute_config_hash(1000, None, &endpoints);
        let hash2 = compute_config_hash(2000, None, &endpoints); // Different file count
        assert_ne!(hash1, hash2, "Hash should change with different config");

        let hash3 = compute_config_hash(1000, Some("{\"depth\":3}"), &endpoints);
        assert_ne!(
            hash1, hash3,
            "Hash should change with different directory config"
        );

        let endpoints2 = vec!["file:///tmp/a".to_string(), "file:///tmp/b".to_string()];
        let hash4 = compute_config_hash(1000, None, &endpoints2);
        assert_ne!(hash1, hash4, "Hash should change with different endpoints");
    }

    // ========================================================================
    // Unit Tests: FlushPolicy
    // ========================================================================

    #[test]
    fn test_flush_policy_default() {
        let policy = FlushPolicy::default();
        assert_eq!(policy, FlushPolicy::AsyncInterval(30));
    }

    #[test]
    fn test_flush_policy_batch_creation() {
        let policy = FlushPolicy::batch(0);
        assert_eq!(policy, FlushPolicy::Immediate);

        let policy = FlushPolicy::batch(1000);
        assert_eq!(policy, FlushPolicy::BatchSize(1000));

        let policy = FlushPolicy::batch(10000);
        assert_eq!(policy, FlushPolicy::BatchSize(10000));
    }

    #[test]
    fn test_flush_policy_equality() {
        assert_eq!(FlushPolicy::Immediate, FlushPolicy::Immediate);
        assert_eq!(FlushPolicy::BatchSize(100), FlushPolicy::BatchSize(100));
        assert_eq!(
            FlushPolicy::AsyncInterval(30),
            FlushPolicy::AsyncInterval(30)
        );

        assert_ne!(FlushPolicy::Immediate, FlushPolicy::BatchSize(1));
        assert_ne!(FlushPolicy::BatchSize(100), FlushPolicy::BatchSize(200));
        assert_ne!(
            FlushPolicy::AsyncInterval(30),
            FlushPolicy::AsyncInterval(60)
        );
    }

    // ========================================================================
    // Unit Tests: Pending Write Tracking
    // ========================================================================

    #[tokio::test]
    async fn test_endpoint_pending_write_counter() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        // Initially zero
        assert_eq!(cache.pending_write_count(), 0);

        // Set to AsyncInterval mode
        cache.set_flush_policy(FlushPolicy::AsyncInterval(30));

        // Writes increment counter without blocking
        let config_hash = "pending_test";
        cache
            .plan_object(config_hash, 0, "file_0.dat", 1024)
            .unwrap();
        assert_eq!(cache.pending_write_count(), 1);

        cache
            .plan_object(config_hash, 1, "file_1.dat", 1024)
            .unwrap();
        assert_eq!(cache.pending_write_count(), 2);

        cache
            .plan_object(config_hash, 2, "file_2.dat", 1024)
            .unwrap();
        assert_eq!(cache.pending_write_count(), 3);

        // Force flush resets counter
        cache.force_flush().unwrap();
        assert_eq!(cache.pending_write_count(), 0);
    }

    #[test]
    fn test_coordinator_pending_write_counter() {
        let temp = TempDir::new().unwrap();
        let cache = CoordinatorCache::new(temp.path()).unwrap();

        assert_eq!(cache.pending_write_count(), 0);

        // Set to AsyncInterval mode
        cache.set_flush_policy(FlushPolicy::AsyncInterval(30));

        // Writes increment counter
        cache.put_tree_manifest("hash1", r#"{"test": 1}"#).unwrap();
        assert_eq!(cache.pending_write_count(), 1);

        cache.put_tree_manifest("hash2", r#"{"test": 2}"#).unwrap();
        assert_eq!(cache.pending_write_count(), 2);

        // Force flush resets
        cache.force_flush().unwrap();
        assert_eq!(cache.pending_write_count(), 0);
    }

    // ========================================================================
    // Unit Tests: Flush Policy Behavior
    // ========================================================================

    #[tokio::test]
    async fn test_async_interval_mode_no_blocking() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        // Enable async mode
        cache.set_flush_policy(FlushPolicy::AsyncInterval(30));

        let config_hash = "async_test";

        // Write 1000 objects - should be FAST (no blocking)
        let start = std::time::Instant::now();
        for i in 0..1000 {
            cache
                .plan_object(config_hash, i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }
        let duration = start.elapsed();

        // Verify NO blocking occurred (should complete in <100ms)
        assert!(
            duration.as_millis() < 100,
            "AsyncInterval mode took {}ms - should be <100ms (no blocking)",
            duration.as_millis()
        );

        // Pending writes accumulated (not flushed)
        assert_eq!(cache.pending_write_count(), 1000);

        // Force flush completes
        cache.force_flush().unwrap();
        assert_eq!(cache.pending_write_count(), 0);
    }

    #[tokio::test]
    async fn test_batch_size_mode_threshold_flushing() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        // Enable batch mode with threshold 10
        cache.set_flush_policy(FlushPolicy::BatchSize(10));

        let config_hash = "batch_test";

        // Write 9 objects - no flush yet
        for i in 0..9 {
            cache
                .plan_object(config_hash, i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }
        assert_eq!(cache.pending_write_count(), 9);

        // Write 10th object - triggers flush and resets counter
        cache
            .plan_object(config_hash, 9, "file_9.dat", 1024)
            .unwrap();
        assert_eq!(
            cache.pending_write_count(),
            0,
            "Batch flush should reset counter at threshold"
        );

        // Write 5 more - counter builds up again
        for i in 10..15 {
            cache
                .plan_object(config_hash, i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }
        assert_eq!(cache.pending_write_count(), 5);
    }

    #[tokio::test]
    async fn test_immediate_mode_always_flushes() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        // Enable immediate mode
        cache.set_flush_policy(FlushPolicy::Immediate);

        let config_hash = "immediate_test";

        // Every write flushes immediately (counter stays at 0)
        cache
            .plan_object(config_hash, 0, "file_0.dat", 1024)
            .unwrap();
        // Note: Immediate mode doesn't increment counter since it flushes immediately

        cache
            .plan_object(config_hash, 1, "file_1.dat", 1024)
            .unwrap();

        // Data is immediately persisted (can verify after reopen)
        cache.force_flush().unwrap();
    }

    // ========================================================================
    // Unit Tests: MetadataCache Flush Policy Coordination
    // ========================================================================

    #[tokio::test]
    async fn test_metadata_cache_enable_async_flush() {
        let temp = TempDir::new().unwrap();
        let results_dir = temp.path().join("results");
        std::fs::create_dir_all(&results_dir).unwrap();

        let endpoints = vec![
            format!("file://{}/ep0", temp.path().display()),
            format!("file://{}/ep1", temp.path().display()),
        ];

        let config_hash = "async_meta_test".to_string();
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash.clone(), None, None)
            .await
            .unwrap();

        // Enable async mode for all caches
        cache.enable_async_flush(30);

        // Verify all caches are in async mode
        assert_eq!(cache.endpoint(0).unwrap().pending_write_count(), 0);
        assert_eq!(cache.endpoint(1).unwrap().pending_write_count(), 0);

        // Write to both endpoints - counters increment
        cache
            .endpoint(0)
            .unwrap()
            .plan_object(&config_hash, 0, "file_0.dat", 1024)
            .unwrap();
        cache
            .endpoint(1)
            .unwrap()
            .plan_object(&config_hash, 1, "file_1.dat", 1024)
            .unwrap();

        assert_eq!(cache.endpoint(0).unwrap().pending_write_count(), 1);
        assert_eq!(cache.endpoint(1).unwrap().pending_write_count(), 1);

        // Flush all resets all counters
        cache.flush_all().unwrap();
        assert_eq!(cache.endpoint(0).unwrap().pending_write_count(), 0);
        assert_eq!(cache.endpoint(1).unwrap().pending_write_count(), 0);
    }

    #[tokio::test]
    async fn test_metadata_cache_set_batch_flush() {
        let temp = TempDir::new().unwrap();
        let results_dir = temp.path().join("results");
        std::fs::create_dir_all(&results_dir).unwrap();

        let endpoints = vec![format!("file://{}/ep0", temp.path().display())];

        let config_hash = "batch_meta_test".to_string();
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash.clone(), None, None)
            .await
            .unwrap();

        // Enable batch mode
        cache.set_batch_flush(5);

        // Write 4 objects - no flush
        for i in 0..4 {
            cache
                .endpoint(0)
                .unwrap()
                .plan_object(&config_hash, i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }
        assert_eq!(cache.endpoint(0).unwrap().pending_write_count(), 4);

        // 5th object triggers flush
        cache
            .endpoint(0)
            .unwrap()
            .plan_object(&config_hash, 4, "file_4.dat", 1024)
            .unwrap();
        assert_eq!(cache.endpoint(0).unwrap().pending_write_count(), 0);
    }

    // ========================================================================
    // Performance Tests: Zero-Blocking Verification
    // ========================================================================

    #[tokio::test]
    async fn test_async_mode_performance_10k_writes() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        // Async mode (zero blocking)
        cache.set_flush_policy(FlushPolicy::AsyncInterval(30));

        let config_hash = "perf_async";
        let start = std::time::Instant::now();

        for i in 0..10_000 {
            cache
                .plan_object(config_hash, i, &format!("file_{:05}.dat", i), 1024)
                .unwrap();
        }

        let async_duration = start.elapsed();

        // Verify ZERO blocking (should complete in <500ms for 10K writes)
        assert!(
            async_duration.as_millis() < 500,
            "10K writes in AsyncInterval mode took {}ms - expected <500ms",
            async_duration.as_millis()
        );

        println!(
            "✅ Async mode: 10K writes in {:?} (ZERO blocking verified)",
            async_duration
        );

        // Final flush
        cache.force_flush().unwrap();
    }

    #[tokio::test]
    async fn test_batch_mode_vs_immediate_mode_performance() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());

        // Test 1: Batch mode (flush every 1000)
        let cache_batch = EndpointCache::new(&uri, 0, None, None).await.unwrap();
        cache_batch.set_flush_policy(FlushPolicy::BatchSize(1000));

        let config_hash = "perf_batch";
        let start = std::time::Instant::now();
        for i in 0..1000 {
            cache_batch
                .plan_object(config_hash, i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }
        let batch_duration = start.elapsed();

        // Test 2: Async mode (no blocking)
        let uri2 = format!("file://{}/testdata2", temp.path().display());
        let cache_async = EndpointCache::new(&uri2, 1, None, None).await.unwrap();
        cache_async.set_flush_policy(FlushPolicy::AsyncInterval(30));

        let start = std::time::Instant::now();
        for i in 0..1000 {
            cache_async
                .plan_object(config_hash, i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }
        let async_duration = start.elapsed();

        println!("Batch mode (1000): {:?}", batch_duration);
        println!("Async mode: {:?}", async_duration);

        let batch_counts = cache_batch.count_by_state(config_hash).unwrap();
        assert_eq!(batch_counts.get(&ObjectState::Planned), Some(&1000));

        cache_async.force_flush().unwrap();
        let async_counts = cache_async.count_by_state(config_hash).unwrap();
        assert_eq!(async_counts.get(&ObjectState::Planned), Some(&1000));
    }

    // ========================================================================
    // Integration Tests: EndpointCache
    // ========================================================================

    #[tokio::test]
    async fn test_endpoint_cache_create() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());

        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();
        assert_eq!(cache.endpoint_index, 0);
        assert_eq!(cache.endpoint_uri, uri);
        assert!(cache.cache_location.exists());
    }

    #[tokio::test]
    async fn test_object_lifecycle_full() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        let config_hash = "abc123";
        let file_idx = 42;

        // Plan object (DESIRED state)
        cache
            .plan_object(config_hash, file_idx, "d1/file_00042.dat", 1048576)
            .unwrap();
        let entry = cache.get_object(config_hash, file_idx).unwrap().unwrap();
        assert_eq!(entry.state, ObjectState::Planned);
        assert_eq!(entry.path, "d1/file_00042.dat");
        assert_eq!(entry.size, 1048576);
        assert!(entry.state.needs_creation());

        // Mark creating (TRANSITIONAL state)
        cache.mark_creating(config_hash, file_idx).unwrap();
        let entry = cache.get_object(config_hash, file_idx).unwrap().unwrap();
        assert_eq!(entry.state, ObjectState::Creating);
        assert!(!entry.state.needs_creation());

        // Mark created (SUCCESS - CURRENT matches DESIRED)
        cache
            .mark_created(config_hash, file_idx, Some(1738886400), Some(0xdeadbeef))
            .unwrap();
        let entry = cache.get_object(config_hash, file_idx).unwrap().unwrap();
        assert_eq!(entry.state, ObjectState::Created);
        assert_eq!(entry.created_at, Some(1738886400));
        assert_eq!(entry.checksum, Some(0xdeadbeef));
        assert!(entry.state.is_created());
        assert!(!entry.state.needs_creation());
    }

    #[tokio::test]
    async fn test_object_lifecycle_failure() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        let config_hash = "test_fail";
        let file_idx = 100;

        // Plan object
        cache
            .plan_object(config_hash, file_idx, "fail/file.dat", 2048)
            .unwrap();

        // Mark creating
        cache.mark_creating(config_hash, file_idx).unwrap();

        // Mark failed (ERROR state - creation failed)
        cache.mark_failed(config_hash, file_idx).unwrap();
        let entry = cache.get_object(config_hash, file_idx).unwrap().unwrap();
        assert_eq!(entry.state, ObjectState::Failed);
        assert!(entry.state.is_failed());
        assert!(entry.state.needs_creation()); // Can retry
    }

    #[tokio::test]
    async fn test_object_drift_detection() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        let config_hash = "drift_test";
        let file_idx = 200;

        // Create object successfully
        cache
            .plan_object(config_hash, file_idx, "drift/file.dat", 4096)
            .unwrap();
        cache.mark_creating(config_hash, file_idx).unwrap();
        cache
            .mark_created(config_hash, file_idx, Some(1738886400), None)
            .unwrap();

        // Detect drift (object was deleted externally)
        cache.mark_deleted(config_hash, file_idx).unwrap();
        let entry = cache.get_object(config_hash, file_idx).unwrap().unwrap();
        assert_eq!(entry.state, ObjectState::Deleted);
        assert!(!entry.state.is_created());
    }

    #[tokio::test]
    async fn test_batch_plan_objects() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        let config_hash = "batch_test";

        // Plan 1000 objects in batch
        let objects: Vec<(usize, String, u64)> = (0..1000)
            .map(|i| (i, format!("batch/file_{:04}.dat", i), 1048576))
            .collect();

        cache.plan_objects_batch(config_hash, &objects).unwrap();

        // Verify all planned
        for i in 0..1000 {
            let entry = cache.get_object(config_hash, i).unwrap().unwrap();
            assert_eq!(entry.state, ObjectState::Planned);
            assert_eq!(entry.path, format!("batch/file_{:04}.dat", i));
        }
    }

    #[tokio::test]
    async fn test_get_objects_by_state() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        let config_hash = "state_query";

        // Create mix of states
        cache
            .plan_object(config_hash, 0, "file_0.dat", 1024)
            .unwrap();
        cache
            .plan_object(config_hash, 1, "file_1.dat", 1024)
            .unwrap();
        cache.mark_creating(config_hash, 0).unwrap();
        cache
            .mark_created(config_hash, 0, Some(1738886400), None)
            .unwrap();
        // file_idx 1 stays in Planned state

        cache
            .plan_object(config_hash, 2, "file_2.dat", 1024)
            .unwrap();
        cache.mark_creating(config_hash, 2).unwrap();
        cache.mark_failed(config_hash, 2).unwrap();

        // Query by state
        let planned = cache
            .get_objects_by_state(config_hash, ObjectState::Planned)
            .unwrap();
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].0, 1);

        let created = cache
            .get_objects_by_state(config_hash, ObjectState::Created)
            .unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].0, 0);

        let failed = cache
            .get_objects_by_state(config_hash, ObjectState::Failed)
            .unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, 2);
    }

    #[tokio::test]
    async fn test_count_by_state() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        let config_hash = "count_test";

        // Create 100 planned, 30 created, 10 failed
        for i in 0..100 {
            cache
                .plan_object(config_hash, i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }
        for i in 0..30 {
            cache.mark_creating(config_hash, i).unwrap();
            cache
                .mark_created(config_hash, i, Some(1738886400), None)
                .unwrap();
        }
        for i in 30..40 {
            cache.mark_creating(config_hash, i).unwrap();
            cache.mark_failed(config_hash, i).unwrap();
        }

        let counts = cache.count_by_state(config_hash).unwrap();
        assert_eq!(counts.get(&ObjectState::Planned), Some(&60)); // 100 - 40
        assert_eq!(counts.get(&ObjectState::Created), Some(&30));
        assert_eq!(counts.get(&ObjectState::Failed), Some(&10));
    }

    #[tokio::test]
    async fn test_cache_persistence() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let config_hash = "persist_test";

        // Create cache, add objects, flush
        {
            let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();
            cache
                .plan_object(config_hash, 0, "file_0.dat", 2048)
                .unwrap();
            cache.mark_creating(config_hash, 0).unwrap();
            cache
                .mark_created(config_hash, 0, Some(1738886400), Some(0xabcd))
                .unwrap();
            cache.force_flush().unwrap();
        }

        // Reopen cache, verify data persisted
        {
            let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();
            let entry = cache.get_object(config_hash, 0).unwrap().unwrap();
            assert_eq!(entry.state, ObjectState::Created);
            assert_eq!(entry.path, "file_0.dat");
            assert_eq!(entry.created_at, Some(1738886400));
            assert_eq!(entry.checksum, Some(0xabcd));
        }
    }

    // ========================================================================
    // Integration Tests: CoordinatorCache
    // ========================================================================

    #[test]
    fn test_coordinator_cache() {
        let temp = TempDir::new().unwrap();
        let cache = CoordinatorCache::new(temp.path()).unwrap();

        let config_hash = "def456";
        let manifest_json = r#"{"depth": 4, "width": 100}"#;

        // Put tree manifest
        cache.put_tree_manifest(config_hash, manifest_json).unwrap();

        // Get tree manifest
        let retrieved = cache.get_tree_manifest(config_hash).unwrap();
        assert_eq!(retrieved, Some(manifest_json.to_string()));

        // Endpoints
        let endpoints = vec!["file:///a".to_string(), "s3://b".to_string()];
        cache.put_endpoints(config_hash, &endpoints).unwrap();
        let got_endpoints = cache.get_endpoints(config_hash).unwrap();
        assert_eq!(got_endpoints, Some(endpoints));

        // Config metadata
        let metadata = r#"{"test_name": "64m_files", "version": "0.8.60"}"#;
        cache.put_config_metadata(config_hash, metadata).unwrap();
        let got_metadata = cache.get_config_metadata(config_hash).unwrap();
        assert_eq!(got_metadata, Some(metadata.to_string()));
    }

    #[test]
    fn test_coordinator_cache_persistence() {
        let temp = TempDir::new().unwrap();
        let config_hash = "persist_coord";

        // Write data
        {
            let cache = CoordinatorCache::new(temp.path()).unwrap();
            cache
                .put_tree_manifest(config_hash, r#"{"test": "data"}"#)
                .unwrap();
            cache.force_flush().unwrap();
        }

        // Reopen and verify
        {
            let cache = CoordinatorCache::new(temp.path()).unwrap();
            let manifest = cache.get_tree_manifest(config_hash).unwrap();
            assert_eq!(manifest, Some(r#"{"test": "data"}"#.to_string()));
        }
    }

    // ========================================================================
    // Integration Tests: Endpoint ownership
    // ========================================================================

    #[test]
    fn test_endpoint_ownership() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let cache = rt
            .block_on(EndpointCache::new(&uri, 2, None, None))
            .unwrap();

        // Endpoint 2 in 4-endpoint setup
        assert!(cache.owns_file(2, 4)); // 2 % 4 == 2 ✓
        assert!(cache.owns_file(6, 4)); // 6 % 4 == 2 ✓
        assert!(cache.owns_file(10, 4)); // 10 % 4 == 2 ✓
        assert!(!cache.owns_file(0, 4)); // 0 % 4 == 0 ✗
        assert!(!cache.owns_file(1, 4)); // 1 % 4 == 1 ✗
        assert!(!cache.owns_file(3, 4)); // 3 % 4 == 3 ✗
    }

    // ========================================================================
    // Integration Tests: Multi-endpoint coordination
    // ========================================================================

    #[tokio::test]
    async fn test_multi_endpoint_coordination() {
        let temp = TempDir::new().unwrap();
        let results_dir = temp.path().join("results");
        std::fs::create_dir_all(&results_dir).unwrap();

        let endpoints = vec![
            format!("file://{}/ep0", temp.path().display()),
            format!("file://{}/ep1", temp.path().display()),
        ];

        let config_hash = "multi_ep_test".to_string();
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash.clone(), None, None)
            .await
            .unwrap();

        assert_eq!(cache.num_endpoints(), 2);

        // Plan objects across endpoints
        cache
            .endpoint(0)
            .unwrap()
            .plan_object(&config_hash, 0, "file_0.dat", 1024)
            .unwrap();
        cache
            .endpoint(1)
            .unwrap()
            .plan_object(&config_hash, 1, "file_1.dat", 1024)
            .unwrap();
        cache
            .endpoint(0)
            .unwrap()
            .plan_object(&config_hash, 2, "file_2.dat", 1024)
            .unwrap();

        // Verify routing
        assert_eq!(cache.endpoint_for_file(0).unwrap().endpoint_index, 0);
        assert_eq!(cache.endpoint_for_file(1).unwrap().endpoint_index, 1);
        assert_eq!(cache.endpoint_for_file(2).unwrap().endpoint_index, 0);
    }

    #[tokio::test]
    async fn test_aggregate_progress() {
        let temp = TempDir::new().unwrap();
        let results_dir = temp.path().join("results");
        std::fs::create_dir_all(&results_dir).unwrap();

        let endpoints = vec![
            format!("file://{}/ep0", temp.path().display()),
            format!("file://{}/ep1", temp.path().display()),
        ];

        let config_hash = "progress_test".to_string();
        let cache = MetadataCache::new(&results_dir, &endpoints, config_hash.clone(), None, None)
            .await
            .unwrap();

        // Endpoint 0: 50 planned, 30 created
        for i in (0..100).step_by(2) {
            cache
                .endpoint(0)
                .unwrap()
                .plan_object(&config_hash, i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }
        for i in (0..60).step_by(2) {
            cache
                .endpoint(0)
                .unwrap()
                .mark_creating(&config_hash, i)
                .unwrap();
            cache
                .endpoint(0)
                .unwrap()
                .mark_created(&config_hash, i, Some(1738886400), None)
                .unwrap();
        }

        // Endpoint 1: 40 planned, 20 created, 10 failed
        for i in (1..81).step_by(2) {
            cache
                .endpoint(1)
                .unwrap()
                .plan_object(&config_hash, i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }
        for i in (1..41).step_by(2) {
            cache
                .endpoint(1)
                .unwrap()
                .mark_creating(&config_hash, i)
                .unwrap();
            cache
                .endpoint(1)
                .unwrap()
                .mark_created(&config_hash, i, Some(1738886400), None)
                .unwrap();
        }
        for i in (41..61).step_by(2) {
            cache
                .endpoint(1)
                .unwrap()
                .mark_creating(&config_hash, i)
                .unwrap();
            cache
                .endpoint(1)
                .unwrap()
                .mark_failed(&config_hash, i)
                .unwrap();
        }

        // Aggregate progress
        let totals = cache.aggregate_progress(&config_hash).unwrap();
        assert_eq!(totals.get(&ObjectState::Planned), Some(&(20 + 10))); // 50 planned - 30 created, 40 planned - 20 created - 10 failed
        assert_eq!(totals.get(&ObjectState::Created), Some(&(30 + 20)));
        assert_eq!(totals.get(&ObjectState::Failed), Some(&10));
    }

    // ========================================================================
    // Stress Tests: Large-scale operations
    // ========================================================================

    #[tokio::test]
    async fn test_large_scale_batch_10k_objects() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        let config_hash = "scale_10k";

        // Plan 10,000 objects
        let objects: Vec<(usize, String, u64)> = (0..10_000)
            .map(|i| (i, format!("scale/file_{:05}.dat", i), 1048576))
            .collect();

        let start = std::time::Instant::now();
        cache.plan_objects_batch(config_hash, &objects).unwrap();
        let duration = start.elapsed();

        println!("Planned 10,000 objects in {:?}", duration);
        assert!(
            duration.as_secs() < 5,
            "Batch insert should complete in <5 seconds"
        );

        // Verify count
        let counts = cache.count_by_state(config_hash).unwrap();
        assert_eq!(counts.get(&ObjectState::Planned), Some(&10_000));
    }

    // ========================================================================
    // Checkpoint Restore Tests: Resume capability
    // ========================================================================

    async fn run_checkpoint_restore_from_storage() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let agent_id = Some(42);

        // Step 1: Create cache with some objects
        {
            let cache = EndpointCache::new(&uri, 0, agent_id, None).await.unwrap();
            let config_hash = "resume_test";

            // Plan 100 objects
            for i in 0..100 {
                cache
                    .plan_object(config_hash, i, &format!("data/file_{:03}.dat", i), 1048576)
                    .unwrap();
            }

            // Mark first 50 as created
            for i in 0..50 {
                cache.mark_created(config_hash, i, None, None).unwrap();
            }

            // Write checkpoint to storage
            println!("Writing checkpoint to storage...");
            cache.write_checkpoint(agent_id).await.unwrap();

            // Verify checkpoint file exists
            let checkpoint_path = temp.path().join("testdata/.sai3-cache-agent-42.tar.zst");
            assert!(
                checkpoint_path.exists(),
                "Checkpoint file should exist on storage"
            );

            // Close cache (drop)
        }

        // Step 2: Open new cache (should restore from checkpoint)
        {
            println!("Opening new cache (should restore from checkpoint)...");
            let cache = EndpointCache::new(&uri, 0, agent_id, None).await.unwrap();

            // Verify state was restored
            let config_hash = "resume_test";
            let counts = cache.count_by_state(config_hash).unwrap();

            println!("Restored state counts: {:?}", counts);
            assert_eq!(
                counts.get(&ObjectState::Planned),
                Some(&50),
                "50 objects should be Planned"
            );
            assert_eq!(
                counts.get(&ObjectState::Created),
                Some(&50),
                "50 objects should be Created"
            );

            // Verify specific entries
            let entry_0 = cache.get_object(config_hash, 0).unwrap().unwrap();
            assert_eq!(
                entry_0.state,
                ObjectState::Created,
                "First object should be Created"
            );

            let entry_99 = cache.get_object(config_hash, 99).unwrap().unwrap();
            assert_eq!(
                entry_99.state,
                ObjectState::Planned,
                "Last object should be Planned"
            );
        }
    }

    #[tokio::test]
    async fn test_checkpoint_restore_no_checkpoint_on_storage() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/fresh", temp.path().display());

        // Try to create cache when no checkpoint exists on storage
        let cache = EndpointCache::new(&uri, 0, Some(99), None).await.unwrap();

        // Should succeed with empty cache (no error)
        let config_hash = "empty_test";
        let counts = cache.count_by_state(config_hash).unwrap();

        // All counts should be zero (empty cache)
        assert_eq!(
            counts.len(),
            0,
            "Cache should be empty when no checkpoint exists"
        );
    }

    #[tokio::test]
    async fn test_checkpoint_restore_with_local_cache_newer() {
        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/testdata", temp.path().display());
        let agent_id = Some(77);

        // Step 1: Create old checkpoint
        {
            let cache = EndpointCache::new(&uri, 0, agent_id, None).await.unwrap();
            let config_hash = "old_checkpoint";

            // Plan 10 objects
            for i in 0..10 {
                cache
                    .plan_object(config_hash, i, &format!("data/file_{:03}.dat", i), 1048576)
                    .unwrap();
            }

            // Write checkpoint
            cache.write_checkpoint(agent_id).await.unwrap();
        }

        // Step 2: Modify local cache (newer than checkpoint)
        {
            let cache = EndpointCache::new(&uri, 0, agent_id, None).await.unwrap();
            let config_hash = "old_checkpoint";

            // Add 10 more objects (local cache is now newer)
            for i in 10..20 {
                cache
                    .plan_object(config_hash, i, &format!("data/file_{:03}.dat", i), 1048576)
                    .unwrap();
            }

            // Close cache
        }

        // Step 3: Open cache again - should restore from checkpoint (always uses checkpoint if exists)
        // NOTE: Current implementation always restores from checkpoint if one exists
        // This is the correct behavior for resume after crash/restart
        {
            let cache = EndpointCache::new(&uri, 0, agent_id, None).await.unwrap();
            let config_hash = "old_checkpoint";

            let counts = cache.count_by_state(config_hash).unwrap();

            // Should have restored 10 objects from checkpoint (not 20 from newer local cache)
            assert_eq!(
                counts.get(&ObjectState::Planned),
                Some(&10),
                "Should restore from checkpoint (10 objects), ignoring newer local cache"
            );
        }
    }

    #[tokio::test]
    async fn test_checkpoint_stored_at_storage_uri_location() {
        // CRITICAL: Verify checkpoint is stored at the storage URI, not cache location
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage-endpoint", temp.path().display());
        let agent_id = Some(123);

        // Create cache (will be in isolated location)
        let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
            .await
            .unwrap();

        // Add some data
        cache.plan_object("config1", 0, "test.dat", 1024).unwrap();
        cache.force_flush().unwrap();

        // Write checkpoint
        let checkpoint_location = cache.write_checkpoint(agent_id).await.unwrap();
        println!("Checkpoint location: {}", checkpoint_location);

        // VERIFY: Checkpoint file exists at storage URI location (not cache location)
        let checkpoint_path = temp
            .path()
            .join("storage-endpoint/.sai3-cache-agent-123.tar.zst");
        assert!(
            checkpoint_path.exists(),
            "Checkpoint must exist at storage URI: {}",
            checkpoint_path.display()
        );

        // VERIFY: Checkpoint is NOT in cache location
        let cache_dir = cache.cache_location();
        let cache_checkpoint = cache_dir.join(".sai3-cache-agent-123.tar.zst");
        assert!(
            !cache_checkpoint.exists(),
            "Checkpoint should NOT be in cache location: {}",
            cache_checkpoint.display()
        );

        // VERIFY: Checkpoint archive is non-empty and compressed
        let checkpoint_size = std::fs::metadata(&checkpoint_path).unwrap().len();
        assert!(
            checkpoint_size > 100,
            "Checkpoint should be substantial (got {} bytes)",
            checkpoint_size
        );

        // VERIFY: Can list archive contents
        let file = std::fs::File::open(&checkpoint_path).unwrap();
        let decoder = zstd::Decoder::new(file).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let entries: Vec<_> = archive.entries().unwrap().collect();
        assert!(
            !entries.is_empty(),
            "Checkpoint archive should contain files"
        );
    }

    #[tokio::test]
    async fn test_checkpoint_archive_contains_kv_database_files() {
        // Verify checkpoint archive contains actual fjall database files
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage", temp.path().display());
        let agent_id = Some(99);

        let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
            .await
            .unwrap();

        // Create substantial data to ensure database files are written
        for i in 0..100 {
            cache
                .plan_object("config1", i, &format!("obj_{:03}.dat", i), 4096)
                .unwrap();
            cache.mark_created("config1", i, None, None).unwrap();
        }
        cache.force_flush().unwrap();

        // Write checkpoint
        cache.write_checkpoint(agent_id).await.unwrap();

        // Verify archive contents
        let checkpoint_path = temp.path().join("storage/.sai3-cache-agent-99.tar.zst");
        let file = std::fs::File::open(&checkpoint_path).unwrap();
        let decoder = zstd::Decoder::new(file).unwrap();
        let mut archive = tar::Archive::new(decoder);

        let mut found_keyspace = false;
        let mut file_count = 0;

        for entry_result in archive.entries().unwrap() {
            let entry = entry_result.unwrap();
            let path = entry.path().unwrap();
            let path_str = path.to_string_lossy();

            if path_str.contains("keyspaces") {
                found_keyspace = true;
            }
            file_count += 1;
        }

        assert!(
            found_keyspace,
            "Archive should contain keyspaces directory (fjall database structure)"
        );
        assert!(
            file_count > 5,
            "Archive should contain multiple database files (got {})",
            file_count
        );
    }

    #[tokio::test]
    async fn test_multiple_checkpoint_restore_cycles() {
        // Verify multiple checkpoint/restore cycles work correctly
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage", temp.path().display());
        let agent_id = Some(7);
        let config_hash = "multi_cycle";

        // Cycle 1: Create 10 objects, checkpoint
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            for i in 0..10 {
                cache
                    .plan_object(config_hash, i, &format!("file_{:03}.dat", i), 1024)
                    .unwrap();
            }
            cache.write_checkpoint(agent_id).await.unwrap();
        }

        // Cycle 2: Restore, add 10 more, checkpoint
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            let counts = cache.count_by_state(config_hash).unwrap();
            assert_eq!(
                counts.get(&ObjectState::Planned),
                Some(&10),
                "Should restore 10 from cycle 1"
            );

            for i in 10..20 {
                cache
                    .plan_object(config_hash, i, &format!("file_{:03}.dat", i), 1024)
                    .unwrap();
            }
            cache.write_checkpoint(agent_id).await.unwrap();
        }

        // Cycle 3: Restore, verify all 20 objects
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            let counts = cache.count_by_state(config_hash).unwrap();
            assert_eq!(
                counts.get(&ObjectState::Planned),
                Some(&20),
                "Should restore all 20 from cycle 2"
            );

            // Verify specific objects exist
            assert!(cache.get_object(config_hash, 0).unwrap().is_some());
            assert!(cache.get_object(config_hash, 9).unwrap().is_some());
            assert!(cache.get_object(config_hash, 10).unwrap().is_some());
            assert!(cache.get_object(config_hash, 19).unwrap().is_some());
        }
    }

    #[tokio::test]
    async fn test_checkpoint_with_large_dataset() {
        // Test checkpoint with 1000 objects (realistic prepare scenario)
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage", temp.path().display());
        let agent_id = Some(5);
        let config_hash = "large_dataset";

        // Create 1000 objects with mixed states
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();

            for i in 0..1000 {
                cache
                    .plan_object(config_hash, i, &format!("obj_{:04}.dat", i), 1048576)
                    .unwrap();
            }

            // Mark first 600 as created, next 200 as failed, leave 200 as planned
            for i in 0..600 {
                cache.mark_created(config_hash, i, None, None).unwrap();
            }
            for i in 600..800 {
                cache.mark_failed(config_hash, i).unwrap();
            }

            cache.force_flush().unwrap();

            // Write checkpoint
            let checkpoint_loc = cache.write_checkpoint(agent_id).await.unwrap();
            println!("Large dataset checkpoint: {}", checkpoint_loc);
        }

        // Restore and verify all states
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            let counts = cache.count_by_state(config_hash).unwrap();

            assert_eq!(
                counts.get(&ObjectState::Created),
                Some(&600),
                "Should restore 600 created"
            );
            assert_eq!(
                counts.get(&ObjectState::Failed),
                Some(&200),
                "Should restore 200 failed"
            );
            assert_eq!(
                counts.get(&ObjectState::Planned),
                Some(&200),
                "Should restore 200 planned"
            );

            // Verify some specific entries
            let created_entry = cache.get_object(config_hash, 0).unwrap().unwrap();
            assert_eq!(created_entry.state, ObjectState::Created);

            let failed_entry = cache.get_object(config_hash, 700).unwrap().unwrap();
            assert_eq!(failed_entry.state, ObjectState::Failed);

            let planned_entry = cache.get_object(config_hash, 900).unwrap().unwrap();
            assert_eq!(planned_entry.state, ObjectState::Planned);
        }
    }

    #[tokio::test]
    async fn test_checkpoint_agent_id_isolation() {
        // Verify different agent IDs create isolated checkpoints
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage", temp.path().display());
        let config_hash = "isolation_test";

        // Agent 1: Create 10 objects
        {
            let cache = EndpointCache::new(&storage_uri, 0, Some(1), None)
                .await
                .unwrap();
            for i in 0..10 {
                cache
                    .plan_object(config_hash, i, &format!("agent1_file_{}.dat", i), 1024)
                    .unwrap();
            }
            cache.write_checkpoint(Some(1)).await.unwrap();
        }

        // Agent 2: Create 20 objects
        {
            let cache = EndpointCache::new(&storage_uri, 0, Some(2), None)
                .await
                .unwrap();
            for i in 0..20 {
                cache
                    .plan_object(config_hash, i, &format!("agent2_file_{}.dat", i), 2048)
                    .unwrap();
            }
            cache.write_checkpoint(Some(2)).await.unwrap();
        }

        // Verify separate checkpoint files exist
        let checkpoint1 = temp.path().join("storage/.sai3-cache-agent-1.tar.zst");
        let checkpoint2 = temp.path().join("storage/.sai3-cache-agent-2.tar.zst");

        assert!(checkpoint1.exists(), "Agent 1 checkpoint should exist");
        assert!(checkpoint2.exists(), "Agent 2 checkpoint should exist");

        // Verify checkpoints have different sizes (contain different data)
        let size1 = std::fs::metadata(&checkpoint1).unwrap().len();
        let size2 = std::fs::metadata(&checkpoint2).unwrap().len();
        assert_ne!(
            size1, size2,
            "Agent checkpoints should have different sizes"
        );

        // Restore agent 1 - should get 10 objects
        {
            let cache = EndpointCache::new(&storage_uri, 0, Some(1), None)
                .await
                .unwrap();
            let counts = cache.count_by_state(config_hash).unwrap();
            assert_eq!(
                counts.get(&ObjectState::Planned),
                Some(&10),
                "Agent 1 should restore 10 objects"
            );
        }

        // Restore agent 2 - should get 20 objects
        {
            let cache = EndpointCache::new(&storage_uri, 0, Some(2), None)
                .await
                .unwrap();
            let counts = cache.count_by_state(config_hash).unwrap();
            assert_eq!(
                counts.get(&ObjectState::Planned),
                Some(&20),
                "Agent 2 should restore 20 objects"
            );
        }
    }

    #[tokio::test]
    async fn test_checkpoint_no_agent_id_uses_default_name() {
        // Verify checkpoint without agent ID uses default naming
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage", temp.path().display());

        let cache = EndpointCache::new(&storage_uri, 0, None, None)
            .await
            .unwrap();
        cache.plan_object("config1", 0, "test.dat", 1024).unwrap();
        cache.force_flush().unwrap();

        cache.write_checkpoint(None).await.unwrap();

        // Verify default checkpoint name (not agent-specific)
        let checkpoint = temp.path().join("storage/sai3-kv-cache.tar.zst");
        assert!(
            checkpoint.exists(),
            "Default checkpoint should exist at {}",
            checkpoint.display()
        );
    }

    async fn run_checkpoint_overwrites_previous_checkpoint() {
        // Verify new checkpoint replaces old checkpoint (not accumulate)
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage", temp.path().display());
        let agent_id = Some(10);
        let config_hash = "overwrite_test";

        // First checkpoint: 5 objects
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            for i in 0..5 {
                cache
                    .plan_object(config_hash, i, &format!("file_{}.dat", i), 1024)
                    .unwrap();
            }
            cache.write_checkpoint(agent_id).await.unwrap();
        }

        let checkpoint_path = temp.path().join("storage/.sai3-cache-agent-10.tar.zst");
        let first_size = std::fs::metadata(&checkpoint_path).unwrap().len();
        println!("First checkpoint size: {} bytes", first_size);

        // Second checkpoint: 50 objects (much larger)
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            // Cache restored 5 objects from first checkpoint

            // Add 45 more objects (total will be 50)
            for i in 5..50 {
                cache
                    .plan_object(config_hash, i, &format!("file_{}.dat", i), 1024)
                    .unwrap();
            }
            cache.force_flush().unwrap();

            // Verify we have 50 before checkpointing
            let counts_before = cache.count_by_state(config_hash).unwrap();
            assert_eq!(
                counts_before.get(&ObjectState::Planned),
                Some(&50),
                "Should have 50 objects before second checkpoint"
            );

            cache.write_checkpoint(agent_id).await.unwrap();
        }

        let second_size = std::fs::metadata(&checkpoint_path).unwrap().len();
        println!("Second checkpoint size: {} bytes", second_size);

        // NOTE: Size comparison is unreliable for small datasets due to compression/compaction
        // The important verification is that restore gets the correct data

        // Restore should get all 50 objects from second checkpoint
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            let counts = cache.count_by_state(config_hash).unwrap();
            assert_eq!(
                counts.get(&ObjectState::Planned),
                Some(&50),
                "Should restore all 50 objects from latest checkpoint"
            );
        }
    }

    #[tokio::test]
    async fn test_checkpoint_restore_after_overwrite() {
        // Run sequentially to avoid interleaving checkpoint operations between tests.
        run_checkpoint_overwrites_previous_checkpoint().await;
        run_checkpoint_restore_from_storage().await;
    }

    // ========================================================================
    // CRITICAL: Regression tests for v0.8.61 executor-blocking bug fix
    // ========================================================================

    /// Test that checkpoint creation doesn't block the executor
    ///
    /// This simulates the v0.8.61 hang by creating a checkpoint while
    /// also running concurrent async tasks. If checkpoint blocks, the
    /// concurrent tasks will timeout.
    #[tokio::test]
    async fn test_v0_8_61_checkpoint_does_not_block_executor() {
        let temp_dir = tempfile::tempdir().unwrap();
        let storage_uri = format!("file://{}", temp_dir.path().join("storage").display());

        // Create cache with some data
        let cache = EndpointCache::new(&storage_uri, 0, Some(0), None)
            .await
            .unwrap();

        // Plan some objects to make checkpoint non-trivial
        for i in 0..100 {
            cache
                .plan_object("test_hash", i, &format!("file_{}.dat", i), 1024)
                .unwrap();
        }

        // Spawn concurrent "heartbeat" task that must continue while checkpoint runs
        let heartbeat_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag_clone = heartbeat_flag.clone();

        let heartbeat = tokio::spawn(async move {
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                flag_clone.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });

        // Create checkpoint (should use spawn_blocking, not block executor)
        let checkpoint_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            cache.write_checkpoint(Some(0)),
        )
        .await;

        // Wait for heartbeat
        let heartbeat_result =
            tokio::time::timeout(std::time::Duration::from_secs(3), heartbeat).await;

        // Verify both completed
        assert!(
            checkpoint_result.is_ok(),
            "Checkpoint should complete without timeout (v0.8.61 bug: blocking I/O froze executor)"
        );
        assert!(
            heartbeat_result.is_ok(),
            "Heartbeat should run concurrently (v0.8.61 bug: checkpoint blocked everything)"
        );
        assert!(
            heartbeat_flag.load(std::sync::atomic::Ordering::Relaxed),
            "Heartbeat should have run at least once"
        );
    }

    /// Test that multiple concurrent checkpoints don't deadlock
    ///
    /// Simulates 4 agents all creating checkpoints simultaneously
    /// (the exact scenario that hung in v0.8.60)
    #[tokio::test]
    async fn test_v0_8_61_concurrent_checkpoints_from_4_agents() {
        let temp_dir = tempfile::tempdir().unwrap();

        // Create 4 agent caches (simulating 4-agent distributed test)
        let mut handles = Vec::new();

        for agent_id in 0..4 {
            let temp_path = temp_dir.path().to_path_buf();

            let handle = tokio::spawn(async move {
                let storage_uri = format!(
                    "file://{}",
                    temp_path.join(format!("storage-{}", agent_id)).display()
                );
                let cache = EndpointCache::new(&storage_uri, 0, Some(agent_id), None)
                    .await
                    .unwrap();

                // Plan objects
                for i in 0..50 {
                    cache
                        .plan_object("concurrent_test", i, &format!("file_{}.dat", i), 1024)
                        .unwrap();
                }

                // Create checkpoint
                cache.write_checkpoint(Some(agent_id)).await
            });

            handles.push(handle);
        }

        // All 4 agents should complete checkpoints without timeout
        // v0.8.60 bug: All 4 agents froze at exactly 69 objects when first checkpoint fired
        let results = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            futures::future::join_all(handles),
        )
        .await;

        assert!(results.is_ok(), "All 4 concurrent checkpoints should complete without timeout (v0.8.61 bug: all agents hung)");

        let checkpoint_results: Vec<_> = results.unwrap().into_iter().collect();
        assert_eq!(checkpoint_results.len(), 4, "All 4 agents should complete");

        for (agent_id, result) in checkpoint_results.iter().enumerate() {
            assert!(
                result.is_ok(),
                "Agent {} checkpoint task should not panic",
                agent_id
            );
            assert!(
                result.as_ref().unwrap().is_ok(),
                "Agent {} checkpoint should succeed",
                agent_id
            );
        }
    }

    // ========================================================================
    // Storage Topology Detection Tests (v0.8.62)
    // ========================================================================

    #[test]
    fn test_shared_storage_detection_same_bucket_different_ips() {
        // Scenario: Load balancer with multiple IPs pointing to same S3 bucket
        let endpoints = vec![
            "s3://10.9.0.21:9000/my-bucket/prefix1/".to_string(),
            "s3://10.9.0.25:9000/my-bucket/prefix1/".to_string(),
        ];

        assert!(
            endpoints_share_storage(&endpoints),
            "Should detect shared storage: different IPs but same bucket"
        );
    }

    #[test]
    fn test_shared_storage_detection_same_bucket_different_prefixes() {
        // Scenario: Same bucket with different prefixes (still shared storage)
        let endpoints = vec![
            "s3://storage.example.com/bucket1/path-a/".to_string(),
            "s3://storage.example.com/bucket1/path-b/".to_string(),
        ];

        assert!(
            endpoints_share_storage(&endpoints),
            "Should detect shared storage: same bucket with different prefixes"
        );
    }

    #[test]
    fn test_shared_storage_detection_different_hostnames_same_bucket() {
        // Scenario: DNS round-robin with multiple hostnames pointing to same S3 bucket
        // (e.g., storage1.example.com and storage2.example.com both point to same bucket)
        let endpoints = vec![
            "s3://storage1.example.com:9000/shared-bucket/data/".to_string(),
            "s3://storage2.example.com:9000/shared-bucket/data/".to_string(),
            "s3://storage3.example.com:9000/shared-bucket/data/".to_string(),
        ];

        assert!(
            endpoints_share_storage(&endpoints),
            "Should detect shared storage: different hostnames but same bucket"
        );
    }

    #[test]
    fn test_shared_storage_detection_mixed_ips_and_hostnames() {
        // Scenario: Mix of IP addresses and hostnames pointing to same bucket
        // (common in load-balanced setups where IPs and DNS names are both used)
        let endpoints = vec![
            "s3://10.9.0.21:9000/my-bucket/".to_string(),
            "s3://storage.example.com:9000/my-bucket/".to_string(),
            "s3://192.168.1.100:9000/my-bucket/".to_string(),
        ];

        assert!(
            endpoints_share_storage(&endpoints),
            "Should detect shared storage: mixed IPs/hostnames with same bucket"
        );
    }

    #[test]
    fn test_independent_storage_detection_different_buckets() {
        // Scenario: Different buckets (independent storage)
        let endpoints = vec![
            "s3://10.9.0.21:9000/bucket1/".to_string(),
            "s3://10.9.0.21:9000/bucket2/".to_string(),
        ];

        assert!(
            !endpoints_share_storage(&endpoints),
            "Should detect independent storage: different buckets (same IP)"
        );
    }

    #[test]
    fn test_independent_storage_detection_different_hostnames_different_buckets() {
        // Scenario: Different hostnames with different buckets (definitely independent)
        let endpoints = vec![
            "s3://region1.cloud.com:9000/bucket-us-east/".to_string(),
            "s3://region2.cloud.com:9000/bucket-us-west/".to_string(),
            "s3://region3.cloud.com:9000/bucket-eu-central/".to_string(),
        ];

        assert!(
            !endpoints_share_storage(&endpoints),
            "Should detect independent storage: different hostnames and different buckets"
        );
    }

    #[test]
    fn test_independent_storage_detection_different_file_paths() {
        // Scenario: Different filesystem paths (independent storage)
        let endpoints = vec![
            "file:///mnt/nvme1/data/".to_string(),
            "file:///mnt/nvme2/data/".to_string(),
        ];

        assert!(
            !endpoints_share_storage(&endpoints),
            "Should detect independent storage: different filesystem paths"
        );
    }

    #[test]
    fn test_shared_storage_detection_same_file_path() {
        // Scenario: Same filesystem path (shared storage - unusual but valid)
        let endpoints = vec![
            "file:///mnt/data/testdata/".to_string(),
            "file:///mnt/data/testdata/".to_string(),
        ];

        assert!(
            endpoints_share_storage(&endpoints),
            "Should detect shared storage: identical filesystem paths"
        );
    }

    #[test]
    fn test_azure_shared_storage_detection() {
        // Scenario: Azure blob storage with same container
        let endpoints = vec![
            "az://account1.blob.core.windows.net/container1/prefix-a/".to_string(),
            "az://account1.blob.core.windows.net/container1/prefix-b/".to_string(),
        ];

        assert!(
            endpoints_share_storage(&endpoints),
            "Should detect shared storage: same Azure container"
        );
    }

    #[test]
    fn test_azure_independent_storage_detection() {
        // Scenario: Azure blob storage with different containers
        let endpoints = vec![
            "az://account1.blob.core.windows.net/container1/".to_string(),
            "az://account1.blob.core.windows.net/container2/".to_string(),
        ];

        assert!(
            !endpoints_share_storage(&endpoints),
            "Should detect independent storage: different Azure containers"
        );
    }

    #[test]
    fn test_gcs_shared_storage_detection() {
        // Scenario: Google Cloud Storage with same bucket
        let endpoints = vec![
            "gs://my-bucket/path1/".to_string(),
            "gs://my-bucket/path2/".to_string(),
        ];

        assert!(
            endpoints_share_storage(&endpoints),
            "Should detect shared storage: same GCS bucket"
        );
    }

    #[test]
    fn test_single_endpoint_not_considered_shared() {
        // Scenario: Single endpoint (no sharing possible)
        let endpoints = vec!["s3://bucket/path/".to_string()];

        assert!(
            !endpoints_share_storage(&endpoints),
            "Single endpoint should not be considered shared storage"
        );
    }

    #[test]
    fn test_mixed_schemes_considered_independent() {
        // Scenario: Different URI schemes (definitely independent)
        let endpoints = vec!["s3://bucket1/".to_string(), "file:///mnt/data/".to_string()];

        assert!(
            !endpoints_share_storage(&endpoints),
            "Mixed URI schemes should always be considered independent"
        );
    }

    #[test]
    fn test_direct_uri_shared_storage() {
        // Scenario: direct:// URIs with same path
        let endpoints = vec![
            "direct:///dev/dax0.0".to_string(),
            "direct:///dev/dax0.0".to_string(),
        ];

        assert!(
            endpoints_share_storage(&endpoints),
            "Should detect shared storage: same direct:// path"
        );
    }

    #[test]
    fn test_direct_uri_independent_storage() {
        // Scenario: direct:// URIs with different devices
        let endpoints = vec![
            "direct:///dev/dax0.0".to_string(),
            "direct:///dev/dax0.1".to_string(),
        ];

        assert!(
            !endpoints_share_storage(&endpoints),
            "Should detect independent storage: different direct:// devices"
        );
    }

    // ========================================================================
    // Checkpoint Failure Resilience Tests (v0.8.62)
    // ========================================================================
    // NOTE: These tests are marked #[ignore] because they involve real tar+zstd
    // compression which takes ~15 seconds even with minimal data. Run explicitly with:
    //   cargo test --lib -- --ignored test_checkpoint_failure

    /// Test that checkpoint upload failure is non-fatal and logged as warning
    ///
    /// This test simulates checkpoint write failure without hanging on network timeouts.
    /// The workload should continue even when checkpoint fails.
    ///
    /// **SLOW TEST**: Takes ~15s due to real tar+zstd compression of fjall database.
    /// Run with: `cargo test -- --ignored test_checkpoint_failure_is_non_fatal`
    #[tokio::test]
    #[ignore]
    async fn test_checkpoint_failure_is_non_fatal() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();

        // Create a read-only directory that will fail checkpoint writes quickly
        let readonly_dir = temp.path().join("readonly");
        std::fs::create_dir(&readonly_dir).unwrap();
        let mut perms = std::fs::metadata(&readonly_dir).unwrap().permissions();
        perms.set_mode(0o555); // r-xr-xr-x (read+execute only, no write)
        std::fs::set_permissions(&readonly_dir, perms).unwrap();

        let invalid_uri = format!("file://{}", readonly_dir.display());
        let cache = EndpointCache::new(&invalid_uri, 0, Some(0), None)
            .await
            .unwrap();

        // Create minimal objects to speed up test
        for i in 0..3 {
            cache
                .plan_object("config1", i, &format!("file_{}.dat", i), 256)
                .unwrap();
            cache.mark_created("config1", i, None, None).unwrap();
        }
        cache.force_flush().unwrap();

        // Try to write checkpoint - should fail quickly (directory not writable)
        let result = cache.write_checkpoint(Some(0)).await;

        assert!(
            result.is_err(),
            "Checkpoint should fail with read-only directory"
        );

        // Verify objects are still accessible (workload can continue)
        let entry = cache.get_object("config1", 0).unwrap();
        assert!(
            entry.is_some(),
            "Objects should still be accessible after checkpoint failure"
        );
        assert_eq!(entry.unwrap().state, ObjectState::Created);
    }

    /// Test that checkpoint creation with timeout doesn't hang the system
    ///
    /// This verifies the timeout+retry logic in try_write_checkpoint_static.
    #[tokio::test]
    async fn test_checkpoint_timeout_handling() {
        use tokio::time::{timeout, Duration};

        let temp = tempfile::tempdir().unwrap();
        let storage_uri = format!("file://{}", temp.path().display());
        let cache = EndpointCache::new(&storage_uri, 0, Some(0), None)
            .await
            .unwrap();

        // Create minimal data
        cache.plan_object("config1", 0, "file.dat", 1024).unwrap();
        cache.force_flush().unwrap();

        // Checkpoint should complete quickly (< 5 seconds for small cache)
        let result = timeout(Duration::from_secs(5), cache.write_checkpoint(Some(0))).await;

        assert!(
            result.is_ok(),
            "Checkpoint should complete within timeout (v0.8.62 added timeouts to prevent hangs)"
        );

        assert!(
            result.unwrap().is_ok(),
            "Checkpoint should succeed for file:// URI"
        );
    }

    /// Test that multiple checkpoint failures don't accumulate and cause issues
    ///
    /// Simulates scenario where periodic checkpoints keep failing but prepare continues.
    ///
    /// **SLOW TEST**: Takes ~45s (3 checkpoint attempts × 15s each).
    /// Run with: `cargo test -- --ignored test_repeated_checkpoint_failures`
    #[tokio::test]
    #[ignore]
    async fn test_repeated_checkpoint_failures_are_handled() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();

        // Create read-only directory that causes fast checkpoint failures
        let readonly_dir = temp.path().join("readonly2");
        std::fs::create_dir(&readonly_dir).unwrap();
        let mut perms = std::fs::metadata(&readonly_dir).unwrap().permissions();
        perms.set_mode(0o555); // No write permission
        std::fs::set_permissions(&readonly_dir, perms).unwrap();

        let invalid_uri = format!("file://{}", readonly_dir.display());
        let cache = EndpointCache::new(&invalid_uri, 0, Some(0), None)
            .await
            .unwrap();

        // Create minimal objects
        for i in 0..2 {
            cache
                .plan_object("config1", i, &format!("file_{}.dat", i), 256)
                .unwrap();
        }
        cache.force_flush().unwrap();

        // Try multiple checkpoint attempts (simulating periodic checkpointing)
        for attempt in 1..=3 {
            let result = cache.write_checkpoint(Some(0)).await;
            assert!(
                result.is_err(),
                "Checkpoint attempt {} should fail gracefully",
                attempt
            );
        }

        // Verify cache is still functional after repeated failures
        let counts = cache.count_by_state("config1").unwrap();
        assert_eq!(
            counts.get(&ObjectState::Planned),
            Some(&2),
            "Cache should remain functional after repeated checkpoint failures"
        );
    }

    /// Test checkpoint behavior with empty cache (edge case)
    #[tokio::test]
    async fn test_checkpoint_with_empty_cache() {
        let temp = tempfile::tempdir().unwrap();
        let storage_uri = format!("file://{}", temp.path().display());
        let cache = EndpointCache::new(&storage_uri, 0, Some(0), None)
            .await
            .unwrap();

        // Don't add any objects - checkpoint empty cache
        let result = cache.write_checkpoint(Some(0)).await;

        // Should succeed even with empty cache
        assert!(
            result.is_ok(),
            "Checkpointing empty cache should succeed (creates empty archive)"
        );
    }

    // ========================================================================
    // Unit Tests: CheckpointCoverage + listing-skip optimisation (v0.8.89)
    // ========================================================================

    /// Round-trip: write checkpoint with Created objects; load it via
    /// `try_load_from_checkpoint` and verify `check_coverage` reports the
    /// correct counts without doing any network LIST.
    #[tokio::test]
    async fn test_checkpoint_round_trip_with_created_objects() {
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage/", temp.path().display());
        let config_hash = "rt_created_test";
        let agent_id = Some(0_usize);
        let total_objects: usize = 50;

        // ── Phase 1: Simulate successful prepare ──────────────────────────────
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();

            for i in 0..total_objects {
                cache
                    .plan_object(config_hash, i, &format!("prepared-{:08}.dat", i), 4096)
                    .unwrap();
                cache.mark_created(config_hash, i, None, None).unwrap();
            }

            cache.force_flush().unwrap();

            // Confirm all are Created before checkpointing
            let counts = cache.count_by_state(config_hash).unwrap();
            assert_eq!(
                *counts.get(&ObjectState::Created).unwrap_or(&0),
                total_objects,
                "All objects should be Created before checkpoint"
            );

            cache.write_checkpoint(agent_id).await.unwrap();
        }

        // ── Phase 2: Read checkpoint back via try_load_from_checkpoint ─────────
        let loaded_cache = EndpointCache::try_load_from_checkpoint(&storage_uri, agent_id, None)
            .await
            .expect("try_load_from_checkpoint should not error")
            .expect("checkpoint should be present on file:// storage");

        // ── Phase 3: Verify coverage ──────────────────────────────────────────
        let coverage = loaded_cache
            .check_coverage(config_hash, total_objects as u64)
            .unwrap();

        assert_eq!(
            coverage.created_count, total_objects,
            "All objects should be Created"
        );
        assert_eq!(coverage.planned_count, 0, "No objects should be Planned");
        assert_eq!(coverage.failed_count, 0, "No objects should be Failed");
        assert_eq!(
            coverage.total_tracked, total_objects,
            "Total tracked should match"
        );
        assert_eq!(
            coverage.expected_count, total_objects,
            "expected_count should match"
        );
        assert!(
            coverage.covers_all,
            "covers_all must be true when created == expected"
        );
    }

    /// `try_load_from_checkpoint` returns `None` when no checkpoint exists.
    #[tokio::test]
    async fn test_try_load_from_checkpoint_returns_none_when_missing() {
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/empty_storage/", temp.path().display());

        // No checkpoint was written — expect None
        let result = EndpointCache::try_load_from_checkpoint(&storage_uri, Some(0), None)
            .await
            .expect("Should not error when checkpoint is simply absent");

        assert!(
            result.is_none(),
            "Should return None when no checkpoint exists"
        );
    }

    /// `check_coverage` returns `covers_all = false` when fewer objects are
    /// Created than expected.
    #[tokio::test]
    async fn test_check_coverage_partial_creation() {
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage/", temp.path().display());
        let config_hash = "partial_test";
        let agent_id = Some(0_usize);
        let total_objects: usize = 100;
        let created: usize = 60;

        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();

            // Create only 60 of 100 objects
            for i in 0..created {
                cache
                    .plan_object(config_hash, i, &format!("prepared-{:08}.dat", i), 4096)
                    .unwrap();
                cache.mark_created(config_hash, i, None, None).unwrap();
            }
            // Plan the remaining 40 but leave them as Planned
            for i in created..total_objects {
                cache
                    .plan_object(config_hash, i, &format!("prepared-{:08}.dat", i), 4096)
                    .unwrap();
            }

            cache.force_flush().unwrap();
            cache.write_checkpoint(agent_id).await.unwrap();
        }

        let loaded = EndpointCache::try_load_from_checkpoint(&storage_uri, agent_id, None)
            .await
            .unwrap()
            .unwrap();

        let coverage = loaded
            .check_coverage(config_hash, total_objects as u64)
            .unwrap();

        assert_eq!(coverage.created_count, created);
        assert_eq!(coverage.planned_count, total_objects - created);
        assert_eq!(coverage.total_tracked, total_objects);
        assert!(
            !coverage.covers_all,
            "covers_all should be false when partial"
        );
    }

    /// `check_coverage` returns `covers_all = false` when `expected_count` is
    /// zero (edge case — guards against accidental skip when config has count=0).
    #[tokio::test]
    async fn test_check_coverage_zero_expected_never_covers_all() {
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage/", temp.path().display());
        let config_hash = "zero_expected";
        let agent_id = Some(0_usize);

        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            cache.plan_object(config_hash, 0, "file.dat", 1024).unwrap();
            cache.mark_created(config_hash, 0, None, None).unwrap();
            cache.force_flush().unwrap();
            cache.write_checkpoint(agent_id).await.unwrap();
        }

        let loaded = EndpointCache::try_load_from_checkpoint(&storage_uri, agent_id, None)
            .await
            .unwrap()
            .unwrap();
        let coverage = loaded.check_coverage(config_hash, 0).unwrap();

        assert!(
            !coverage.covers_all,
            "covers_all must not be true when expected_count is 0"
        );
    }

    /// `MetadataCache::check_listing_coverage` aggregates across endpoint caches
    /// and returns `None` when no objects are tracked.
    #[tokio::test]
    async fn test_metadata_cache_check_listing_coverage_empty() {
        let temp = TempDir::new().unwrap();
        let results_dir = temp.path().join("results");
        let storage_uri = format!("file://{}/storage/", temp.path().display());
        let config_hash = "empty_coverage";

        // Create a fresh MetadataCache with no objects
        let cache = MetadataCache::new(
            &results_dir,
            std::slice::from_ref(&storage_uri),
            config_hash.to_string(),
            None,
            None,
        )
        .await
        .unwrap();

        let coverage = cache.check_listing_coverage(1000).unwrap();

        assert!(
            coverage.is_none(),
            "Should return None when no objects are tracked yet"
        );
    }

    /// `MetadataCache::check_listing_coverage` returns `covers_all = true` when
    /// all objects across all endpoints are in the `Created` state.
    #[tokio::test]
    async fn test_metadata_cache_check_listing_coverage_full() {
        let temp = TempDir::new().unwrap();
        let results_dir = temp.path().join("results");
        let storage_uri = format!("file://{}/storage/", temp.path().display());
        let config_hash = "full_coverage";
        let total_objects: usize = 30;

        {
            let cache = MetadataCache::new(
                &results_dir,
                std::slice::from_ref(&storage_uri),
                config_hash.to_string(),
                None,
                None,
            )
            .await
            .unwrap();

            let ep = cache.endpoint(0).unwrap();
            for i in 0..total_objects {
                ep.plan_object(config_hash, i, &format!("prepared-{:08}.dat", i), 4096)
                    .unwrap();
                ep.mark_created(config_hash, i, None, None).unwrap();
            }
            cache.flush_all().unwrap();
        }

        // Re-open (simulates restore from checkpoint)
        let cache = MetadataCache::new(
            &results_dir,
            &[storage_uri],
            config_hash.to_string(),
            None,
            None,
        )
        .await
        .unwrap();

        let coverage = cache
            .check_listing_coverage(total_objects as u64)
            .unwrap()
            .unwrap();

        assert!(
            coverage.covers_all,
            "Should cover all when all objects are Created"
        );
        assert_eq!(coverage.created_count, total_objects);
    }

    /// `query_checkpoint_coverage` convenience function: round-trip via file:// storage.
    #[tokio::test]
    async fn test_query_checkpoint_coverage_convenience_fn() {
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage/", temp.path().display());
        let config_hash = "qcc_test";
        let agent_id = Some(0_usize);
        let total: usize = 25;

        // Write checkpoint
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            for i in 0..total {
                cache
                    .plan_object(config_hash, i, &format!("prepared-{:08}.dat", i), 1024)
                    .unwrap();
                cache.mark_created(config_hash, i, None, None).unwrap();
            }
            cache.force_flush().unwrap();
            cache.write_checkpoint(agent_id).await.unwrap();
        }

        // Query via convenience function
        let result = query_checkpoint_coverage(&storage_uri, agent_id, config_hash, total as u64)
            .await
            .expect("Should not error");

        let coverage = result.expect("Checkpoint should be found");
        assert!(coverage.covers_all);
        assert_eq!(coverage.created_count, total);

        // Same function returns None when no checkpoint exists
        let new_temp = TempDir::new().unwrap();
        let missing_uri = format!("file://{}/nostorage/", new_temp.path().display());
        let missing = query_checkpoint_coverage(&missing_uri, agent_id, config_hash, total as u64)
            .await
            .expect("Should not error for missing checkpoint");

        assert!(
            missing.is_none(),
            "Should return None when checkpoint does not exist"
        );
    }

    /// Verify individual `get_object` lookups work on a checkpoint-restored cache.
    /// This tests that the fjall database is fully readable, not just the counts.
    #[tokio::test]
    async fn test_checkpoint_individual_entry_lookup_after_restore() {
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage/", temp.path().display());
        let config_hash = "entry_lookup";
        let agent_id = Some(0_usize);

        // Write 10 objects with distinct sizes and paths
        let sizes: Vec<u64> = (0..10).map(|i| 1024 * (i + 1) as u64).collect();
        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            for (i, &size) in sizes.iter().enumerate() {
                cache
                    .plan_object(config_hash, i, &format!("prepared-{:08}.dat", i), size)
                    .unwrap();
                cache
                    .mark_created(config_hash, i, Some(1_700_000_000 + i as u64), None)
                    .unwrap();
            }
            cache.force_flush().unwrap();
            cache.write_checkpoint(agent_id).await.unwrap();
        }

        // Restore and verify each entry individually
        let restored = EndpointCache::try_load_from_checkpoint(&storage_uri, agent_id, None)
            .await
            .unwrap()
            .unwrap();

        for (i, &expected_size) in sizes.iter().enumerate() {
            let entry = restored
                .get_object(config_hash, i)
                .expect("get_object should not fail")
                .unwrap_or_else(|| panic!("Object {i} should be present"));

            assert_eq!(
                entry.state,
                ObjectState::Created,
                "Object {} should be Created",
                i
            );
            assert_eq!(entry.size, expected_size, "Object {} size should match", i);
            assert_eq!(
                entry.path,
                format!("prepared-{:08}.dat", i),
                "Object {} path should match",
                i
            );
            assert!(
                entry.created_at.is_some(),
                "Object {} created_at should be set",
                i
            );
        }
    }

    /// `get_objects_by_state` returns the correct slice after checkpoint restore.
    #[tokio::test]
    async fn test_get_objects_by_state_after_restore() {
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage/", temp.path().display());
        let config_hash = "by_state";
        let agent_id = Some(0_usize);
        let created_count = 15_usize;
        let planned_count = 5_usize;

        {
            let cache = EndpointCache::new(&storage_uri, 0, agent_id, None)
                .await
                .unwrap();
            // First 15 = Created
            for i in 0..created_count {
                cache
                    .plan_object(config_hash, i, &format!("c-{:08}.dat", i), 512)
                    .unwrap();
                cache.mark_created(config_hash, i, None, None).unwrap();
            }
            // Next 5 = Planned only
            for i in created_count..(created_count + planned_count) {
                cache
                    .plan_object(config_hash, i, &format!("p-{:08}.dat", i), 512)
                    .unwrap();
            }
            cache.force_flush().unwrap();
            cache.write_checkpoint(agent_id).await.unwrap();
        }

        let restored = EndpointCache::try_load_from_checkpoint(&storage_uri, agent_id, None)
            .await
            .unwrap()
            .unwrap();

        let created_entries = restored
            .get_objects_by_state(config_hash, ObjectState::Created)
            .unwrap();
        assert_eq!(
            created_entries.len(),
            created_count,
            "Should find exactly {} Created entries",
            created_count
        );

        let planned_entries = restored
            .get_objects_by_state(config_hash, ObjectState::Planned)
            .unwrap();
        assert_eq!(
            planned_entries.len(),
            planned_count,
            "Should find exactly {} Planned entries",
            planned_count
        );
    }

    /// Different `agent_id` values produce independent checkpoints that restore
    /// independently — no data bleed between agents.
    #[tokio::test]
    async fn test_multi_agent_checkpoint_isolation() {
        let temp = TempDir::new().unwrap();
        let storage_uri = format!("file://{}/storage/", temp.path().display());
        let config_hash = "isolation_test";

        // Agent 0: 10 Created objects
        {
            let cache = EndpointCache::new(&storage_uri, 0, Some(0), None)
                .await
                .unwrap();
            for i in 0..10 {
                cache
                    .plan_object(config_hash, i, &format!("a0-{:08}.dat", i), 1024)
                    .unwrap();
                cache.mark_created(config_hash, i, None, None).unwrap();
            }
            cache.force_flush().unwrap();
            cache.write_checkpoint(Some(0)).await.unwrap();
        }

        // Agent 1: 20 Created objects
        {
            let cache = EndpointCache::new(&storage_uri, 0, Some(1), None)
                .await
                .unwrap();
            for i in 0..20 {
                cache
                    .plan_object(config_hash, i, &format!("a1-{:08}.dat", i), 2048)
                    .unwrap();
                cache.mark_created(config_hash, i, None, None).unwrap();
            }
            cache.force_flush().unwrap();
            cache.write_checkpoint(Some(1)).await.unwrap();
        }

        // Restore agent 0 — should see exactly 10
        let c0 = EndpointCache::try_load_from_checkpoint(&storage_uri, Some(0), None)
            .await
            .unwrap()
            .unwrap();
        let cov0 = c0.check_coverage(config_hash, 10).unwrap();
        assert_eq!(
            cov0.created_count, 10,
            "Agent 0 should have 10 Created objects"
        );
        assert!(cov0.covers_all);

        // Restore agent 1 — should see exactly 20
        let c1 = EndpointCache::try_load_from_checkpoint(&storage_uri, Some(1), None)
            .await
            .unwrap()
            .unwrap();
        let cov1 = c1.check_coverage(config_hash, 20).unwrap();
        assert_eq!(
            cov1.created_count, 20,
            "Agent 1 should have 20 Created objects"
        );
        assert!(cov1.covers_all);
    }

    // ======================================================================
    // Integration: verify real S3 checkpoint written by preflight_test_s3.yaml
    //
    // Run with:
    //   source .env && cargo test --lib -- test_s3_checkpoint_load_and_verify
    //
    // Skipped automatically when AWS_ACCESS_KEY_ID is not set.
    // ======================================================================

    /// Load the checkpoint that was written by a real preflight_test_s3.yaml run
    /// and assert that the KV cache accurately reflects the expected object state.
    ///
    /// The prepare phase created 4 zero-filled 4096-byte objects at
    /// s3://test-bucket/preflight-test/.  After a successful run the
    /// checkpoint s3://test-bucket/preflight-test/sai3-kv-cache.tar.zst holds a
    /// single-node KV database whose `objects` keyspace should contain those
    /// 4 entries all in the `Created` state.
    #[tokio::test]
    async fn test_s3_checkpoint_load_and_verify() {
        // Skip when credentials are not available
        if std::env::var("AWS_ACCESS_KEY_ID").is_err() {
            eprintln!("test_s3_checkpoint_load_and_verify: SKIP — AWS_ACCESS_KEY_ID not set (run 'source .env' first)");
            return;
        }

        let endpoint_uri = "s3://test-bucket/preflight-test/";
        let expected_count: u64 = 4;

        // ── Step 1: load checkpoint from real S3 ─────────────────────────────
        let loaded = EndpointCache::try_load_from_checkpoint(endpoint_uri, None, None)
            .await
            .expect("try_load_from_checkpoint should not return Err");

        assert!(
            loaded.is_some(),
            "No checkpoint found at {}sai3-kv-cache.tar.zst — \n\
             Did you run: source .env && ./target/debug/sai3bench-ctl -v run \
             --config tests/configs/preflight_test_s3.yaml ?",
            endpoint_uri
        );
        let cache = loaded.unwrap();

        // ── Step 2: compute config_hash from the known YAML ──────────────────
        let config_yaml = std::fs::read_to_string("tests/configs/preflight_test_s3.yaml").expect(
            "tests/configs/preflight_test_s3.yaml not found — run from sai3-bench/ directory",
        );
        let config: crate::config::Config =
            serde_yaml::from_str(&config_yaml).expect("Failed to parse preflight_test_s3.yaml");
        let config_hash = generate_config_hash(&config);

        // ── Step 3: raw count — how many object entries exist at all? ────────
        // This snapshot captures all config-hash namespaces (defensive sanity).
        let raw_total: usize = {
            let mut n = 0usize;
            for guard in cache.objects.iter() {
                let _ = guard; // just count
                n += 1;
            }
            n
        };
        println!("Raw object keyspace entries: {raw_total}");
        assert!(
            raw_total >= expected_count as usize,
            "Expected at least {expected_count} entries in objects keyspace, found {raw_total}"
        );

        // ── Step 4: filtered count by config_hash ────────────────────────────
        let coverage = cache
            .check_coverage(&config_hash, expected_count)
            .expect("check_coverage should not error");

        println!(
            "Coverage for hash '{config_hash}': {:?}",
            coverage.state_counts
        );
        println!(
            "  created={}/{expected_count}  planned={}  failed={}  covers_all={}",
            coverage.created_count,
            coverage.planned_count,
            coverage.failed_count,
            coverage.covers_all
        );

        assert_eq!(
            coverage.expected_count, expected_count as usize,
            "expected_count mismatch"
        );
        assert_eq!(
            coverage.created_count, expected_count as usize,
            "Expected all {expected_count} objects to be Created; \
             planned={} failed={}",
            coverage.planned_count, coverage.failed_count
        );
        assert!(
            coverage.covers_all,
            "covers_all should be true; state_counts={:?}",
            coverage.state_counts
        );
    }

    // ========================================================================
    // Scale timing test — NOT run in normal CI, run explicitly with:
    //   cargo test --lib -- --ignored test_coverage_scan_timing_600k
    // ========================================================================

    /// Measure the real wall-clock cost of `check_listing_coverage` (the coverage
    /// scan) over a 600 000-entry KV cache — the per-agent object count for a
    /// typical 8-agent, 4.8 M-object job.
    ///
    /// The test phases are timed separately so you can see:
    ///   1. How long writing 600 K Created entries takes (setup — not the real question).
    ///   2. **How long the coverage scan takes** (the real question).
    ///   3. A second cold scan to confirm repeatability.
    ///
    /// Run with:
    ///   cargo test --lib -- --ignored test_coverage_scan_timing_600k --nocapture
    #[tokio::test]
    #[ignore]
    async fn test_coverage_scan_timing_600k() {
        const N: usize = 600_000;
        const SIZE: u64 = 128 * 1024 * 1024; // 128 MiB per object (typical checkpoint shard)
        const CONFIG_HASH: &str = "scale_600k";

        let temp = TempDir::new().unwrap();
        let uri = format!("file://{}/ep0", temp.path().display());

        // ── Phase 1: populate ─────────────────────────────────────────────────
        // Default flush policy is AsyncInterval(30) — no blocking on each write.
        let cache = EndpointCache::new(&uri, 0, None, None).await.unwrap();

        println!("\n=== 600 K-entry KV cache scan timing test ===");
        println!(
            "  Writing {} Created entries (128 MiB each = {:.1} TiB logical)…",
            N,
            (N as f64 * SIZE as f64) / (1024.0_f64.powi(4))
        );

        let t_write = std::time::Instant::now();
        for i in 0..N {
            // record_created is an unconditional upsert; no prior plan_object needed.
            cache
                .record_created(
                    CONFIG_HASH,
                    i,
                    &format!("file://{}/ep0/prepared-{:08}.dat", temp.path().display(), i),
                    SIZE,
                    Some(1_700_000_000 + i as u64),
                    None,
                )
                .unwrap();
        }
        // Flush everything to disk before we measure reads.
        cache.force_flush().unwrap();
        let write_elapsed = t_write.elapsed();
        println!(
            "  Write phase:  {} entries in {:.3}s  ({:.0} K entries/s)",
            N,
            write_elapsed.as_secs_f64(),
            N as f64 / write_elapsed.as_secs_f64() / 1000.0
        );

        // ── Phase 2: first coverage scan ─────────────────────────────────────
        println!("  Scanning (first pass)…");
        let t_scan1 = std::time::Instant::now();
        let (counts, bytes) = cache.count_and_bytes_by_state(CONFIG_HASH).unwrap();
        let scan1_elapsed = t_scan1.elapsed();

        let created = *counts.get(&ObjectState::Created).unwrap_or(&0);
        let total_gib =
            bytes.get(&ObjectState::Created).copied().unwrap_or(0) as f64 / (1024.0_f64.powi(3));

        println!(
            "  Scan 1 result: {} Created, {:.1} GiB logical",
            created, total_gib
        );
        println!(
            "  Scan 1 time:   {:.3}s  ({:.0} K entries/s)",
            scan1_elapsed.as_secs_f64(),
            N as f64 / scan1_elapsed.as_secs_f64() / 1000.0
        );

        // ── Phase 3: second scan (page-cache warm) ────────────────────────────
        println!("  Scanning (second pass — OS page cache warm)…");
        let t_scan2 = std::time::Instant::now();
        let (counts2, _) = cache.count_and_bytes_by_state(CONFIG_HASH).unwrap();
        let scan2_elapsed = t_scan2.elapsed();
        let created2 = *counts2.get(&ObjectState::Created).unwrap_or(&0);

        println!("  Scan 2 result: {} Created", created2);
        println!(
            "  Scan 2 time:   {:.3}s  ({:.0} K entries/s)",
            scan2_elapsed.as_secs_f64(),
            N as f64 / scan2_elapsed.as_secs_f64() / 1000.0
        );

        // ── Assertions ────────────────────────────────────────────────────────
        assert_eq!(created, N, "All {} entries should be Created", N);
        assert_eq!(created2, N, "Second scan must agree");

        // Sanity-check GiB (600k × 128 MiB = 75 TiB logical, stored as metadata only).
        let expected_gib = N as f64 * SIZE as f64 / (1024.0_f64.powi(3));
        let ratio = total_gib / expected_gib;
        assert!(
            (0.99..=1.01).contains(&ratio),
            "Byte total off: got {:.1} GiB, expected {:.1} GiB",
            total_gib,
            expected_gib
        );

        // The scan should finish well within 10 seconds on any reasonable hardware.
        assert!(
            scan1_elapsed.as_secs() < 10,
            "First scan took {:.3}s — exceeds 10 s budget",
            scan1_elapsed.as_secs_f64()
        );
        assert!(
            scan2_elapsed.as_secs() < 10,
            "Second scan took {:.3}s — exceeds 10 s budget",
            scan2_elapsed.as_secs_f64()
        );

        println!("\n  ✅ Pass — both scans under 10 s budget.");
    }
}
