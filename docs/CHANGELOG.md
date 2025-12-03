# Changelog

All notable changes to sai3-bench are documented in this file.

**For historical changes (v0.1.0 - v0.8.4)**, see [archive/CHANGELOG_v0.1.0-v0.8.4.md](archive/CHANGELOG_v0.1.0-v0.8.4.md).

---

## [0.8.10] - 2025-12-02

### Added

- **Replay backpressure system** for graceful handling when I/O rate cannot be sustained
  - `BackpressureController`: Monitors replay lag and triggers mode transitions
  - `FlappingTracker`: Prevents rapid mode oscillation (max 3 transitions/minute)
  - `ReplayMode`: Normal (strict timing) vs BestEffort (no delays, catch-up mode)
  - Enhanced `ReplayStats`: tracks mode_transitions, flap_exit, peak_lag_ms, best_effort_time_ms

- **YAML-based replay configuration** via `--config` flag
  - New `ReplayConfig` struct with humantime Duration parsing
  - Fields: `lag_threshold`, `recovery_threshold`, `max_flaps_per_minute`, `drain_timeout`, `max_concurrent`
  - Sample config: `tests/configs/replay_backpressure.yaml`

- **Replay constants** in `src/constants.rs`
  - `DEFAULT_REPLAY_LAG_THRESHOLD`: 5 seconds
  - `DEFAULT_REPLAY_RECOVERY_THRESHOLD`: 1 second  
  - `DEFAULT_REPLAY_MAX_FLAPS_PER_MINUTE`: 3
  - `DEFAULT_REPLAY_DRAIN_TIMEOUT`: 10 seconds
  - `DEFAULT_REPLAY_MAX_CONCURRENT`: 16

### Changed

- **Replay CLI** now accepts `--config <YAML>` for backpressure configuration
  - Config validated with dry-run before execution
  - Backpressure metrics included in summary output
  
- **Renamed** `ReplayConfig` → `ReplayRunConfig` in replay_streaming.rs
  - Distinguishes runtime config from YAML parsing struct
  - Internal refactoring, no user-facing impact

### Documentation

- **Updated USAGE.md** with new Replay Backpressure section
  - Documents all configuration options and defaults
  - Provides usage examples and sample YAML config

### Testing

- ✅ 103 tests passing (68 unit + 35 config tests)
- ✅ 11 new backpressure unit tests (mode transitions, flap detection, stats)
- ✅ 5 new ReplayConfig YAML parsing tests
- ✅ Dry-run validation tested with sample config

---

## [0.8.9] - 2025-12-02

### Added

- **Flexible multi-stage system** for workload lifecycle tracking
  - `WorkloadStage` enum: STAGE_PREPARE, STAGE_WORKLOAD, STAGE_CLEANUP, STAGE_CUSTOM
  - Stage progress tracking: `stage_progress_current`, `stage_progress_total`, `stage_elapsed_s`
  - Stage name field for custom/user-defined stages
  - Proto fields: `current_stage`, `stage_name`, `stage_progress_current/total`, `stage_elapsed_s`

- **Stage-aware controller display**
  - Prepare phase: `📦 Preparing: 50000/100000 objects (50%)`
  - Workload phase: `GET: 35726 ops/s, 2.5 GiB/s` (time-based progress bar)
  - Cleanup phase: `🧹 Cleanup: 25/50 deleted (50%) | 167 DEL/s` (count-based progress bar)
  - Progress bar dynamically switches between count-based and time-based display

- **ControllerAgentState::Cleaning** - New state for cleanup phase in state machine
  - Proper state transitions: Ready → Preparing → Running → Cleaning → Completed
  - Recovery from disconnected state now handles cleanup phase

### Changed

- **LiveStatsTracker stage API**
  - `set_stage(stage, total)`: Set current stage with progress total
  - `set_stage_with_name(stage, name, total)`: Set stage with custom name
  - `increment_stage_progress()`: Increment progress counter
  - `set_stage_progress(current)`: Set progress to specific value
  - Stage elapsed time automatically tracked from stage start

- **Agent stage transitions**
  - `set_stage(StagePrepare)` before prepare phase
  - `set_stage(StageWorkload)` before workload execution
  - `set_stage(StageCleanup)` before cleanup (with total object count)
  - `increment_stage_progress()` called after each DELETE operation

### Fixed

- **Cleanup phase display** - Controller now shows cleanup DELETE progress instead of stale workload stats
  - Previously: Controller displayed 35726 ops/s GET during cleanup
  - Now: Controller displays `🧹 Cleanup: 50/100 deleted | 167 DEL/s`
  - State machine transitions correctly: Running → Cleaning → Completed

- **Critical store creation efficiency** - Fixed multiple locations where `create_store_for_uri()` was called per-operation
  - **cleanup.rs**: Store created ONCE before parallel DELETE loop (was: per object)
  - **prepare.rs**: Store/store cache created ONCE before parallel PUT loop (was: per object, 2 locations)
  - **replay_streaming.rs**: Shared `StoreCache` across all replay operations (was: per operation)
  - Impact at scale (100M objects): Eliminates 27+ hours of unnecessary overhead
  - Root cause: Each `create_store_for_uri()` creates HTTP client, connection pool, credential provider

- **Cleanup responsiveness** - Ctrl-C now works immediately during cleanup phase
  - Previously: Heavy per-object initialization blocked tokio's task scheduler
  - Now: Async runtime remains responsive with pre-initialized store

- **Public StoreCache API** - Added `pub type StoreCache` and `get_or_create_store()` for efficient store reuse
  - New functions: `get_object_cached_simple()`, `put_object_cached_simple()`, `delete_object_cached_simple()`
  - Also: `list_objects_cached_simple()`, `stat_object_cached_simple()`
  - Uses base URI as cache key for connection pool reuse

- **Rate-control test configs using invalid prepare syntax**
  - Fixed 4 configs: `rate_1000_exponential.yaml`, `rate_5000_uniform.yaml`, `rate_deterministic.yaml`, `rate_max.yaml`
  - Old (invalid): `prepare.objects`, `prepare.object_size`, `prepare.pattern` fields
  - New (correct): `prepare.ensure_objects` array with `base_uri`, `count`, `min_size`, `max_size`, `fill`
  - Note: Old fields were silently ignored by serde, causing 0 objects to be prepared

### Documentation

- Updated `.github/copilot-instructions.md` with v0.8.9 stage system documentation
- Verified CONFIG_SYNTAX.md and USAGE.md use correct `ensure_objects` syntax
- All example configs validated with `--dry-run`

### Testing

- ✅ 57 unit tests passing
- ✅ All rate-control configs now parse correctly and show proper prepare phase

---

## [0.8.8] - 2025-12-02

### Changed

- **Logging verbosity improvements**
  - Per-I/O operation logs (GET/PUT/LIST/STAT/DELETE starting/completing) moved from DEBUG to TRACE level
  - Status and coordination messages remain at DEBUG level
  - `-vv` now shows operational information without flooding with per-operation details
  - `-vvv` enables full per-I/O transaction logging for debugging

- **s3dlio logging level offset** - All three binaries now properly offset s3dlio logging:
  - `-v`: sai3-bench=info, s3dlio=warn
  - `-vv`: sai3-bench=debug, s3dlio=info
  - `-vvv`: sai3-bench=trace, s3dlio=debug
  - Previously only `sai3-bench` CLI had this; now `sai3bench-agent` and `sai3bench-ctl` also include it

### Fixed

- **Agent idle timeout removed** - Agents now run indefinitely as long-lived services
  - Removed 30-second IDLE timeout that caused agents to fail after inactivity
  - READY timeout (60s) retained: auto-resets to IDLE if controller fails to send START
  - Agents survive 5+ minutes idle and accept new workloads without restart

### Documentation

- Updated `.github/copilot-instructions.md` with v0.8.8 features
- Clarified logging level behavior and s3dlio integration

---

## [0.8.7] - 2025-11-26

### Added

- **Dedicated cleanup module** (`src/cleanup.rs`)
  - `list_existing_objects()`: Lists ALL existing objects without creating any new ones
  - Simplified from `prepare_objects()` - removed all file creation logic
  - Supports both flat and directory tree modes
  - Proper separation: prepare creates, cleanup deletes

- **Cleanup-only mode with storage listing** (`cleanup_only: true, skip_verification: false`)
  - Lists existing objects from storage (expensive for large datasets on shared storage)
  - No file creation during cleanup-only mode
  - Each agent sees ALL objects, distribution handled via modulo in cleanup function
  - Proper URI handling: uses full URIs from store.list(), passes to store.delete()
  
- **Cleanup as counted workload** (v0.8.7)
  - DELETE operations tracked as META via LiveStatsTracker
  - Minimum 3-second stats reporting for fast-completing workloads
  - Prevents controller panic on instant completion
  - Workload completes when N deletions finish (event-based, not timed)

### Changed

- **Moved cleanup_prepared_objects()** from `prepare.rs` to `cleanup.rs`
  - Re-exported via `workload.rs` for backward compatibility
  - Clean module organization and separation of concerns

- **Fixed cleanup distribution logic**
  - `list_existing_objects()` returns complete list (no filtering)
  - `cleanup_prepared_objects()` handles modulo distribution
  - Previously each agent only saw ~50% of files (incorrect filtering during listing)

### Fixed

- **cleanup_only flag detection** - Use `PrepareConfig.cleanup_only` exclusively
  - Removed duration==0 checks (incorrect heuristic)
  - All code paths now check cleanup_only flag properly

- **Workload timer synchronization** - Reset timer on prepare→workload transition
  - Controller timer now matches agent reporting exactly
  - Fixed: timer included prepare time before

- **Minimum stats duration** - Agents send stats for 3 seconds minimum
  - Prevents controller panic when workload completes in <2ms
  - Continues sending final cumulative values until minimum met

### Documentation

- Updated module organization documentation
- Added warnings about N×listing overhead for shared storage with many files
- Clarified cleanup-only mode behavior (listing vs generation)

### Testing

- ✅ 55 passing Rust tests (previously 148 combined integration + unit)
- ✅ Verified cleanup-only with skip_verification=false deletes all files
- ✅ Tested distributed cleanup across 2 agents (45 files deleted correctly)
- ✅ Verified proper modulo distribution (23 + 22 = 45 total deletions)

---

## [Unreleased] - Operation Logging Enhancements

### Added

- **Operation logging with client identification** (requires s3dlio v0.9.22+)
  - Standalone mode: client_id = "standalone" or SAI3_CLIENT_ID env var
  - Distributed mode: client_id = agent_id (e.g., "agent-1", "agent-2")
  - Enables per-agent filtering in merged oplogs
  
- **Clock offset synchronization for distributed oplogs**
  - Agent automatically calculates offset from controller's start_timestamp_ns
  - All operation timestamps adjusted to controller's reference time
  - Enables accurate cross-agent timeline reconstruction
  
- **Approximate first_byte tracking** (via s3dlio v0.9.22)
  - GET operations: first_byte ≈ end (when complete data available)
  - PUT operations: first_byte = start (upload begins)
  - Metadata operations: first_byte = None (not applicable)
  - See s3dlio OPERATION_LOGGING.md for detailed explanation and limitations

### Changed

- **Updated s3dlio dependency** to local path (will switch to v0.9.22 git tag after release)
  - Added s3dlio-oplog workspace member dependency
  - Enables new client_id and first_byte tracking features

### Documentation

- **Enhanced USAGE.md** with oplog format documentation
  - Explained client_id field and clock synchronization
  - Documented first_byte tracking with clear limitations
  - Added link to comprehensive s3dlio OPERATION_LOGGING.md guide
  
- **Corrected oplog sorting documentation**
  - Removed references to non-existent S3DLIO_OPLOG_SORT environment variable
  - Clarified that oplogs are NOT sorted during capture (concurrent writes)
  - Added proper post-processing workflow using `sai3-bench sort` command
  - Updated: src/config.rs, src/bin/agent.rs, docs/USAGE.md, docs/CONFIG_SYNTAX.md
  - Added: docs/OPLOG_SORTING_CLARIFICATION.md (comprehensive guide)
  - Note: Sorted oplogs compress ~30-40% better than unsorted
  
- **Important**: first_byte is an *approximation* due to ObjectStore trait limitations
  - Use for: Throughput analysis, relative comparisons, small object benchmarking
  - Don't use for: Precise TTFB metrics on large objects (>10MB)

### Testing

- ✅ Verified client_id populated in standalone mode ("standalone")
- ✅ Verified SAI3_CLIENT_ID env var override works
- ✅ Verified first_byte timestamps present for GET operations
- ✅ Verified first_byte empty for LIST (metadata-only) operations
- ✅ Verified oplog sorting: Unsorted (319KB) → Sorted (199KB, 38% reduction)

### Migration Notes

**No breaking changes.** Existing oplogs continue to work (client_id was always present but empty before).

**New capabilities**:
- Set custom client_id via SAI3_CLIENT_ID environment variable
- first_byte field now populated (was empty before v0.9.22)
- Distributed agents automatically sync timestamps to controller

**Requirements**: s3dlio v0.9.22+ (will update dependency after s3dlio release)

---

## [0.8.6] - 2025-11-25

### Added

- **Prand data generation support** using s3dlio v0.9.21
  - New `fill: prand` option for pseudo-random data generation
  - 31% faster than `random` (1340µs vs 1954µs per operation)
  - **⚠️ WARNING**: Produces 87-90% compressible data (unrealistic for storage testing)
  - Use only when data generation CPU is a proven bottleneck

### Changed

- **Updated s3dlio dependency** from v0.9.10 to v0.9.21 (git tag)
  - Adds DataGenAlgorithm enum (Random, Prand)
  - Adds clock offset support for distributed op-log synchronization
  - See s3dlio v0.9.21 changelog for full details

### Documentation

- **Added performance comparison** to DATA_GENERATION.md
  - Measured compressibility: random 0%, prand 90%, zero 100%
  - Measured latency: random 1954µs, prand 1340µs, zero 2910µs
  - **Clear guidance**: Always use `fill: random` for storage testing
  
- **Added Data Generation section** to USAGE.md
  - Performance comparison table with measured metrics
  - Explanation of why compressibility matters for benchmarking
  - When to use each fill method

- **Enhanced DATA_GENERATION.md**
  - Added "Storage Test Quality" column to comparison table
  - Clarified that high compressibility is BAD for storage testing
  - Updated recommendations to strongly prefer `random` over `prand`

### Testing

- ✅ Validated all three fill methods (random, prand, zero)
- ✅ Compression test: 64KB samples with zstd -19
  - Random: 0% compressed (truly incompressible)
  - Prand: 90% compressed (unrealistic)
  - Zero: 100% compressed (completely unrealistic)
- ✅ Performance test: prepare phase metrics extraction
- ✅ All 148 tests pass, zero warnings

### Migration Notes

**No breaking changes.** Existing configs work without modification.

**New fill option**: `fill: prand` now available but NOT recommended for storage testing. Continue using `fill: random` (or omit fill parameter, as `random` is now the effective default for realistic testing).

**s3dlio upgrade**: Using s3dlio v0.9.21 from GitHub (git tag dependency).

---

## [0.8.5] - 2025-11-24

### Major Changes: Bidirectional Streaming Architecture

**This release fundamentally improves distributed execution reliability** by replacing unidirectional streaming with a bidirectional architecture featuring separate control and stats channels.

### Added

- **Bidirectional streaming RPC** (`ExecuteWorkload`) with separate control and stats channels
  - Control channel: Controller → Agent (PING, START, ABORT commands)
  - Stats channel: Agent → Controller (READY, RUNNING, COMPLETED status)
  - Non-blocking: Agent can send stats while waiting for control messages
  
- **Clock synchronization testing infrastructure** (`TESTING_CLOCK_SYNC.md`)
  - Simulated clock skew testing
  - Coordinated start verification
  - Test scripts: `test_clock_sync.sh`, `test_coordinated_start.sh`

- **Consolidated timeout constants** in `src/constants.rs`
  - 13 timeout constants centralized from scattered locations
  - Single source of truth for all distributed timing parameters

- **Prepare phase concurrency improvements**
  - Now uses workload concurrency value instead of hardcoded 32
  - Better parallelism control during object pre-population

### Fixed

- **Critical: Repeated READY messages bug** 
  - Old: Agents sent READY status every second (keepalive in single channel)
  - New: Agents send READY exactly once, wait silently for coordinated start
  - Agents now start within milliseconds of each other

- **Clock offset adjustment bug**
  - Removed incorrect clock offset subtraction from absolute timestamps
  - Coordinated start now uses controller's reference time correctly

- **Controller blocking during prepare phase**
  - Old: Single stream blocked while agents ran prepare phase
  - New: Bidirectional streams allow stats updates during any phase

### Changed

- **Controller RPC**: `run_workload_with_live_stats` → `execute_workload` (bidirectional)
- **Agent state machine**: Simplified to 3 states (Idle → Ready → Running)
- **Protocol buffer**: Added `ControlMessage` enum with PING/START/ABORT commands
- **Status codes**: Added ABORTED (5) and ACKNOWLEDGE (6) for better control flow

### Documentation

- **Added**: `BIDIRECTIONAL_STREAMING.md` - Comprehensive architecture guide
  - State machines (agent + controller)
  - Communication model and RPC design
  - Testing results and troubleshooting
  
- **Removed**: 7 obsolete implementation docs (consolidated into single guide)
  - STATE_MACHINES.md, STATE_TRANSITION_RECOVERY_ANALYSIS.md
  - AGENT_STATE_MACHINE.md, CONTROLLER_STATE_MACHINE.md
  - TWO_CHANNEL_IMPLEMENTATION_PLAN.md, PHASE4_IMPLEMENTATION_STATUS.md
  - ROBUSTNESS_ANALYSIS.md

- **Added**: `TESTING_CLOCK_SYNC.md` - Clock synchronization testing guide

### Testing

- ✅ All 148 tests pass (55 unit + 93 integration)
- ✅ Zero compiler warnings
- ✅ Distributed test: 2 agents, 105K operations, 10.1 GiB/s
- ✅ Synchronization: Agents start within 0.55 ms
- ✅ Prepare metrics: Correctly collected and aggregated

### Migration Notes

**No breaking changes to YAML configuration.** Existing config files work without modification.

**Controller binary name unchanged**: `sai3bench-ctl` continues to work as before.

**Internal protocol change**: Agents and controller must both be v0.8.5+ (not compatible with v0.8.4 or earlier).

### Performance

- Same throughput as v0.8.4 (no performance regression)
- Improved reliability under high load (no false positive timeouts)
- Better coordination (sub-millisecond synchronization)

---

## Version History

- **v0.8.5** (2025-11-24): Bidirectional streaming, improved reliability
- **v0.8.4** (2025-11-22): Clock synchronization foundation
- **v0.8.0-v0.8.3**: State machine enhancements, error handling
- **v0.7.x**: Directory trees, parallel prepare, distributed stats
- **v0.6.x**: Multi-host coordination, SSH deployment
- **v0.5.x**: Size distributions, workload replay
- **Earlier versions**: See [archive/CHANGELOG_v0.1.0-v0.8.4.md](archive/CHANGELOG_v0.1.0-v0.8.4.md)
