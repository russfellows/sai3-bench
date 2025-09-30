# s3-bench Integration Context for dl-driver Replay Functionality

## Project Context
This document provides context for extending s3-bench to support file:// and direct:// URIs for dl-driver's replay functionality.

## Background
dl-driver has integrated s3-bench as a GitHub dependency to handle operation log replay instead of maintaining duplicate functionality. However, s3-bench currently only supports S3 URIs (`s3://`), while dl-driver needs to support multiple storage backends for replay operations.

## Current State
- **dl-driver**: Uses s3dlio pinned to revision `cd4ee2e` for multi-backend support
- **s3-bench**: Uses s3dlio from main branch (unpinned), only supports S3 URIs
- **Integration**: s3-bench is included as GitHub dependency in dl-driver's Cargo.toml

## Problem Statement
Replay logs contain relative file paths like `train_file_000001.npz` but need complete URIs for actual I/O operations:
- `file:///tmp/test/train_file_000001.npz` (local filesystem)
- `direct:///mnt/data/train_file_000001.npz` (DirectIO)
- `s3://my-bucket/train_file_000001.npz` (S3)

Currently, s3-bench can only handle S3 URIs, limiting replay functionality to S3-only environments.

## Required Changes

### 1. Pin s3dlio Version (PRIORITY 1)
Update `/home/eval/Documents/Code/s3-bench/Cargo.toml`:
```toml
# BEFORE:
s3dlio = { git = "https://github.com/russfellows/s3dlio" }

# AFTER:
s3dlio = { git = "https://github.com/russfellows/s3dlio.git", rev = "cd4ee2e" }
```

### 2. Extend URI Parsing (PRIORITY 2)
Current s3-bench code in `src/workload.rs` and `src/main.rs` uses `parse_s3_uri()` which only accepts `s3://` schemes.

Need to:
- Add support for `file://` and `direct://` URI schemes
- Update `parse_s3_uri()` to `parse_storage_uri()` with multi-backend support
- Modify workload engine to route operations to appropriate backends

### 3. Update Storage Operations (PRIORITY 3)
- Use s3dlio's `ObjectStore` trait for unified storage access
- Replace direct AWS SDK calls with s3dlio store operations for non-S3 backends
- Maintain S3 performance optimizations where possible

### 4. Workload Configuration Updates (PRIORITY 4)
Extend s3-bench's config system to support:
```yaml
workload:
  - op: get
    uri: file:///tmp/test/data/
    weight: 50
  - op: put
    bucket: /tmp/test/output  # local path for file://
    prefix: results/
    object_size: 1048576
    weight: 50
```

## Technical Implementation Notes

### URI Scheme Detection
```rust
fn detect_storage_backend(uri: &str) -> StorageBackend {
    if uri.starts_with("s3://") => StorageBackend::S3,
    if uri.starts_with("file://") => StorageBackend::File,
    if uri.starts_with("direct://") => StorageBackend::DirectIO,
    // fallback to file:// for relative paths
    else => StorageBackend::File,
}
```

### s3dlio Integration Pattern
```rust
use s3dlio::{store_for_uri, ObjectStore};

// Replace S3-specific operations with:
let store = store_for_uri(uri).await?;
let data = store.get_object(path).await?;
```

### Backward Compatibility
- Keep existing S3 functionality unchanged
- Extend rather than replace current APIs
- Maintain performance for S3 workloads

## Files to Modify

### Core Files
1. `src/workload.rs` - Main workload execution engine
2. `src/main.rs` - CLI URI parsing and validation
3. `src/config.rs` - Configuration parsing and validation
4. `Cargo.toml` - s3dlio version pinning

### Binary Files (if needed)
1. `src/bin/agent.rs` - gRPC agent server
2. `src/bin/controller.rs` - gRPC controller client
3. `src/bin/run.rs` - Standalone workload runner

### Test Files
1. Test configurations for file:// and direct:// backends
2. Integration tests with s3dlio backends

## Expected dl-driver Integration
Once s3-bench supports multiple backends, dl-driver's replay functionality will:
1. Parse base URI to determine storage backend
2. Combine base URI + relative paths from operation logs
3. Pass complete URIs to s3-bench workload engine
4. Get unified performance metrics across all backends

## Testing Strategy
1. **Unit Tests**: URI parsing for all schemes
2. **Integration Tests**: s3dlio backend operations
3. **End-to-End Tests**: Full workload execution on file:// and direct://
4. **Performance Tests**: Ensure S3 performance regression testing

## Success Criteria
- [ ] s3-bench compiles with pinned s3dlio version
- [ ] s3-bench accepts `file://` and `direct://` URIs without errors
- [ ] s3-bench can execute GET/PUT operations on local filesystems
- [ ] dl-driver replay works with file:// and direct:// base URIs
- [ ] Performance parity maintained for S3 workloads
- [ ] All existing s3-bench functionality remains intact

## Next Steps
1. Pin s3dlio version in Cargo.toml
2. Extend URI parsing to support multiple schemes
3. Integrate s3dlio ObjectStore for unified backend access
4. Test with file:// URIs using test data in `/tmp/replay_test_data/`
5. Validate integration with dl-driver replay functionality

## Reference Information
- **s3dlio revision**: `cd4ee2e` (performance monitoring v0.8.7)
- **dl-driver integration**: S3BenchReplayEngine in `crates/core/src/replay.rs`
- **Test data location**: `/tmp/replay_test_data/` (created with sample files)
- **Storage backends**: File, S3, Azure, DirectIO via s3dlio ObjectStore trait

---
*Created: 2025-09-29 for dl-driver replay functionality extension*