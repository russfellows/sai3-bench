# Changelog

All notable changes to sai3-bench will be documented in this file.

## [0.6.7] - 2025-10-15

### 📊 TSV Export Enhancement: Aggregate Summary Rows

**Added aggregate summary rows** to TSV export for easy analysis of total GET/PUT/META operations across all size buckets.

#### Features

- **Aggregate Rows**: Each operation type (GET, PUT, META) now includes an "ALL" summary row
  - Combines statistics across all size buckets
  - Proper HDR histogram merging for accurate latency percentiles
  - Sum of operations count and throughput across all buckets
  - Naturally sorted to end of output (META=97, GET=98, PUT=99)
  
#### What's in the Aggregate Rows

Each "ALL" row includes:
- **Count**: Total operations across all size buckets
- **Throughput**: Total MiB/s (sum of all buckets)
- **Latency Statistics**: Properly merged HDR histogram values
  - Mean, P50, P90, P95, P99, Max latencies
  - Uses HDR histogram merge operation for accuracy
- **Average Bytes**: Weighted average object size
- **Bucket Index**: Operation-specific (META=97, GET=98, PUT=99) for natural sorting

#### Example TSV Output

```
operation  size_bucket   bucket_idx  mean_us  p50_us  p90_us   p95_us   throughput_mibps  count
GET        1B-8KiB       1           226.21   204.00  328.00   397.00   3.68              19022
GET        8KiB-64KiB    2           419.28   386.00  574.00   690.00   176.34            14258
GET        64KiB-512KiB  3           863.18   778.00  1260.00  1423.00  946.67            9568
GET        ALL           98          432.69   338.00  837.00   1038.00  1126.69           42848
PUT        8KiB-64KiB    2           137.55   120.00  186.00   222.00   14.93             4828
PUT        ALL           99          137.55   120.00  186.00   222.00   14.93             4828
```

Note: Aggregate rows use operation-specific bucket_idx (META=97, GET=98, PUT=99) ensuring they naturally sort to the end after per-bucket rows (0-8).

#### Benefits

1. **Quick Analysis**: Total GET/PUT performance at a glance without manual summation
2. **Accurate Statistics**: HDR histogram merging ensures correct latency percentiles
3. **Natural Sorting**: Operation-specific bucket_idx (META=97, GET=98, PUT=99) places aggregates at end
4. **Machine Readable**: Easy to parse and filter by bucket_idx
5. **Backward Compatible**: Existing per-bucket rows unchanged

#### Implementation Details

- New method: `TsvExporter::collect_aggregate_row()`
- Uses `OpHists::combined_histogram()` for HDR merging
- Aggregates `SizeBins` data for accurate byte counts
- Operation-specific bucket indices: META=97, GET=98, PUT=99
- All rows sorted by bucket_idx for natural ordering

---

## [0.6.6] - 2025-10-11

### ⚠️ BREAKING CHANGE: Command Structure Restructured

**Utility commands now nested under `util` subcommand** to emphasize core workload execution features (run/replay). This change improves tool clarity and aligns with its primary purpose as a benchmarking suite.

#### Breaking Changes
- **All utility commands require `util` prefix**:
  - ❌ OLD: `sai3-bench health --uri "s3://bucket/"`
  - ✅ NEW: `sai3-bench util health --uri "s3://bucket/"`
  
- **Affected commands**: health, list, stat, get, put, delete
- **Unaffected commands**: run, replay (these are now prominently featured)
- **Error message**: If you try to use old syntax (e.g., `sai3-bench health`), you'll get "unrecognized subcommand" error. Simply add `util` before the command name.

#### Command Structure
```
OLD Structure:
  sai3-bench {health|list|stat|get|delete|put|run|replay}

NEW Structure (v0.6.6):
  sai3-bench {run|replay|util}
    util subcommand: {health|list|stat|get|put|delete}
```

#### Benefits
1. **Core features prominently displayed**: `run` and `replay` now appear first
2. **Cleaner help output**: Only 3 top-level commands instead of 8
3. **Clear tool purpose**: Emphasizes benchmarking/workload execution
4. **Better organization**: Utility operations clearly separated from core functionality

#### Migration Guide
Update all utility command invocations:
```bash
# Health checks
sai3-bench util health --uri "file:///tmp/test/"
sai3-bench util health --uri "s3://bucket/"
sai3-bench util health --uri "az://account/container/"
sai3-bench util health --uri "gs://bucket/"

# List operations
sai3-bench util list --uri "s3://bucket/prefix/"

# Get/Put/Delete operations
sai3-bench util get --uri "s3://bucket/*" --jobs 8
sai3-bench util put --uri "file:///tmp/" --object-size 1024 --objects 100
sai3-bench util delete --uri "s3://bucket/prefix/*"
```

#### Note on Utility Commands
For comprehensive storage CLI operations, consider using `s3-cli` from the [s3dlio package](https://github.com/russfellows/s3dlio). The utility commands in sai3-bench are provided as convenience helpers for quick testing and validation.

---

## [0.6.5] - 2025-10-11

### 🔍 Configuration Validation & Documentation Enhancements

**Config Validation with --dry-run**: Added comprehensive configuration validation and test summary display before execution. Users can now verify YAML syntax, validate configuration structure, and preview test details without running the workload.

#### Core Features
- **`--dry-run` Flag**: Parse and validate config with detailed summary display
  - ✅ YAML syntax validation with clear error messages
  - ✅ Configuration structure and required fields verification
  - ✅ Test summary with duration, concurrency, backend detection
  - ✅ Prepare phase details (size distributions, fill patterns, dedup/compress factors)
  - ✅ Workload operations with weight percentages and concurrency overrides
  - ✅ RangeEngine configuration display (when enabled)
  
- **Data Generation Documentation**: Created comprehensive DATA_GENERATION.md guide
  - Fill pattern recommendations: **Use `fill: random` for realistic storage testing**
  - Clear explanation: `fill: zero` produces artificially high compression ratios
  - Deduplication and compression factor documentation with examples
  - Implementation details showing exact Rust code paths

- **Documentation Cleanup**: Removed outdated WARP_PARITY_STATUS.md
  - File was feature-planning focused and out of date
  - References removed from README.md and docs/README.md

#### Usage Examples
```bash
# Validate config before running
sai3-bench run --config my-workload.yaml --dry-run

# Example output shows:
# - Test configuration (duration, concurrency, backend)
# - Prepare phase details (if configured)
# - Workload operations with percentages
# - Configuration validation status
```

#### Files Changed
- `src/main.rs` - Added `--dry-run` flag and `display_config_summary()` function
- `docs/DATA_GENERATION.md` - NEW: Comprehensive data generation guide (165 lines)
- `docs/CONFIG_SYNTAX.md` - Added configuration validation section with examples
- `README.md` - Added --dry-run to Quick Start, updated documentation links
- `docs/README.md` - Updated to v0.6.5, added DATA_GENERATION.md reference
- `docs/WARP_PARITY_STATUS.md` - REMOVED (outdated)

#### Testing
- Validated with `tests/configs/mixed.yaml` - Basic GET/PUT operations
- Validated with `tests/configs/size_distributions_test.yaml` - Prepare phase with distributions
- Validated with `tests/configs/per_op_concurrency_test.yaml` - Per-operation concurrency
- Validated with `tests/configs/dedupe_compress_test.yaml` - Random fill with dedup/compress
- Error handling verified with invalid YAML and incomplete configs

## [0.6.4] - 2025-10-11

### 🎯 Enhanced Output with HDR Histogram Merging

**Automatic Results Directories & Consolidated Metrics**: Implemented automatic results directory creation for both single-node and distributed workloads, with HDR histogram merging for mathematically accurate aggregate metrics across multiple agents.

#### Core Features
- **Timestamped Results Directories**: Automatic creation of `sai3-YYYYMMDD-HHMM-{test_name}/` directories
- **Complete Capture**: config.yaml, console.log, metadata.json, results.tsv in every results directory
- **Distributed Results Collection**: Per-agent results in `agents/{id}/` subdirectories with metadata
- **HDR Histogram Merging**: Mathematically accurate percentile aggregation across multiple agents

#### Results Directory Structure
```
sai3-YYYYMMDD-HHMM-{test_name}/
├── config.yaml                    # Controller's workload config
├── console.log                    # Complete execution log
├── metadata.json                  # Test metadata (distributed: true/false)
├── results.tsv                    # Single-node OR consolidated (merged histograms)
└── agents/                        # Only in distributed mode
    ├── agent-1/
    │   ├── metadata.json          # Agent-specific metadata
    │   ├── results.tsv           # Agent-1 individual results
    │   └── agent_local_path.txt  # Points to agent's /tmp/ directory
    └── agent-2/...
```

#### HDR Histogram Merging (Critical Enhancement)
**Problem**: Percentiles cannot be simply averaged across agents. If agent-1 has p95=278µs and agent-2 has p95=280µs, the aggregate p95 is NOT necessarily 279µs.

**Solution**: Proper histogram merging via `hdrhistogram` library:
- Agent serializes 9 size-bucketed histograms per operation type (GET/PUT/META)
- Controller deserializes and merges histograms mathematically
- Consolidated results.tsv contains accurate aggregate percentiles
- Per-agent results preserved for debugging

**Performance Impact**: Negligible overhead (~1ms agent serialization, ~3ms controller merge for 2 agents)

#### gRPC Protocol Extension
Extended `WorkloadSummary` message with histogram fields:
- `histogram_get`, `histogram_put`, `histogram_meta` (bytes) - V2 binary format
- Efficient transfer: Few KB per agent
- Full backward compatibility maintained

#### Files Changed
- `proto/iobench.proto` - Extended with histogram fields
- `src/bin/agent.rs` - Histogram serialization (+109 lines)
- `src/bin/controller.rs` - Histogram merging + consolidated TSV generation (+250 lines)
- `src/pb/iobench.rs` - Auto-generated protobuf code

#### Testing
- Comprehensive test suite with 4 test scenarios
- Verified 2-agent, 4-agent, and multi-size workloads
- All tests passing with exact count verification
- Histogram accuracy validated (proper merging, not averaging)

**Test Scripts**:
- `tests/verify_v0.6.4.sh` - Quick 2-agent verification
- `tests/test_comprehensive_v0.6.4.sh` - Full test suite
- `tests/configs/distributed_mixed_test.yaml` - Mixed workload config

#### Migration Notes
No breaking changes - fully backward compatible. Results directories are created automatically for all workload executions.

## [0.6.3] - 2025-10-10

### 🎯 Critical Performance Fix: s3dlio v0.9.6 Upgrade

**Resolves 20-25% Performance Regression**: Upgraded to s3dlio v0.9.6 which disables RangeEngine by default across all backends, eliminating HEAD request overhead that caused significant slowdowns in typical workloads.

#### Core Library Upgrade
- **s3dlio**: v0.9.5 → **v0.9.6** (git tag)
- **s3dlio-oplog**: v0.9.5 → **v0.9.6** (git tag)

#### Performance Impact
**Problem Identified**: s3dlio v0.9.5 enabled RangeEngine by default, which added a HEAD/STAT request before every GET operation to determine object size. This caused:
- **20-25% throughput degradation** for typical workloads with mixed object sizes
- **60% more requests** for small-object workloads (HEAD + GET vs just GET)
- HEAD overhead exceeds RangeEngine benefits for objects < 64 MiB

**Solution**: s3dlio v0.9.6 disables RangeEngine by default for ALL backends:
- `AzureConfig::default()`: `enable_range_engine = false`
- `GcsConfig::default()`: `enable_range_engine = false`
- `FileSystemConfig::default()`: `enable_range_engine = false`
- `DirectIOConfig::default()`: `enable_range_engine = false`

**Performance Validation** (GCS with 64 MiB objects):
- **Disabled (v0.9.6 default)**: 53.85 MiB/s GET throughput
- **Enabled (explicit opt-in)**: 54.47 MiB/s GET throughput (+1.2%)
- **Conclusion**: Minimal benefit when network bandwidth is the bottleneck

#### Configuration Changes
**Default Behavior** (v0.6.3):
- RangeEngine **DISABLED** by default (via s3dlio v0.9.6)
- Optimal for typical workloads with mixed object sizes
- Single GET request per operation (no HEAD overhead)

**Explicit Opt-In** (for large-file workloads ≥ 64 MiB):
```yaml
range_engine:
  enabled: true
  min_split_size: 16777216  # 16 MiB threshold
  chunk_size: 67108864      # 64 MiB chunks
  max_concurrent_ranges: 16
```

#### Enhanced Logging
**RangeEngine Status Visibility**: Added clear logging for all backends:
```
INFO sai3_bench: RangeEngine DISABLED for Google Cloud Storage backend (default for optimal performance)
INFO sai3_bench: RangeEngine ENABLED for Google Cloud Storage backend - files >= 16 MiB
```

**Applies to**: File, DirectIO, S3, Azure, and GCS backends

#### Testing & Validation
**Comprehensive Testing**:
- ✅ File backend: Confirmed disabled by default (940.83 MiB/s GET)
- ✅ GCS with 1 MiB objects: 36.83 ops/s baseline performance
- ✅ GCS with 64 MiB objects: 53.85 MiB/s disabled vs 54.47 MiB/s enabled
- ✅ Network bandwidth validation: Real GCS testing over 500 Mb/s connection

**Key Insight**: RangeEngine provides benefit only when:
1. Objects are large (≥ 64 MiB)
2. Network bandwidth is NOT the bottleneck (10+ Gbps)
3. Parallel range downloads can overcome other bottlenecks

For typical cloud storage workloads, disabled is optimal.

#### Breaking Changes
**None for most users**: Default behavior now faster for typical workloads.

**Large-file workloads only**: Users relying on automatic RangeEngine for large files must explicitly enable it in configuration.

#### Modified Files
- `Cargo.toml`: Updated s3dlio dependencies to v0.9.6
- `Cargo.lock`: Locked to s3dlio v0.9.6 commit hash
- `src/config.rs`: Updated documentation for disabled default
- `src/main.rs`: Enhanced RangeEngine status logging for all backends

---

## [0.6.1] - 2025-10-10

### 🚀 Major Upgrade: s3dlio v0.9.4 with RangeEngine

**Breaking Through Performance Limits**: Upgraded to s3dlio v0.9.4 with RangeEngine support, providing 30-50% throughput improvements for large files on network backends.

#### Core Library Upgrade
- **s3dlio**: v0.8.22 (git main) → **v0.9.4** (stable git tag)
- **s3dlio-oplog**: v0.8.22 (git main) → **v0.9.4** (stable git tag)
- **Pinned to stable release**: Using git tag instead of branch for production stability

#### RangeEngine Technology
**What is RangeEngine?**: Introduced in s3dlio v0.9.3, RangeEngine dramatically improves download performance for large files by using concurrent byte-range requests. Instead of downloading a 128MB file sequentially, RangeEngine downloads it in parallel 64MB chunks.

**Performance Gains** (from comprehensive testing):
- **Google Cloud Storage**: 45.26 ops/s, 173ms mean latency
- **Azure Blob Storage**: 8.46 ops/s, 912ms mean latency  
- **GCS vs Azure**: **5.3x faster** with RangeEngine on GCS
- **Activation**: Automatic for files ≥ 4MB
- **Scaling**: 128MB files use 2 concurrent ranges, 256MB use 4 ranges

**Key Findings**:
- ✅ 8MB files: 1 range activation confirmed on Azure & GCS
- ✅ 128MB files: 2 ranges activation confirmed on Azure & GCS
- ✅ File backend: RangeEngine activates but shows limited benefit (already fast local I/O)
- ✅ Network backends: 30-50% throughput improvement for large files

#### Agent Modernization: Universal Backend Support
**Problem**: Agent previously used S3-specific `s3_utils` functions, limiting it to S3 backend only.

**Solution**: Complete migration to universal `ObjectStore` pattern:
- **Before**: `get_object()`, `put_object_async()` from s3_utils (S3-only)
- **After**: `store_for_uri()` with ObjectStore trait (all backends)
- **Benefit**: Agent now supports file://, direct://, s3://, az://, gs:// URIs
- **Automatic RangeEngine**: All network operations benefit from RangeEngine

**Modified**: `src/bin/agent.rs`
- Removed: S3-specific imports
- Added: `store_for_uri()` for universal backend support
- Refactored: `run_get()` and `run_put()` to use ObjectStore pattern
- New helper: `list_keys_for_uri()` for multi-backend listing

#### API Compatibility Fix
**s3dlio v0.9.4 API Change**: `ObjectStore::get()` now returns `bytes::Bytes` instead of `Vec<u8>`.

**Fix**: Added `.to_vec()` conversions in workload operations:
- `src/workload.rs`: `get_object_multi_backend()` line 513
- `src/workload.rs`: `get_object_no_log()` lines 584-585

**Performance Impact**: Minimal - single memory copy, maintains compatibility.

#### RangeEngine Configuration
**New**: `RangeEngineConfig` structure in `src/config.rs` for future configurability:
```yaml
# Optional in workload configs (uses s3dlio defaults if omitted)
range_engine:
  enabled: true
  chunk_size: 67108864      # 64 MB
  max_concurrent_ranges: 32
  min_split_size: 4194304   # 4 MB threshold
  range_timeout_secs: 30
```

**Current Status**: Documentary only - s3dlio uses optimal defaults automatically. Configuration exposed for advanced tuning if needed.

#### TSV Output Improvement
**Enhancement**: TSV benchmark results now sorted by `bucket_idx` for better readability.

**Before**: Rows grouped by operation (GET, PUT, META) with mixed bucket order  
**After**: All rows sorted by bucket_idx (0 → 8), making size-bucket analysis intuitive

**Example**:
```
operation  size_bucket      bucket_idx  mean_us  ...
GET        1B-8KiB          1          2347.39   ...
PUT        1B-8KiB          1          250.55    ...
GET        64KiB-512KiB     3          13419.45  ...
PUT        64KiB-512KiB     3          360.61    ...
```

**Modified**: `src/tsv_export.rs`

### 📊 Comprehensive Testing

#### Backend Coverage
- ✅ **File backend**: 11,188 ops/s, 19GB workload tested
- ✅ **Azure Blob Storage**: 8.46 ops/s, RangeEngine confirmed (1-4 ranges)
- ✅ **Google Cloud Storage**: 45.26 ops/s, RangeEngine confirmed (1-2 ranges)

#### Quality Metrics
- ✅ Unit tests: 18/18 passing
- ✅ Integration tests: 1/1 passing
- ✅ Distributed workload: Tested and working
- ✅ No regressions detected
- ✅ Zero compilation warnings

### 📚 New Documentation
- **`docs/S3DLIO_V0.9.4_MIGRATION.md`**: Complete 400+ line migration guide
  - All changes explained with examples
  - Deprecation analysis
  - RangeEngine technical deep-dive
  - Performance expectations and tuning
- **`docs/S3DLIO_V0.9.4_TEST_RESULTS.md`**: Comprehensive test results
  - Per-backend performance metrics
  - RangeEngine validation details
  - Latency percentiles
- **`docs/S3DLIO_V0.9.4_TESTING_PLAN.md`**: Testing methodology and matrix
- **`tests/configs/README.md`**: Complete guide to all test configurations
  - Quick reference table
  - Usage examples
  - Environment setup
  - Troubleshooting

### 🧪 New Test Configurations
- **`tests/configs/gcs_rangeengine_test.yaml`**: GCS RangeEngine performance test
- **`tests/configs/azure_rangeengine_test.yaml`**: Azure RangeEngine test (full)
- **`tests/configs/azure_rangeengine_simple.yaml`**: Azure minimal example
- **`tests/configs/azure_rangeengine_disabled_test.yaml`**: Baseline comparison
- **`tests/configs/range_engine_test.yaml`**: File backend RangeEngine demo

### 🔧 Technical Details

#### Files Modified (13 total)
**Core Code** (5 files):
- `Cargo.toml` - Dependencies and version
- `src/workload.rs` - Bytes compatibility
- `src/bin/agent.rs` - ObjectStore migration
- `src/config.rs` - RangeEngine config structure
- `src/tsv_export.rs` - Output sorting

**Test Configs** (6 files):
- 3 modified: Fixed SizeSpec syntax
- 3 created: New RangeEngine examples

**Documentation** (3 files):
- All new comprehensive guides

#### Deprecation Status
**Clean**: sai3-bench does NOT use any deprecated s3dlio APIs:
- ✅ `get_object()` - Still supported
- ✅ `put_object_async()` - Still supported
- ✅ `parse_s3_uri()` - Still supported
- ⚠️ `list_objects()` - Deprecated, but we don't use it

### 🎯 Performance Summary

| Backend | Throughput | Latency | RangeEngine | Relative |
|---------|------------|---------|-------------|----------|
| **GCS** | 45.26 ops/s | 173ms | ✅ 1-2 ranges | **5.3x faster** |
| **File** | 11,188 ops/s | <1ms | ✅ Activated | Baseline |
| **Azure** | 8.46 ops/s | 912ms | ✅ 1-4 ranges | Expected |

### 🚀 Upgrade Path

**Backward Compatible**: No breaking changes to user-facing APIs.

**Migration Steps**:
1. Update dependency: `sai3-bench = "0.6.1"`
2. Rebuild: `cargo build --release`
3. Test: `sai3-bench run --config your-config.yaml`
4. Optional: Review RangeEngine logs with `-vv` flag

**See Also**: `docs/S3DLIO_V0.9.4_MIGRATION.md` for detailed migration guide.

### 🙏 Acknowledgments
- s3dlio maintainers for RangeEngine implementation
- Testing on Google Cloud Platform and Azure

---

## [0.6.0] - 2025-10-07

### 🚀 Major Features

#### Distributed Multi-Host Workload Execution
**New Capability**: Run coordinated benchmarks across multiple agent nodes using gRPC.

**Problem**: Previous versions could only run single-node benchmarks. Testing distributed systems at scale required manual coordination.

**Solution**: Added complete distributed workload infrastructure:
- **New gRPC RPC**: `RunWorkload` - Controller sends config to agents for execution
- **Per-Agent Path Isolation**: Each agent operates in isolated subdirectory (e.g., `agent-1/`, `agent-2/`)
- **Coordinated Start Time**: All agents begin workload simultaneously (configurable delay)
- **Result Aggregation**: Controller collects and displays per-agent and aggregate statistics
- **Shared vs Local Storage Detection**: Automatic handling based on URI scheme
  - **Shared storage** (S3/GCS/Azure): All agents use same prepared dataset
  - **Local storage** (file://): Each agent prepares own isolated dataset

**New Components**:
- `sai3bench-ctl run` subcommand - Distributed workload orchestration
- `RunWorkloadRequest` protobuf message - Config distribution with agent metadata
- `WorkloadSummary` protobuf message - Per-agent execution results
- `Config::apply_agent_prefix()` - Path isolation with storage-aware prepare handling

**Usage**:
```bash
# Start agents on multiple hosts
sai3bench-agent --listen 0.0.0.0:7761  # On host 1
sai3bench-agent --listen 0.0.0.0:7761  # On host 2

# Run distributed workload from controller
sai3bench-ctl --insecure --agents host1:7761,host2:7761 \
    run --config workload.yaml --start-delay 2

# Output shows per-agent and aggregate results
=== Distributed Results ===
Total agents: 2

--- Agent: agent-1 ---
  Wall time: 3.03s
  Total ops: 30966 (10215.21 ops/s)
  Total bytes: 25.67 MB (8.47 MiB/s)
  GET: 21602 ops, 21.10 MB, mean: 225µs, p95: 315µs
  PUT: 9364 ops, 4.57 MB, mean: 109µs, p95: 155µs

--- Agent: agent-2 ---
  Wall time: 3.03s
  Total ops: 30868 (10179.66 ops/s)
  Total bytes: 25.63 MB (8.45 MiB/s)
  GET: 21630 ops, 21.12 MB, mean: 225µs, p95: 319µs
  PUT: 9238 ops, 4.51 MB, mean: 108µs, p95: 153µs

--- Aggregate ---
  Total ops: 61834
  Total bytes: 51.30 MB
  Combined throughput: 20394.87 ops/s
```

**Controller Flags**:
- `--config <file>` - YAML workload configuration file
- `--path-template <template>` - Agent path prefix template (default: `agent-{id}/`)
- `--agent-ids <list>` - Custom agent identifiers (default: `agent-1`, `agent-2`, ...)
- `--start-delay <seconds>` - Coordinated start delay (default: 2)
- `--shared-prepare` - Override auto-detected storage mode

**Technical Details**:
- Auto-detection based on URI scheme:
  - Shared: `s3://`, `az://`, `gs://`
  - Local: `file://`, `direct://`
- Prepare config modification only for local storage
- Target URI always gets agent prefix for workload isolation
- Operation paths remain relative (resolve against modified target)
- Each agent executes prepare phase independently if needed
- Coordinated start time uses nanosecond-precision timestamp

**Testing**:
- `tests/distributed_local_test.sh` - Integration test with file:// backend
- Verified with 2 agents creating isolated datasets (10 prepare objects each)
- Confirmed path isolation (agent-1/ and agent-2/ subdirectories)
- Validated result aggregation and throughput calculations

**Documentation**:
- `docs/V0.6.0_DISTRIBUTED_DESIGN.md` - Complete design document
- `docs/USAGE.md` - Updated with distributed examples (TBD)

### 🔧 Technical Changes
- Extended `proto/iobench.proto` with `RunWorkload` RPC
- Added `shared_storage` field to `RunWorkloadRequest`
- Implemented `Agent::run_workload()` handler with prepare phase
- Added storage detection helpers in controller
- Enhanced logging with `-v`/`-vv` flags in controller and agent
- Fixed prepare `base_uri` rewriting for path isolation

### 📊 Performance Characteristics
- Tested with 2 agents on localhost (file:// backend)
- Each agent: ~10,000 ops/s, ~25 MB in 3 seconds
- Combined throughput: ~20,000 ops/s
- No synchronization overhead (agents run independently)
- Coordinated start ensures fair comparison

## [0.5.9] - 2025-10-07

### ✨ Improvements

#### Output Organization & Clarity
**Problem**: Console output had duplication and missing metrics:
- Latency histograms shown separately, then repeated in Results section
- TSV export message shown twice (with and without checkmark)
- Mean/average latency was missing from Results output (only p50/p95/p99 shown)

**Solution**: Consolidated and enhanced Results output:
- **Added mean latency**: Now shows `Latency mean: Xµs, p50: Xµs, p95: Xµs, p99: Xµs` for all operations
- **Removed duplicate histogram**: Detailed histograms removed from workload output (was redundant)
- **Removed duplicate TSV message**: Single export confirmation with checkmark emoji
- **Cleaner flow**: Results section now shows all key metrics in one organized place

#### Branding Consistency
**Problem**: Some references still used legacy "io-bench" terminology instead of "sai3-bench".

**Solution**: Fixed all remaining references:
- Updated CLI help examples in `sai3-bench put`, `run`, and `replay` commands
- Updated code comments in replay.rs and test files
- Consistent branding across all user-facing messages

### 🔧 Technical Changes
- Added `mean_us` field to `OpAgg` struct
- Calculate mean from HDR histograms using `hist.mean()`
- Display mean alongside percentiles in Results output
- Code comment updates for branding consistency

### 📊 Example Output
```
=== Results ===
Wall time: 3.03s
Total ops: 71317
Total bytes: 102281216 (97.54 MB)
Throughput: 23507.30 ops/s

GET operations:
  Ops: 42750 (14091.13 ops/s)
  Bytes: 43776000 (41.75 MB)
  Throughput: 13.76 MiB/s
  Latency mean: 181µs, p50: 175µs, p95: 273µs, p99: 338µs

PUT operations:
  Ops: 28567 (9416.17 ops/s)
  Bytes: 58505216 (55.79 MB)
  Throughput: 18.39 MiB/s
  Latency mean: 92µs, p50: 84µs, p95: 143µs, p99: 193µs

✅ TSV results exported to: sai3bench-2025-10-07-150959-test_mean_output-results.tsv
```

---

## [0.5.8] - 2025-10-07

### 🐛 Bug Fix

#### GCS Pagination Fix (via s3dlio v0.8.22)
**Problem**: Google Cloud Storage (GCS) list and delete operations were limited to 1,000 objects due to missing pagination handling in s3dlio v0.8.21 and earlier.

**Solution**: Updated to s3dlio v0.8.22 which implements proper pagination:
- List operations now retrieve all objects (not just first 1,000)
- Delete operations now remove all matched objects (not just first 1,000)
- Operations process in batches of 1,000 as per GCS API limits
- Affects `list`, `delete`, and glob pattern operations on GCS

**Impact**: 
- **Critical for GCS users with >1,000 objects**: Previous versions silently failed to process beyond first page
- **No impact on other backends**: S3 and Azure already had correct pagination
- **Workload prepare**: Now correctly deletes all objects during cleanup phase
- **DELETE operations**: Now remove all matched objects, not just first 1,000

### 🔧 Technical Changes
- Updated `s3dlio` dependency: v0.8.21 → v0.8.22 (main branch)
- Updated `s3dlio-oplog` dependency: v0.8.21 → v0.8.22 (main branch)
- Using `branch = "main"` until v0.8.22 tag is created in s3dlio repo

### 📚 Recommendation
- **GCS users**: Upgrade immediately if working with >1,000 objects
- **Other users**: Optional upgrade, but recommended for latest fixes

---

## [0.5.7] - 2025-10-07

### 🔥 Critical Bug Fix

#### DELETE Pool Corruption Fixed
**Problem**: In v0.5.6 and earlier, all operations (GET, STAT, DELETE) shared a single object pool. DELETE operations removed objects during execution, causing GET/STAT to fail with 404 errors in mixed workloads.

**Solution**: Implemented automatic separate object pools (MinIO Warp approach):
- **Readonly pool** (`prepared-*.dat`): Used by GET and STAT operations, never deleted
- **Deletable pool** (`deletable-*.dat`): Used by DELETE operations, consumed during test
- **Automatic detection**: Detects mixed workloads (DELETE + GET/STAT) and creates separate pools
- **Pattern rewriting**: Transparently rewrites patterns to route operations to correct pools
- **100% backward compatible**: Single pool created when no DELETE or no GET/STAT operations

Example:
```yaml
workload:
  - op: get
    path: "prepared-*.dat"
    weight: 60
  - op: delete
    path: "prepared-*.dat"  # Auto-rewritten to deletable-*.dat
    weight: 20
  - op: stat
    path: "prepared-*.dat"
    weight: 20
```

Console output:
```
Mixed workload: Using separate object pools (readonly for GET/STAT, deletable for DELETE)
Prepared 100 objects
  50 prepared-*.dat (readonly pool)
  50 deletable-*.dat (consumable pool)
```

**Impact**: Eliminates 404 errors in mixed workloads. All users running DELETE operations should upgrade immediately.

### ✨ New Features

#### Automatic TSV Export with Smart Naming
**Previous behavior**: TSV export required `--results-tsv` flag (easy to forget)

**New behavior**: TSV export is automatic and mandatory with intelligent naming:

```bash
# Automatic timestamp-based naming
sai3-bench run --config test.yaml
  → Creates: sai3bench-2025-10-07-143052-test-results.tsv

# Custom naming
sai3-bench run --config test.yaml --tsv-name my-benchmark
  → Creates: my-benchmark-results.tsv
```

**Filename format**: `sai3bench-YYYY-MM-DD-HHMMSS-<config_basename>-results.tsv`
- Ensures unique files for repeated runs
- Easy identification of which config was used
- Auto-ignored via `.gitignore` pattern

**Breaking change**: `--results-tsv` flag removed (use `--tsv-name` for custom naming)

#### Comprehensive Throughput Reporting
Added MiB/s throughput and per-operation ops/s to console output:

```
=== Results ===
Wall time: 10.10s
Total ops: 313303
Total bytes: 1933875200 (1844.29 MB)
Throughput: 31011.97 ops/s

GET operations:
  Ops: 188855 (18693.61 ops/s)          # NEW: Per-operation ops/s
  Bytes: 1933875200 (1844.29 MB)
  Throughput: 182.55 MiB/s              # NEW: Actual data throughput!
  Latency p50: 390µs, p95: 597µs, p99: 717µs

PUT operations:
  Ops: 7371 (488.29 ops/s)
  Bytes: 481286288 (458.99 MB)
  Throughput: 30.41 MiB/s               # NEW: Write throughput!
  Latency p50: 369µs, p95: 1887µs, p99: 5215µs
```

Formula: `MiB/s = (bytes / 1,048,576) / wall_seconds`

Matches TSV export `throughput_mibps` column for consistency.

### 📝 Configuration Examples

#### New Test Configurations
- `tests/configs/v057_mixed_workload_test.yaml` - Tests automatic pool separation
- `tests/configs/v057_readonly_only_test.yaml` - Tests backward compatibility (single pool)
- `tests/configs/v057_delete_only_test.yaml` - Tests DELETE-only workload
- `tests/configs/comprehensive_test.yaml` - All operations with varied sizes

### 🔧 Technical Changes

**Modified Files**:
- `src/workload.rs`: Pool separation logic, pattern rewriting, prepare enhancement
- `src/main.rs`: TSV auto-export, `--tsv-name` flag, throughput reporting
- `.gitignore`: Added `sai3bench-*.tsv` pattern
- `Cargo.toml`: Version 0.5.6 → 0.5.7

**New Functions**:
- `detect_pool_requirements()`: Analyzes workload for pool needs
- `rewrite_pattern_for_pool()`: Rewrites patterns for correct pool routing

### 📚 Documentation
- Added `docs/V0.5.7_RELEASE_SUMMARY.md` (comprehensive release notes)
- Updated `docs/CHANGELOG.md` (this file)
- Updated `README.md` (brief v0.5.7 mention)

### ⚠️ Breaking Changes
- Removed `--results-tsv` flag (use `--tsv-name` for custom naming, or rely on automatic naming)

### 🎯 Migration Guide

**From v0.5.6**:
1. Remove `--results-tsv` from scripts
2. Optionally add `--tsv-name <basename>` for custom naming
3. Mixed workloads with DELETE now work correctly (no config changes needed!)

**Backward Compatibility**:
- All YAML configs work unchanged
- Readonly-only workloads use single pool (no overhead)
- DELETE-only workloads use single pool (no overhead)
- Mixed workloads automatically get separate pools (fixes 404 errors)

---

## [0.5.6] - 2025-10-07

### � New Features

#### Configurable Post-Prepare Delay
**Problem**: Cloud storage backends (GCS, S3, Azure) have eventual consistency. Objects created during the prepare phase might not be immediately readable, causing 404 errors when the workload starts.

**Solution**: Added YAML-configurable delay between prepare and workload phases:
```yaml
prepare:
  post_prepare_delay: 5  # Wait 5 seconds after creating objects
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 200
```

**Recommendations**:
- Local storage (`file://`, `direct://`): 0 seconds (default)
- Cloud storage (S3, GCS, Azure): 2-5 seconds
- Large object counts (>1000): 5-10 seconds

#### Manual Phase Execution
Added CLI flags for manual control of benchmark phases:

**`--verify`**: Verify prepared objects exist and are accessible
```bash
# Step 1: Prepare objects
sai3-bench run --config test.yaml --prepare-only

# Step 2: Wait for propagation (manual)
sleep 60

# Step 3: Verify all objects are accessible
sai3-bench run --config test.yaml --verify

# Step 4: Run workload (skip prepare since objects exist)
sai3-bench run --config test.yaml --skip-prepare
```

**`--skip-prepare`**: Skip prepare phase, assume objects already exist
```bash
# First run: prepare and run
sai3-bench run --config test.yaml --no-cleanup

# Subsequent runs: skip prepare, reuse existing objects
sai3-bench run --config test.yaml --skip-prepare
```

**Verification Output**:
```
=== Verification Phase ===
Verifying objects at gs://bucket/data/
✓ 200/200 objects verified and accessible at gs://bucket/data/
```

### 🐛 Bug Fixes

#### Cloud Storage Eventual Consistency Protection
**Impact**: Benchmarks on cloud storage failed with "404 Not Found" errors immediately after prepare completed.

**Root Cause**: When prepare created objects very quickly (>300 objects/sec), cloud storage eventual consistency meant objects weren't immediately readable in all zones.

**Example Error**:
```
Error: Failed to get object from URI: gs://bucket/prepared-00000117.dat
Caused by:
    GCS GET failed: HTTP status client error (404 Not Found)
```

**Fix**: 
- Added `post_prepare_delay` field to `PrepareConfig`
- Delay only applies if new objects were created (not if they already existed)
- Users have full control via YAML configuration
- Manual workflow supported via `--verify` flag

**Files Changed**: 
- `src/config.rs` - Added `post_prepare_delay` field
- `src/main.rs` - Added `--verify` and `--skip-prepare` flags, configurable delay
- `src/workload.rs` - Added `verify_prepared_objects()` function

### 📚 Documentation

- Added `examples/cloud-storage-with-delay.yaml` - Complete example with phased execution
- Updated `docs/CONFIG_SYNTAX.md` - Documented `post_prepare_delay` field and recommendations
- Added detailed CLI usage examples for phased workflows

### 🔄 Migration Guide

**Before (v0.5.5)**:
```yaml
prepare:
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 200
```
Objects created, workload started immediately → 404 errors on cloud storage

**After (v0.5.6)**:
```yaml
prepare:
  post_prepare_delay: 3  # Add this line for cloud storage
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 200
```
Objects created, waits 3 seconds, workload starts → no errors

**No changes required for local storage** (`file://`, `direct://`) - default delay is 0.

## [0.5.5] - 2025-10-06

### 🚀 Critical Performance & Correctness Fixes

This release fixes **critical bugs** that prevented DELETE and STAT operations from working with glob patterns, and adds **parallel execution** to prepare and cleanup stages for 30x performance improvement.

### 🐛 Critical Bug Fixes

#### Pattern Resolution for DELETE and STAT Operations
**Problem**: DELETE and STAT operations were completely broken when using glob patterns. They attempted to delete/stat the pattern string itself (e.g., `prepared-*.dat`) instead of resolving it to actual object URIs.

**Impact**: 
- DELETE operations always failed with "No such object" errors
- STAT operations always failed with "No such object" errors
- Mixed workloads (similar to MinIO Warp benchmarks) were impossible to run
- Cloud storage benchmarking was severely limited

**Fix**: Extended pre-resolution logic to handle DELETE and STAT operations:
- All three operations (GET, DELETE, STAT) now pre-resolve glob patterns at startup
- Workers randomly sample from pre-resolved URI lists during execution
- Added `UriSource` tracking for each operation type
- User sees: `Resolving 3 operation patterns (1 GET, 1 DELETE, 1 STAT)...`

**Files Changed**: `src/workload.rs` (lines 903-927, 563-650, 810-850)

**Example**:
```yaml
workload:
  - op: delete
    path: "bench/mixed/prepared-*.dat"  # ✅ Now works - resolves to actual objects
```

See: `docs/PATTERN_RESOLUTION_FIX.md` for complete details

### ⚡ Performance Improvements

#### Parallel Prepare Stage (30x faster)
**Problem**: Prepare stage created objects sequentially (one at a time), making cloud storage preparation extremely slow.

**Impact Before**:
- 20,000 objects took ~35-50 seconds
- Throughput: ~400-600 objects/sec
- Cloud benchmarking required long waits before tests could begin

**Fix**: Implemented parallel execution with 32 workers using semaphore-controlled concurrency:
- Uses same pattern as main workload execution
- Pre-generates URIs and sizes, then executes in parallel with `FuturesUnordered`
- Semaphore limits concurrent tasks to prevent resource exhaustion

**Impact After**:
- Small objects (100 KiB): **13,179-18,677 objects/sec** (30x improvement)
- Large objects (1 MiB): **788 objects/sec at 788 MB/sec throughput**
- 20,000 objects (1 MiB): **25.4 seconds** vs 50+ seconds

**Files Changed**: `src/workload.rs` (lines 135-197)

#### Parallel Cleanup Stage (30x faster)
**Problem**: Cleanup stage deleted objects sequentially (one at a time), causing slow cleanup after benchmarks.

**Fix**: Applied same parallel execution pattern as prepare stage:
- 32 parallel workers with semaphore control
- Graceful error handling (logs warnings but continues on single delete failures)
- Best-effort deletion approach

**Impact**:
- 2,000 objects: **< 0.2 seconds** (was ~3-5 seconds)
- 5,000 objects: **< 0.3 seconds** (was ~7-12 seconds)
- Throughput: **>10,000 objects/sec** for small objects

**Files Changed**: `src/workload.rs` (lines 236-310)

See: `docs/PREPARE_PERFORMANCE_FIX.md` for complete benchmarking results

### 📝 Configuration Syntax Updates

#### Glob Patterns (Not Brace Expansions)
sai3-bench uses **glob patterns with wildcards**, not bash-style brace expansions:

**Correct** ✅:
```yaml
workload:
  - op: get
    path: "bench/mixed/prepared-*.dat"  # Glob pattern with wildcard
```

**Incorrect** ❌:
```yaml
workload:
  - op: get
    path: "bench/mixed/obj_{00000..19999}"  # Brace expansion NOT supported
```

#### Object Naming in Prepare Stage
Prepare stage creates objects with this naming pattern:
- Format: `prepared-NNNNNNNN.dat` (8-digit zero-padded with `.dat` extension)
- Example: `prepared-00000000.dat`, `prepared-00000001.dat`, etc.

Match your workload patterns accordingly:
```yaml
prepare:
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 1000

workload:
  - op: get
    path: "data/prepared-*.dat"  # ✅ Matches prepare naming
```

### 📚 Documentation Improvements

#### New Documentation
- `docs/PATTERN_RESOLUTION_FIX.md` - Complete guide to pattern resolution fix
- `docs/PREPARE_PERFORMANCE_FIX.md` - Performance improvements with benchmarks (updated for cleanup)
- `examples/README.md` - Comprehensive guide to all operation types
- `examples/mixed-workload-cloud.yaml` - Production-ready cloud benchmark example
- `examples/all-operations.yaml` - Demonstrates all 5 operation types

#### Updated Examples
- Moved YAML examples from `docs/` to `examples/` directory
- All examples now use correct glob pattern syntax
- Added detailed comments explaining each operation type
- Included weight balancing best practices

### 🧪 Testing
- Added regression tests in `tests/configs/prepare-performance/`
- Added regression tests in `tests/configs/cleanup-performance/`
- Added pattern resolution test: `tests/configs/pattern-resolution-test.yaml`
- Validated all operation types work correctly with glob patterns

### 🔧 Technical Details

**Concurrency Model**:
- Prepare: 32 parallel workers (configurable in future release)
- Workload: 32 parallel workers (configurable per-operation)
- Cleanup: 32 parallel workers (matches prepare/workload)

**Pattern Resolution**:
- GET, DELETE, STAT: Pre-resolve patterns → sample random URIs during execution
- PUT: Generate unique names dynamically (no pre-resolution needed)
- LIST: Operates on directories (no pre-resolution needed)

**Error Handling**:
- Prepare: Fails immediately on any PUT error (strict)
- Cleanup: Best-effort deletion (logs warnings, continues on errors)
- Workload: Propagates errors to maintain benchmark integrity

### 🎯 Migration from v0.5.4

**Update your configs** to use glob patterns:

Old (broken):
```yaml
- op: delete
  path: "data/obj_{00000..19999}"
```

New (working):
```yaml
- op: delete
  path: "data/prepared-*.dat"
```

**No code changes needed** - just update YAML configs to use wildcard patterns.

---

## [0.5.4] - 2025-10-04

### 💥 BREAKING CHANGES
- **Project Renamed**: `io-bench` → `sai3-bench` (final name reflecting S3/Azure/I3 unified benchmarking)
- **Binary Names Changed**:
  - `io-bench` → `sai3-bench`
  - `iobench-agent` → `sai3bench-agent`
  - `iobench-ctl` → `sai3bench-ctl`
  - `iobench-run` → `sai3bench-run`
- **Module/Crate Name**: `io_bench` → `sai3_bench` in Rust code

### 📝 Documentation Updates
- Updated all documentation to reflect new project name
- Updated all command examples with new binary names
- Updated test configuration file paths (s3bench-test → sai3bench-test)

### ℹ️ Migration Notes
If upgrading from v0.5.3 (io-bench):
1. Update any scripts: `io-bench` → `sai3-bench`
2. Update agent/controller scripts: `iobench-*` → `sai3bench-*`
3. Rebuild: `cargo build --release`
4. All functionality remains identical—only names changed

---

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

#### Deduplication and Compression Control
Leverage `s3dlio`'s controlled data generation to test storage system efficiency:

```yaml
prepare:
  - path: "highly-dedupable/"
    num_objects: 100
    size_distribution:
      type: fixed
      size: 1048576
    dedup_factor: 10      # 10% unique blocks (90% duplicate)
    compress_factor: 1    # Uncompressible (random data)

workload:
  - op: put
    path: "compressible/"
    weight: 50
    size_distribution:
      type: lognormal
      mean: 1048576
      std_dev: 524288
    dedup_factor: 1       # 100% unique (no dedup)
    compress_factor: 3    # 67% zeros (3:1 compression ratio)
```

**Parameters**:
- `dedup_factor`: Controls block uniqueness
  - `1` = all unique blocks (no deduplication)
  - `2` = 1/2 unique blocks (50% dedup ratio)
  - `3` = 1/3 unique blocks (67% dedup ratio)
  - Higher values = more duplication
- `compress_factor`: Controls compressibility
  - `1` = random data (uncompressible)
  - `2` = 50% zeros (2:1 compression ratio)
  - `3` = 67% zeros (3:1 compression ratio)
  - Higher values = more compressible

**Use cases**:
- Test storage deduplication engines (NetApp, EMC, etc.)
- Validate compression effectiveness (ZFS, Btrfs)
- Measure real-world storage efficiency with realistic data patterns
- Benchmark cloud storage with various data types (logs, backups, media)

**Implementation**:
- Uses `s3dlio::generate_controlled_data(size, dedup, compress)` API
- Block-based generation (BLK_SIZE = 512 bytes) with Bresenham distribution
- Defaults both to `1` for backward compatibility
- Available in both `prepare` steps and `PUT` operations

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
With v0.5.3, **sai3-bench surpasses MinIO Warp** in:
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
sai3-bench replay --op-log production.tsv.zst --remap remap.yaml

# With speed control
sai3-bench replay --op-log ops.tsv.zst --remap fanout.yaml --speed 2.0

# Multi-target fanout
sai3-bench replay --op-log prod.tsv.zst --remap remap_fanout.yaml
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
MinIO Warp compatibility features enabling near-identical test workflows between sai3-bench and Warp.

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
  sai3-bench run --config mixed.yaml --prepare-only
  sai3-bench run --config mixed.yaml
  sai3-bench run --config mixed.yaml
  
  # Run with cleanup disabled
  sai3-bench run --config mixed.yaml --no-cleanup
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
- **Documented Approach**: Added comments explaining sai3-bench's URI pattern matching
  - s3dlio's ObjectStore doesn't provide native glob support in `list()`
  - sai3-bench implements list-and-filter for `*` patterns at application level
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
  - Round-trip test: s3dlio generates op-log → sai3-bench replays
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
    - `-v`: INFO level for sai3-bench, minimal s3dlio output
    - `-vv`: DEBUG level for sai3-bench, INFO level for s3dlio (operational details)
    - `-vvv`: TRACE level for sai3-bench, DEBUG level for s3dlio (full debugging)
  - **Cascading Levels**: sai3-bench verbosity automatically configures s3dlio logging (one level less)
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
- **Project Renamed**: `s3-bench` → `sai3-bench` (reflects multi-protocol nature)
- **Binary Names Changed**:
  - `s3-bench` → `sai3-bench`
  - `s3bench-agent` → `sai3bench-agent`
  - `s3bench-ctl` → `sai3bench-ctl`
  - `s3bench-run` → `sai3bench-run`
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