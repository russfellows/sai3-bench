# Filesystem and Nested Path Testing Guide

**Version:** 0.7.0  
**Feature:** Comprehensive filesystem operations and nested path testing for object storage

## Overview

Version 0.7.0 enables realistic filesystem testing and object storage testing with nested paths (hierarchical key structures). This includes traditional filesystem operations (directory management, file I/O) and object storage operations with path-like keys that simulate directory hierarchies. The implementation supports metadata operations (LIST, STAT, DELETE, MKDIR, RMDIR) as part of comprehensive filesystem workload testing.

## Table of Contents

- [Filesystem vs Object Storage](#filesystem-vs-object-storage)
- [Testing Capabilities](#testing-capabilities)
- [Backend Compatibility](#backend-compatibility)
- [Configuration](#configuration)
- [Operation Details](#operation-details)
- [Examples](#examples)
- [Best Practices](#best-practices)
- [Troubleshooting](#troubleshooting)

## Filesystem vs Object Storage

### Traditional Filesystems (file://, direct://)

**Hierarchical structure** with actual directories:

```
/mnt/shared/
├── data/
│   ├── users/
│   │   ├── alice/
│   │   │   └── file1.dat
│   │   └── bob/
│   │       └── file2.dat
│   └── shared/
│       └── file3.dat
```

**Characteristics:**
- Real directory inodes
- POSIX operations (mkdir, rmdir, readdir, stat, unlink)
- Hierarchical permissions
- Must create directories before files
- Directory metadata (timestamps, permissions)

### Object Storage (s3://, az://, gs://)

**Flat namespace** with nested path keys:

```
s3://bucket/
  data/users/alice/file1.dat    <- Single object key
  data/users/bob/file2.dat      <- Single object key
  data/shared/file3.dat         <- Single object key
```

**Characteristics:**
- No actual directories (just key prefixes)
- Slash (/) in key name simulates hierarchy
- "Directories" are implicit (derived from prefixes)
- No mkdir needed (PUT creates full path)
- List operations filter by prefix

### Unified Testing

**sai3-bench abstracts both models:**

```yaml
# Same config works on BOTH filesystems and object storage
directory_tree:
  width: 3
  depth: 2
  files_per_dir: 10
  
workload:
  - op: get
  - op: put
  - op: stat
  - op: list

# Backend-specific behavior handled automatically:
# - file://  → Creates actual directories
# - s3://    → Uses implicit directories (key prefixes)
```

## Testing Capabilities

### Filesystem Operations

Test comprehensive filesystem behaviors:

1. **Directory Tree Creation** - Hierarchical structures (width × depth)
2. **File Distribution** - Files at leaves only or all levels
3. **Path-Based Operations** - GET/PUT/STAT with nested paths
4. **Directory Management** - MKDIR/RMDIR (filesystem only)
5. **Metadata Queries** - LIST (enumerate), STAT (metadata)
6. **Cleanup Operations** - DELETE files and directories

### MKDIR - Create Directories

Create directory structures for filesystem-based backends.

**Supported backends:** file://, direct://  
**Skipped backends:** s3://, az://, gs:// (implicit directories)

```yaml
workload:
  - op: mkdir
    path: "test-dirs/"
    weight: 10
```

**Behavior:**
- **Local filesystems**: Creates actual directories using POSIX mkdir
- **Object storage**: Operation skipped (directories are implicit)
- **Error handling**: Ignores "already exists" errors

### RMDIR - Remove Directories

Remove empty directories from filesystem-based backends.

**Supported backends:** file://, direct://  
**Skipped backends:** s3://, az://, gs:// (no directories to remove)

```yaml
workload:
  - op: rmdir
    path: "test-dirs/"
    weight: 5
```

**Behavior:**
- **Local filesystems**: Removes empty directories using POSIX rmdir
- **Object storage**: Operation skipped
- **Error handling**: Only removes empty directories (fails if non-empty)

### LIST - List Objects/Files

List objects or files with optional prefix filtering.

**Supported backends:** All (file://, direct://, s3://, az://, gs://)

```yaml
workload:
  - op: list
    path: "data/"
    weight: 20
```

**Behavior:**
- **Local filesystems**: Directory listing with readdir
- **Object storage**: Prefix-based object listing
- **Returns:** Object keys/paths matching prefix

### STAT - Query Metadata

Query object/file metadata (size, modified time, etc.).

**Supported backends:** All (file://, direct://, s3://, az://, gs://)

```yaml
workload:
  - op: stat
    path: "data/*.dat"
    weight: 15
```

**Behavior:**
- **Local filesystems**: stat() syscall
- **Object storage**: HEAD request for object metadata
- **Returns:** Size, last modified time, etag (cloud)

### DELETE - Remove Objects/Files

Delete objects or files from storage.

**Supported backends:** All (file://, direct://, s3://, az://, gs://)

```yaml
workload:
  - op: delete
    path: "temp/*.dat"
    weight: 5
```

**Behavior:**
- **Local filesystems**: unlink() syscall
- **Object storage**: DELETE request
- **Batch support**: Efficient batch deletion on supported backends

## Backend Compatibility

### Compatibility Matrix

| Operation | file:// | direct:// | s3:// | az:// | gs:// |
|-----------|---------|-----------|-------|-------|-------|
| **MKDIR** | ✅ Creates dirs | ✅ Creates dirs | ⚠️ Skipped | ⚠️ Skipped | ⚠️ Skipped |
| **RMDIR** | ✅ Removes dirs | ✅ Removes dirs | ⚠️ Skipped | ⚠️ Skipped | ⚠️ Skipped |
| **LIST** | ✅ readdir | ✅ readdir | ✅ List API | ✅ List blobs | ✅ List objects |
| **STAT** | ✅ stat() | ✅ stat() | ✅ HEAD | ✅ HEAD | ✅ HEAD |
| **DELETE** | ✅ unlink() | ✅ unlink() | ✅ DELETE | ✅ DELETE | ✅ DELETE |

**Legend:**
- ✅ Fully supported
- ⚠️ Skipped (operation not applicable)

### Why Object Storage Skips MKDIR/RMDIR

Object storage systems (S3, Azure Blob, GCS) use **flat namespaces** with key-based addressing. What looks like directories are actually **key prefixes**:

```
# Not actual directories:
s3://bucket/data/subdir/file.dat
              ^^^^^^^^^^^  <-- This is part of the object KEY

# Object key: "data/subdir/file.dat"
# No directory objects actually exist
```

**Implications:**
- No need to create directories before PUT operations
- Cannot list "directories" independently
- DELETE must remove objects, not directories
- Performance advantage: No mkdir overhead

### Local Filesystem Behavior

Local filesystems use **hierarchical directories**:

```
# Actual directory structure:
/mnt/shared/data/subdir/file.dat
             ^^^^^^^^^^  <-- Real directory inode

# Must create directories before writing files
mkdir -p /mnt/shared/data/subdir
```

**Implications:**
- MKDIR required before file creation
- Directories have metadata (permissions, timestamps)
- RMDIR only works on empty directories
- Filesystem limits apply (max inodes, path length)

## Configuration

### Mixed Operation Workload

```yaml
target: "file:///mnt/shared-fs/test/"
duration: 60
concurrency: 16

workload:
  - op: get
    path: "data/*.dat"
    weight: 50
    
  - op: put
    path: "data/"
    weight: 20
    size_distribution:
      type: uniform
      min: 4096
      max: 16384
    
  - op: stat
    path: "data/*.dat"
    weight: 15
    
  - op: list
    path: "data/"
    weight: 10
    
  - op: delete
    path: "temp/*.dat"
    weight: 5
```

### Metadata-Heavy Workload

```yaml
target: "s3://benchmark-bucket/metadata-test/"
duration: 30
concurrency: 32

prepare:
  ensure_objects:
    - base_uri: "s3://benchmark-bucket/metadata-test/"
      count: 1000
      size_distribution:
        type: fixed
        size: 4096
      fill: random

workload:
  - op: list
    path: "metadata-test/"
    weight: 40
    
  - op: stat
    path: "metadata-test/*.dat"
    weight: 40
    
  - op: get
    path: "metadata-test/*.dat"
    weight: 20
```

### Directory Management (Filesystem Only)

```yaml
target: "file:///tmp/dir-test/"
duration: 20
concurrency: 8

workload:
  - op: mkdir
    path: "dirs/"
    weight: 30
    
  - op: put
    path: "dirs/"
    weight: 40
    size_distribution:
      type: fixed
      size: 1024
    
  - op: stat
    path: "dirs/*"
    weight: 20
    
  - op: rmdir
    path: "dirs/"
    weight: 10
```

## Operation Details

### MKDIR Operation

**Purpose:** Create directory structures for testing.

**Syntax:**
```yaml
- op: mkdir
  path: "base-path/"
  weight: 10
```

**Implementation:**
- Local filesystems: `std::fs::create_dir_all()`
- Object storage: No-op (logged at DEBUG level)

**Error handling:**
- Ignores "AlreadyExists" errors
- Logs other errors but continues execution
- Does not count as failed operation

**Use cases:**
- Prepare phase directory creation
- Directory contention testing
- Filesystem hierarchy benchmarking

**Example with directory tree:**
```yaml
directory_tree:
  width: 3
  depth: 2
  files_per_dir: 10
  distribution: bottom
  # MKDIR automatically called during prepare phase
```

### RMDIR Operation

**Purpose:** Remove empty directories.

**Syntax:**
```yaml
- op: rmdir
  path: "cleanup-path/"
  weight: 5
```

**Implementation:**
- Local filesystems: `std::fs::remove_dir()`
- Object storage: No-op

**Error handling:**
- Fails if directory non-empty
- Ignores "NotFound" errors
- Logs errors but continues

**Use cases:**
- Cleanup operations
- Directory lifecycle testing
- Filesystem space reclamation

**Important:** Only removes **empty** directories. Use DELETE to remove files first.

### LIST Operation

**Purpose:** Enumerate objects/files under a prefix.

**Syntax:**
```yaml
- op: list
  path: "data/"
  weight: 20
```

**Implementation:**
- **Local filesystems:**
  - `std::fs::read_dir()` for directory listing
  - Recursive traversal for subdirectories
  - Returns full paths

- **Object storage:**
  - Backend-specific list API (ListObjectsV2, List Blobs, etc.)
  - Prefix-based filtering
  - Returns object keys

**Performance considerations:**
- **S3:** Paginated (1000 objects per page), efficient prefix filtering
- **Azure Blob:** Paginated (5000 blobs per page), flat namespace
- **GCS:** Paginated (1000 objects per page), delimiter support
- **Local FS:** Depends on directory size, inode cache

**Use cases:**
- Discovery phase before operations
- Metadata-heavy workloads (CI/CD, analytics)
- Testing eventual consistency (S3)

**Example output:**
```
# Local filesystem
/mnt/shared/data/file_00000001.dat
/mnt/shared/data/file_00000002.dat
/mnt/shared/data/subdir/file_00000003.dat

# Object storage
data/file_00000001.dat
data/file_00000002.dat
data/subdir/file_00000003.dat
```

### STAT Operation

**Purpose:** Query object/file metadata without reading contents.

**Syntax:**
```yaml
- op: stat
  path: "data/*.dat"
  weight: 15
```

**Implementation:**
- **Local filesystems:**
  - `std::fs::metadata()` syscall
  - Returns: size, modified time, permissions, type
  - Very fast (cached in inode)

- **Object storage:**
  - HEAD request to backend
  - Returns: size, last-modified, etag, content-type
  - Network round-trip required

**Returned metadata:**

| Field | file:// | s3:// | az:// | gs:// |
|-------|---------|-------|-------|-------|
| Size | ✅ | ✅ | ✅ | ✅ |
| Modified time | ✅ | ✅ | ✅ | ✅ |
| ETag/Hash | ❌ | ✅ | ✅ | ✅ |
| Content-Type | ❌ | ✅ | ✅ | ✅ |
| Storage class | ❌ | ✅ | ✅ | ✅ |

**Use cases:**
- Existence checks before operations
- Size validation
- Timestamp verification
- Cheaper than GET for metadata-only needs

**Performance:**
- **Local FS:** <1ms (inode cache hit)
- **S3:** 20-50ms (HEAD request)
- **Azure Blob:** 30-80ms (HEAD request)
- **GCS:** 20-60ms (HEAD request)

### DELETE Operation

**Purpose:** Remove objects/files from storage.

**Syntax:**
```yaml
- op: delete
  path: "temp/*.dat"
  weight: 5
```

**Implementation:**
- **Local filesystems:**
  - `std::fs::remove_file()` for files
  - Immediate deletion
  
- **Object storage:**
  - Backend-specific DELETE API
  - May support batch deletion
  - Eventually consistent (S3)

**Batch deletion support:**

| Backend | Batch Delete | Max per Batch |
|---------|--------------|---------------|
| file:// | ❌ Individual | N/A |
| direct:// | ❌ Individual | N/A |
| s3:// | ✅ Yes | 1000 objects |
| az:// | ❌ Individual | N/A |
| gs:// | ✅ Yes | 100 objects |

**Error handling:**
- Ignores "NotFound" errors (idempotent)
- Logs other errors but continues
- Counts as successful if object removed

**Use cases:**
- Cleanup after tests
- Space reclamation
- Lifecycle testing (create → use → delete)
- Testing delete performance

**Important:** DELETE is **permanent** and **immediate** (local FS) or **eventually consistent** (S3).

## Examples

### Example 1: Metadata-Only Benchmark

```yaml
# Test metadata operations without data transfer
target: "s3://benchmark-bucket/metadata/"
duration: 60
concurrency: 64

prepare:
  ensure_objects:
    - base_uri: "s3://benchmark-bucket/metadata/"
      count: 10000
      size_distribution:
        type: fixed
        size: 1024  # Small files
      fill: zero

workload:
  - op: list
    path: "metadata/"
    weight: 30
    
  - op: stat
    path: "metadata/*.dat"
    weight: 70
```

**Purpose:** Test S3 LIST and HEAD performance at scale.

### Example 2: Directory Lifecycle (Filesystem)

```yaml
# Test directory create → use → remove
target: "file:///mnt/nfs/lifecycle/"
duration: 30
concurrency: 16

workload:
  - op: mkdir
    path: "test-dirs/"
    weight: 20
    
  - op: put
    path: "test-dirs/"
    weight: 40
    size_distribution:
      type: fixed
      size: 4096
    
  - op: get
    path: "test-dirs/*.dat"
    weight: 20
    
  - op: delete
    path: "test-dirs/*.dat"
    weight: 15
    
  - op: rmdir
    path: "test-dirs/"
    weight: 5
```

**Purpose:** Full directory lifecycle testing.

### Example 3: Azure Blob Metadata Testing

```yaml
# Azure Blob metadata operations
target: "az://mystorageaccount/container/test/"
duration: 45
concurrency: 32

prepare:
  ensure_objects:
    - base_uri: "az://mystorageaccount/container/test/"
      count: 5000
      size_distribution:
        type: uniform
        min: 1024
        max: 10240
      fill: random

workload:
  - op: list
    path: "test/"
    weight: 25
    
  - op: stat
    path: "test/*.dat"
    weight: 35
    
  - op: get
    path: "test/*.dat"
    weight: 30
    
  - op: delete
    path: "test/*.dat"
    weight: 10
```

**Purpose:** Comprehensive Azure Blob testing with metadata ops.

### Example 4: Mixed Workload All Backends

```yaml
# Works on ANY backend (MKDIR/RMDIR skipped for cloud)
target: "{{ TARGET }}"  # Can be file://, s3://, az://, gs://
duration: 60
concurrency: 16

directory_tree:
  width: 3
  depth: 2
  files_per_dir: 20
  distribution: all
  size:
    type: uniform
    min_size_kb: 4
    max_size_kb: 16
  fill: random

workload:
  - op: get
    weight: 40
  - op: put
    weight: 25
  - op: stat
    weight: 20
  - op: list
    weight: 10
  - op: delete
    weight: 5
```

**Purpose:** Universal config that works across all backends.

## Best Practices

### 1. Understand Backend Behavior

**Local filesystems:**
- MKDIR required before file creation
- RMDIR only for empty directories
- Very fast metadata operations (inode cache)

**Object storage:**
- No mkdir needed (implicit directories)
- LIST can be expensive at scale
- STAT is a network round-trip

### 2. Use Appropriate Operation Weights

**Metadata-light workload:**
```yaml
workload:
  - op: get
    weight: 70
  - op: put
    weight: 25
  - op: stat
    weight: 5
```

**Metadata-heavy workload:**
```yaml
workload:
  - op: list
    weight: 30
  - op: stat
    weight: 40
  - op: get
    weight: 30
```

### 3. Consider Eventual Consistency

**S3 example:**
```yaml
# After PUT, object may not immediately appear in LIST
- op: put
  path: "data/"
  weight: 50

- op: list
  path: "data/"
  weight: 50  # May miss recently PUT objects
```

**Solution:** Use separate test phases for create → list.

### 4. Batch Deletes for Cleanup

**S3 batch delete:**
```bash
# Use s3-cli for efficient cleanup
./s3-cli delete -r s3://bucket/test/
# Automatically batches 1000 objects per request
```

### 5. Monitor API Rate Limits

**S3:**
- LIST: 3,500 requests/s per prefix
- HEAD (STAT): Unlimited (best-effort)
- DELETE: Unlimited

**Azure Blob:**
- 20,000 operations/s per storage account (all ops)

**GCS:**
- 5,000 writes/s
- 50,000 reads/s

### 6. Test Cleanup

Always clean up test data:
```bash
# After testing
./s3-cli delete -r {{ TARGET_URI }}
```

## Troubleshooting

### "Directory not empty" on RMDIR

**Cause:** Attempting to remove directory containing files.

**Solution:** Delete files first:
```yaml
- op: delete
  path: "test-dir/*"
  weight: 80
  
- op: rmdir
  path: "test-dir/"
  weight: 20
```

### "Permission denied" on MKDIR

**Cause:** Insufficient filesystem permissions.

**Solution:** Check parent directory permissions:
```bash
ls -la /mnt/shared/
chmod 755 /mnt/shared/
```

### LIST returns zero objects (S3)

**Cause:** Eventual consistency - recently PUT objects not visible.

**Solution:** Add delay or use separate prepare phase:
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/test/"
      count: 1000
      # Objects created BEFORE workload starts
```

### STAT performance degradation (cloud)

**Cause:** Each STAT is a network round-trip.

**Solution:** Reduce STAT weight or increase concurrency:
```yaml
concurrency: 64  # More workers for network-bound ops

workload:
  - op: stat
    weight: 10  # Reduce from higher value
```

### Azure Blob "ContainerNotFound"

**Cause:** Incorrect URI format or missing container.

**Solution:** Use correct format and ensure container exists:
```yaml
# CORRECT
target: "az://storageaccount/container/prefix/"

# Create container first if needed
az storage container create --name container --account-name storageaccount
```

## See Also

- [Directory Tree Guide](DIRECTORY_TREE_GUIDE.md) - Hierarchical structure testing
- [Config Syntax Reference](CONFIG_SYNTAX.md) - Complete YAML reference
- [Cloud Storage Setup](CLOUD_STORAGE_SETUP.md) - Authentication and setup
- [Usage Guide](USAGE.md) - General usage patterns
- [Test Configurations](../tests/configs/README.md) - Example configs

## Version History

- **0.7.0** (2025-10-31) - Initial release
  - MKDIR, RMDIR, LIST, STAT, DELETE operations
  - Full backend support (file://, direct://, s3://, az://, gs://)
  - Conditional mkdir for object storage
  - Azure Blob Storage validation
