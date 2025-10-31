# Directory Tree Workload Guide

**Version:** 0.7.0  
**Feature:** Hierarchical filesystem structure testing

## Overview

Directory tree workloads enable realistic shared filesystem testing by creating configurable hierarchical directory structures with files distributed across the tree. This feature supports both local filesystems and cloud object storage backends.

## Table of Contents

- [Quick Start](#quick-start)
- [Configuration](#configuration)
- [File Distribution Strategies](#file-distribution-strategies)
- [Size Specifications](#size-specifications)
- [Data Fill Types](#data-fill-types)
- [Backend Compatibility](#backend-compatibility)
- [Enhanced Dry-Run](#enhanced-dry-run)
- [Examples](#examples)
- [Best Practices](#best-practices)
- [Distributed Testing](#distributed-testing)
- [Performance Considerations](#performance-considerations)

## Quick Start

### Basic Local Filesystem Test

```yaml
# tests/configs/directory-tree/my-tree-test.yaml
target: "file:///tmp/sai3-tree-test/"
duration: 30
concurrency: 8

directory_tree:
  width: 3              # 3 subdirectories per level
  depth: 2              # 2 levels deep
  files_per_dir: 10     # 10 files per directory
  distribution: bottom  # Files only in leaf directories
  
  size:
    type: uniform
    min_size_kb: 4
    max_size_kb: 16
  
  fill: random          # Default: realistic random data

workload:
  - op: get
    weight: 60
  - op: put
    weight: 30
  - op: stat
    weight: 10
```

**Run with dry-run validation:**
```bash
./sai3-bench run --config my-tree-test.yaml --dry-run
# Output shows: Total Directories: 9, Total Files: 90, Total Data: 900 KiB

./sai3-bench run --config my-tree-test.yaml
```

### Azure Blob Storage Test

```yaml
target: "az://mystorageaccount/container/test-tree/"
duration: 20
concurrency: 8

directory_tree:
  width: 3
  depth: 2
  files_per_dir: 5
  distribution: all     # Files at every level (root + intermediate + leaf)
  
  size:
    type: uniform
    min_size_kb: 4
    max_size_kb: 16
  
  fill: random
```

## Configuration

### Complete Directory Tree Configuration

```yaml
directory_tree:
  # Tree structure
  width: 3              # Required: Subdirectories per level (1-100)
  depth: 2              # Required: Tree depth (1-10)
  files_per_dir: 10     # Required: Files per directory (1-10000)
  
  # File distribution strategy
  distribution: bottom  # Required: "bottom" or "all"
  
  # File size specification
  size:                 # Required: Size configuration
    type: uniform       # "uniform", "lognormal", or "fixed"
    # For uniform:
    min_size_kb: 4
    max_size_kb: 16
    
    # For lognormal:
    # mean_kb: 512
    # std_dev_kb: 256
    # min_size_kb: 1
    # max_size_kb: 10240
    
    # For fixed:
    # size_kb: 1024
  
  # Data generation
  fill: random          # Optional: "random" (default), "zero", or "sequential"
  dedup_factor: 1       # Optional: Deduplication factor (default: 1 = no dedup)
  compress_factor: 1    # Optional: Compression factor (default: 1 = uncompressible)
```

### Tree Dimensions

The tree structure is defined by `width` and `depth`:

- **width**: Number of subdirectories at each level
- **depth**: Number of levels in the tree (not counting root)

**Examples:**

| Width | Depth | Total Directories | Structure |
|-------|-------|-------------------|-----------|
| 2 | 1 | 2 | `root/d1/`, `root/d2/` |
| 2 | 2 | 6 | 2 level-1 + 4 level-2 |
| 3 | 2 | 12 | 3 level-1 + 9 level-2 |
| 4 | 3 | 84 | 4 + 16 + 64 directories |

**Formula:** Total dirs = width + width² + width³ + ... + width^depth

## File Distribution Strategies

### Bottom Distribution (Leaf-Only)

Files are placed **only in leaf directories** (deepest level).

```yaml
distribution: bottom
```

**Example:** 3×2 tree with 10 files per directory
- Root: **0 files**
- Level 1 (3 dirs): **0 files each**
- Level 2 (9 dirs): **10 files each**
- **Total: 90 files**

**Use cases:**
- Traditional shared filesystem patterns
- Deep hierarchies with data at leaves
- Maximum directory depth testing

### All-Levels Distribution

Files are placed **at every level** including root, intermediate, and leaf directories.

```yaml
distribution: all
```

**Example:** 3×2 tree with 5 files per directory
- Root: **5 files**
- Level 1 (3 dirs): **5 files each** = 15 files
- Level 2 (9 dirs): **5 files each** = 45 files
- **Total: 65 files** (5 + 15 + 45)

**Use cases:**
- Realistic mixed-depth access patterns
- Testing metadata operations at all levels
- Shallow + deep file access

## Size Specifications

### Uniform Distribution

Evenly distributed sizes between min and max.

```yaml
size:
  type: uniform
  min_size_kb: 4      # Minimum: 4 KB
  max_size_kb: 16     # Maximum: 16 KB
```

**Use cases:**
- Simple predictable testing
- Bandwidth testing with consistent sizes
- Quick validation tests

### Lognormal Distribution

Many small files, few large files (realistic pattern).

```yaml
size:
  type: lognormal
  mean_kb: 512        # Mean: 512 KB
  std_dev_kb: 256     # Standard deviation: 256 KB
  min_size_kb: 1      # Floor: 1 KB
  max_size_kb: 10240  # Ceiling: 10 MB
```

**Use cases:**
- Realistic object storage patterns
- Mixed small/large file workloads
- Production-like testing

### Fixed Size

All files same size.

```yaml
size:
  type: fixed
  size_kb: 1024       # All files: 1 MB
```

**Use cases:**
- Consistent I/O patterns
- Simplified performance analysis
- Baseline testing

## Data Fill Types

### Random Fill (Default, Recommended)

Generates realistic random data using s3dlio's data generator.

```yaml
fill: random
dedup_factor: 1       # 1 = no dedup, 20 = 95% duplicate blocks
compress_factor: 1    # 1 = uncompressible, 2 = 50% compressible
```

**Characteristics:**
- Compression-resistant by default
- Realistic storage behavior
- Controllable dedup/compression rates

**Use cases:**
- Production-realistic testing
- Deduplication testing
- Compression testing

### Zero Fill

All files filled with zeros (fast but unrealistic).

```yaml
fill: zero
```

**Characteristics:**
- Very fast generation
- Highly compressible/dedupable
- Unrealistic for storage testing

**Use cases:**
- Quick validation tests
- Network-only testing (no disk I/O)
- Development/debugging

### Sequential Fill

Sequential patterns for debugging.

```yaml
fill: sequential
```

**Use cases:**
- Data integrity verification
- Corruption detection
- Debugging

## Backend Compatibility

### Local Filesystems

**Fully supported** with explicit directory creation:

```yaml
target: "file:///mnt/shared-fs/test/"
target: "direct:///dev/nvme0n1/test/"  # Direct I/O
```

**Operations:**
- ✅ MKDIR: Creates actual directories
- ✅ RMDIR: Removes directories
- ✅ LIST: Directory listing
- ✅ STAT: File metadata
- ✅ DELETE: File deletion

### Object Storage (Cloud)

**Fully supported** with implicit directories:

```yaml
target: "s3://bucket/prefix/"
target: "az://account/container/prefix/"
target: "gs://bucket/prefix/"
```

**Operations:**
- ⚠️ MKDIR: **Skipped** (implicit directories)
- ⚠️ RMDIR: **Skipped** (no directories to remove)
- ✅ LIST: Object listing with prefix filtering
- ✅ STAT: Object metadata (HEAD)
- ✅ DELETE: Object deletion

**Key difference:** Object storage uses "key prefixes" that look like directories but don't require explicit creation.

### Backend Comparison

| Feature | file:// | direct:// | s3:// | az:// | gs:// |
|---------|---------|-----------|-------|-------|-------|
| Directory creation | ✅ Explicit | ✅ Explicit | ⚠️ Implicit | ⚠️ Implicit | ⚠️ Implicit |
| Directory removal | ✅ Yes | ✅ Yes | ⚠️ N/A | ⚠️ N/A | ⚠️ N/A |
| File operations | ✅ Full | ✅ Full | ✅ Full | ✅ Full | ✅ Full |
| Performance | Fast | Fastest | Moderate | Moderate | Moderate |
| Tested | ✅ Yes | ✅ Yes | ✅ Compatible | ✅ **TESTED** | ✅ Compatible |

## Enhanced Dry-Run

**Version 0.7.0** adds comprehensive pre-execution validation:

```bash
./sai3-bench run --config tree-test.yaml --dry-run
```

**Output includes:**
```
Directory Tree Configuration:
  Width:              3
  Depth:              2
  Distribution:       all (files at every level)
  Files per dir:      5
  
Total Directories:  12
Total Files:        60
Total Data:         600 KiB (614400 bytes)
  
Size Distribution:  uniform (4 KiB - 16 KiB)
Fill:               random
```

**Benefits:**
- Validates configuration before execution
- Shows exact directory/file counts
- Calculates total data size
- Prevents resource exhaustion

## Examples

### Example 1: Small Validation Test

```yaml
# Quick test with minimal files
target: "file:///tmp/quick-test/"
duration: 10
concurrency: 4

directory_tree:
  width: 2
  depth: 2
  files_per_dir: 4
  distribution: bottom
  size:
    type: fixed
    size_kb: 1024
  fill: random
```

**Result:** 4 leaf dirs × 4 files = **16 files**, 16 MB total

### Example 2: Realistic Shared Filesystem

```yaml
# Large shared filesystem simulation
target: "file:///mnt/shared-nfs/benchmark/"
duration: 300
concurrency: 32

directory_tree:
  width: 5
  depth: 3
  files_per_dir: 20
  distribution: all
  size:
    type: lognormal
    mean_kb: 256
    std_dev_kb: 128
    min_size_kb: 1
    max_size_kb: 5120
  fill: random

workload:
  - op: get
    weight: 70
  - op: put
    weight: 20
  - op: stat
    weight: 10
```

**Result:** 155 dirs × 20 files = **3,100 files**, ~800 MB total

### Example 3: Azure Blob Multi-Level

```yaml
# Azure Blob with all-levels distribution
target: "az://mystorageaccount/container/test-tree/"
duration: 60
concurrency: 16

directory_tree:
  width: 4
  depth: 2
  files_per_dir: 10
  distribution: all
  size:
    type: uniform
    min_size_kb: 8
    max_size_kb: 32
  fill: random
  dedup_factor: 1
  compress_factor: 1

workload:
  - op: get
    weight: 60
  - op: put
    weight: 30
  - op: stat
    weight: 10
```

**Result:** 20 dirs × 10 files = **200 files**, ~4 MB total

### Example 4: S3 Deep Hierarchy

```yaml
# S3 with deep tree, leaf-only distribution
target: "s3://benchmark-bucket/deep-tree/"
duration: 120
concurrency: 64

directory_tree:
  width: 3
  depth: 4
  files_per_dir: 5
  distribution: bottom
  size:
    type: uniform
    min_size_kb: 4
    max_size_kb: 16
  fill: random

workload:
  - op: get
    weight: 80
  - op: put
    weight: 15
  - op: stat
    weight: 5
```

**Result:** 81 leaf dirs × 5 files = **405 files**, ~4 MB total

## Best Practices

### 1. Always Use Dry-Run First

```bash
# Validate before executing
./sai3-bench run --config config.yaml --dry-run
```

**Why:** Prevents resource exhaustion, validates configuration, shows exact counts.

### 2. Use Random Fill for Realistic Testing

```yaml
fill: random  # Default in v0.7.0+
```

**Why:** Zero fill is unrealistic (100% compressible/dedupable).

### 3. Start Small, Scale Up

```yaml
# Start with small tree
width: 2
depth: 1
files_per_dir: 10

# Scale up after validation
width: 5
depth: 3
files_per_dir: 100
```

**Why:** Easier to debug, faster iteration, prevents overwhelming storage.

### 4. Match Distribution to Use Case

**Bottom distribution** for:
- Traditional filesystem layouts
- Deep directory testing
- Leaf-focused workloads

**All-levels distribution** for:
- Realistic mixed access
- Metadata operation testing
- Shallow + deep patterns

### 5. Consider Backend Characteristics

**Local filesystem:**
- Use `direct://` for maximum performance
- Consider filesystem limits (inodes, max files per dir)

**Object storage:**
- Understand implicit directory behavior
- Consider eventual consistency (S3)
- Account for API rate limits

### 6. Monitor Resource Usage

```bash
# Check disk space
df -h /mnt/shared-fs

# Check inode usage (filesystem only)
df -i /mnt/shared-fs

# Watch during execution
watch -n 1 'du -sh /mnt/shared-fs/benchmark'
```

## Distributed Testing

Directory tree workloads support **distributed coordination** via TreeManifest:

```yaml
distributed:
  agents:
    - address: "vm1:7761"
      id: "agent-1"
    - address: "vm2:7761"
      id: "agent-2"
    - address: "vm3:7761"
      id: "agent-3"

directory_tree:
  width: 3
  depth: 2
  files_per_dir: 30  # 90 files total per agent
  distribution: bottom
  size:
    type: uniform
    min_size_kb: 4
    max_size_kb: 16
  fill: random
```

**Key features:**
- **Shared tree structure:** All agents see same directory layout
- **Collision-free numbering:** TreeManifest assigns unique file ranges per agent
- **Accurate totals:** Controller correctly aggregates directory/file counts

**Example:** 3 agents, 90 files each
- Agent 1: `file_00000000.dat` - `file_00000089.dat`
- Agent 2: `file_00000090.dat` - `file_00000179.dat`
- Agent 3: `file_00000180.dat` - `file_00000269.dat`
- **Total:** 270 files across shared tree

## Performance Considerations

### File Count Impact

| Files | Prepare Time | Memory Usage | Disk Space (10KB avg) |
|-------|--------------|--------------|----------------------|
| 100 | <1s | Low | 1 MB |
| 1,000 | ~5s | Low | 10 MB |
| 10,000 | ~50s | Moderate | 100 MB |
| 100,000 | ~8min | High | 1 GB |

### Tree Depth vs Width

**Wide shallow trees** (e.g., width=10, depth=2):
- More directories at top levels
- Better parallelism
- Easier to navigate

**Narrow deep trees** (e.g., width=2, depth=5):
- Fewer total directories
- More metadata overhead
- Harder to parallelize

### Backend Performance

**Local filesystem** (file://):
- Prepare: Fast (local disk)
- Execution: Very fast (no network)
- Bottleneck: Local disk IOPS

**Direct I/O** (direct://):
- Prepare: Fastest (bypass page cache)
- Execution: Fastest (optimal for SSDs)
- Bottleneck: Device throughput

**Object storage** (s3://, az://, gs://):
- Prepare: Slower (network + API)
- Execution: Moderate (network latency)
- Bottleneck: API rate limits, network bandwidth

### Optimization Tips

1. **Increase concurrency** for cloud storage:
   ```yaml
   concurrency: 64  # More workers for network-bound ops
   ```

2. **Use bottom distribution** for faster prepare:
   ```yaml
   distribution: bottom  # Fewer total files
   ```

3. **Consider file count** vs duration:
   ```yaml
   duration: 60
   files_per_dir: 10  # Enough files to sustain 60s workload
   ```

4. **Monitor API rate limits** (cloud):
   - S3: ~3,500 PUT/s, ~5,500 GET/s per prefix
   - Azure: ~20,000 ops/s per storage account
   - GCS: ~5,000 writes/s, ~50,000 reads/s

## Troubleshooting

### "Not enough files for workload duration"

**Cause:** Workload depletes all files before duration expires.

**Solution:** Increase `files_per_dir` or decrease `duration`:
```yaml
files_per_dir: 50  # More files
# OR
duration: 10       # Shorter duration
```

### "Disk full" during prepare

**Cause:** Total data size exceeds available space.

**Solution:** Use dry-run to check size, reduce files or sizes:
```bash
./sai3-bench run --config config.yaml --dry-run  # Check "Total Data"
```

### "Too many open files"

**Cause:** Operating system file descriptor limit.

**Solution:** Increase ulimit:
```bash
ulimit -n 65536
./sai3-bench run --config config.yaml
```

### Azure Blob "NotFound" errors

**Cause:** Incorrect URI format.

**Solution:** Use correct format:
```yaml
# CORRECT
target: "az://storageaccount/container/prefix/"

# WRONG
target: "az://container/prefix/"  # Missing storage account
```

## See Also

- [Test Configuration Examples](../tests/configs/directory-tree/README.md) - 9 example configs
- [Config Syntax Reference](CONFIG_SYNTAX.md) - Complete YAML reference
- [Cloud Storage Setup](CLOUD_STORAGE_SETUP.md) - Authentication guides
- [Distributed Testing Guide](DISTRIBUTED_TESTING_GUIDE.md) - Multi-node testing
- [Data Generation Guide](DATA_GENERATION.md) - Dedup/compression patterns

## Version History

- **0.7.0** (2025-10-31) - Initial release
  - Directory tree workloads
  - Enhanced dry-run output
  - Metadata operations (MKDIR, RMDIR, LIST, STAT, DELETE)
  - Azure Blob Storage validation
