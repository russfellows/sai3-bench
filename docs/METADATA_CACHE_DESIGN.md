# Metadata Cache Design with fjall

**Version**: v0.8.54+ proposal  
**Author**: Design discussion for 64M+ file scale  
**Date**: February 2026

## Problem Statement

For very large workloads (64 million+ files/objects), sai3-bench faces significant overhead:

### 1. **Directory Tree Generation** (Computational)
- `DirectoryTree::generate()` computes all paths in-memory
- 64M files with depth=4, width=100 → millions of directories
- Path string allocations, HashMap inserts, file range calculations
- **Current**: Every test run regenerates the entire tree

### 2. **File Path Lookups** (O(n) iteration)
```rust
// TreeManifest::get_file_path(global_idx) - SLOW for large n
for (dir_path, (start, end)) in &self.file_ranges {  // ← Iterates ALL dirs
    if global_idx >= *start && global_idx < *end {
        return Some(format!("{}/{}", dir_path, file_name));
    }
}
```
- **64M files scenario**: Up to 64M linear scans for workload path resolution
- **Multi-endpoint**: Additional endpoint calculation per file

### 3. **Object Storage LIST Operations** (I/O bound)
- S3/Azure/GCS listing: ~20k objects/sec maximum throughput
- **64M objects**: 53+ minutes for a single LIST (3200 seconds / 60)
- Multiple agents help, but still hours of total I/O
- **Current**: Every test run LISTs all objects from scratch

### 4. **Multi-Endpoint Distribution** (Repeated calculations)
- Round-robin: `file_idx % num_endpoints` computed millions of times
- Agent assignments: `file_idx % num_agents` for distributed mode
- **Current**: Recomputed every operation

### 5. **Reproducibility** (State tracking)
- No saved RNG seeds → cannot reproduce exact file distributions
- Config changes invalidate previous test data
- Lost metadata when tests abort mid-flight

## Solution: fjall-based Metadata Cache

[fjall](https://docs.rs/fjall/) is an embedded LSM-tree KV store (similar to RocksDB, written in Rust):
- **Embedded**: No external server, just library dependency
- **Fast**: O(log n) lookups vs O(n) iteration
- **Persistent**: Disk-backed, survives restarts
- **Compressed**: Built-in LZ4 compression
- **Transactional**: Atomic cross-keyspace operations
- **Keyspaces**: Multiple logical databases in one physical database

## Architecture Design

### Distributed Cache Strategy

**Key Insight**: Each endpoint stores metadata ONLY for objects it contains. This distributes storage requirements and enables parallel cache access.

### Cache Location (Per-Endpoint)

```
Endpoint 1: file:///mnt/nvme1/testdata/
├── sai3-kv-cache/              ← Cache database for this endpoint only
│   ├── file_paths/             ← Only files on nvme1
│   ├── listing_cache/          ← Only nvme1 LIST results
│   ├── endpoint_metadata/      ← Endpoint-specific config
│   └── seeds/                  ← RNG seeds for this endpoint
├── d1_w1.dir/
│   ├── file_00000000.dat       ← Actual data files
│   └── file_00000004.dat
└── d1_w2.dir/
    └── file_00000008.dat

Endpoint 2: file:///mnt/nvme2/testdata/
├── sai3-kv-cache/              ← Separate cache for nvme2
│   ├── file_paths/
│   ├── listing_cache/
│   └── endpoint_metadata/
├── d1_w1.dir/
│   ├── file_00000001.dat       ← Different files than endpoint 1
│   └── file_00000005.dat
└── ...

Endpoint 3: s3://bucket/testdata/
├── sai3-kv-cache/              ← S3-backed cache
│   ├── file_paths/
│   ├── listing_cache/
│   └── endpoint_metadata/
└── d1_w1.dir/
    ├── file_00000002.dat
    └── ...

Shared Coordinator Cache: {results_dir}/.sai3-coordinator-cache/
├── tree_manifests/             ← Global tree structure (shared)
├── config_metadata/            ← Test run metadata
└── endpoint_registry/          ← List of all endpoints with cache locations
```

### Keyspace Organization (Per-Endpoint)

### Key-Value Schemas

#### Coordinator Cache (Shared, Small)

Stored in results directory: `.sai3-coordinator-cache/`

##### 1. `tree_manifests` Keyspace
```
Key:   "{config_hash}:manifest"
Value: JSON-serialized TreeManifest
```

**Rationale**: Store complete tree structure once, shared across all endpoints.

**Example**:
```json
{
  "09af2e4c1b7d:manifest": {
    "total_dirs": 1000000,
    "total_files": 64000000,
    "files_per_dir": 64,
    "distribution": "bottom",
    "file_ranges": [...],  // Compressed
    "config_hash": "09af2e4c1b7d",
    ...
  }
}
```

**Storage**: ~50 MB for 64M files (compressed JSON)

##### 2. `endpoint_registry` Keyspace
```
Key:   "{config_hash}:endpoint:{endpoint_index}"
Value: JSON endpoint metadata
```

**Example**:
```json
{
  "09af2e4c:endpoint:0": {
    "uri": "file:///mnt/nvme1/testdata/",
    "cache_location": "file:///mnt/nvme1/testdata/sai3-kv-cache/",
    "assigned_file_count": 16000000,
    "strategy": "round_robin",
    "first_file_index": 0,
    "index_pattern": "i % 4 == 0"  // For 4 endpoints
  },
  "09af2e4c:endpoint:1": {
    "uri": "file:///mnt/nvme2/testdata/",
    "cache_location": "file:///mnt/nvme2/testdata/sai3-kv-cache/",
    "assigned_file_count": 16000000,
    "first_file_index": 1,
    "index_pattern": "i % 4 == 1"
  }
}
```

**Storage**: <1 MB (small registry)

#### Per-Endpoint Cache (Distributed, Large)

Each endpoint has its own `sai3-kv-cache/` directory

##### 3. `file_paths` Keyspace (Per-Endpoint)
```
Key:   "{config_hash}:{file_idx:08}"
Value: "relative/path/to/file_NNNNNNNN.dat"
```

**CRITICAL**: Each endpoint only stores paths for files it contains (based on round-robin).

**Endpoint 0 example** (files where `idx % 4 == 0`):
```
"09af2e4c:00000000" → "d1_w1.dir/d2_w1.dir/file_00000000.dat"
"09af2e4c:00000004" → "d1_w1.dir/d2_w1.dir/file_00000004.dat"
"09af2e4c:00000008" → "d1_w1.dir/d2_w2.dir/file_00000008.dat"
```

**Endpoint 1 example** (files where `idx % 4 == 1`):
```
"09af2e4c:00000001" → "d1_w1.dir/d2_w1.dir/file_00000001.dat"
"09af2e4c:00000005" → "d1_w1.dir/d2_w1.dir/file_00000005.dat"
"09af2e4c:00000009" → "d1_w1.dir/d2_w2.dir/file_00000009.dat"
```

**Storage per endpoint**: ~2 GB for 16M files (64M / 4 endpoints, compressed)

**Cache population**:
- **Lazy**: Cache paths as workload accesses them (saves I/O)
- **Eager**: Pre-populate during prepare phase (faster subsequent lookups)

##### 4. `listing_cache` Keyspace (Per-Endpoint)
```
Key:   "{config_hash}:list_timestamp:{timestamp}"
Value: zstd-compressed list of file paths (only this endpoint's files)
```

**Rationale**: Cache LIST results per-endpoint. Each endpoint only caches its own files.

**Endpoint 0 example** (`s3://bucket/testdata/`):
```
"09af2e4c:list_timestamp:1738886400" → 
  [compressed] "d1_w1.dir/file_00000000.dat\nd1_w1.dir/file_00000004.dat\n..."
```

**Key benefit**: Listing 64M files across 4 endpoints = 4 parallel 16M LISTs instead of 1 serial 64M LIST.

**Storage per endpoint**: ~1 GB for 16M files (zstd compressed paths)

**TTL Strategy**:
- Object storage (s3://, az://, gs://): 24 hours default
- Local file:// : No caching (direct filesystem access is fast)
- Force refresh: `--refresh-cache` flag

##### 5. `endpoint_metadata` Keyspace (Per-Endpoint)
```
Key:   "{config_hash}:{me_strategy}:{file_idx:08}"
Value: endpoint_index (u16 or u32)
```

**Rationale**: Pre-compute multi-endpoint round-robin distribution once.

**Example** (4 endpoints, round-robin):
```
"09af2e4c:round_robin:00000000" → 0
"09af2e4c:round_robin:00000001" → 1
"09af2e4c:round_robin:00000002" → 2
"09af2e4c:round_robin:00000003" → 3
"09af2e4c:round_robin:00000004" → 0  (wraps)
```

#### 5. `agent_map` Keyspace
```
Key:   "{config_hash}:{num_agents}:{file_idx:08}"
Value: agent_id (u16)
```

**Rationale**: Pre-compute agent assignments for distributed cleanup/verification.

**Example** (8 agents):
```
"09af2e4c:8:00000000" → 0
"09af2e4c:8:00000001" → 1
"09af2e4c:8:00000007" → 7
"09af2e4c:8:00000008" → 0  (wraps)
```

##### 7. `config_metadata` Keyspace
```
Key:   "{config_hash}"
Value: JSON metadata about test configuration and endpoints
```

**Example**:
```json
{
  "config_hash": "09af2e4c1b7d",
  "last_used": "2026-02-09T19:30:00Z",
  "test_name": "64m_files_s3_benchmark",
  "total_files": 64000000,
  "total_endpoints": 4,
  "endpoints": [
    "file:///mnt/nvme1/testdata/",
    "file:///mnt/nvme2/testdata/",
    "s3://bucket/testdata/",
    "az://container/testdata/"
  ],
  "distribution_strategy": "round_robin",
  "tree_manifest_cached": true,
  "cache_version": "1.0"
}
```

**Storage**: <1 MB total

## Implementation Plan

### Phase 1: Core Infrastructure (v0.8.54)

**New module**: `src/metadata_cache.rs`

```rust
use fjall::{Config, Keyspace, PartitionCreateOptions};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::collections::HashMap;

/// Distributed metadata cache - per-endpoint storage
pub struct EndpointCache {
    db: fjall::PartitionHandle,
    
    // Keyspaces (per-endpoint)
    file_paths: Keyspace,
    listing_cache: Keyspace,
    endpoint_metadata: Keyspace,
    seeds: Keyspace,
    
    // Endpoint info
    endpoint_uri: String,
    endpoint_index: usize,
    cache_location: PathBuf,
    enable_compression: bool,
}

/// Coordinator cache - shared global metadata
pub struct CoordinatorCache {
    db: fjall::PartitionHandle,
    
    // Keyspaces (coordinator)
    tree_manifests: Keyspace,
    endpoint_registry: Keyspace,
    config_metadata: Keyspace,
    
    cache_dir: PathBuf,
}

/// Multi-endpoint cache manager - orchestrates distributed caches
pub struct MetadataCache {
    coordinator: CoordinatorCache,
    endpoints: HashMap<usize, EndpointCache>,
    config_hash: String,
}

impl EndpointCache {
    /// Create or open cache database at endpoint location
    /// 
    /// Cache location: {endpoint_uri}/sai3-kv-cache/
    pub async fn new(
        endpoint_uri: &str,
        endpoint_index: usize,
        enable_compression: bool,
    ) -> Result<Self> {
        // Construct cache path at endpoint root
        let cache_subpath = "sai3-kv-cache";
        let cache_uri = if endpoint_uri.ends_with('/') {
            format!("{}{}", endpoint_uri, cache_subpath)
        } else {
            format!("{}/{}", endpoint_uri, cache_subpath)
        };
        
        // For object storage (s3://, az://, gs://), cache is stored AS objects
        // For local file://, cache is a directory
        let cache_location = Self::resolve_cache_location(&cache_uri)?;
        
        let mut config = Config::new(&cache_location);
        if enable_compression {
            config = config.compression(fjall::CompressionType::Lz4);
        }
        
        let db = config.open()?;
        
        // Create keyspaces (per-endpoint: only this endpoint's data)
        let file_paths = db.open_keyspace("file_paths", 
            PartitionCreateOptions::default())?;
        let listing_cache = db.open_keyspace("listing_cache", 
            PartitionCreateOptions::default())?;
        let endpoint_metadata = db.open_keyspace("endpoint_metadata", 
            PartitionCreateOptions::default())?;
        let seeds = db.open_keyspace("seeds", 
            PartitionCreateOptions::default())?;
        
        Ok(EndpointCache {
            db,
            file_paths,
            listing_cache,
            endpoint_metadata,
            seeds,
            endpoint_uri: endpoint_uri.to_string(),
            endpoint_index,
            cache_location,
            enable_compression,
        })
    }
    
    /// Resolve cache location based on storage backend
    fn resolve_cache_location(cache_uri: &str) -> Result<PathBuf> {
        if cache_uri.starts_with("file://") {
            // Local filesystem: strip file:// prefix
            let path = cache_uri.strip_prefix("file://").unwrap();
            Ok(PathBuf::from(path))
        } else if cache_uri.starts_with("s3://") || cache_uri.starts_with("az://") || 
                  cache_uri.starts_with("gs://") {
            // Object storage: Download cache to local temp directory
            // Cache is stored AS objects in cloud, synced to local for fjall access
            let hash = seahash::hash(cache_uri.as_bytes());
            let temp_cache = std::env::temp_dir()
                .join("sai3-cache")
                .join(format!("endpoint-{:x}", hash));
            std::fs::create_dir_all(&temp_cache)?;
            Ok(temp_cache)
        } else {
            anyhow::bail!("Unsupported cache URI scheme: {}", cache_uri);
        }
    }
    
    /// Check if this endpoint should handle a specific file index
    /// Uses modulo distribution: file_idx % total_endpoints == endpoint_index
    pub fn owns_file(&self, file_idx: usize, total_endpoints: usize) -> bool {
        file_idx % total_endpoints == self.endpoint_index
    }
}

impl CoordinatorCache {
    /// Create or open coordinator cache in results directory
    pub fn new(results_dir: &Path) -> Result<Self> {
        let cache_dir = results_dir.join(".sai3-coordinator-cache");
        std::fs::create_dir_all(&cache_dir)?;
        
        let config = Config::new(&cache_dir)
            .compression(fjall::CompressionType::Lz4);
        
        let db = config.open()?;
        
        // Create coordinator keyspaces (global metadata)
        let tree_manifests = db.open_keyspace("tree_manifests", 
            PartitionCreateOptions::default())?;
        let endpoint_registry = db.open_keyspace("endpoint_registry", 
            PartitionCreateOptions::default())?;
        let config_metadata = db.open_keyspace("config_metadata", 
            PartitionCreateOptions::default())?;
        
        Ok(CoordinatorCache {
            db,
            tree_manifests,
            endpoint_registry,
            config_metadata,
            cache_dir,
        })
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
        enable_compression: bool,
    ) -> Result<Self> {
        info!("Initializing distributed metadata cache:");
        info!("  Config hash: {}", config_hash);
        info!("  Endpoints: {}", endpoint_uris.len());
        
        // Open coordinator cache (shared global metadata)
        let coordinator = CoordinatorCache::new(results_dir)?;
        info!("  ✓ Coordinator cache: {}", results_dir.join(".sai3-coordinator-cache").display());
        
        // Open per-endpoint caches (distributed data)
        let mut endpoints = HashMap::new();
        for (idx, uri) in endpoint_uris.iter().enumerate() {
            let endpoint_cache = EndpointCache::new(uri, idx, enable_compression).await?;
            info!("  ✓ Endpoint {} cache: {}", idx, endpoint_cache.cache_location.display());
            endpoints.insert(idx, endpoint_cache);
        }
        
        Ok(MetadataCache {
            coordinator,
            endpoints,
            config_hash,
        })
    }
    
    /// Get endpoint cache for a specific file index
    pub fn endpoint_for_file(&self, file_idx: usize) -> Option<&EndpointCache> {
        let endpoint_idx = file_idx % self.endpoints.len();
        self.endpoints.get(&endpoint_idx)
    }
    
    /// Get endpoint cache by index
    pub fn endpoint(&self, endpoint_idx: usize) -> Option<&EndpointCache> {
        self.endpoints.get(&endpoint_idx)
    }
    
    /// Get tree manifest from coordinator cache (global, shared)
    pub fn get_tree_manifest(&self) -> Result<Option<TreeManifest>> {
        let key = format!("{}:manifest", self.config_hash);
        if let Some(bytes) = self.coordinator.tree_manifests.get(key)? {
            let json = std::str::from_utf8(&bytes)?;
            let manifest = TreeManifest::from_json(json)?;
            Ok(Some(manifest))
        } else {
            Ok(None)
        }
    }
    
    /// Cache tree manifest in coordinator cache
    pub fn put_tree_manifest(&self, manifest: &TreeManifest) -> Result<()> {
        let key = format!("{}:manifest", self.config_hash);
        let json = manifest.to_json()?;
        self.coordinator.tree_manifests.insert(key, json.as_bytes())?;
        self.coordinator.db.persist(fjall::PersistMode::SyncAll)?;
        info!("✓ Cached TreeManifest in coordinator cache");
        Ok(())
    }
    
    /// Get file path for a specific file index (O(1) lookup from correct endpoint)
    pub fn get_file_path(&self, file_idx: usize) -> Result<Option<String>> {
        // Route to correct endpoint based on file index
        if let Some(endpoint) = self.endpoint_for_file(file_idx) {
            let key = format!("{}:{:08}", self.config_hash, file_idx);
            if let Some(bytes) = endpoint.file_paths.get(key)? {
                let path = String::from_utf8(bytes.to_vec())?;
                return Ok(Some(path));
            }
        }
        Ok(None)
    }
    
    /// Cache file path at correct endpoint
    pub fn put_file_path(&self, file_idx: usize, path: &str) -> Result<()> {
        if let Some(endpoint) = self.endpoint_for_file(file_idx) {
            let key = format!("{}:{:08}", self.config_hash, file_idx);
            endpoint.file_paths.insert(key, path.as_bytes())?;
            endpoint.db.persist(fjall::PersistMode::SyncData)?;
        }
        Ok(())
    }
    
    /// Batch insert file paths at correct endpoints (distributed)
    pub fn put_file_paths_batch(&self, paths: Vec<(usize, String)>) -> Result<()> {
        // Group paths by endpoint
        let mut endpoint_batches: HashMap<usize, Vec<(usize, String)>> = HashMap::new();
        for (idx, path) in paths {
            let endpoint_idx = idx % self.endpoints.len();
            endpoint_batches.entry(endpoint_idx).or_default().push((idx, path));
        }
        
        // Write batches to each endpoint in parallel
        info!("Distributing {} file paths across {} endpoints", 
              endpoint_batches.values().map(|v| v.len()).sum::<usize>(),
              endpoint_batches.len());
        
        for (endpoint_idx, batch) in endpoint_batches {
            if let Some(endpoint) = self.endpoints.get(&endpoint_idx) {
                let mut fjall_batch = endpoint.file_paths.batch();
                for (idx, path) in batch {
                    let key = format!("{}:{:08}", self.config_hash, idx);
                    fjall_batch.insert(key.as_bytes(), path.as_bytes());
                }
                fjall_batch.commit()?;
                endpoint.db.persist(fjall::PersistMode::SyncData)?;
                info!("  ✓ Endpoint {}: cached {} paths", endpoint_idx, 
                      endpoint_batches.get(&endpoint_idx).map(|v| v.len()).unwrap_or(0));
            }
        }
        
        Ok(())
    }
    
    /// Get cached listing results (if not expired)
    pub fn get_listing_cache(&self, uri_prefix: &str, ttl_secs: u64) -> Result<Option<Vec<String>>> {
        // Find most recent listing for this prefix
        let pattern = format!("{}:list_timestamp:", uri_prefix);
        
        // Iterate in reverse to find newest first
        for entry in self.listing_cache.range(&pattern[..]).rev() {
            if let Ok((key_bytes, value_bytes)) = entry {
                let key = String::from_utf8(key_bytes.to_vec())?;
                
                // Parse timestamp from key
                if let Some(ts_str) = key.strip_prefix(&pattern) {
                    if let Ok(timestamp) = ts_str.parse::<u64>() {
                        let age_secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_secs() - timestamp;
                        
                        if age_secs <= ttl_secs {
                            // Cache hit - decompress and return
                            let compressed = value_bytes.to_vec();
                            let json = zstd::decode_all(&compressed[..])?;
                            let files: Vec<String> = serde_json::from_slice(&json)?;
                            return Ok(Some(files));
                        } else {
                            // Expired - delete and continue searching
                            self.listing_cache.remove(key)?;
                        }
                    }
                }
            }
        }
        
        Ok(None)  // Cache miss
    }
    
    /// Cache listing results
    pub fn put_listing_cache(&self, uri_prefix: &str, files: &[String]) -> Result<()> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        
        let key = format!("{}:list_timestamp:{}", uri_prefix, timestamp);
        
        // Compress file list with zstd (better than LZ4 for text)
        let json = serde_json::to_vec(files)?;
        let compressed = zstd::encode_all(&json[..], 3)?;  // Level 3 = good compression/speed
        
        self.listing_cache.insert(key, &compressed)?;
        self.db.persist(fjall::PersistMode::SyncData)?;  // Faster persist, still safe
        Ok(())
    }
    
    /// Get endpoint index for file (pre-computed distribution)
    pub fn get_endpoint_index(&self, config_hash: &str, me_strategy: &str, 
                                file_idx: usize) -> Result<Option<usize>> {
        let key = format!("{}:{}:{:08}", config_hash, me_strategy, file_idx);
        if let Some(bytes) = self.endpoint_map.get(key)? {
            let idx = u32::from_le_bytes(bytes.as_ref().try_into()?);
            Ok(Some(idx as usize))
        } else {
            Ok(None)
        }
    }
    
    /// Pre-compute and cache endpoint distribution (batch operation)
    pub fn populate_endpoint_map(&self, config_hash: &str, me_strategy: &str,
                                   total_files: usize, num_endpoints: usize) -> Result<()> {
        info!("Pre-computing endpoint distribution: {} files across {} endpoints", 
              total_files, num_endpoints);
        
        let mut batch = self.endpoint_map.batch();
        const BATCH_SIZE: usize = 100_000;
        
        for file_idx in 0..total_files {
            let endpoint_idx = match me_strategy {
                "round_robin" => file_idx % num_endpoints,
                "random" => {
                    // Use seeded RNG for reproducibility
                    let seed = self.get_seed(config_hash, "endpoint_rng", 0)?.unwrap_or(42);
                    let mut rng = rand::rngs::StdRng::seed_from_u64(seed + file_idx as u64);
                    rng.gen_range(0..num_endpoints)
                }
                _ => 0,  // Fallback
            };
            
            let key = format!("{}:{}:{:08}", config_hash, me_strategy, file_idx);
            batch.insert(key.as_bytes(), &(endpoint_idx as u32).to_le_bytes());
            
            // Commit batch every BATCH_SIZE inserts
            if (file_idx + 1) % BATCH_SIZE == 0 {
                batch.commit()?;
                batch = self.endpoint_map.batch();
                
                if file_idx % 1_000_000 == 0 {
                    info!("  Endpoint map progress: {}/{} files ({:.1}%)", 
                          file_idx, total_files, 
                          (file_idx as f64 / total_files as f64) * 100.0);
                }
            }
        }
        
        // Commit remaining
        batch.commit()?;
        self.db.persist(fjall::PersistMode::SyncAll)?;
        
        info!("Endpoint map population complete: {} entries", total_files);
        Ok(())
    }
    
    /// Similar methods for agent_map, seeds, etc...
}
```

### Phase 2: Integration with Prepare Phase (v0.8.54)

**Modify**: `src/prepare/mod.rs`

```rust
pub async fn prepare_objects(
    config: &PrepareConfig,
    // ... existing args ...
    metadata_cache: Option<&MetadataCache>,  // ← NEW
) -> Result<(Vec<PreparedObject>, Option<TreeManifest>, PrepareMetrics)> {
    
    // Compute config hash for cache key
    let config_hash = compute_config_hash(config);
    
    // Try to load tree manifest from cache
    let tree_manifest = if let Some(cache) = metadata_cache {
        if let Some(cached_manifest) = cache.get_tree_manifest(&config_hash)? {
            info!("✓ Loaded TreeManifest from cache (config hash: {})", config_hash);
            Some(cached_manifest)
        } else {
            // Cache miss - generate and cache
            let manifest = create_directory_tree(...).await?;
            cache.put_tree_manifest(&config_hash, &manifest)?;
            info!("✓ Cached TreeManifest for future runs (config hash: {})", config_hash);
            Some(manifest)
        }
    } else {
        // No cache - generate normally
        create_directory_tree(...).await?
    };
    
    // ... rest of prepare logic ...
}
```

**Modify**: `src/prepare/listing.rs`

```rust
pub async fn list_existing_objects_distributed(
    store: &dyn ObjectStore,
    base_uri: &str,
    tree_manifest: Option<&TreeManifest>,
    agent_id: usize,
    num_agents: usize,
    live_stats_tracker: Option<&Arc<LiveStatsTracker>>,
    expected_total: u64,
    metadata_cache: Option<&MetadataCache>,  // ← NEW
    cache_ttl_secs: u64,  // ← NEW
) -> Result<ListingResult> {
    
    // Try cache first (for object storage only)
    if let Some(cache) = metadata_cache {
        if base_uri.starts_with("s3://") || base_uri.starts_with("az://") || 
           base_uri.starts_with("gs://") {
            
            if let Some(cached_files) = cache.get_listing_cache(base_uri, cache_ttl_secs)? {
                info!("✓ Loaded {} files from listing cache (age < {}s)", 
                      cached_files.len(), cache_ttl_secs);
                
                // Parse and return cached listing
                let mut result = ListingResult::default();
                for path in cached_files {
                    process_list_item(&path, &mut result);
                }
                return Ok(result);
            } else {
                info!("Listing cache miss or expired - performing fresh LIST");
            }
        }
    }
    
    // Cache miss - perform LIST and cache results
    let mut result = ListingResult::default();
    
    // ... existing LIST logic ...
    
    // Cache the results for next time
    if let Some(cache) = metadata_cache {
        if result.file_count > 10_000 {  // Only cache large listings
            let files: Vec<String> = result.indices.iter()
                .filter_map(|&idx| {
                    if let Some(manifest) = tree_manifest {
                        manifest.get_file_path(idx)
                    } else {
                        None
                    }
                })
                .collect();
            
            cache.put_listing_cache(base_uri, &files)?;
            info!("✓ Cached {} file paths for future runs", files.len());
        }
    }
    
    Ok(result)
}
```

### Phase 3: CLI Integration (v0.8.54)

**Add to** `Config` struct:

```rust
pub struct Config {
    // ... existing fields ...
    
    /// Enable metadata caching (default: true for workloads >100k files)
    #[serde(default)]
    pub enable_metadata_cache: bool,
    
    /// Cache directory location (default: .sai3-cache in results dir)
    pub cache_dir: Option<PathBuf>,
    
    /// Listing cache TTL in seconds (default: 86400 = 24h for object storage)
    #[serde(default = "default_cache_ttl")]
    pub listing_cache_ttl: u64,
    
    /// Force refresh cached data (ignore existing cache)
    #[serde(default)]
    pub refresh_cache: bool,
}

fn default_cache_ttl() -> u64 { 86400 }  // 24 hours
```

**CLI flags**:

```bash
# Use cache (auto-enabled for large workloads)
sai3bench-ctl run --config huge_test.yaml

# Force refresh cache (re-LIST everything)
sai3bench-ctl run --config huge_test.yaml --refresh-cache

# Disable cache entirely
sai3bench-ctl run --config huge_test.yaml --no-cache

# Custom cache directory
sai3bench-ctl run --config huge_test.yaml --cache-dir /path/to/cache

# Custom TTL (1 hour for frequently changing data)
sai3bench-ctl run --config huge_test.yaml --cache-ttl 3600
```

### Phase 4: Performance Optimizations (v0.8.55+)

#### File Path Lookup Optimization

**Before** (O(n) iteration):
```rust
// TreeManifest::get_file_path() - slow
for (dir_path, (start, end)) in &self.file_ranges {  // 64M iterations worst case
    if global_idx >= *start && global_idx < *end {
        return Some(format!("{}/{}", dir_path, file_name));
    }
}
```

**After** (O(1) cache lookup):
```rust
// Fast path: Check cache first
if let Some(cache) = self.metadata_cache {
    if let Some(path) = cache.get_file_path(&self.config_hash, file_idx)? {
        return Ok(path);  // O(1) - instant
    }
}

// Fallback: Compute and cache
let path = self.compute_file_path_uncached(file_idx)?;
if let Some(cache) = self.metadata_cache {
    cache.put_file_path(&self.config_hash, file_idx, &path)?;
}
Ok(path)
```

**Performance impact**:
- 64M file workload with 1M GET operations
- **Before**: 64M × 1M = 64 trillion comparisons (hours of CPU time)
- **After**: 1M cache lookups (milliseconds)

#### Endpoint Distribution Pre-computation

```rust
// During prepare phase (one-time cost)
if let Some(cache) = metadata_cache {
    if !cache.has_endpoint_map(&config_hash, me_strategy)? {
        info!("Pre-computing endpoint distribution (one-time cost)...");
        cache.populate_endpoint_map(&config_hash, me_strategy, 
                                      total_files, num_endpoints)?;
        info!("Endpoint map cached - future runs will be instant");
    }
}

// During workload phase (O(1) lookup)
let endpoint_idx = metadata_cache
    .get_endpoint_index(&config_hash, me_strategy, file_idx)?
    .unwrap_or_else(|| file_idx % num_endpoints);  // Fallback
```

## Expected Performance Improvements

### 64M Files, 4 Endpoints (Mixed S3 + Local), 8 Agents

| Operation | Without Cache | With Cache (First Run) | With Cache (Subsequent) | Speedup |
|-----------|---------------|------------------------|-------------------------|---------|
| Tree Generation | 45s | 45s (generate + cache) | 0.5s (load from cache) | **90x** |
| LIST Operation | 3200s (53 min) | 3200s (LIST + cache) | **8s (4 parallel loads)** | **400x** |
| File Path Lookups | 64M × O(n) | 64M × O(log n) | 64M × O(1) distributed | **10000x** |
| Endpoint Routing | 64M × modulo | 64M × calculation | **Implicit in cache key** | **Instant** |
| **Total Prepare Time** | **~55 minutes** | **~55 minutes** | **~10 seconds** | **330x** |

### Distributed Cache Benefits

**Parallel Operations** (4 endpoints):
- **LIST caching**: 4 × 16M files in parallel instead of 1 × 64M serial
  - Each endpoint LISTs and caches its 16M files independently
  - Total time: max(LIST_endpoint) instead of sum(LIST_all)
  
- **Path lookups**: Distributed across 4 fjall databases
  - No single cache bottleneck
  - Each endpoint serves ~25% of lookups
  - Scales linearly with endpoint count

**Subsequent Test Runs (Same Config)**:
1. **Tree manifest**: Load from coordinator cache **(0.5s)**
2. **Listing**: Load 4 × 16M paths in parallel **(~8 seconds total)**
   - Endpoint 0: 16M paths from nvme1 cache (2s)
   - Endpoint 1: 16M paths from nvme2 cache (2s)
   - Endpoint 2: 16M paths from S3 cache (8s, bottleneck)
   - Endpoint 3: 16M paths from Azure cache (6s)
   - **Parallel execution = 8s total** (not 2+2+8+6 = 18s)
3. **Path lookups**: O(1) distributed fjall lookups **(microseconds)**
4. **Ready to run workload**: **~10 seconds** vs **55 minutes** without cache

### Cache Discovery (Reattaching to Existing Data)

Scenario: Test run from 6 months ago, want to re-run workload

**Without cache**:
- LIST 64M objects from S3 (53 minutes)
- Compute tree structure (45 seconds)
- Total: ~55 minutes before workload starts

**With distributed cache**:
1. Check each endpoint for `sai3-kv-cache/` directory **(instant)**
2. Load coordinator cache from results dir **(0.5s)**
3. Open all endpoint caches in parallel **(2s)**
4. Validate config hash matches **(instant)**
5. **Ready to run: ~3 seconds**

**Auto-discovery**: If config hash doesn't match, cache is automatically invalidated and regenerated.

## Cache Invalidation Strategy

### Automatic Invalidation

1. **Config hash mismatch**: New width/depth/distribution → new cache key
2. **Listing TTL expiration**: Configurable per-storage-type
3. **Force refresh flag**: `--refresh-cache` CLI option

### Manual Management

```bash
# View cache stats
sai3-bench util cache-stats --cache-dir .sai3-cache

# Clear specific keyspace
sai3-bench util cache-clear --keyspace listing_cache

# Clear all cache
sai3-bench util cache-clear --all

# Export cache for sharing
sai3-bench util cache-export --output cache_backup.tar.zst

# Import cache from another run
sai3-bench util cache-import --input cache_backup.tar.zst
```

## Storage Requirements

### Distributed Storage (64M files, 4 endpoints example)

#### Coordinator Cache (Shared, in results directory)
```
.sai3-coordinator-cache/
├── tree_manifests:      ~50 MB   (global tree structure)
├── endpoint_registry:   ~1 KB    (endpoint metadata)
└── config_metadata:     ~1 MB    (test run metadata)
---------------------------------------------------
Coordinator Total:       ~51 MB   (shared across all endpoints)
```

#### Per-Endpoint Cache (Distributed across endpoints)

Each endpoint stores ONLY its assigned files (16M / 64M total):

```
Endpoint 0: file:///mnt/nvme1/testdata/sai3-kv-cache/
├── file_paths:          ~2 GB    (16M × 128 bytes, compressed)
├── listing_cache:       ~1 GB    (16M paths, zstd compressed)
├── endpoint_metadata:   ~1 KB    (endpoint config)
└── seeds:               <1 KB    (RNG seeds)
---------------------------------------------------
Per-Endpoint Total:      ~3 GB    (per endpoint)

Endpoint 1: file:///mnt/nvme2/testdata/sai3-kv-cache/  (~3 GB)
Endpoint 2: s3://bucket/testdata/sai3-kv-cache/        (~3 GB)
Endpoint 3: az://container/testdata/sai3-kv-cache/     (~3 G (listing cache)
seahash = "4.1"           # Fast hash for cache key generation
rand = { version = "0.8", features = ["std_rng"] }  # Seeded RNG
```

**No external services required** - fjall is fully embedded.

**Object storage sync**: For s3://, az://, gs:// endpoints, cache objects are synced bidirectionally:
- **First run**: Local fjall cache → Upload as objects to endpoint
- **Subsequent runs**: Download cache objects → Local fjall database
- Uses s3dlio `ObjectStore` trait for unified access across all backends
```
Coordinator cache:       ~51 MB    (results directory)
Endpoint 0 cache:        ~3 GB     (nvme1)
Endpoint 1 cache:        ~3 GB     (nvme2)
Endpoint 2 cache:        ~3 GB     (S3 bucket)
Endpoint 3 cache:        ~3 GB     (Azure container)
---------------------------------------------------
Total (distributed):     ~12 GB    (automatically distributed)
```

**Key Benefits**:
- ✅ **Distributed I/O**: Cache reads/writes happen in parallel across endpoints
- ✅ **Proportional scaling**: More endpoints = more distributed cache storage
- ✅ **Locality**: Metadata stored with data it describes
- ✅ **Automatic cleanup**: Delete endpoint → cache deleted automatically
- ✅ **Failure isolation**: One endpoint's cache corruption doesn't affect others

**Compression ratio**: ~4-5x with fjall's LZ4 + zstd for listings

**Disk I/O**: LSM-tree design = sequential writes (SSD friendly)

**Object storage caching**: For s3://, az://, gs://, cache is stored AS objects in the bucket/container, then synced to local temp directory for fjall access. This enables cache sharing across distributed agents.

## Dependencies

**Add to** `Cargo.toml`:

```toml
[dependencies]
fjall = "3.0"              # Embedded LSM-tree KV store
zstd = "0.13"             # Better compression for text data
rand = { version = "0.8", features = ["std_rng"] }  # Seeded RNG
```

**No external services required** - fjall is fully embedded.

## Migration Path

### v0.8.54: Opt-in Cache
- Feature flag: `--enable-cache` (default: off)
- Warning if workload >1M files without cache
- Comprehensive logging of cache hits/misses

### v0.8.55: Auto-enable for Large Workloads
- Auto-enable if `total_files > 100_000`
- User can still override with `--no-cache`

### v0.8.56: Default Enabled
- Cache enabled by default
- Minimal performance impact for small workloads (<10k files)
- Users can disable with `--no-cache`

## Testing Strategy

1. **Unit tests**: `tests/metadata_cache_tests.rs`
   - Keyspace operations
   - Compression/decompression
   - TTL expiration logic

2. **Integration tests**: `tests/cached_prepare_tests.rs`
   - First run (cache population)
   - Second run (cache hit)
   - Config change (cache invalidation)

3. **Performance benchmarks**: `benches/cache_perf.rs`
   - Cache vs no-cache for 1M, 10M, 64M files
   - Listing cache compression ratios
   - Lookup performance (O(1) validation)

4. **Distributed tests**: Multi-agent with shared cache directory

## Security Considerations
Cache Synchronization for Object Storage

For object storage endpoints (s3://, az://, gs://), cache is stored AS objects:

### Upload Strategy (After Prepare)
```
Local fjall database → Serialize keyspaces → Upload as objects

Example S3 structure:
s3://bucket/testdata/sai3-kv-cache/
├── file_paths.db.0001      ← LSM segments
├── file_paths.db.0002
├── listing_cache.db.0001
├── endpoint_metadata.db.0001
└── MANIFEST                ← fjall manifest file
```

**Implementation**:
Distributed fjall-based metadata caching solves the fundamental scalability bottleneck for 64M+ file workloads:

✅ **Eliminates hours of LIST overhead** (400x faster subsequent runs)  
✅ **Converts O(n) path lookups to O(1)** (10000x faster, distributed)  
✅ **Distributed storage** (cache scales with endpoints, no single bottleneck)  
✅ **Endpoint locality** (metadata stored with data it describes)  
✅ **Reproducible test data** (seeds, config hashes per endpoint)  
✅ **Zero external dependencies** (embedded library)  
✅ **Production-ready** (LSM-tree battle-tested design)  
✅ **Object storage native** (cache syncs to s3://, az://, gs://)  
✅ **Auto-discovery** (reattach to existing test data instantly)  

**Key Advantage Over Centralized**:
- **Parallel I/O**: 4 endpoints = 4 parallel cache operations
- **Proportional scaling**: 100 endpoints = 100-way parallelism
- **No single point of failure**: Each endpoint cache is independent
- **Storage distribution**: 64M files → 16M per endpoint (4 endpoints)

**Example Performance** (64M files, 4 endpoints):
- **First run**: 55 minutes (LIST + cache creation)
- **Second run**: 10 seconds (parallel cache loads)
- **Reattach after 6 months**: 3 seconds (auto-discovery)

**Next steps**: 
1. ✅ Review and approve distributed design
2. Implement Phase 1 (EndpointCache + CoordinatorCache)
3. Implement object storage sync (upload/download cache)
4. Benchmark with 1M file test (4 endpoints)
5. Scale test with 10M files (validate performance)
6. Production deployment with 64M+ filesheck for cache)
2. Download to /tmp/sai3-cache/endpoint-{hash}/
3. Open fjall database from local directory
4. Use for O(1) lookups during workload
```

**Auto-detection**:
```rust
// Check if cache exists at endpoint
if endpoint_has_cache(&endpoint_uri).await? {
    let cache = EndpointCache::from_storage(endpoint_uri).await?;
    info!("✓ Loaded existing cache from {}", endpoint_uri);
} else {
    info!("No cache found - will create during prepare");
}
```

### Benefits
- ✅ **Persistent**: Cache survives between test runs
- ✅ **Shareable**: Multiple agents download same cache
- ✅ **Versioned**: Config hash in object keys prevents conflicts
- ✅ **Automatic**: Transparent sync, users don't manage it

## Future Enhancements (v0.9.0+)

1. **Incremental cache updates**: Delta sync for append-only workloads
2. **Predictive pre-population**: Pre-load likely queries during prepare
3. **Query statistics**: Track cache hit rates per endpoint
4. **Snapshot versioning**: Tag caches by test iteration
5. **Cross-region cache replication**: Share cache across distributed regions
6. **Cache compression tuning**: Adaptive compression based on storage type

1. **Remote cache sharing**: Multiple agents share cache via NFS/S3
2. **Incremental updates**: Delta caching for partial LIST results
3. **Predictive pre-population**: Cache likely queries before workload starts
4. **Query statistics**: Track cache hit rates, optimize keyspace sizes
5. **Snapshot versioning**: Tag caches by test iteration

---

## Conclusion

fjall-based metadata caching solves the fundamental scalability bottleneck for 64M+ file workloads:

✅ **Eliminates hours of LIST overhead** (subsequent runs)  
✅ **Converts O(n) path lookups to O(1)** (10000x faster)  
✅ **Reproducible test data** (seeds, config hashes)  
✅ **Zero external dependencies** (embedded library)  
✅ **Compression-native** (13 GB for 64M files)  
✅ **Production-ready** (LSM-tree battle-tested design)  

**Next steps**: 
1. Review and approve design
2. Implement Phase 1 (core infrastructure)
3. Benchmark with 1M file test case
4. Iterate based on real-world performance data
