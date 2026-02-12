# Changelog

All notable changes to sai3-bench are documented in this file.

**For historical changes:**
- **v0.8.5 - v0.8.19**: See [archive/CHANGELOG_v0.8.5-v0.8.19.md](archive/CHANGELOG_v0.8.5-v0.8.19.md)
- **v0.1.0 - v0.8.4**: See [archive/CHANGELOG_v0.1.0-v0.8.4.md](archive/CHANGELOG_v0.1.0-v0.8.4.md)

## [0.8.62] - 2026-02-11

**Prepare Streaming + Perf Log Timing + Dry-Run Memory Sampling**

This release removes the large precompute memory spike in prepare, restores parallel size mixing while streaming, and adds visibility into dry-run sample memory. It also aligns perf-log timing with stage transitions and improves the analyze tooling.

### Added

- **Dry-run sample generation with memory/time reporting**
  - Always generates a fixed sample (100k) of prepare paths/sizes
  - Reports elapsed time and RSS delta to surface scaling risks

- **Per-agent perf-log export in analyze tool**
  - Adds worksheets for `agents/*/perf_log.tsv`
  - Ensures unique worksheet names and safe 31-char limits
  - Adds `--overwrite` flag and smarter default output naming

### Changed

- **Parallel prepare now streams and interleaves**
  - Interleaves `ensure_objects` entries to preserve mixed sizes
  - Uses deterministic on-the-fly size generation with bounded chunking
  - Keeps memory flat by dropping per-chunk task vectors

- **Sequential prepare no longer precomputes sizes**
  - Deterministic streaming generation for large datasets

- **Directory tree path resolution is now O(1)**
  - Avoids linear scans in `TreeManifest::get_file_path()`

- **Perf-log elapsed timing resets on stage transitions**
  - Controller and per-agent perf-log writers reset stage elapsed time on transitions
  - Ensures stage timing alignment without resetting counters

- **Directory tree counts applied to parsed configs**
  - `sai3-bench` and `sai3bench-ctl` now apply directory tree counts before dry-run and controller dispatch

### Testing

- **551 tests passing** (release profile)

---

## [0.8.61] - 2026-02-11

**Stage and Barrier Alignment + Test Reliability**

This release tightens distributed stage orchestration requirements, standardizes barrier identifiers, and improves test reliability and store setup behavior.

### Breaking

- **Distributed stages are required**
  - Implicit stage generation has been removed for distributed runs
  - Use the built-in `convert` command to update legacy YAML files

### Added

- **Config conversion command for legacy YAML files**
  - `sai3-bench convert --config <file.yaml>`
  - `sai3-bench convert --files <glob>`
  - `sai3bench-ctl convert --config <file.yaml>`
  - `sai3bench-ctl convert --files <glob>`

### Changed

- **Distributed stages are now required**
  - `distributed.stages` must be explicitly defined (empty list is invalid)
  - Removes legacy default stage generation for distributed runs
  - **Impact**: Configs must declare stage ordering explicitly for distributed mode

- **Barrier identifiers are numeric stage indices**
  - Barrier coordination uses stage order indices instead of string names
  - Aligns controller barrier messaging with stage transition sequencing

### Fixed

- **Store cache pre-creation for PUT operations**
  - Uses PUT-specific URI resolution to avoid metadata-only path handling
  - Prevents runtime panic when pre-creating stores for PUT workloads

- **Checkpoint tests made deterministic**
  - Sequential checkpoint overwrite/restore flow avoids cross-test interference

- **Performance test flakiness removed**
  - Validation now asserts correctness instead of timing comparisons

### Testing

- **547 tests passing** (release profile)

---

## [0.8.60] - 2026-02-10

**KV Cache Checkpoint Restoration - Complete Resume Capability**

This release completes the checkpoint implementation with automatic restoration on startup, enabling full resume capability after crashes or restarts. Checkpoints are now created AND restored for both standalone and distributed modes.

### Added

- **Checkpoint restoration on startup** (CRITICAL)
  - `EndpointCache::new()` now calls `try_restore_from_checkpoint()` BEFORE opening database
  - Downloads checkpoint from storage (`{endpoint}/.sai3-cache-agent-{id}.tar.zst`)
  - Extracts tar.zst to local cache location
  - Opens restored fjall database with all object states intact
  - **Impact**: Agents can resume long-running prepare operations after crashes/restarts
  - **Files changed**: `src/metadata_cache.rs` (+120 lines restoration logic)

- **Checkpoint creation for standalone mode**
  - `sai3-bench run` now passes `results_dir` and `config` to `prepare_objects()`
  - Periodic checkpoints created every 5 minutes (configurable via `cache_checkpoint_interval_secs`)
  - Works with all storage backends (file://, s3://, az://, gs://)
  - **Impact**: Single-node workloads can now resume after interruption
  - **Files changed**: `src/main.rs` (+2 lines enable cache)

### Fixed

- **Race condition in checkpoint creation** (CRITICAL)
  - Changed from `maybe_flush()` to guaranteed `force_flush()` before archiving
  - Added database file verification after flush to ensure files are on disk
  - Prevents corruption from intervening writes between flush and archive creation
  - **Impact**: Checkpoints now reliably contain all committed object states
  - **Files changed**: `src/metadata_cache.rs` (+25 lines verification)

- **Checkpoint extraction verification**
  - Added comprehensive diagnostics for restoration process
  - Verifies cache directory and database files exist after extraction
  - Logs restored object counts for validation
  - **Impact**: Easier debugging of restoration issues
  - **Files changed**: `src/metadata_cache.rs` (+35 lines diagnostics)

### Enhanced

- **9 comprehensive checkpoint tests** covering all scenarios:
  - Archive contains KV database files
  - Restoration from storage
  - No checkpoint on storage (graceful fallback)
  - Local cache newer than checkpoint (skip restore)
  - Agent ID isolation (separate checkpoints per agent)
  - Checkpoint overwrites (latest wins)
  - Storage location verification (checkpoints at storage URI, not cache location)
  - Large dataset (1000 objects)
  - Multiple restore cycles

### Testing

- **545 tests passing** (14 ignored performance tests)
  - All checkpoint restoration tests passing
  - All integration tests updated for new `prepare_objects()` signature
  - All doc tests updated for new `MetadataCache::new()` signature
  - **0 test failures, 0 compilation errors, 0 warnings**

### Configuration

**New top-level field:**
```yaml
cache_checkpoint_interval_secs: 300  # Default: 5 minutes, 0 = disabled
```

**Checkpoint locations:**
- `file:///path/` → `{path}/.sai3-cache-agent-{id}.tar.zst`
- `s3://bucket/` → `s3://bucket/.sai3-cache-agent-{id}.tar.zst`
- `az://container/` → `az://container/.sai3-cache-agent-{id}.tar.zst`
- `gs://bucket/` → `gs://bucket/.sai3-cache-agent-{id}.tar.zst`

---

## [0.8.53] - 2026-02-09

**Critical Fixes: Multi-Endpoint + Directory Tree Workloads**

This release fixes critical bugs affecting multi-endpoint configurations with directory tree structures, and enhances dry-run validation output for better visibility.

### Fixed

- **Multi-endpoint + directory tree workload routing** (CRITICAL)
  - GET/PUT/STAT/DELETE operations now correctly route to the endpoint where each file was created
  - Fixed round-robin endpoint calculation: extracts file index from filename, computes `endpoint_idx = file_idx % num_endpoints`
  - Applies to all tree mode operations in distributed workloads
  - **Impact**: Workloads that previously failed with 0 ops now execute correctly
  - Example: 2 agents × 2 endpoints = 4 total mount points, all files now accessible
  - **Files changed**: `src/workload.rs` (+94 lines endpoint routing logic)

- **"duplicate field `timeout_secs`" deserialization error in distributed validation stage**
  - Removed duplicate `timeout_secs` field from `StageConfig::Validation` variant
  - Configuration now uses single top-level `timeout_secs` field
  - **Impact**: Agents no longer crash on startup when parsing YAML with validation stages
  - **Files changed**: `src/config.rs` (-7 lines), `src/bin/agent.rs` (2 lines field access)

### Enhanced

- **Dry-run validation now shows complete file distribution across ALL endpoints**
  - Previously: Only showed first agent's first endpoint
  - Now: Displays all endpoints from all agents with full URIs
  - Shows round-robin pattern visualization (e.g., "indices 0,4,8,12 → endpoint1")
  - Displays first 2 sample files per endpoint with complete file:// / s3:// / az:// URIs
  - Grouped by agent for multi-agent configurations
  - **Impact**: Users can verify correct endpoint distribution before running expensive workloads
  - **Files changed**: `src/validation.rs` (+146 lines multi-endpoint display logic)

### Testing

- Verified with 2 agents × 2 endpoints = 4 total mount points
- 80 files distributed perfectly (20 per endpoint, round-robin by index)
- 11 GET operations succeeded (previously failed with 0 ops)
- Dry-run shows complete URI distribution across all 4 endpoints
- All existing tests passing (446 total)

---

## [0.8.52] - 2026-02-06

**Maintainability Release: Adaptive Retry + Code Refactoring + Enhanced UX**

This release improves code maintainability, enhances prepare phase resilience with adaptive retry strategies, and adds user-friendly numeric formatting across the codebase.

### Added

- **Adaptive retry strategy for prepare phase failures** (80-20 rule)
  - Intelligent retry based on failure rate:
    - `<20%` failures: Full retry (10 attempts) - likely transient issues
    - `20-80%` failures: Limited retry (3 attempts) - potential systemic problems
    - `>80%` failures: Skip retry - clear systemic failure, no point retrying
  - Deferred retry runs AFTER main prepare phase completes
  - More aggressive exponential backoff (500ms initial, 30s max, 2.0× multiplier)
  - Prevents "missing object" errors during execution phase
  - Eliminates fast path performance impact
  - **11 comprehensive unit tests** for all failure rate scenarios
  - Total test count: **446 tests passing** (+27 from v0.8.51)

- **Thousand separator formatting for numeric output**
  - Dry-run displays: `64,032,768 files` instead of `64032768 files`
  - TSV column headers: sizes like `1,048,576` for improved readability
  - All validation messages: clearer numeric output
  - Dependency: `num-format` crate (v0.4+)

- **YAML numeric input with thousand separators** (v0.8.52)
  - Support for commas, underscores, and spaces in YAML numbers
  - Examples: `count: 64,032,768`, `count: 64_032_768`, `count: 64 032 768`
  - Custom serde deserializers handle all three separator formats
  - Applies to all numeric fields: `count`, `min_size`, `max_size`, `width`, `depth`, `files_per_dir`
  - Backward compatible: plain numbers (`64032768`) still work
  - See [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for examples

- **Human-readable time units in YAML configuration** (v0.8.52)
  - Support for time unit suffixes: `s` (seconds), `m` (minutes), `h` (hours), `d` (days)
  - Examples: `duration: "5m"`, `timeout: "2h"`, `delay: "30s"`
  - Applies to all timeout/duration fields:
    - `duration`, `start_delay`, `post_prepare_delay`
    - `grpc_keepalive_interval`, `grpc_keepalive_timeout`
    - `agent_ready_timeout`, `query_timeout`, `agent_barrier_timeout`
    - `default_heartbeat_interval`, `default_query_timeout`
    - SSH `timeout` and all barrier sync timeouts
  - Backward compatible: plain integers still interpreted as seconds
  - **10 comprehensive unit tests** for duration parsing
  - See [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for examples

- **Configuration conflict detection warnings**
  - Warns when `cleanup: false` conflicts with YAML cleanup stage
  - Helps prevent unintended behavior in multi-stage configurations
  - Non-fatal: displays warning but continues execution

### Changed

- **Code refactoring: Split prepare.rs into 10 focused modules** (Phase 1)
  - Original: monolithic 4,778-line file
  - New structure: 10 modules averaging ~478 lines each
  - **Module breakdown:**
    - `error_tracking.rs` (181 lines) - PrepareErrorTracker, ListingErrorTracker
    - `retry.rs` (231 lines) - Adaptive retry with 80-20 rule
    - `metrics.rs` (87 lines) - PreparedObject, PrepareMetrics types
    - `listing.rs` (364 lines) - Distributed listing with progress
    - `sequential.rs` (587 lines) - Sequential prepare strategy
    - `parallel.rs` (812 lines) - Parallel prepare strategy
    - `directory_tree.rs` (709 lines) - Tree operations and agent assignment
    - `cleanup.rs` (382 lines) - Cleanup and verification
    - `tests.rs` (1,312 lines) - All 178 prepare phase tests
    - `mod.rs` (237 lines) - Public API and orchestration
  - **Benefits:**
    - Average module size reduced 10× (4,778 → ~478 lines)
    - Clear separation of concerns
    - Easier code navigation and maintenance
    - Test isolation in dedicated module
    - Improved IDE performance and analysis
  - **Verification:**
    - All 178 prepare tests passing
    - Zero compilation warnings
    - Build time unchanged (~12s)
    - No functionality changes - pure refactoring

- **Improved validation error messages**
  - All numeric values use thousand separators for readability
  - Directory structure validation shows formatted counts
  - Conflict warnings highlight specific configuration issues

### Fixed

- **Custom serde deserializers for numeric fields**
  - New `src/serde_helpers.rs` module with deserializer utilities
  - Applied to `DirectoryStructure` and `EnsureObjects` structs
  - Handles all separator formats transparently

### Documentation

- Updated [README.md](../README.md) with v0.8.52 feature descriptions
- Added thousand separator examples to [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md)
- All example configurations updated to show readable numeric format

### Migration Guide

**No breaking changes** - All updates are backward compatible.

**Optional enhancements:**
1. Use thousand separators in YAML configs for better readability:
   ```yaml
   prepare:
     ensure_objects:
       - count: 10,000,000  # More readable than 10000000
         min_size: 1,048,576  # Clearly shows 1 MiB
   ```
2. Review prepare phase retry behavior in logs - new adaptive strategy may change retry patterns
3. Monitor prepare phase warnings for configuration conflicts

---

## [0.8.51] - 2026-02-06

**Critical Release: Blocking I/O Fixes for Large-Scale Deployments**

This release addresses four critical executor starvation issues identified in production large-scale testing (>100K files). These fixes are essential for distributed deployments where agents must validate extensive directory structures and create millions of objects without blocking the async executor.

### Added

- **Comprehensive unit test suite for blocking I/O fixes** (12 new tests)
  - `test_agent_ready_timeout_default` - Verifies 120s default timeout
  - `test_agent_ready_timeout_custom` - Tests custom timeout parsing (60s-600s)
  - `test_agent_ready_timeout_scale_recommendations` - Scale-based timeout validation
  - `test_timeout_duration_conversion` - Duration conversion logic
  - `test_timeout_realistic_values` - Real-world scenarios (10K-1M files)
  - `test_backward_compatibility_no_timeout` - Default fallback behavior
  - `test_glob_does_not_block_executor` - Concurrent task progress during glob
  - `test_glob_large_directory` - 10K file glob without executor starvation
  - `test_prepare_yields_during_creation` - 500 object prepare with heartbeats
  - `test_prepare_sequential_yields` - Sequential mode yielding validation
  - `test_executor_responsiveness_large_prepare` - 1000 object integration test
  - `test_small_prepare_still_works` - Regression test for small workloads
  - Total test count: **419 tests passing**

### Changed

- **Configurable agent_ready_timeout** (default: 120s, was hardcoded 30s)
  - New configuration field: `distributed.agent_ready_timeout`
  - Allows agents time to complete glob validation at scale
  - Scale recommendations: 60s (small), 120s (medium), 300s (large), 600s (very large)
  - Prevents "Agent did not send READY within 30s" errors in large-scale tests
  - See [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for configuration details

- **Non-blocking glob operations** (spawn_blocking)
  - `agent.rs` line 3828: Moved blocking glob to thread pool
  - `controller.rs` line 4606: Moved blocking glob to thread pool
  - Prevents 5-30 second executor stalls during file path expansion
  - Critical for configurations with >100K files and glob patterns

- **Periodic yielding in prepare loops** (tokio::task::yield_now)
  - `prepare.rs` line 1269: Yield every 100 operations in parallel prepare
  - `prepare.rs` line 2011: Yield every 100 operations in sequential prepare
  - `prepare.rs` line 2999: Yield every 100 operations in cleanup
  - Allows heartbeats, READY signals, and stats updates during million-object operations
  - Uses `.is_multiple_of(100)` per clippy suggestion

### Fixed

- **Executor starvation during large-scale operations**
  - Fixed blocking glob preventing gRPC heartbeats
  - Fixed prepare phase blocking stats writer task
  - Fixed cleanup phase blocking barrier coordination
  - All fixes validated with concurrent task execution tests

- **Test fixture compilation errors**
  - Updated 5 test functions with new `agent_ready_timeout` field
  - `src/preflight/distributed.rs`: 4 test fixtures updated
  - `tests/distributed_config_tests.rs`: 1 test fixture updated
  - All 419 tests passing with zero compilation errors

### Documentation

- Updated [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) with agent_ready_timeout recommendations
- Added comprehensive blocking I/O fix documentation
- Design documents archived in [docs/archive/](archive/):
  - `BLOCKING_IO_FIXES_REQUIRED.md`
  - `LARGE_SCALE_TIMEOUT_ANALYSIS.md`
  - `TIMEOUT_FIX_IMPLEMENTATION.md`

### Migration Guide

**No breaking changes** - All fixes are backward compatible. Existing configurations will use default timeout values.

**Recommended actions for large-scale deployments:**
1. Add `agent_ready_timeout` to distributed config based on file count
2. Verify glob patterns resolve quickly or increase timeout accordingly
3. Monitor agent READY times in logs to tune timeout values

---

## [0.8.50] - 2026-02-05

**Major Release: YAML-Driven Stage Orchestration + Barrier Synchronization + Configurable Timeouts**

This release represents a fundamental architectural evolution of sai3-bench, transitioning from simple prepare→workload execution to a flexible, multi-stage orchestration framework with precise synchronization control and production-grade timeout management.

### Added

- **YAML-driven stage orchestration framework (Phases 1-3 complete)**
  - Multi-stage test definition via YAML configuration (preflight, prepare, execute, cleanup)
  - Each stage configurable with independent operations, durations, and targets
  - Support for 4 distinct stage types:
    - **Preflight**: Configuration validation (file existence, directory structure)
    - **Prepare**: Data preparation (object creation, tree building)
    - **Execute**: Performance workload (read/write/list operations)
    - **Cleanup**: Test teardown (directory removal, object deletion)
  - Automatic stage detection and execution ordering
  - Backward compatibility with legacy single-stage configurations

- **Numbered stage output format for Excel organization**
  - New TSV file naming: `NN_stagename_results.tsv` (e.g., `01_preflight_results.tsv`)
  - Stage numbers preserve execution order in multi-tab Excel workbooks
  - Enables clear visual progression: 01→02→03→04 in tab ordering
  - Each stage file includes descriptive comment header with phase information

- **Barrier synchronization framework**
  - Pre-stage barriers ensure all agents ready before stage execution
  - Post-stage barriers ensure stage completion before proceeding
  - Barrier timeout configuration per stage (default: 300 seconds)
  - Prevents timing skew in distributed multi-stage tests
  - Critical for coordinated multi-host testing scenarios
  - See [STAGE_ORCHESTRATION_README.md](STAGE_ORCHESTRATION_README.md) for architecture details

- **Comprehensive timeout configuration system**
  - **Agent operation timeouts**: Per-stage configurable (default: 600s for prepare, 3600s for execute/cleanup)
  - **Barrier synchronization timeouts**: Per-stage configurable (default: 300s)
  - **gRPC call timeouts**: Configurable at controller level (default: 300s)
  - **Health check timeouts**: Fixed 30s for rapid failure detection
  - Prevents indefinite hangs in distributed testing environments
  - Validates timeout values at config load time (≥60s minimum)
  - See [docs/TIMEOUT_CONFIGURATION.md](docs/TIMEOUT_CONFIGURATION.md) for complete reference

- **sai3-analyze multi-stage support**
  - Parses numbered stage TSV files: `01_preflight_results.tsv`, `02_prepare_results.tsv`, etc.
  - Excel tab naming: `workload-NN_stagename` (e.g., `test_barriers-01_preflight`)
  - Automatic comment line filtering (lines starting with `#`)
  - Stage number extraction for proper tab ordering
  - Backward compatibility with legacy `perf_log.tsv` format
  - Timestamp column formatting preserved (epoch → Excel datetime)

- **37 validated multi-stage YAML test configurations**
  - Comprehensive test coverage across all stage types:
    - 8 preflight validation tests (config checks, file existence)
    - 11 prepare tests (data generation, tree building, multi-endpoint)
    - 12 execute tests (workload runs, distributed operations, barriers)
    - 6 cleanup tests (directory removal, object deletion)
  - All configs validated with `--dry-run` before release
  - Pruned from 92 original configs to focus on essential test patterns
  - See `tests/configs/` directory for examples

- **Large-scale distributed testing validation**
  - Successfully tested with 300,000+ directories
  - Successfully tested with 64 million+ files
  - Proven barrier synchronization across multiple test hosts
  - Multi-stage execution with coordinated prepare→execute→cleanup workflows

### Changed

- **BREAKING: Stage-aware config structure**
  - Added top-level `stages` array to YAML configuration
  - Each stage requires: `name`, `stage_type`, and stage-specific configuration
  - Legacy single-stage configs still supported (auto-converted to single execute stage)
  - Stage types: `preflight`, `prepare`, `execute`, `cleanup`

- **BREAKING: Output file structure**
  - TSV files now numbered: `01_preflight_results.tsv` instead of `preflight_results.tsv`
  - Preserves execution order in multi-file analysis scenarios
  - Old format: `{stage}_results.tsv` → New format: `{NN}_{stage}_results.tsv`

- **Enhanced results directory organization**
  - Each test run creates timestamped directory: `sai3-YYYYMMDD-HHMM-{workload_name}/`
  - Stage results grouped within run directory
  - Comment headers in TSV files document stage execution details
  - Improved traceability for multi-stage test runs

- **Version bump: v0.8.26 → v0.8.50**
  - Reflects major architectural changes (stage orchestration framework)
  - Aligns with feature scope (not just incremental changes)

### Fixed

- **sai3-analyze stage name extraction**
  - Correctly strips `_results.tsv` suffix while preserving stage number prefix
  - Handles both new numbered format (`01_preflight_results.tsv`) and legacy format
  - Fixed parsing logic: iterate through chars to find first non-digit, then strip suffix
  - Tab names now properly formatted: `workload-01_preflight` (no timestamp clutter)

- **Comment line handling in TSV parsing**
  - Filters lines starting with `#` (descriptive headers)
  - Prevents parsing errors when stage metadata included in output files
  - Maintains backward compatibility with comment-free legacy files

- **Tab naming timestamp removal**
  - Removed timestamp logic from Excel tab names (user preference)
  - Timestamps retained in directory names for run identification
  - Tab format: `{workload}-{NN}_{stage}` for clarity and ordering

### Testing

- ✅ All 4 sai3-analyze unit tests passing:
  - Stage extraction from numbered filenames
  - Tab name generation (workload-stage format)
  - Tab name truncation (31-char Excel limit)
  - Legacy TSV parsing compatibility
  
- ✅ Real directory validation:
  - Tested with `sai3-20260205-1440-test_barriers_local/` (4 stage files)
  - Generated 11KB Excel workbook with 4 tabs
  - Tab ordering: 01_preflight → 02_prepare → 03_execute → 04_cleanup
  
- ✅ 37 YAML configs validated with `--dry-run`:
  - All preflight validation tests (8 configs)
  - All prepare stage tests (11 configs)
  - All execute stage tests (12 configs)
  - All cleanup stage tests (6 configs)
  
- ✅ Large-scale distributed testing:
  - 300,000+ directories with barrier synchronization
  - 64 million+ files across multi-host test environment
  - Multi-stage workflows (prepare → execute → cleanup)

### Documentation

- **New guides:**
  - [STAGE_ORCHESTRATION_README.md](STAGE_ORCHESTRATION_README.md) - Complete stage framework architecture
  - [docs/TIMEOUT_CONFIGURATION.md](docs/TIMEOUT_CONFIGURATION.md) - Comprehensive timeout reference
  - [docs/examples/multi_stage_workflow_example.yaml](docs/examples/multi_stage_workflow_example.yaml) - Full 4-stage example

- **Updated guides:**
  - [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) - Added `stages` array documentation
  - [YAML_MULTI_STAGE_GUIDE.md](YAML_MULTI_STAGE_GUIDE.md) - Stage orchestration patterns
  - [README.md](README.md) - Feature list, test count updates (pending)

- **Archived documentation:**
  - Created [archive/CHANGELOG_v0.8.5-v0.8.19.md](archive/CHANGELOG_v0.8.5-v0.8.19.md)
  - 933 lines of historical changes (15 versions)
  - Maintains complete project history

### Configuration Examples

**Multi-stage workflow with barriers:**
```yaml
stages:
  - name: preflight
    stage_type: preflight
    barrier_sync: true
    barrier_timeout_secs: 300
    checks:
      - file_exists: /mnt/storage/benchmark/
  
  - name: prepare
    stage_type: prepare
    barrier_sync: true
    agent_timeout_secs: 600
    workload:
      object_count: 10000
      object_size: 1048576
  
  - name: execute
    stage_type: execute
    barrier_sync: true
    agent_timeout_secs: 3600
    barrier_timeout_secs: 300
    workload:
      duration_secs: 300
      operations:
        - type: get
          weight: 80
        - type: put
          weight: 20
  
  - name: cleanup
    stage_type: cleanup
    barrier_sync: true
    workload:
      remove_objects: true
```

**Per-stage timeout customization:**
```yaml
# Global gRPC timeout
distributed:
  grpc_timeout_secs: 300

# Per-stage overrides
stages:
  - name: long_prepare
    stage_type: prepare
    agent_timeout_secs: 1800  # 30 minutes for large dataset
    barrier_timeout_secs: 600  # 10 minutes barrier sync
```

### Technical Details

- Stage orchestration leverages existing distributed testing infrastructure
- Barrier synchronization implemented via gRPC coordination messages
- Timeout handling propagates from controller through agents to s3dlio operations
- TSV comment headers use `#` prefix for metadata (filtered during analysis)
- Excel tab name format: max 31 chars, truncated with "..." suffix if needed
- sai3-analyze supports both new numbered format and legacy single-file format

### Dependencies

- s3dlio v0.9.16+ (git tag dependency, can use v0.9.27+ for latest features)
- Rust 1.75+ for Duration API and async timeout handling
- Compatible with all storage backends (S3, Azure, GCS, file://, direct://)

### Migration Notes

**From v0.8.23 and earlier:**
- Old single-stage configs still work (auto-converted to single execute stage)
- To use multi-stage features, restructure config with `stages` array
- sai3-analyze automatically detects numbered vs legacy TSV format
- No action required for existing test scripts or automation

**Excel workbook changes:**
- Tab names no longer include timestamps (cleaner, more readable)
- Tab names now include stage numbers for proper ordering (01_, 02_, etc.)
- Multi-stage tests produce multiple tabs per workload (one per stage)

---

## [0.8.23] - 2026-02-03

### Added

- **Distributed configuration pre-flight validation** (prevents common misconfigurations)
  - New `src/preflight/distributed.rs` validation module (315 lines, 4 comprehensive tests)
  - Detects `base_uri` specified in isolated mode with per-agent storage (the h2 protocol error bug)
  - Validates `shared_filesystem` semantics: warns if agents lack `multi_endpoint` in shared mode
  - Integrated into controller workflow - runs before agent pre-flight validation
  - Provides actionable error messages with configuration fix recommendations
  
- **EnsureSpec.base_uri made optional for isolated mode**
  - `base_uri` field now `Option<String>` (was `String`)
  - When `None` with `use_multi_endpoint: true`, each agent uses its first endpoint for listing
  - Enables correct distributed listing in isolated mode without base_uri conflicts
  - New `get_base_uri()` helper method for transparent fallback logic
  
- **Comprehensive test coverage for base_uri handling**
  - 10 unit tests in `src/config_tests.rs` covering all base_uri edge cases
  - 4 validation tests in `src/preflight/distributed.rs` for configuration scenarios
  - Fixed existing integration tests (3 files) to wrap base_uri in `Some()`
  - Real integration tests prove buggy config is blocked, fixed config passes

### Changed

- **Validation logic respects shared_filesystem semantics**
  - `shared_filesystem: true` - All agents access same storage (multi_endpoint optional)
  - `shared_filesystem: false` - Per-agent isolated storage (base_uri in isolated mode is error)
  - No assumptions about network topology or endpoint accessibility
  - Supports all valid deployment patterns: shared NFS, independent disks, mixed scenarios

### Fixed

- **h2 protocol errors in distributed isolated mode** (THE BUG)
  - Root cause: `base_uri` specified when agents have different `multi_endpoint` configurations
  - Listing phase used `base_uri` instead of each agent's first endpoint
  - Pre-flight now blocks this misconfiguration before runtime with clear fix recommendation
  - Example error: "1 agents cannot access 'file:///mnt/filesys1/benchmark/'"

### Testing

- **323 tests passing** (was 312)
  - 121 lib tests (10 new config tests, 1 integration test fix)
  - 202 integration tests (3 new distributed validation tests)
  - Validated with real agents: buggy config blocked, fixed config runs successfully

---

## [0.8.22] - 2026-01-29

### Added

- **Multi-endpoint statistics tracking** (endpoint-level performance visibility)
  - New `workload_endpoint_stats.tsv` file with per-endpoint metrics
  - New `prepare_endpoint_stats.tsv` file for data preparation phase
  - Columns: endpoint, operation_count, bytes, errors, latency percentiles (p50, p90, p99, p99.9)
  - Essential for diagnosing multi-endpoint load balancing effectiveness
  - Works with both global and per-agent endpoint configurations

- **Multi-endpoint load balancing for distributed storage systems**
  - Enables distributing I/O operations across multiple storage endpoints (IPs, mount points)
  - Perfect for multi-NIC storage systems (VAST, Weka, MinIO clusters)
  - Supports both global and per-agent endpoint configuration
  
- **Static per-agent endpoint mapping**
  - Assign specific endpoints to specific agents for optimal load distribution
  - Example: 4 test hosts × 2 endpoints/host = 8 storage IPs fully utilized
  - Prevents endpoint overlap in distributed testing scenarios
  
- **Multi-endpoint NFS support**
  - Load balance across multiple NFS mount points with identical namespaces
  - Works with any distributed filesystem presenting unified namespace (VAST, Weka, Lustre)
  
- **Configuration schema enhancements**
  - Added `multi_endpoint` field to top-level Config (global configuration)
  - Added `multi_endpoint` field to AgentConfig (per-agent override)
  - Strategy options: `round_robin` (simple), `least_connections` (adaptive)
  
- **New workload creation API**
  - Added `create_store_from_config()` function for multi-endpoint aware store creation
  - Respects configuration priority: per-agent > global > target fallback
  - Added `create_multi_endpoint_store()` internal helper function

- **Excel timestamp formatting** in sai3-analyze
  - Auto-detects timestamp columns by suffix (_ms, _us, _ns)
  - Converts epoch timestamps to Excel datetime format: "2026-01-29 22:21:51.000"
  - Column width: 22 for timestamps, 15 for regular columns
  - Fixes scientific notation display (1.77E+18 → readable dates)

- **Version option** for sai3-analyze
  - Added `-V` and `--version` flags
  - Displays: "sai3bench-analyze 0.8.22"

- **Comprehensive documentation consolidation**
  - Created [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md) (620+ lines)
    - Multi-host architecture with verified YAML examples
    - Multi-endpoint testing patterns (critical common use case)
    - Real-world examples: multi-region, multi-account, load balancing
    - Troubleshooting guide and performance analysis tools
  - Created [DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md)
    - Consolidated SSH_SETUP_GUIDE + CONTAINER_DEPLOYMENT_GUIDE + DOCKER_PRODUCTION_DEPLOYMENT
    - SSH deployment with automated ssh-setup command
    - Container deployment with host network mode
    - Production best practices (tmux + daemon patterns)
  - Updated [README.md](README.md) as clean documentation index
  - Reduced from 27+ documentation files to 9 essential docs
  - Moved 18 planning/design docs to archive/ directory

### Configuration Examples

**Global multi-endpoint** (all agents share endpoints):
```yaml
multi_endpoint:
  strategy: round_robin
  endpoints:
    - s3://192.168.1.10:9000/bucket/
    - s3://192.168.1.11:9000/bucket/
    - s3://192.168.1.12:9000/bucket/
```

**Per-agent static mapping** (each agent gets specific endpoints):
```yaml
distributed:
  agents:
    - address: "testhost1:7761"
      id: agent1
      multi_endpoint:
        endpoints:
          - s3://192.168.1.10:9000/bucket/
          - s3://192.168.1.11:9000/bucket/
    
    - address: "testhost2:7761"
      id: agent2
      multi_endpoint:
        endpoints:
          - s3://192.168.1.12:9000/bucket/
          - s3://192.168.1.13:9000/bucket/
```

**NFS multi-mount**:
```yaml
multi_endpoint:
  strategy: round_robin
  endpoints:
    - file:///mnt/nfs1/benchmark/
    - file:///mnt/nfs2/benchmark/
```

### Changed

- **Breaking: perf_log.tsv format** - Removed `is_warmup` column
  - Old format: 31 columns (with is_warmup)
  - New format: 30 columns (cleaner, less redundant)
  - sai3-analyze updated to handle both formats

- **Controller config serialization** - Per-agent YAML generation
  - Controller now serializes agent-specific YAML with only assigned endpoints
  - Lines 1272-1301 in `src/bin/controller.rs`
  - Prevents agents from seeing/accessing non-assigned endpoints
  - Critical for endpoint isolation in distributed testing

### Fixed

- **Critical: Per-agent endpoint isolation bug**
  - **Problem**: Controller sent same config YAML to all agents (all endpoints visible)
  - **Impact**: Each agent accessed ALL endpoints instead of only assigned ones
  - **Root cause**: Controller didn't override `multi_endpoint` in serialized agent config
  - **Fix**: Controller generates per-agent YAML with `multi_endpoint` override matching AgentConfig
  - **Validated**: Real distributed tests confirm Agent-1 only accesses endpoints A&B, Agent-2 only C&D

- **Excel timestamp scientific notation**
  - Timestamps no longer display as 1.77E+18
  - Auto-detection: Parses column names for time unit suffixes
  - Conversion: Applies correct divisor (1000 for ms, 1000000 for us, 1000000000 for ns)

### Validated

- ✅ Two distributed workload tests compared in Excel (multi_endpoint_comparison.xlsx)
- ✅ Endpoint stats files prove per-agent endpoint isolation
  - Agent-1: Only endpoints A & B in endpoint_stats.tsv
  - Agent-2: Only endpoints C & D in endpoint_stats.tsv
- ✅ All YAML examples verified with `--dry-run`:
  - `tests/configs/local_test_2agents.yaml` - 2 agents, 320 files, tree structure
  - `tests/configs/distributed_mixed_test.yaml` - 3 size groups (1KB, 128KB, 1MB)
  - `tests/configs/multi_endpoint_prepare.yaml` - Per-agent endpoints, 100 objects
  - `tests/configs/multi_endpoint_workload.yaml` - 20s duration, 75/20/5 GET/PUT/LIST
- ✅ 3-phase distributed test: prepare → replicate → workload (full validation)

### Technical Details

- Leverages s3dlio v0.9.37 zero-copy `Bytes` API (277 tests passing)
- Multi-endpoint support from s3dlio v0.9.14+ `MultiEndpointStore` with per-endpoint statistics
- Compatible with all storage backends (S3, Azure, GCS, file://, direct://)
- Requires identical namespace across all endpoints (same files accessible from each)
- Load balancing strategies implemented at s3dlio layer (zero overhead in sai3-bench)

### Test Configurations

- `tests/configs/local_test_2agents.yaml` - Basic 2-agent distributed test
- `tests/configs/distributed_mixed_test.yaml` - Mixed size workload (1KB, 128KB, 1MB)
- `tests/configs/multi_endpoint_prepare.yaml` - Per-agent endpoint assignment (Phase 1: prepare)
- `tests/configs/multi_endpoint_workload.yaml` - Multi-endpoint workload test (Phase 3)
- All configs validated with `--dry-run` before release

### Documentation

- See [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md) for multi-endpoint architecture and examples
- See [DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md) for SSH and container deployment
- See [archive/MULTI_ENDPOINT_ENHANCEMENT_PLAN.md](archive/MULTI_ENDPOINT_ENHANCEMENT_PLAN.md) for design rationale
- See [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for multi-endpoint configuration reference
- See [README.md](README.md) for documentation index

### Dependencies

- s3dlio v0.9.37 (zero-copy Bytes API)
- Compatible with s3dlio v0.9.16+ (multi-endpoint support)

---

## [0.8.21] - 2026-01-26

### Changed

- **BREAKING: Zero-copy API migration for s3dlio v0.9.36 compatibility**
  - Updated all PUT operations to accept `Bytes` instead of `&[u8]` (eliminates hidden memcpy)
  - Updated all GET operations to return `Bytes` instead of `Vec<u8>` (true zero-copy throughout)
  - Maintains s3dlio's zero-copy architecture: no data copies from storage backend to application

- **Enhanced data generation with fill_controlled_data()**
  - Replaced deprecated `generate_controlled_data_prand()` with high-performance `fill_controlled_data()`
  - Performance improvement: 86-163 GB/s (parallel) vs 3-4 GB/s (old sequential method)
  - Zero-copy workflow: `BytesMut` → in-place fill → `freeze()` to `Bytes`
  - All FillPattern variants now use zero-copy: Zero, Random, and Prand

- **Updated s3dlio dependency to v0.9.36**
  - API breaking change: `ObjectStore::put()` now takes `Bytes` for true zero-copy
  - New `fill_controlled_data()` function for in-place buffer filling
  - Deprecated `generate_controlled_data_prand()` removed from hot paths

### Performance Impact

- **Data generation**: 20-50x faster (86-163 GB/s vs 3-4 GB/s)
- **PUT operations**: Eliminated hidden `.to_vec()` copy (s3dlio v0.9.35 → v0.9.36 migration)
- **GET operations**: Eliminated all `.to_vec()` conversions (returned `Bytes` only used for `.len()`)
- **Memory efficiency**: No intermediate Vec<u8> allocations in hot path

---

## [0.8.20] - 2025-01-20

### Changed

- **Updated s3dlio dependency to v0.9.35**
  - Integrated hardware detection API for automatic optimal configuration
  - Per-run unique RNG seeding ensures non-repeating data patterns across distributed agents
  - Leverages 51.09 GB/s data generation performance from s3dlio v0.9.35
  - Zero-copy architecture with reduced memory overhead

- **Hardware-aware data generation**
  - Automatically detects NUMA nodes and CPU count at runtime
  - Configures optimal parallelism for data generation without manual tuning
  - Each workload run gets unique seed (agent_id + PID + nanosecond timestamp)
  - Eliminates data pattern repetition in multi-run and distributed scenarios

### Fixed

- **PerfLogEntry struct compatibility**
  - Added `get_mean_us`, `put_mean_us`, `meta_mean_us` fields to all test initializations
  - Updated field count assertions from 28 to 31 columns
  - Fixed doctest import in `set_global_rng_seed` example
