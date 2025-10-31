# Phase 1: Metadata Operations Implementation Plan

**Vision**: Unified metadata operation testing across all backends (file://, direct://, s3://, az://, gs://)

## Conceptual Framework

The key insight is that **metadata operations** are distinct from **data operations**:

- **Data Operations**: GET, PUT (transfer bytes)
- **Metadata Operations**: LIST, STAT, DELETE, plus new filesystem-style operations

### Cross-Protocol Metadata Operation Matrix

| Operation | file:// | direct:// | s3:// | gs:// | az:// | Description |
|-----------|---------|-----------|-------|-------|-------|-------------|
| **List** | ✅ Full | ✅ Full | ✅ Full | ✅ Full | ✅ Full | Enumerate objects/files |
| **Stat** | ✅ Full | ✅ Full | ✅ HEAD | ✅ HEAD | ✅ HEAD | Get metadata without download |
| **Delete** | ✅ Full | ✅ Full | ✅ Full | ✅ Full | ✅ Full | Remove object/file |
| **Mkdir** | ✅ POSIX | ✅ POSIX | 🔶 Prefix | 🔶 Prefix | 🔶 Prefix | Create directory/prefix |
| **Rmdir** | ✅ POSIX | ✅ POSIX | 🔶 List+Del | 🔶 List+Del | 🔶 List+Del | Remove empty directory |
| **Copy** | ✅ POSIX | ✅ POSIX | ✅ Native | ✅ Native | ✅ Native | Duplicate object |
| **Move** | ✅ POSIX | ✅ POSIX | 🔶 Copy+Del | 🔶 Copy+Del | 🔶 Copy+Del | Rename/relocate object |
| **SetAttr** | ✅ chmod/chown | ✅ chmod/chown | ✅ Metadata | ✅ Metadata | ✅ Metadata | Modify attributes |
| **GetAttr** | ✅ stat() | ✅ stat() | ✅ HEAD | ✅ HEAD | ✅ HEAD | Read attributes |
| **Access** | ✅ access() | ✅ access() | 🔶 HEAD | 🔶 HEAD | 🔶 HEAD | Test permissions |

**Legend**:
- ✅ Full: Native support with full semantics
- 🔶 Emulated: Implemented via multiple operations or with different semantics
- ❌ Not supported

## Architecture Design

### 1. Trait-Based Metadata Abstraction

Create new trait in `src/metadata_ops.rs` to complement ObjectStore:

```rust
/// Metadata operations that can be tested across different storage backends
#[async_trait::async_trait]
pub trait MetadataOperations {
    /// Create a directory or prefix (semantics vary by backend)
    async fn mkdir(&self, path: &str) -> Result<()>;
    
    /// Remove an empty directory or prefix
    async fn rmdir(&self, path: &str) -> Result<()>;
    
    /// Copy an object/file to a new location
    async fn copy(&self, source: &str, destination: &str) -> Result<()>;
    
    /// Move/rename an object/file
    async fn move_obj(&self, source: &str, destination: &str) -> Result<()>;
    
    /// Set attributes (chmod/chown for POSIX, metadata for cloud)
    async fn set_attr(&self, path: &str, attrs: MetadataAttributes) -> Result<()>;
    
    /// Get attributes (stat for POSIX, HEAD for cloud)
    async fn get_attr(&self, path: &str) -> Result<ObjectMetadata>;
    
    /// Test access permissions
    async fn access(&self, path: &str, mode: AccessMode) -> Result<bool>;
}

/// Unified metadata representation across backends
#[derive(Debug, Clone)]
pub struct ObjectMetadata {
    pub path: String,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    
    // POSIX-specific (file://, direct://)
    pub permissions: Option<u32>,
    pub owner_uid: Option<u32>,
    pub owner_gid: Option<u32>,
    
    // Cloud-specific (s3://, gs://, az://)
    pub storage_class: Option<String>,
    pub custom_metadata: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum MetadataAttributes {
    /// POSIX permissions (file://, direct://)
    Posix {
        mode: Option<u32>,      // chmod bits
        uid: Option<u32>,       // chown user
        gid: Option<u32>,       // chown group
    },
    
    /// Cloud storage metadata (s3://, gs://, az://)
    Cloud {
        content_type: Option<String>,
        cache_control: Option<String>,
        custom: HashMap<String, String>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum AccessMode {
    Read,
    Write,
    Execute,
    Exists,
}
```

### 2. Backend-Specific Implementations

#### File/DirectIO Backend (Full POSIX Support)

```rust
// In src/fs_metadata.rs
use std::fs;
use std::os::unix::fs::PermissionsExt;

pub struct FileMetadataOps {
    base_path: String,
}

#[async_trait::async_trait]
impl MetadataOperations for FileMetadataOps {
    async fn mkdir(&self, path: &str) -> Result<()> {
        let full_path = self.resolve_path(path);
        tokio::fs::create_dir_all(&full_path).await?;
        Ok(())
    }
    
    async fn rmdir(&self, path: &str) -> Result<()> {
        let full_path = self.resolve_path(path);
        tokio::fs::remove_dir(&full_path).await?;
        Ok(())
    }
    
    async fn copy(&self, source: &str, destination: &str) -> Result<()> {
        let src = self.resolve_path(source);
        let dst = self.resolve_path(destination);
        tokio::fs::copy(&src, &dst).await?;
        Ok(())
    }
    
    async fn move_obj(&self, source: &str, destination: &str) -> Result<()> {
        let src = self.resolve_path(source);
        let dst = self.resolve_path(destination);
        tokio::fs::rename(&src, &dst).await?;
        Ok(())
    }
    
    async fn set_attr(&self, path: &str, attrs: MetadataAttributes) -> Result<()> {
        let full_path = self.resolve_path(path);
        if let MetadataAttributes::Posix { mode, uid, gid } = attrs {
            if let Some(m) = mode {
                let metadata = tokio::fs::metadata(&full_path).await?;
                let mut perms = metadata.permissions();
                perms.set_mode(m);
                tokio::fs::set_permissions(&full_path, perms).await?;
            }
            // uid/gid require sudo, can use nix crate for chown()
        }
        Ok(())
    }
    
    async fn get_attr(&self, path: &str) -> Result<ObjectMetadata> {
        let full_path = self.resolve_path(path);
        let metadata = tokio::fs::metadata(&full_path).await?;
        
        Ok(ObjectMetadata {
            path: path.to_string(),
            size: metadata.len(),
            modified: metadata.modified().ok(),
            content_type: None,
            etag: None,
            permissions: Some(metadata.permissions().mode()),
            owner_uid: None,  // Can use nix crate for stat()
            owner_gid: None,
            storage_class: None,
            custom_metadata: HashMap::new(),
        })
    }
    
    async fn access(&self, path: &str, mode: AccessMode) -> Result<bool> {
        let full_path = self.resolve_path(path);
        // Use nix::unistd::access() for proper POSIX semantics
        // For now, simple existence check:
        Ok(tokio::fs::metadata(&full_path).await.is_ok())
    }
}
```

#### Cloud Backend (S3/GCS/Azure)

```rust
// In src/cloud_metadata.rs
pub struct CloudMetadataOps {
    store: Box<dyn ObjectStore>,
}

#[async_trait::async_trait]
impl MetadataOperations for CloudMetadataOps {
    async fn mkdir(&self, path: &str) -> Result<()> {
        // Cloud storage has no true directories, create empty marker object
        let marker_path = if path.ends_with('/') {
            format!("{}/.keep", path)
        } else {
            format!("{}/.keep", path)
        };
        self.store.put(&marker_path, bytes::Bytes::new()).await?;
        info!("Created prefix marker for cloud storage: {}", path);
        Ok(())
    }
    
    async fn rmdir(&self, path: &str) -> Result<()> {
        // List and delete all objects under prefix
        let objects = self.store.list(path).await?;
        if objects.is_empty() {
            return Ok(());
        }
        
        // Cloud "rmdir" means delete all objects with prefix
        for obj in objects {
            self.store.delete(&obj).await?;
        }
        Ok(())
    }
    
    async fn copy(&self, source: &str, destination: &str) -> Result<()> {
        // Use native copy operation if available
        // S3: CopyObject, GCS: rewriteFrom, Azure: Copy Blob
        self.store.copy(source, destination).await?;
        Ok(())
    }
    
    async fn move_obj(&self, source: &str, destination: &str) -> Result<()> {
        // Cloud storage has no atomic move, use copy + delete
        self.copy(source, destination).await?;
        self.store.delete(source).await?;
        Ok(())
    }
    
    async fn set_attr(&self, path: &str, attrs: MetadataAttributes) -> Result<()> {
        if let MetadataAttributes::Cloud { content_type, cache_control, custom } = attrs {
            // Cloud storage: copy object with new metadata
            // This is backend-specific - needs ObjectStore extension
            warn!("set_attr for cloud storage requires backend-specific implementation");
            Ok(())
        } else {
            bail!("POSIX attributes not supported for cloud storage")
        }
    }
    
    async fn get_attr(&self, path: &str) -> Result<ObjectMetadata> {
        // Use HEAD request (already implemented as Stat operation)
        let head_result = self.store.head(path).await?;
        
        Ok(ObjectMetadata {
            path: path.to_string(),
            size: head_result.size,
            modified: head_result.last_modified,
            content_type: head_result.content_type,
            etag: head_result.etag,
            permissions: None,
            owner_uid: None,
            owner_gid: None,
            storage_class: None,  // Can extract from storage-specific headers
            custom_metadata: HashMap::new(),
        })
    }
    
    async fn access(&self, path: &str, mode: AccessMode) -> Result<bool> {
        // For cloud storage, "access" means HEAD request succeeds
        match self.store.head(path).await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}
```

### 3. Configuration Extension

Extend `OpSpec` enum in `src/config.rs`:

```rust
pub enum OpSpec {
    // Existing operations
    Get { path: String },
    Put { /* ... existing fields ... */ },
    List { path: String },
    Stat { path: String },
    Delete { path: String },
    
    // NEW: Metadata operations (v0.7.0+)
    
    /// Create directory (POSIX) or prefix marker (cloud)
    Mkdir { 
        path: String,
        /// Optional permissions for POSIX filesystems (e.g., 0o755)
        #[serde(default)]
        mode: Option<u32>,
    },
    
    /// Remove empty directory or delete all objects under prefix
    Rmdir { 
        path: String,
        /// If true, delete recursively (cloud: all objects, POSIX: rm -rf)
        #[serde(default)]
        recursive: bool,
    },
    
    /// Copy object/file
    Copy {
        source: String,
        destination: String,
    },
    
    /// Move/rename object/file
    Move {
        source: String,
        destination: String,
    },
    
    /// Set attributes (chmod for POSIX, metadata for cloud)
    SetAttr {
        path: String,
        #[serde(flatten)]
        attributes: AttributeSpec,
    },
    
    /// Get attributes (stat for POSIX, HEAD for cloud)
    GetAttr {
        path: String,
    },
    
    /// Test access permissions
    Access {
        path: String,
        #[serde(default)]
        mode: AccessMode,
    },
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum AttributeSpec {
    /// POSIX-style attributes
    Posix {
        #[serde(default)]
        mode: Option<u32>,
        #[serde(default)]
        uid: Option<u32>,
        #[serde(default)]
        gid: Option<u32>,
    },
    
    /// Cloud storage metadata
    Cloud {
        #[serde(default)]
        content_type: Option<String>,
        #[serde(default)]
        cache_control: Option<String>,
        #[serde(default)]
        custom: HashMap<String, String>,
    },
}
```

### 4. Workload Integration

Extend `src/workload.rs` to handle new operations:

```rust
// In execute_workload function:
match op {
    OpSpec::Get { path } => { /* existing */ },
    OpSpec::Put { /* ... */ } => { /* existing */ },
    OpSpec::List { path } => { /* existing */ },
    OpSpec::Stat { path } => { /* existing */ },
    OpSpec::Delete { path } => { /* existing */ },
    
    // NEW: Metadata operations
    OpSpec::Mkdir { path, mode } => {
        let start = Instant::now();
        let metadata_ops = create_metadata_ops(&cfg.target, backend_type)?;
        
        let result = metadata_ops.mkdir(path).await;
        let duration_ns = start.elapsed().as_nanos() as u64;
        
        if let Err(e) = result {
            failed_ops.fetch_add(1, Ordering::Relaxed);
            debug!("mkdir failed for {}: {}", path, e);
        } else {
            successful_ops.fetch_add(1, Ordering::Relaxed);
            // Record to histogram
            record_metadata_op_latency(&mut hist_map, "mkdir", 0, duration_ns);
        }
    },
    
    OpSpec::Copy { source, destination } => {
        let start = Instant::now();
        let metadata_ops = create_metadata_ops(&cfg.target, backend_type)?;
        
        // Get object size for metrics
        let size = metadata_ops.get_attr(source).await?.size;
        
        let result = metadata_ops.copy(source, destination).await;
        let duration_ns = start.elapsed().as_nanos() as u64;
        
        if let Err(e) = result {
            failed_ops.fetch_add(1, Ordering::Relaxed);
            debug!("copy failed from {} to {}: {}", source, destination, e);
        } else {
            successful_ops.fetch_add(1, Ordering::Relaxed);
            bytes_transferred.fetch_add(size, Ordering::Relaxed);
            record_metadata_op_latency(&mut hist_map, "copy", size, duration_ns);
        }
    },
    
    // ... similar for other metadata operations
}

/// Create metadata operations handler for backend
fn create_metadata_ops(
    base_uri: &str,
    backend_type: BackendType,
) -> Result<Box<dyn MetadataOperations>> {
    match backend_type {
        BackendType::File | BackendType::DirectIO => {
            Ok(Box::new(FileMetadataOps::new(base_uri)))
        },
        BackendType::S3 | BackendType::Gcs | BackendType::Azure => {
            let store = create_store_for_uri(base_uri)?;
            Ok(Box::new(CloudMetadataOps::new(store)))
        },
    }
}
```

## Implementation Phases

### Week 1, Days 1-2: Core Infrastructure

1. Create `src/metadata_ops.rs` with trait definition
2. Create `src/fs_metadata.rs` with File/DirectIO implementation
3. Create `src/cloud_metadata.rs` with cloud backend implementation
4. Add dependencies: `async-trait = "0.1"`, `nix = { version = "0.27", features = ["fs"] }`

### Week 1, Days 3-4: Configuration & Integration

1. Extend `OpSpec` enum with 7 new operations
2. Update `src/workload.rs` to dispatch metadata operations
3. Add metadata operation metrics collection
4. Create example configs in `tests/configs/metadata_*.yaml`

### Testing Strategy

#### Unit Tests (`tests/metadata_ops_tests.rs`)

```rust
#[tokio::test]
async fn test_file_mkdir_rmdir() {
    let temp_dir = tempfile::tempdir().unwrap();
    let ops = FileMetadataOps::new(temp_dir.path().to_str().unwrap());
    
    // Test mkdir
    ops.mkdir("test_dir").await.unwrap();
    assert!(temp_dir.path().join("test_dir").exists());
    
    // Test rmdir
    ops.rmdir("test_dir").await.unwrap();
    assert!(!temp_dir.path().join("test_dir").exists());
}

#[tokio::test]
async fn test_file_copy_move() {
    let temp_dir = tempfile::tempdir().unwrap();
    let ops = FileMetadataOps::new(temp_dir.path().to_str().unwrap());
    
    // Create source file
    tokio::fs::write(temp_dir.path().join("source.txt"), "test data").await.unwrap();
    
    // Test copy
    ops.copy("source.txt", "copy.txt").await.unwrap();
    assert!(temp_dir.path().join("copy.txt").exists());
    
    // Test move
    ops.move_obj("copy.txt", "moved.txt").await.unwrap();
    assert!(!temp_dir.path().join("copy.txt").exists());
    assert!(temp_dir.path().join("moved.txt").exists());
}
```

#### Integration Tests

```bash
# Test file backend with metadata operations
./target/release/sai3-bench run --config tests/configs/metadata_file_test.yaml

# Test GCS backend with metadata operations
./target/release/sai3-bench run --config tests/configs/metadata_gcs_test.yaml
```

Example test config:

```yaml
# tests/configs/metadata_file_test.yaml
target: "file:///tmp/sai3bench-metadata-test/"

prepare:
  ensure_objects:
    - base_uri: "file:///tmp/sai3bench-metadata-test/data/"
      count: 100
      size_distribution:
        type: fixed
        size: 1048576

workload:
  - op: mkdir
    path: "new_dir/"
    weight: 10
    
  - op: copy
    source: "data/*"
    destination: "backup/"
    weight: 30
    
  - op: move
    source: "backup/*"
    destination: "archive/"
    weight: 20
    
  - op: getattr
    path: "data/*"
    weight: 25
    
  - op: access
    path: "data/*"
    mode: read
    weight: 15

duration: 30s
concurrency: 16
```

## Success Metrics

- ✅ All 7 new metadata operations implemented for file:// backend
- ✅ Partial support (4-5 operations) for cloud backends
- ✅ Unified trait abstraction across all backends
- ✅ Graceful fallback when operation not supported
- ✅ Performance metrics collected (latency histograms)
- ✅ Documentation with cross-protocol comparison table
- ✅ 10+ example configurations

## Cross-Protocol Compatibility Guide

### When to Use Each Operation

**Universal (all backends)**:
- `getattr` - Always supported (stat for POSIX, HEAD for cloud)
- `list` - Always supported with consistent semantics
- `delete` - Always supported

**POSIX-optimized (file://, direct://)**:
- `mkdir`, `rmdir` - True directory semantics
- `move` - Atomic rename operation
- `setattr` with mode/uid/gid - Full POSIX permissions

**Cloud-friendly (s3://, gs://, az://)**:
- `copy` - Native server-side copy (no download)
- `mkdir` - Creates prefix marker (eventual consistency)
- `rmdir` with recursive - Bulk delete under prefix

**Best Practice**: Test workloads with backend-appropriate operations. Use configuration conditionals if needed:

```yaml
# Future enhancement: conditional operations by backend
workload:
  - op: mkdir
    path: "test/"
    backends: [file, direct]  # Only run for POSIX backends
    weight: 10
  
  - op: copy
    source: "data/*"
    destination: "backup/"
    backends: [s3, gs, az]  # Prefer native copy for cloud
    weight: 30
```

## Next Steps

Once Phase 1 is complete:
- Phase 2: Advanced access patterns (hot-banding, Zipfian)
- Phase 3: Block device support
- Phase 4: Polish & integration

This metadata-first approach provides immediate value and establishes patterns for future enhancements.
