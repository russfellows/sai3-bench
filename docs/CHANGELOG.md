# Changelog

All notable changes to s3-bench will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.3] - 2025-09-30

### Added
- **Microsecond Precision Metrics**: All timing measurements now use microsecond precision instead of milliseconds
- **Three-Category Operation Tracking**: Added META-DATA operations category alongside GET and PUT
  - LIST operations: Directory/prefix listings with timing metrics
  - STAT operations: Object metadata queries (HEAD requests)
  - DELETE operations: Object removal with timing tracking
- Enhanced configuration support for new operation types: `list`, `stat`, `delete`
- Comprehensive per-operation reporting with separate latency percentiles for GET, PUT, and META-DATA
- Updated reporting displays with microsecond (µs) units across all binaries

### Changed
- **BREAKING**: Changed timing precision from milliseconds to microseconds in all metrics
- **BREAKING**: Updated histogram bounds from (1, 60_000, 3) to (1, 60_000_000, 3) for microsecond scale
- **BREAKING**: Renamed struct fields from `p50_ms/p95_ms/p99_ms` to `p50_us/p95_us/p99_us`
- Enhanced `OpSpec` enum with `List`, `Stat`, and `Delete` variants
- Updated `Summary` and `OpAgg` structures to include `meta` fields for metadata operations
- Improved help message to reflect multi-backend I/O testing capabilities

### Fixed
- Consistent microsecond reporting across main CLI and run binary
- Proper operation category separation in metrics collection and reporting
- Enhanced ObjectStore integration for metadata operations

## [0.2.2] - 2025-09-30

### Added
- **Stage 2 Migration Complete**: Full ObjectStore trait implementation for all operations
- Multi-backend URI support: `file://`, `direct://`, and `s3://` schemes
- Comprehensive logging infrastructure with tracing crate
- CLI verbosity options: `-v` for info level, `-vv` for debug level logging
- Debug and info logging throughout workload execution and ObjectStore operations
- File backend testing and validation with successful operations

### Changed
- **BREAKING**: Migrated from AWS SDK direct calls to ObjectStore trait for all operations
- Replaced `get_object()` and `put_object_async()` with `get_object_multi_backend()` and `put_object_multi_backend()`
- Updated URI handling to use full URIs with ObjectStore instead of bucket/key splitting
- Improved prefetch operations to use `ObjectStore::list()` instead of AWS SDK `list_objects_v2()`
- Enhanced configuration pattern matching to use proper destructuring

### Removed
- Deprecated `prefetch_keys()` and `list_keys_async()` functions using AWS SDK
- Unused AWS SDK imports: `aws_config`, `aws_sdk_s3`, `RegionProviderChain`
- Legacy `parse_s3_uri` usage in workload.rs (still available in main.rs for CLI operations)
- Dead code: unused struct fields and variables in pattern matching

### Fixed
- All compiler warnings resolved through proper code analysis (not cheap underscore fixes)
- ObjectStore URI usage corrected to use full URIs following s3dlio test patterns
- Redundant pattern destructuring where config methods handled field extraction
- Proper handling of GetSource struct with only necessary fields

### Performance
- File backend testing shows excellent performance: 25,462 ops/s with 38.77 MB/s throughput
- Multi-backend operations maintain low latency: p50: 1ms, p95: 1ms, p99: 1ms
- Successful concurrent operations with proper semaphore-based concurrency control

### Technical Details
- ObjectStore operations now use `store_for_uri()` for automatic backend detection
- All operations handle full URIs natively without bucket/key splitting
- Logging provides visibility into ObjectStore creation and operation execution
- Clean compilation with zero warnings after proper code analysis

## [0.2.1] - 2025-09-29

### Added
- s3dlio v0.8.7 integration with ObjectStore trait support
- Multi-backend foundation for file:// and direct:// URI support
- Fork patch system for aws-smithy-http-client v1.1.1 compatibility

### Changed
- **BREAKING**: Updated to s3dlio v0.8.7 (pinned to rev cd4ee2e)
- Updated import structure to support both legacy s3_utils and new object_store APIs
- Improved BackendType::from_uri to use string matching for URI scheme detection

### Fixed
- Compilation issues with AWS SDK version conflicts
- list_objects function calls now include required recursive parameter
- Removed unused imports, variables, and dead code warnings
- Applied aws-smithy-http-client fork patch to expose hyper_builder method

### Technical Details
- All binaries (s3-bench, s3bench-agent, s3bench-ctl) compile successfully
- Maintains backward compatibility with existing S3 workloads
- Prepares foundation for Stage 2 migration to ObjectStore trait operations

## [0.2.0] - Previous Release
- Initial distributed execution with gRPC agents
- HDR histogram metrics collection
- Single-node CLI and multi-agent controller modes