# RangeEngine Configuration Status

**Status**: Not yet implemented - Future enhancement  
**Date**: October 2025  
**Version**: v0.6.2

## Overview

sai3-bench currently uses s3dlio's default RangeEngine settings for all backends. Custom RangeEngine configuration (enabling/disabling, custom thresholds, etc.) is **not yet supported** but is planned for a future release.

## Current Behavior

All workloads use s3dlio's built-in RangeEngine defaults:
- **File backend**: 4MB threshold, 64MB chunks, 32 concurrent ranges
- **Network backends** (S3/Azure/GCS): 16MB threshold, 64MB chunks, 16 concurrent ranges
- RangeEngine automatically activates for files ≥ threshold size
- No way to disable or customize these settings currently

## Test Configurations (Documentation Only)

The following test configs exist as **examples** of the desired configuration syntax:
- ✅ `tests/configs/rangeengine_enabled.yaml` - Shows default settings
- ✅ `tests/configs/rangeengine_disabled.yaml` - Shows disable syntax
- ✅ `tests/configs/rangeengine_custom.yaml` - Shows custom thresholds

**Note**: These configs currently have no effect - they demonstrate the intended YAML syntax for when this feature is implemented.

## Why Not Implemented Yet

During the v0.6.2 upgrade to s3dlio v0.9.5, we explored adding RangeEngine configuration support. However:

1. **Requires extensive integration**: Would need to thread `Config` through ~14 call sites in workload execution
2. **Complex backend handling**: Different config structs for File, Azure, GCS, S3 backends
3. **Risk vs. benefit**: Large refactor for a feature that works well with defaults
4. **Out of scope**: The v0.6.2 focus was the s3dlio upgrade, not new features

## Future Implementation Plan

### Phase 1: Basic Integration
1. **Add config parameter to store creation**:
   - Thread `range_engine: Option<&RangeEngineConfig>` through workload execution
   - Update ~14 call sites in `src/workload.rs`
   - Use `store_for_uri_with_config()` instead of `store_for_uri()`

2. **Backend-specific config mapping**:
   - File/Direct: `FileSystemConfig` (already in s3dlio)
   - Azure: `AzureConfig` (s3dlio support needed)
   - GCS: `GcsConfig` (s3dlio support needed)
   - S3: May need s3dlio API additions

3. **Type conversions**:
   ```rust
   // Convert sai3-bench RangeEngineConfig → s3dlio backend configs
   let file_config = FileSystemConfig {
       enable_range_engine: cfg.enabled,
       range_engine: S3dlioRangeEngineConfig {
           chunk_size: cfg.chunk_size as usize,
           max_concurrent_ranges: cfg.max_concurrent_ranges,
           min_split_size: cfg.min_split_size,
           range_timeout: Duration::from_secs(cfg.range_timeout_secs),
       },
       ..Default::default()
   };
   ```

### Phase 2: Testing & Validation
1. Verify enabled vs. disabled performance difference
2. Test custom thresholds (2MB, 8MB, 32MB)
3. Validate concurrency limits (8, 16, 32, 64)
4. Cross-backend testing (all 5 backends)
5. Add integration tests

### Phase 3: Documentation
1. Update YAML config examples
2. Add RangeEngine tuning guide
3. Document backend-specific defaults
4. Performance tuning recommendations

## Workaround (Current)

Until implemented, all workloads use s3dlio's optimized defaults:
- **File backend**: 4MB threshold, 64MB chunks, 32 concurrent ranges
- **Network backends**: 16MB threshold, 64MB chunks, 16 concurrent ranges
- Automatic activation for large files
- Well-tested and performant for most use cases

**When custom config is needed**:
- Benchmarking RangeEngine impact (enable vs. disable comparisons)
- Hardware-specific tuning (adjust concurrency for available bandwidth)
- Memory constraints (smaller chunks for low-memory environments)
- Testing storage backend behavior under different access patterns

For **95% of users**, the defaults are ideal. Custom configuration is an advanced feature for specialized testing scenarios.

## References

- s3dlio RangeEngine documentation: See `s3dlio` crate docs
- Test configs (syntax examples): `tests/configs/rangeengine_*.yaml`
- s3dlio v0.9.5 release notes: RangeEngine improvements and default tuning
