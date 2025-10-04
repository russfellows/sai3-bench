# Changelog

All notable changes to io-bench will be documented in this file.

## [0.5.3] - 2025-10-04

### 🎯 Realistic Size Distributions & Advanced Configurability
Surpasses MinIO Warp with realistic object size modeling and fine-grained concurrency control.

### ✨ New Features

#### Object Size Distributions (`src/size_generator.rs`)
**Problem**: Warp's "random" distribution is unrealistic. Real-world storage shows lognormal patterns (many small files, few large ones).

**Solution**: Three distribution types for PUT operations and prepare steps:

1. **Fixed Size** (backward compatible):
   ```yaml
   object_size: 1048576  # Exactly 1 MB
   ```

2. **Uniform Distribution** (evenly distributed):
   ```yaml
   size_distribution:
     type: uniform
     min: 1024        # 1 KB
     max: 10485760    # 10 MB
   ```

3. **Lognormal Distribution** (realistic - recommended):
   ```yaml
   size_distribution:
     type: lognormal
     mean: 1048576      # Mean: 1 MB
     std_dev: 524288    # Std dev: 512 KB
     min: 1024          # Floor
     max: 10485760      # Ceiling
   ```

**Implementation**:
- New `size_generator` module with `SizeSpec` enum and `SizeGenerator` struct
- Uses `rand_distr` crate for statistical distributions
- Rejection sampling for lognormal to respect min/max bounds
- Comprehensive unit tests for all distribution types

#### Per-Operation Concurrency
Fine-grained worker pool control per operation type:

```yaml
concurrency: 32  # Global default

workload:
  - op: get
    path: "data/*"
    weight: 70
    concurrency: 64  # Override: More GET workers
  
  - op: put
    path: "data/"
    object_size: 1048576
    weight: 30
    concurrency: 8   # Override: Fewer PUT workers
```

**Use cases**:
- Model read-heavy vs write-heavy workloads
- Simulate slow backend write performance
- Test different concurrency levels per operation type

**Implementation**:
- Per-operation semaphores (replaces single global semaphore)
- Optional `concurrency` field in `WeightedOp`
- Logs custom concurrency settings for visibility

### 📚 Enhanced Documentation

#### Prepare Profiles
Documented realistic multi-tier preparation patterns in `docs/CONFIG.sample.yaml`:
- Small files (metadata, configs) with lognormal distribution
- Medium files (documents, images) with lognormal distribution
- Large files (videos, backups) with uniform distribution
- Complete production-clone example with 3 tiers

#### Advanced Remapping Examples
Added **N↔N (many-to-many)** remapping example to README:
```yaml
# Map 3 source buckets → 2 destination buckets
remap:
  - pattern: "s3://source-1/(.+)"
    replacement: "s3://dest-a/$1"
  - pattern: "s3://source-2/(.+)"
    replacement: "s3://dest-a/$1"
  - pattern: "s3://source-3/(.+)"
    replacement: "s3://dest-b/$1"
```

#### Competitive Advantage Table
Added comprehensive comparison vs Warp in README showing superiority in:
- Size distributions (Uniform + Lognormal vs Random only)
- Concurrency control (Per-operation vs Global only)
- Backend support (5 backends vs S3 only)
- Output format (TSV vs Text)
- Memory usage (Constant vs High)

### 🔧 Improvements
- **Config backward compatibility**: Old `object_size` and `min_size`/`max_size` syntax still works
- **Migration helpers**: `get_size_spec()` method converts legacy syntax to new `SizeSpec`
- **Helper methods**: `SizeGenerator::description()` for logging, `expected_mean()` for validation
- **Human-readable formatting**: `human_bytes()` function for size display

### 🧪 Testing
- **Unit tests**: 5 tests in `size_generator::tests` (fixed, uniform, lognormal, invalid specs, human_bytes)
- **Integration tests**:
  - `tests/configs/size_distributions_test.yaml` - Mixed lognormal, uniform, and fixed sizes
  - `tests/configs/per_op_concurrency_test.yaml` - Different concurrency per operation
- **Performance validation**: No regression (3649-5587 ops/s depending on config)

### 📊 Test Results
```
size_distributions_test.yaml:
- Total ops: 18457 in 5.06s (3649 ops/s)
- Observed realistic size distributions across 9 buckets
- Lognormal PUT: Many small (1B-8KiB: 131 ops), few large (4MiB-32MiB: 3 ops)

per_op_concurrency_test.yaml:
- Total ops: 28268 in 5.06s (5587 ops/s)
- GET ops: 17074 (60% of total, concurrency=64)
- PUT ops: 11194 (40% of total, concurrency=4)
- Confirmed per-op concurrency via logs
```

### 🏆 Competitive Position
With v0.5.3, **io-bench surpasses MinIO Warp** in:
1. **Realistic workload modeling** - Lognormal distributions match real-world storage patterns
2. **Configurability** - Per-operation concurrency for advanced scenarios
3. **Backend support** - 5 protocols vs S3-only
4. **Output quality** - Machine-readable TSV with 13 columns
5. **Memory efficiency** - Constant memory streaming replay

## [0.5.2] - 2025-10-04

### 📚 Documentation Cleanup & Polish
Streamlined documentation for clarity and removed obsolete files.

### 🗑️ Removed Files
- **Old release notes**: Removed 4 version-specific release note files (v0.3.0, v0.3.2, v0.4.0, v0.4.3)
  - All release information consolidated into CHANGELOG.md
- **Completed design docs**: Removed 4 completed implementation documents
  - MIGRATION_PLAN.md (multi-backend migration complete)
  - OP_LOG_REPLAY_DESIGN.md (replay feature implemented)
  - REPLAY_FUTURE_WORK.md (features completed in v0.5.0)
  - DEVELOPMENT_NOTES.md (old v0.3.0 technical notes)

### ✨ Improvements
- **README.md**: Updated to v0.5.2 with comprehensive badges and modern examples
  - Added version, build, tests, license, and Rust version badges
  - Updated Quick Start with TSV export and advanced remapping examples
  - Added dedicated TSV Export section
  - Updated performance characteristics for all 5 backends
- **Documentation index**: Created docs/README.md for easy navigation
- **Warp parity tracking**: Added WARP_PARITY_STATUS.md showing 95% completion
- **Kept recent work**: Preserved v0.5.x implementation summaries and reference materials

### 📂 Documentation Structure (40% reduction)
From 20 files to 12 essential documents:
- 4 user guides (USAGE, AZURE_SETUP, CONFIG samples)
- 4 reference docs (CHANGELOG, INTEGRATION_CONTEXT, Warp parity docs)
- 4 recent implementation records (v0.5.0/v0.5.1 summaries, POLARWARP analysis)

## [0.5.1] - 2025-10-04

### 🎯 Machine-Readable Results & Enhanced Metrics
Phase 2.5 of Warp Parity: Add TSV export for automated analysis and complete size-bucketed histogram collection.

### ✨ New Features

#### TSV Export (`src/tsv_export.rs`)
- **Machine-readable results**: Export benchmark data in tab-separated format for automated analysis
- **CLI flag**: `--results-tsv <path>` for run command (creates `<path>-results.tsv`)
- **Format**: 13-column TSV with operation, size_bucket, bucket_idx, mean_us, p50_us, p90_us, p95_us, p99_us, max_us, avg_bytes, ops_per_sec, throughput_mibps, count
- **Per-bucket metrics**: Detailed statistics for each of 9 size buckets (zero, 1B-8KiB, 8KiB-64KiB, etc.)
- **Accurate throughput**: Uses actual bytes from SizeBins (not estimated), reported in MiB/s
- **Automated parsing**: Compatible with pandas, polars, and standard TSV tools

#### Enhanced Metrics Collection
- **Shared metrics module** (`src/metrics.rs`): Unified histogram infrastructure
- **Mean (average) latency**: Added alongside median (p50) for better statistical analysis
- **Size-bucketed histograms**: Now consistent across all execution modes (CLI commands and workload runs)
- **9 size buckets**: Tracks performance characteristics by object size from 0 bytes to >2GiB
- **Histogram exposure**: Summary struct now includes OpHists for TSV export
- **Actual byte tracking**: SizeBins.by_bucket provides real ops/bytes per size bucket

### 🐛 Fixes
- **Workload histogram consistency**: Fixed missing size-bucketed collection in `workload::run()`
- **Code deduplication**: Removed 90+ lines of duplicate histogram code from main.rs

### 🔧 Changes
- **Default concurrency**: Increased from 20 to 32 workers
- **Cargo.toml**: Version bump to 0.5.1, added chrono dependency

### 📚 Documentation
- **POLARWARP_ANALYSIS.md**: Reference analysis of polarWarp TSV format
- **V0.5.1_PLAN.md**: Complete implementation plan for TSV export
- **V0.5.1_PROGRESS.md**: Progress tracking and validation results

### ✅ Validation
- **Multi-size test**: 4 size ranges (1KB, 128KB, 2MB, 16MB) showing 65x latency scaling
- **Mean vs median**: Demonstrated mean significantly higher than p50 for small objects (up to 934% difference)
- **TSV parsing**: Verified machine-readability with 13 properly formatted columns
- **Performance**: Maintained 19.6k ops/s on file backend with bucketed collection

## [0.5.0] - 2025-10-04

### 🎯 Warp Parity Phase 2: Advanced Replay Remapping
Complete warp-replay compatibility with flexible URI remapping for multi-target testing and migration.

### ✨ New Features

#### Remap Engine (`src/remap.rs`)
Advanced URI remapping system supporting multiple mapping strategies:

1. **Simple 1→1 Remapping**
   - Direct bucket/prefix replacement
   - Use case: Migrate workloads between environments
   ```yaml
   rules:
     - match: {bucket: "prod"}
       map_to: {bucket: "staging", prefix: "test/"}
   ```

2. **1→N Fanout**
   - Distribute operations across multiple targets
   - Three strategies: `round_robin`, `random`, `sticky_key`
   - Use case: Load testing, multi-region replication, chaos engineering
   ```yaml
   rules:
     - match: {bucket: "source"}
       map_to_many:
         targets:
           - {bucket: "dest1", prefix: ""}
           - {bucket: "dest2", prefix: ""}
           - {bucket: "dest3", prefix: ""}
         strategy: "round_robin"
   ```

3. **N→1 Consolidation**
   - Merge multiple sources to single target
   - Use case: Data consolidation, backup aggregation
   ```yaml
   rules:
     - match_any:
         - {bucket: "temp-1"}
         - {bucket: "temp-2"}
       map_to: {bucket: "consolidated", prefix: "merged/"}
   ```

4. **N↔N Regex-based Remapping**
   - Pattern matching with capture groups
   - Use case: Complex transformations, dynamic routing
   ```yaml
   rules:
     - regex: "^s3://prod-([^/]+)/(.*)$"
       replace: "s3://staging-$1/$2"
   ```

#### Fanout Strategies

**round_robin**: Sequential distribution for even load balancing
- Operation 1 → Target A
- Operation 2 → Target B
- Operation 3 → Target C
- Operation 4 → Target A (cycles)

**random**: Random selection for chaos testing
- Each operation randomly assigned to a target
- Useful for testing eventual consistency

**sticky_key**: Consistent hashing for session affinity
- Same object always goes to same target
- Deterministic based on object key hash
- Maintains data locality across runs

### 🔧 CLI Enhancements

#### New Replay Flag
```bash
# Basic remapping
io-bench replay --op-log production.tsv.zst --remap remap.yaml

# With speed control
io-bench replay --op-log ops.tsv.zst --remap fanout.yaml --speed 2.0

# Multi-target fanout
io-bench replay --op-log prod.tsv.zst --remap remap_fanout.yaml
```

### 🏗️ Architecture

#### Core Components
- **`RemapConfig`**: YAML-driven configuration with ordered rules
- **`RemapRule`**: Enum supporting 4 rule types (Simple, Fanout, Consolidate, Regex)
- **`RemapEngine`**: Rule execution engine with state management
- **`ParsedUri`**: S3/file URI parser extracting scheme/bucket/prefix/key

#### Integration
- Extended `ReplayConfig` with optional `remap_config` field
- Remap applied before each operation execution in replay loop
- Zero overhead when remapping not used

### 📝 Configuration Schema

#### MatchSpec (for matching source URIs)
```yaml
match:
  host: "optional-host"      # Match specific host (rarely used)
  bucket: "bucket-name"      # Match specific bucket
  prefix: "path/prefix/"     # Match objects under prefix
```

#### TargetSpec (for destination URIs)
```yaml
map_to:
  bucket: "dest-bucket"
  prefix: "dest/path/"       # Empty = preserve original path
```

#### Path Preservation Logic
- **Empty target prefix**: Preserve original path structure
  - `s3://src/data/file.dat` → `s3://dest/data/file.dat`
- **Non-empty target prefix**: Replace original prefix
  - `s3://src/old/file.dat` + `new/` → `s3://dest/new/file.dat`

### ✅ Testing & Validation

#### Unit Tests (10 tests, all passing)
- **`test_parse_uri_s3`**: S3 URI parsing (`s3://bucket/prefix/key`)
- **`test_parse_uri_file`**: File URI parsing (`file:///path/to/file`)
- **`test_simple_remap`**: 1→1 bucket/prefix replacement
- **`test_fanout_round_robin`**: Sequential distribution across 3 targets
- **`test_regex_remap`**: Pattern-based prod→staging transformation
- **`test_no_match_returns_original`**: Fallback to original URI
- Existing replay tests continue to pass

#### Build Verification
```bash
cargo test --lib
test result: ok. 10 passed; 0 failed

cargo build --release
Finished `release` profile [optimized] in 13.20s
```

### 🔨 Implementation Details

#### State Management
- **Round-robin**: `HashMap<rule_idx, counter>` per rule
- **Sticky-key**: `HashMap<key, target_idx>` for consistent hashing
- Thread-safe with `Arc<Mutex<>>`
- Per-rule state prevents interference

#### Ordered Rule Matching
- Rules evaluated sequentially; first match wins
- Allows specific rules before general rules
- Override patterns via ordering

#### URI Parsing
```rust
// S3 URI structure
s3://bucket/prefix/path/to/key.dat
  ├── scheme: "s3"
  ├── bucket: "bucket"
  ├── prefix: "prefix/path/to/"
  └── key: "key.dat"
```

### 📦 Files Modified

**New Files**:
- `src/remap.rs` (488 lines): Complete remap engine
- `tests/configs/remap_examples.yaml`: Comprehensive configuration examples
- `tests/configs/remap_fanout_test.yaml`: Simple fanout test

**Modified Files**:
- `Cargo.toml`: Version 0.4.3 → 0.5.0
- `src/lib.rs`: Added `pub mod remap;`
- `src/replay_streaming.rs`: Extended `ReplayConfig`, integrated remap engine
- `src/main.rs`: Added `--remap` CLI flag, config loading
- `.github/copilot-instructions.md`: Updated to v0.5.0-dev

### 🎓 Example Configurations

#### Complete Multi-Rule Configuration
```yaml
rules:
  # Rule 1: Simple bucket rename
  - match: {bucket: "old-bucket", prefix: "logs/"}
    map_to: {bucket: "new-bucket", prefix: "archived/"}
  
  # Rule 2: Fanout for load testing
  - match: {bucket: "source"}
    map_to_many:
      targets:
        - {bucket: "replica1", prefix: ""}
        - {bucket: "replica2", prefix: ""}
        - {bucket: "replica3", prefix: ""}
      strategy: "round_robin"
  
  # Rule 3: Consolidation
  - match_any:
      - {bucket: "temp-1"}
      - {bucket: "temp-2"}
    map_to: {bucket: "consolidated", prefix: "merged/"}
  
  # Rule 4: Regex transformation
  - regex: "^s3://prod-([^/]+)/(.*)$"
    replace: "s3://staging-$1/$2"
```

### 🐛 Bug Fixes

#### Path Structure Preservation
**Fixed**: URI remapping now correctly handles:
- Empty target prefix → preserve original path structure
- Non-empty target prefix → replace prefix, keep key
- Prevents both "lost path" and "double path" issues

#### S3 URI Parsing
**Fixed**: S3 URIs correctly parsed without "host" component
- `s3://bucket/path` → bucket="bucket", not host="bucket"
- Consistent with S3 URL structure

### 🚀 Roadmap

#### v0.5.1 (Phase 3): UX Polish
- Enhanced documentation with examples
- Variable substitution in configs
- Improved CLI help text
- Warp-compatible defaults

#### v0.5.2 (Phase 4): Testing & Validation
- Integration tests with real backends
- Performance benchmarking
- Warp comparison validation
- Production-ready hardening

---

## [0.4.3] - 2025-10-04

### 🎯 Warp Parity Phase 1: Prepare/Pre-population
MinIO Warp compatibility features enabling near-identical test workflows between io-bench and Warp.

### ✨ New Features

#### Prepare Step (Pre-population)
- **ensure_objects**: Guarantee test objects exist before timed workload execution
  - Configurable count, size range (min_size/max_size), and fill pattern (zero/random)
  - Skips creation if sufficient objects already exist (idempotent)
  - Progress bar with ops/sec tracking during preparation
  - Example: Create 50K objects @ 1 MiB each before 5-minute mixed workload
  
- **cleanup**: Optional automatic cleanup after test completion
  - Only deletes objects created during prepare step
  - Preserves objects created during actual workload
  - CLI override via `--no-cleanup` flag

#### CLI Enhancements
- `--prepare-only`: Execute prepare step then exit (for pre-populating test data)
- `--no-cleanup`: Override config cleanup setting (keep all objects)
- Examples:
  ```bash
  # Prepare objects once, run tests multiple times
  io-bench run --config mixed.yaml --prepare-only
  io-bench run --config mixed.yaml
  io-bench run --config mixed.yaml
  
  # Run with cleanup disabled
  io-bench run --config mixed.yaml --no-cleanup
  ```

### 🔧 Configuration Schema Extensions

#### New PrepareConfig Structure
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/prefix/"
      count: 50000
      min_size: 1048576  # 1 MiB (default)
      max_size: 1048576  # 1 MiB (default)
      fill: zero         # "zero" or "random" (default: zero)
  cleanup: false         # Auto-cleanup after test (default: false)
```

### 🎯 Warp Compatibility Examples
```yaml
# Warp-style mixed workload
duration: 300s      # 5 minutes
concurrency: 20     # Match Warp default (changed from 16)

prepare:
  ensure_objects:
    - base_uri: "s3://bench/test/"
      count: 50000
      min_size: 1048576
  cleanup: false

workload:
  - {weight: 50, op: get, path: "test/*"}
  - {weight: 30, op: put, path: "test/*", object_size: 1048576}
  - {weight: 10, op: list, path: "test/"}
```

### � Bug Fixes & Improvements

#### Progress Bar Consistency
- **Unified Visual Style**: All progress bars now use consistent block characters (`████`)
  - Prepare phase: `[████████████] 1000/1000 objects`
  - Test phase: `[████████████] 10/10s`
  - Cleanup phase: `[████████████] 100/100 objects`
  - Previously: Test phase used ugly `###>---` style

#### s3dlio Native Methods
- **STAT Operations**: Now use s3dlio's native `stat()` method (v0.8.8+)
  - Previously: Used workaround with `get()` to measure size
  - Now: Proper HEAD-like metadata operations
  - Benefit: More efficient, less data transfer, correct semantics
  
#### Pattern Matching Clarification
- **Documented Approach**: Added comments explaining io-bench's URI pattern matching
  - s3dlio's ObjectStore doesn't provide native glob support in `list()`
  - io-bench implements list-and-filter for `*` patterns at application level
  - This is correct: object stores don't have native glob (S3, Azure, GCS all work this way)
  - For local file operations, s3dlio has `generic_upload_files()` with glob crate

### �🔨 Implementation Details
- **src/config.rs**: Added `PrepareConfig`, `EnsureSpec`, `FillPattern` types
  - `default_concurrency()` changed from 16 → 20 (match Warp)
  - All new fields are optional (backward compatible)
  
- **src/workload.rs**: New prepare infrastructure + improvements
  - `prepare_objects()`: Create/verify test objects with progress tracking
  - `cleanup_prepared_objects()`: Delete only created objects
  - `PreparedObject`: Tracks URI, size, and created flag
  - `stat_object_multi_backend()`: Updated to use s3dlio's `stat()` method
  - Progress bar style unified across all phases
  - Uses s3dlio ObjectStore for all operations (consistent with existing code)

- **src/main.rs**: Updated `run_workload()` command handler
  - Three-phase execution: Prepare → Test → Cleanup
  - CLI flag processing for `--prepare-only` and `--no-cleanup`
  - Conditional phase execution with clear console output

### ✅ Validation & Testing
- **Performance**: 500+ objects/sec prepare speed (1 MiB objects on local file backend)
- **Idempotency**: Repeated prepare-only calls skip existing objects
- **Cleanup**: Verified only prepared-*.dat removed, test-* preserved
- **CLI Flags**: All combinations tested (prepare-only, no-cleanup, default)
- **Progress Bars**: Visual consistency verified across all phases
- **STAT Operations**: Confirmed using native s3dlio method
- **Example Configs**: `tests/configs/warp_parity_mixed.yaml`, `tests/configs/warp_cleanup_test.yaml`

### 📦 Dependencies
- **rand**: Updated API usage (deprecated `thread_rng()` → `rng()`, `gen_range()` → `random_range()`)
- **indicatif**: Progress bars for prepare and cleanup phases
- **s3dlio v0.8.8+**: Leveraging native `stat()` method

### 🚀 Roadmap
- **v0.5.0**: Phase 2 - Advanced replay remapping (1→N, N→1, N↔N)
- **v0.5.1**: Phase 3 - UX polish and documentation
- **v0.5.2**: Phase 4 - Comprehensive testing and Warp comparison validation

---

## [0.4.2] - 2025-10-03

### 🌐 Google Cloud Storage Backend Support
- **New Backend**: Added comprehensive GCS support as the 5th storage backend
  - **URI Schemes**: Both `gs://bucket/prefix/` and `gcs://bucket/prefix/` supported
  - **Authentication**: Google Application Default Credentials (ADC) integration
    - Service account JSON via `GOOGLE_APPLICATION_CREDENTIALS`
    - GCE/GKE metadata server (automatic on Google Cloud)
    - gcloud CLI credentials
  - **Full Operation Support**: GET, PUT, DELETE, LIST, STAT operations
  - **Performance**: 9-11 MB/s for large objects (5MB), ~400-600ms latency for small objects

### 🔧 Implementation Details
- **src/workload.rs**: Added `Gcs` variant to `BackendType` enum
  - URI scheme detection for `gs://` and `gcs://`
  - Backend name: "Google Cloud Storage"
  - URI path building compatible with S3-style paths
- **src/main.rs**: Updated URI validation to accept GCS schemes
  - Added `gs` and `gcs` to supported schemes list
  - Updated error messages to include `gs://`

### ✅ Testing & Validation
- **8 Comprehensive Integration Tests** (tests/gcs_tests.rs)
  - Backend detection and ObjectStore creation
  - PUT/GET/DELETE cycle with content verification
  - LIST operations (5 objects)
  - STAT metadata retrieval
  - Concurrent operations (10 objects with tokio::spawn)
  - Large object handling (5MB at 9.48/11.29 MB/s)
  - Alternate URI scheme testing (gs:// and gcs://)
  - **All tests passed** against real GCS bucket (signal65-russ-b1)
  
- **Shell Test Suite** (tests/gcs_backend_test.sh)
  - 9 comprehensive tests covering health check, operations, concurrency
  - Professional output with colors and progress tracking
  - Op-log generation and workload testing

- **YAML Configuration** (tests/configs/gcs_test.yaml)
  - Mixed workload template (PUT 30%, GET 50%, LIST 10%, STAT 10%)
  - Environment variable support for `GCS_BUCKET`

### 📦 Dependencies
- **tokio**: Added to dev-dependencies with "full" features for async tests

### 📝 Documentation
- **README.md**: Updated from "4 storage backends" to "5 storage backends"
- **GCS_INTEGRATION_COMPLETE.md**: Comprehensive integration documentation
  - Authentication setup instructions
  - Usage examples and performance characteristics
  - Test results and verification checklist

### 🎯 Real-World Testing
Successfully tested against Google Cloud Storage:
- Bucket: gs://signal65-russ-b1/
- Project: signal65-testing
- All 8 Rust integration tests passed in 14.76s
- CLI operations verified (health, put, get, delete, list)

## [0.4.1] - 2025-10-03

### 🚀 Major Updates
- **Streaming Op-log Replay**: Memory-efficient replay using s3dlio-oplog streaming reader
  - **Constant Memory Usage**: ~1.5 MB regardless of op-log size (vs. unbounded Vec-based approach)
  - **Background Decompression**: Parallel zstd decompression in dedicated thread
  - **Tunable Performance**: Environment variables for buffer size and chunk size
    - `S3DLIO_OPLOG_READ_BUF`: Channel buffer size (default: 1024 entries)
    - `S3DLIO_OPLOG_CHUNK_SIZE`: Decompression chunk size (default: 1 MB)
  - **Shared Types**: Uses `s3dlio_oplog::{OpLogEntry, OpType, OpLogStreamReader}` for consistency

### 🔧 Technical Improvements
- **Non-logging Replay Operations**: Added `*_no_log()` variants in workload.rs
  - Prevents circular logging during replay (replay operations are not logged)
  - Eliminates "sending on a closed channel" errors when global logger is finalized
  - Functions: `get_object_no_log()`, `put_object_no_log()`, `list_objects_no_log()`, `stat_object_no_log()`, `delete_object_no_log()`

- **Deprecated Legacy Replay**: Old `src/replay.rs` marked deprecated with warnings
  - Backup preserved in `src/replay_v040_backup.rs`
  - New streaming implementation in `src/replay_streaming.rs`
  - Updated `src/main.rs` to use streaming replay by default

### 📦 Dependencies
- **s3dlio**: Updated from tag v0.8.12 to branch "main" (v0.8.19+)
  - Includes 10+ releases with bug fixes and performance improvements
  - GCS backend support (gs:// and gcs:// URIs) - ready for future integration
- **s3dlio-oplog**: New dependency for streaming op-log parsing
  - Separate workspace member in s3dlio repository
  - Provides `OpLogStreamReader` for memory-efficient iteration

### ✅ Testing
- **Comprehensive Integration Tests**: 6 tests validating streaming replay
  - Round-trip test: s3dlio generates op-log → io-bench replays
  - Memory efficiency test: 100+ operations with constant memory
  - URI remapping test: Replay to different storage backend
  - Error handling test: Continue-on-error functionality
  - Concurrent limits test: Configurable concurrency controls
  - Streaming reader test: Iterator-based processing
- **Global Logger Workaround**: Tests structured to work with s3dlio's singleton logger
  - One generation test creates op-log (calls finalize once)
  - Other tests read and replay existing op-logs (no logging)

### 📝 Documentation
- **docs/S3DLIO_UPDATE_PLAN.md**: Comprehensive update strategy for s3dlio v0.8.19+
- **STREAMING_REPLAY_COMPLETE.md**: Implementation summary and validation results

### 🔮 Future Work
- GCS backend integration (Phase 2 - ready in s3dlio)
- Advanced URI remapping (M:N, sticky sessions)
- Op-log filtering and transformation

## [0.4.0] - 2025-10-01
### Added
- **Op-log Replay**: Full timing-faithful workload replay from TSV op-log files
  - Absolute timeline scheduling for microsecond-precision timing (~10µs accuracy)
  - Auto-detection of zstd compression (.zst extension)
  - Speed multiplier (`--speed`) for faster/slower replay
  - Target URI remapping (`--target`) for simple 1:1 backend retargeting
  - Support for all 5 operation types: GET, PUT, DELETE, LIST, STAT
  - Error handling with `--continue-on-error` flag
  - Uses `s3dlio::data_gen::generate_controlled_data()` for PUT operations
  - Microsecond-precision sleep via `std::thread::sleep` wrapped in `tokio::task::spawn_blocking`

### Dependencies
- Added `csv = "1.3"` for TSV parsing
- Added `chrono = "0.4"` for timestamp handling
- Added `zstd = "0.13"` for op-log decompression

## [0.3.2] - 2025-09-30

### ✨ NEW FEATURES
- **Universal Operation Logging (Op-Log)**: Comprehensive operation tracing across all storage backends
  - **Multi-Backend Support**: Captures operations from file://, direct://, s3://, and az:// backends
  - **Automatic Compression**: All op-logs are zstd-compressed (.tsv.zst format)
  - **Detailed Metrics**: Records timestamps, durations, sizes, errors for every operation
  - **CLI Integration**: New `--op-log <PATH>` global flag for all commands
  - **Replay Ready**: TSV format designed for future workload replay functionality (planned v0.4.0)

- **Enhanced Logging System**: Unified tracing framework with pass-through to s3dlio
  - **Verbosity Levels**: 
    - No flags: Warnings and errors only
    - `-v`: INFO level for io-bench, minimal s3dlio output
    - `-vv`: DEBUG level for io-bench, INFO level for s3dlio (operational details)
    - `-vvv`: TRACE level for io-bench, DEBUG level for s3dlio (full debugging)
  - **Cascading Levels**: io-bench verbosity automatically configures s3dlio logging (one level less)
  - **Unified Framework**: Both crates use `tracing` crate for consistent log formatting

### 🚀 DEPENDENCY UPDATES
- **s3dlio**: Upgraded to v0.8.12 (from git tag, no local patches)
  - Universal op-log support across all backends
  - Migration from `log` to `tracing` crate
  - Removed aws-smithy-http-client patch (no longer needed)
  - Operation logger API: `init_op_logger()`, `finalize_op_logger()`, `global_logger()`

### 📊 OPERATION LOGGING FORMAT
```tsv
idx  thread  op  client_id  n_objects  bytes  endpoint  file  error  start  first_byte  end  duration_ns
```
- Compressed with zstd (typically 10-20x reduction)
- Compatible with standard TSV tools after decompression
- Design documented in `docs/OP_LOG_REPLAY_DESIGN.md` for future replay feature

### 🔧 TECHNICAL IMPROVEMENTS
- **Build System**: Simplified dependency management, removed local patches
- **Logging Architecture**: EnvFilter configuration for per-crate log levels
- **ObjectStore Integration**: Enhanced with logger support via `store_for_uri_with_logger()`
- **All Operations Instrumented**: GET, PUT, DELETE, LIST, STAT operations support op-logging

### 📖 DOCUMENTATION
- Added `docs/OP_LOG_REPLAY_DESIGN.md`: Complete replay feature specification for v0.4.0
- Updated `.github/copilot-instructions.md`: Added ripgrep (rg) usage guide and op-log examples
- Enhanced CLI help text: Clarified compression behavior and use cases

### 🧪 TESTING
- Validated op-log capture across all backends (file://, s3://, az://)
- Verified zstd compression and decompression workflow
- Tested logging level pass-through with -v, -vv, -vvv flags
- Confirmed 59K+ operations captured from 5-second workload (9MB compressed)

## [0.3.1] - 2025-09-30

### ✨ NEW FEATURES
- **Interactive Progress Bars**: Professional progress visualization for all operations
  - **Time-based Progress**: Smooth animated progress bars for timed workloads with elapsed/remaining time
  - **Operation-based Progress**: Object count progress tracking for GET, PUT, DELETE commands
  - **Smart Contextual Messages**: Dynamic progress messages showing concurrency, data sizes, and completion status
  - **ETA Calculations**: Estimated time remaining for all operations
  - **Async-friendly Design**: Non-blocking progress updates every 100ms

### 🚀 USER EXPERIENCE IMPROVEMENTS
- **Enhanced Default Output**: Improved feedback without verbose flags
  - **Preparation Status**: Clear indication of workload setup and GET pattern resolution
  - **Execution Progress**: Real-time visual feedback during operation execution
  - **Completion Summary**: Informative completion messages with performance data
- **Better Visual Feedback**: Colored progress bars with professional styling
- **Informative Progress Messages**: Context-aware messages showing worker counts and data transfer amounts

### 🔧 TECHNICAL ENHANCEMENTS
- **indicatif Integration**: Added professional progress bar library for smooth animations
- **Concurrent Progress Tracking**: Thread-safe progress updates using `Arc<ProgressBar>`
- **Minimal Performance Impact**: Efficient 100ms update intervals for smooth user experience
- **Cross-terminal Compatibility**: Progress bars work across different terminal widths and configurations

### 📦 DEPENDENCY UPDATES
- Added `indicatif = "0.17"` for progress bar functionality

## [0.3.0] - 2025-09-30

### 🎉 MAJOR RELEASE: Complete Multi-Backend & Naming Transformation

This release represents a fundamental transformation from an S3-only tool to a comprehensive multi-protocol I/O benchmarking suite.

### 💥 BREAKING CHANGES
- **Project Renamed**: `s3-bench` → `io-bench` (reflects multi-protocol nature)
- **Binary Names Changed**:
  - `s3-bench` → `io-bench`
  - `s3bench-agent` → `iobench-agent`
  - `s3bench-ctl` → `iobench-ctl`
  - `s3bench-run` → `iobench-run`
- **gRPC Protocol**: Package renamed from `s3bench` to `iobench`

### ✨ NEW FEATURES
- **Complete Multi-Backend Support**: Full CLI and workload support for all 4 storage backends
  - File system (`file://`) - Local filesystem operations
  - Direct I/O (`direct://`) - High-performance direct I/O
  - S3 (`s3://`) - Amazon S3 and S3-compatible storage
  - Azure Blob (`az://`) - Microsoft Azure Blob Storage
- **Unified CLI Interface**: All commands work consistently across all backends
- **Enhanced Performance Metrics**: Microsecond precision latency measurements
- **Azure Blob Storage**: Full support with proper authentication and URI format
- **Advanced Glob Patterns**: Cross-backend wildcard support for GET operations

### 🚀 MAJOR IMPROVEMENTS
- **Phase 1: CLI Migration** - Complete transition from S3-specific to multi-backend commands
- **Phase 2: Dependency Analysis** - Thorough investigation and documentation of s3dlio requirements
- **Phase 3: Backend Validation** - Systematic testing and validation of all storage backends
- **Microsecond Precision**: Enhanced HDR histogram metrics with microsecond-level accuracy
- **URI Validation**: Comprehensive URI format validation across all backends
- **Error Handling**: Improved error messages and backend-specific guidance

### 🔧 TECHNICAL ENHANCEMENTS
- **ObjectStore Abstraction**: Complete migration to s3dlio ObjectStore trait
- **Glob Pattern Matching**: Fixed URI scheme normalization for pattern matching
- **Azure Authentication**: Proper support for Azure storage account keys and CLI authentication
- **Configuration System**: Enhanced YAML configuration with `target` URI support
- **Distributed gRPC**: Validated and tested distributed agent/controller functionality

### 🐛 BUG FIXES
- **Direct I/O Glob Patterns**: Fixed glob pattern matching for direct:// backend operations
- **Azure URI Format**: Corrected URI format to `az://STORAGE_ACCOUNT/CONTAINER/`
- **URI Scheme Normalization**: Resolved cross-scheme pattern matching issues
- **Build Dependencies**: Documented and resolved aws-smithy-http-client patch requirements

### 📚 DOCUMENTATION
- **Azure Setup Guide**: Comprehensive Azure Blob Storage configuration documentation
- **Multi-Backend Examples**: Updated all examples to showcase 4-backend support
- **Configuration Samples**: Enhanced config examples with environment variable usage
- **Phase Implementation Reports**: Detailed documentation of migration phases
- **Backend-Specific Guides**: Tailored setup instructions for each storage backend

### 🧪 TESTING & VALIDATION
- **Backend Test Suite**: Created comprehensive test configurations for all backends
- **Performance Validation**: Verified performance characteristics across all storage types
- **Integration Tests**: Updated gRPC integration tests with new binary names
- **Azure Connectivity**: Validated real-world Azure Blob Storage operations

### 📊 PERFORMANCE CHARACTERISTICS
- **File Backend**: 25k+ ops/s, sub-millisecond latencies
- **Direct I/O Backend**: 10+ MB/s throughput, ~100ms latencies
- **Azure Blob Storage**: 2-3 ops/s, ~700ms latencies (network dependent)
- **Cross-Backend Workloads**: Tested mixed workload scenarios

### 🔧 INTERNAL CHANGES
- **Protobuf Schema**: Renamed from `s3bench.proto` to `iobench.proto`
- **Module Structure**: Updated all internal references and imports
- **Binary Generation**: Updated build system for new binary names
- **Test Framework**: Adapted integration tests for renamed binaries

### 📋 MIGRATION GUIDE
For users upgrading from 0.2.x:
1. Update binary names in scripts and automation
2. Review Azure URI format if using Azure backend
3. Update any gRPC integrations to use `iobench` package
4. Verify environment variables for Azure authentication

### 🎯 NEXT STEPS
- Complete S3 backend validation when access becomes available
- Enhanced distributed testing capabilities
- Performance optimization across backends
- Additional storage backend support

---

## [0.2.3] - Previous Release

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