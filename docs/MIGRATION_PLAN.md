# s3-bench Multi-Backend Migration Plan

## Overview
Migrate s3-bench from S3-only operations using s3dlio legacy API to unified multi-backend support using s3dlio 0.8.7's ObjectStore trait. This enables support for S3, Azure Blob, local filesystem, and Direct I/O backends.

## Current State Analysis

### s3dlio Usage Audit
- **s3dlio version**: Unpinned (now pinned to v0.8.7)
- **API used**: Legacy `s3dlio::s3_utils` (S3-only functions)
- **Operations**: `parse_s3_uri()`, `get_object()`, `put_object_async()`, `list_objects()`, `delete_objects()`, `stat_object_uri()`

### Affected Files
1. **src/workload.rs** - Core workload execution engine (most complex)
2. **src/main.rs** - CLI operations and utilities
3. **src/bin/agent.rs** - gRPC agent server
4. **src/bin/controller.rs** - gRPC controller client (minimal changes)
5. **src/config.rs** - Configuration parsing (schema updates)

### Hardcoded S3 Patterns
- URI parsing assumes S3 bucket/key structure
- GetSource struct stores bucket and pattern separately
- PUT operations use bucket + prefix + generated key pattern
- All I/O operations call S3-specific functions directly

## Migration Goals

### Target Capabilities
- ✅ **Multi-backend support**: `s3://`, `file://`, `direct://`, `az://`
- ✅ **Unified ObjectStore API**: Same code works across all backends
- ✅ **Performance optimizations**: O_DIRECT for local files, async I/O
- ✅ **Zero-copy capabilities**: Streaming operations where possible
- ✅ **HDR histogram statistics**: Preserve existing metrics collection
- ✅ **Backward compatibility**: Existing S3 workloads continue working

### New Backend Support
- **S3**: `s3://bucket/path/file` (existing, enhanced)
- **Azure Blob**: `az://account/container/path/file`
- **Local File**: `file:///absolute/path/file` (buffered I/O)
- **Direct I/O**: `direct:///absolute/path/file` (O_DIRECT for performance)

## Three-Stage Implementation Plan

### Stage 1: Foundation & Core API Migration
**Branch**: `feature/stage1-core-api-migration`
**Goal**: Update core dependencies and basic ObjectStore integration without breaking existing functionality

#### Stage 1 Tasks:
1. **Dependencies & Imports**
   - ✅ Pin s3dlio to v0.8.7 in Cargo.toml
   - Update imports from `s3dlio::s3_utils` to `s3dlio::api`
   - Add new ObjectStore trait usage

2. **Basic ObjectStore Integration**
   - Create ObjectStore factory wrapper functions
   - Implement URI scheme detection helpers
   - Add basic multi-backend support infrastructure

3. **Workload Engine Foundation**
   - Update GetSource struct to support full URIs
   - Modify pre-resolution logic for multi-backend
   - Maintain existing S3 functionality during transition

4. **Testing & Validation**
   - Ensure existing S3 workloads still work
   - Add basic file:// backend testing
   - Validate no performance regression for S3

### Stage 2: Core Operations Migration
**Branch**: `feature/stage2-operations-migration`
**Goal**: Replace all S3-specific operations with ObjectStore trait methods

#### Stage 2 Tasks:
1. **GET Operations Migration**
   - Replace `get_object(&bucket, &key)` with `store.get(uri)`
   - Update workload.rs GET operation handling
   - Update main.rs CLI GET operations
   - Update agent.rs gRPC GET operations

2. **PUT Operations Migration**
   - Replace `put_object_async(bucket, &key, &data)` with `store.put(uri, &data)`
   - Update workload.rs PUT operation handling
   - Update main.rs CLI PUT operations
   - Update agent.rs gRPC PUT operations

3. **LIST/STAT Operations Migration**
   - Replace `list_objects()` with `store.list()`
   - Replace `stat_object_uri()` with `store.stat()`
   - Update prefetch logic for GET operations

4. **URI Construction & Handling**
   - Implement proper URI construction for each backend
   - Handle prefix patterns for all backend types
   - Update object key generation for non-S3 backends

### Stage 3: Advanced Features & Configuration
**Branch**: `feature/stage3-advanced-features`
**Goal**: Add advanced multi-backend features and configuration options

#### Stage 3 Tasks:
1. **Configuration Schema Updates**
   - Support new URI schemes in YAML workload definitions
   - Add backend-specific configuration options
   - Maintain backward compatibility with existing configs

2. **Performance Optimizations**
   - Implement Direct I/O support for file:// operations
   - Add streaming/zero-copy operations where available
   - Optimize concurrent operations per backend type

3. **Enhanced gRPC Interface**
   - Update proto definitions for multi-backend support
   - Add backend detection and validation
   - Update controller/agent communication

4. **Documentation & Examples**
   - Update usage documentation
   - Add examples for each backend type
   - Update copilot instructions

## Technical Implementation Details

### New Data Structures

#### Updated GetSource
```rust
// Current
struct GetSource {
    uri: String,
    bucket: String,
    pattern: String,
    keys: Vec<String>,
}

// New
struct GetSource {
    base_uri: String,
    backend_type: BackendType,
    full_uris: Vec<String>,
    store: Arc<dyn ObjectStore>,
}
```

#### Backend Type Enum
```rust
#[derive(Debug, Clone, Copy)]
enum BackendType {
    S3,
    Azure,
    File,
    DirectIO,
}
```

### URI Handling Patterns

#### Before (S3-only)
```rust
let (bucket, pattern) = parse_s3_uri(uri)?;
let bytes = get_object(&bucket, &key).await?;
put_object_async(bucket, &key, &data).await?;
```

#### After (Multi-backend)
```rust
let store = store_for_uri(uri)?;
let bytes = store.get(full_uri).await?;
store.put(full_uri, &data).await?;
```

### Configuration Examples

#### S3 Backend (existing)
```yaml
workload:
  - op: get
    uri: "s3://bucket/prefix/*"
    weight: 70
  - op: put
    bucket: "bucket-name"
    prefix: "bench/"
    object_size: 1048576
    weight: 30
```

#### File Backend (new)
```yaml
workload:
  - op: get
    uri: "file:///tmp/test-data/*"
    weight: 50
  - op: put
    base_uri: "file:///tmp/output"
    prefix: "bench/"
    object_size: 1048576
    weight: 50
```

#### Direct I/O Backend (new)
```yaml
workload:
  - op: get
    uri: "direct:///mnt/nvme/data/*"
    weight: 60
  - op: put
    base_uri: "direct:///mnt/nvme/output"
    prefix: "results/"
    object_size: 4194304
    weight: 40
```

## Success Criteria

### Stage 1 Success
- [ ] Code compiles with s3dlio 0.8.7
- [ ] Existing S3 workloads run without modification
- [ ] Basic file:// GET/PUT operations work
- [ ] No performance regression for S3 operations

### Stage 2 Success
- [ ] All backend types support GET/PUT operations
- [ ] gRPC agent/controller work with all backends
- [ ] HDR histogram metrics preserved across all backends
- [ ] Comprehensive test coverage for all operations

### Stage 3 Success
- [ ] Direct I/O performance optimization functional
- [ ] Configuration schema supports all backend types
- [ ] Documentation updated and comprehensive
- [ ] Ready for dl-driver integration

## Risk Mitigation

### Backward Compatibility
- Maintain existing S3 configuration format
- Preserve all CLI parameters and gRPC interface
- Keep performance characteristics for S3 workloads

### Testing Strategy
- Test each stage independently before merging
- Maintain comprehensive test suite for each backend
- Performance regression testing for S3 operations

### Rollback Plan
- Each stage on separate branch for easy rollback
- Preserve original s3_utils usage patterns as fallback
- Document breaking changes and migration path

## Timeline Estimate
- **Stage 1**: 2-3 days implementation + testing
- **Stage 2**: 3-4 days implementation + testing  
- **Stage 3**: 2-3 days implementation + testing
- **Total**: ~8-10 days for complete migration

## Dependencies
- s3dlio 0.8.7 (pinned)
- Rust async ecosystem compatibility
- Existing test infrastructure
- gRPC/protobuf toolchain