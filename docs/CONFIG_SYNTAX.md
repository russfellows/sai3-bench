# sai3-bench Configuration Syntax Reference

This document defines the correct YAML syntax for sai3-bench workload configurations.

## Configuration Validation

Before running a workload, validate your YAML config file with the `--dry-run` flag:

```bash
# Parse config and display test summary (no execution)
sai3-bench run --config my-workload.yaml --dry-run
```

This will:
- ✅ Parse and validate YAML syntax
- ✅ Check for required fields and correct data types
- ✅ Display test configuration summary (duration, concurrency, backend)
- ✅ Show prepare phase details (if configured)
- ✅ List all workload operations with weights and percentages
- ✅ Report any configuration errors with clear messages

**Example output**:
```
✅ Config file parsed successfully: my-workload.yaml

┌─ Test Configuration ────────────────────────────────────────┐
│ Duration:     60s
│ Concurrency:  32 threads
│ Target URI:   s3://my-bucket/test/
│ Backend:      S3
└─────────────────────────────────────────────────────────────┘

┌─ Workload Operations ───────────────────────────────────────┐
│ 2 operation types, total weight: 100
│
│ Op 1: GET - 70.0% (weight: 70)
│       path: data/*
│
│ Op 2: PUT - 30.0% (weight: 30)
│       path: output/, size: 1048576 bytes
└─────────────────────────────────────────────────────────────┘
```

## Basic Structure

```yaml
# Global settings
target: "gs://bucket-name/"   # Base URI for all operations (optional)
duration: "60s"               # Test duration (examples: "30s", "5m", "1h")
concurrency: 32               # Number of parallel workers

# Prepare stage (optional)
prepare:
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 1000
      min_size: 1048576
      max_size: 1048576
      fill: random
  cleanup: true  # Remove prepared objects after test

# Workload operations
workload:
  - op: get
    path: "data/prepared-*.dat"  # Glob pattern
    weight: 60
  
  - op: put
    path: "data/new-"
    object_size: 1048576
    weight: 25
```

## Target URI

The `target` field sets the base URI for all operations. Paths in workload operations are relative to this base.

```yaml
target: "gs://my-bucket/test/"

workload:
  - op: get
    path: "data/*.dat"  # Resolves to: gs://my-bucket/test/data/*.dat
```

**Supported schemes**:
- `file://` - Local filesystem
- `direct://` - Direct I/O (high performance local)
- `s3://` - Amazon S3 or S3-compatible
- `az://` - Azure Blob Storage
- `gs://` or `gcs://` - Google Cloud Storage

## Pattern Syntax

### Glob Patterns (with wildcards)

sai3-bench uses **glob patterns** with `*` wildcards for matching multiple objects:

```yaml
workload:
  - op: get
    path: "data/prepared-*.dat"  # ✅ Matches: prepared-00000000.dat, prepared-00000001.dat, etc.
  
  - op: delete
    path: "archive/*"             # ✅ Matches all files in archive/ directory
```

### What's NOT Supported

**Brace expansions** (bash-style) are **NOT supported**:

```yaml
workload:
  - op: get
    path: "obj_{00000..19999}"    # ❌ ERROR: This is NOT supported
```

### Pattern Resolution Behavior

Different operations handle patterns differently:

| Operation | Pattern Support | Behavior |
|-----------|----------------|----------|
| **GET** | ✅ Glob patterns | Pre-resolves at startup, samples randomly |
| **DELETE** | ✅ Glob patterns | Pre-resolves at startup, samples randomly |
| **STAT** | ✅ Glob patterns | Pre-resolves at startup, samples randomly |
| **PUT** | ❌ No patterns | Generates unique names dynamically |
| **LIST** | ❌ No patterns | Operates on directory prefixes |

## Operation Types

### GET - Read Objects

Read existing objects. Requires pattern that matches existing objects.

```yaml
- op: get
  path: "data/prepared-*.dat"  # Pattern matching existing objects
  weight: 60                   # Relative weight (60% of operations)
  concurrency: 64              # Optional: override global concurrency
```

**Pattern resolution**:
```
Resolving 1 GET operation patterns...
Found 2000 objects for GET pattern: gs://bucket/data/prepared-*.dat
```

### PUT - Write Objects

Create new objects with auto-generated unique names.

```yaml
- op: put
  path: "output/"              # Base path (object names auto-generated)
  object_size: 1048576         # Fixed size (1 MiB)
  weight: 25

# OR with size distribution:
- op: put
  path: "output/"
  size_distribution:
    type: lognormal
    mean: 1048576
    std_dev: 524288
    min: 1024
    max: 10485760
  weight: 25
```

**Generated names**: `gs://bucket/output/obj_<random_u64>`

**Size specifications**:

1. **Fixed size** (backward compatible):
```yaml
object_size: 1048576  # Always 1 MiB
```

2. **Uniform distribution**:
```yaml
size_distribution:
  type: uniform
  min: 1048576   # 1 MiB
  max: 10485760  # 10 MiB
```

3. **Lognormal distribution** (realistic):
```yaml
size_distribution:
  type: lognormal
  mean: 1048576      # Average: 1 MiB
  std_dev: 524288    # Std dev: 512 KiB
  min: 1024          # Min: 1 KiB
  max: 10485760      # Max: 10 MiB
```

### DELETE - Remove Objects

Delete existing objects matching a pattern.

```yaml
- op: delete
  path: "temp/prepared-*.dat"  # Pattern matching objects to delete
  weight: 10
```

**Important**: Keep DELETE weight lower than PUT weight to avoid exhausting object pool.

### STAT - Query Metadata

Query object metadata (size, modification time, etc.) without downloading content.

```yaml
- op: stat
  path: "data/prepared-*.dat"  # Pattern matching objects to stat
  weight: 5
```

### LIST - List Directory

List all objects in a directory/prefix.

```yaml
- op: list
  path: "data/"        # Directory to list (no glob pattern needed)
  weight: 10
```

**Note**: LIST operates on directory prefixes, not individual objects. No pattern resolution occurs.

## Prepare Stage

The prepare stage creates baseline objects before the workload begins.

```yaml
prepare:
  # Delay after prepare completes (seconds) - for cloud storage eventual consistency
  # Default: 0 (no delay)
  # Recommended: 2-5 for cloud storage (S3, GCS, Azure)
  post_prepare_delay: 5
  
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 2000            # Create 2000 objects
      min_size: 1048576      # Minimum size: 1 MiB
      max_size: 1048576      # Maximum size: 1 MiB (same = fixed size)
      fill: random           # Fill pattern: random, zero, or pattern
      dedup_factor: 1        # Deduplication factor (1 = no dedup)
      compress_factor: 1     # Compression factor (1 = no compression)
  cleanup: true              # Remove prepared objects after test

# You can prepare multiple object sets:
prepare:
  post_prepare_delay: 3      # Wait 3 seconds after creating objects
  ensure_objects:
    - base_uri: "gs://bucket/small/"
      count: 10000
      min_size: 1024
      max_size: 102400
    - base_uri: "gs://bucket/large/"
      count: 100
      min_size: 104857600    # 100 MiB
      max_size: 1073741824   # 1 GiB
```

**Post-Prepare Delay**: The `post_prepare_delay` field controls how long to wait after creating objects before starting the workload. This is essential for cloud storage backends that have eventual consistency:
- **Local storage** (`file://`, `direct://`): 0 seconds (no delay needed)
- **Cloud storage** (S3, GCS, Azure): 2-5 seconds recommended
- **Large object counts** (>1000 objects): 5-10 seconds recommended

The delay only applies if new objects were created. If all objects already existed, no delay occurs.

**Object naming**: Prepare creates objects named `prepared-NNNNNNNN.dat` where N is zero-padded 8-digit number.

Examples:
- `prepared-00000000.dat`
- `prepared-00000001.dat`
- `prepared-00001234.dat`

**Match your workload patterns accordingly**:
```yaml
prepare:
  post_prepare_delay: 3
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 1000

workload:
  - op: get
    path: "data/prepared-*.dat"  # ✅ Matches prepare naming
```

## Size Distributions

Available in both prepare stage and PUT operations.

### Fixed Size
```yaml
object_size: 1048576  # Always exactly 1 MiB
```

### Uniform Distribution
Evenly distributed between min and max:
```yaml
size_distribution:
  type: uniform
  min: 1048576
  max: 10485760
```

### Lognormal Distribution
Realistic distribution (many small, few large files):
```yaml
size_distribution:
  type: lognormal
  mean: 1048576
  std_dev: 524288
  min: 1024
  max: 10485760
```

## Weight System

Weights determine the relative frequency of operations:

```yaml
workload:
  - op: get
    weight: 60   # 60% of operations
  - op: put
    weight: 25   # 25% of operations
  - op: delete
    weight: 10   # 10% of operations
  - op: stat
    weight: 5    # 5% of operations
```

**Best practices**:
1. Ensure `PUT weight ≥ DELETE weight` to avoid exhausting object pool
2. Total weights don't need to sum to 100 (they're relative)
3. Use realistic ratios based on production workloads

## Concurrency Control

### Global Concurrency
```yaml
concurrency: 32  # 32 parallel workers for all operations
```

### Per-Operation Concurrency
```yaml
workload:
  - op: get
    path: "data/*.dat"
    weight: 60
    concurrency: 64  # Override: use 64 workers for GET operations
```

## Complete Example

```yaml
# Production-like mixed workload for Google Cloud Storage
target: "gs://production-bucket/benchmarks/"
duration: "5m"
concurrency: 32

prepare:
  ensure_objects:
    - base_uri: "gs://production-bucket/benchmarks/data/"
      count: 5000
      min_size: 1048576
      max_size: 1048576
      fill: random
  cleanup: true

workload:
  # Read existing objects (60%)
  - op: get
    path: "data/prepared-*.dat"
    weight: 60
  
  # Create new objects (25%)
  - op: put
    path: "data/new-"
    size_distribution:
      type: lognormal
      mean: 1048576
      std_dev: 524288
      min: 1024
      max: 10485760
    weight: 25
  
  # Delete objects (10%)
  - op: delete
    path: "data/prepared-*.dat"
    weight: 10
  
  # Query metadata (5%)
  - op: stat
    path: "data/prepared-*.dat"
    weight: 5
```

## Common Pitfalls

### ❌ Using Brace Expansions
```yaml
- op: get
  path: "obj_{00000..19999}"  # ERROR: Not supported
```

**Fix**: Use glob patterns:
```yaml
- op: get
  path: "prepared-*.dat"  # ✅ Correct
```

### ❌ DELETE Weight Too High
```yaml
workload:
  - op: put
    weight: 10
  - op: delete
    weight: 30  # ERROR: Deletes faster than PUT creates!
```

**Fix**: Ensure PUT ≥ DELETE:
```yaml
workload:
  - op: put
    weight: 30
  - op: delete
    weight: 10  # ✅ Correct
```

### ❌ Pattern Doesn't Match Prepared Objects
```yaml
prepare:
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 1000  # Creates: prepared-NNNNNNNN.dat

workload:
  - op: get
    path: "data/obj-*.dat"  # ERROR: No match!
```

**Fix**: Match the prepare naming:
```yaml
workload:
  - op: get
    path: "data/prepared-*.dat"  # ✅ Correct
```

## See Also

- [Usage Guide](USAGE.md) - Getting started with sai3-bench
- [Data Generation Guide](DATA_GENERATION.md) - Fill patterns, deduplication, and compression testing
- [Examples Directory](../examples/) - Complete example configurations
- [CHANGELOG](CHANGELOG.md) - Version history and breaking changes
