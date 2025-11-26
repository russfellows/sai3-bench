# Operation Log Sorting Clarification

**Date**: November 25, 2025  
**Affects**: sai3-bench v0.8.6+, s3dlio v0.9.22+

## Summary

**Critical Discovery**: The `S3DLIO_OPLOG_SORT` environment variable was documented in multiple places but **never implemented** in s3dlio. This documentation has been corrected.

## Background

Previous documentation incorrectly stated that setting `S3DLIO_OPLOG_SORT=1` would enable automatic operation log sorting during capture. However:

1. grep search of s3dlio codebase returned **zero matches** for "OPLOG_SORT"
2. Testing confirmed: oplogs generated with `S3DLIO_OPLOG_SORT=1` were still unsorted
3. Out-of-order warnings appeared even with environment variable set

## Why Logs Are Unsorted

Operation logs are NOT sorted during capture due to:
- **Concurrent writes**: Multiple threads complete operations at different times
- **Out-of-order completion**: Operations may complete in different order than they started
- **Buffer flushing**: Write buffering further mixes entry order

This is expected behavior for multi-threaded workloads.

## Correct Sorting Workflow

### Post-Processing with sai3-bench

Operation logs MUST be sorted **after capture** using the `sai3-bench sort` command:

```bash
# Sort by start timestamp (creates .sorted.tsv.zst files)
./sai3-bench sort --files /data/oplog.tsv.zst

# In-place sorting (overwrites original)
./sai3-bench sort --files /data/oplog.tsv.zst --in-place

# Sort multiple files
./sai3-bench sort --files /data/oplogs/*.tsv.zst

# Validate sorting
./sai3-bench replay --op-log /data/oplog.sorted.tsv.zst --dry-run
```

### Performance Characteristics

**Test Results** (14,497 operations):
- Original unsorted: 319 KB
- Post-processed sorted: 199 KB (38% reduction)
- Compression improvement: ~30-40% typical
- Sort algorithm: Window-based streaming (default 10,000 lines)
- Memory usage: Constant, regardless of log size

### Validation

```bash
# Unsorted file validation
$ ./sai3-bench replay --op-log /tmp/sorted-oplog.tsv.zst --dry-run
⚠️  WARNING: Op-log is NOT sorted!
    First out-of-order line: 6

# Sorted file validation  
$ ./sai3-bench replay --op-log /tmp/sorted-oplog.sorted.tsv.zst --dry-run
✓ Op-log is sorted (10000 lines checked)
```

## Documentation Updates

The following files have been corrected to remove S3DLIO_OPLOG_SORT references:

### sai3-bench
- `src/config.rs` - Updated op_log_path documentation
- `src/bin/agent.rs` - Updated --op-log flag documentation
- `docs/USAGE.md` - Added post-processing section, removed S3DLIO_OPLOG_SORT
- `docs/CONFIG_SYNTAX.md` - Clarified sorting is post-processing only

### s3dlio
- `docs/OPERATION_LOGGING.md` - Added "Post-Processing: Sorting Operation Logs" section

### Unchanged (Historical)
- `docs/archive/CHANGELOG_v0.1.0-v0.8.4.md` - Archived changelog preserved for history

## Supported Environment Variables

Only the following s3dlio oplog environment variables are supported:

```bash
# Buffer size (default: 64KB)
export S3DLIO_OPLOG_BUF=131072

# Zstd compression level (default: 3)
export S3DLIO_OPLOG_ZSTD_LEVEL=5
```

## Benefits of Sorting

1. **Better compression**: 30-40% smaller files due to improved compression ratios
2. **Chronological replay**: Required for accurate workload replay
3. **Accurate analysis**: Enables time-series analysis and latency correlation
4. **Validation**: Dry-run mode can detect out-of-order entries

## Migration Guide

If you were using `S3DLIO_OPLOG_SORT=1` (which had no effect):

**Before** (ineffective):
```bash
export S3DLIO_OPLOG_SORT=1
./sai3-bench run --config test.yaml
# Log was still unsorted!
```

**After** (correct):
```bash
# Step 1: Capture oplog (no special env var needed)
./sai3-bench run --config test.yaml

# Step 2: Post-process to sort
./sai3-bench sort --files results/oplog.tsv.zst

# Step 3: Use sorted file for replay/analysis
./sai3-bench replay --op-log results/oplog.sorted.tsv.zst
```

## Related Documentation

- [Operation Logging Guide](../../s3dlio/docs/OPERATION_LOGGING.md)
- [sai3-bench Usage Guide](USAGE.md)
- [Config Syntax Reference](CONFIG_SYNTAX.md)

---

**Note**: This correction applies to all versions. The S3DLIO_OPLOG_SORT environment variable was never implemented in any version of s3dlio.
