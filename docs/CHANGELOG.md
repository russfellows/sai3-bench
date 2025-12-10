# Changelog

All notable changes to sai3-bench are documented in this file.

**For historical changes (v0.1.0 - v0.8.4)**, see [archive/CHANGELOG_v0.1.0-v0.8.4.md](archive/CHANGELOG_v0.1.0-v0.8.4.md).

---

## [0.8.16] - 2025-12-10

### Added

- **Per-agent perf_log.tsv files** - Individual performance logs for each agent
  - Written to `agents/{agent-id}/perf_log.tsv` subdirectories
  - Each agent's metrics tracked independently with correct agent_id
  - Aggregate perf_log.tsv (agent_id="controller") contains merged totals
  - Sum of per-agent values exactly equals aggregate values (verified)

- **Synchronized perf_log timing** - All logs write at consistent intervals
  - Uses `tokio::time::interval` for exact timing regardless of stats arrival
  - Per-agent and aggregate logs write in same timer tick
  - Enables direct comparison across all perf_log files

- **Warmup timer reset for workload phase**
  - Warmup period (is_warmup flag) now measured from workload start, not prepare start
  - `elapsed_s` resets to 0 when workload phase begins
  - New `reset_warmup_for_workload()` method in `PerfLogDeltaTracker`
  - Prepare stage has its own elapsed_s timeline

- **New `test_parse_perf_log_no_path` test** for optional path field validation

### Changed

- **`PerfLogConfig.path` field now optional** - No longer required in YAML configs
  - Distributed mode ignores path (writes to results directory)
  - Added `#[serde(default)]` for backward compatibility

- **Renamed `console.log` to `console_log.txt`**
  - Avoids confusion with JavaScript console.log
  - Updated in controller.rs, results_dir.rs, and documentation

### Fixed

- **Aggregate perf_log interval accuracy** - Now writes at exact configured interval
  - Previously tied to stats message arrival timing (could drift)
  - Moved to dedicated interval timer in tokio::select!

---

## [0.8.15] - 2025-12-10

### Added

- **Performance Logging (perf-log) module** - New time-series metrics capture system
  - Captures interval-based aggregate metrics (default 1-second intervals)
  - 28-column TSV format with optional zstd compression
  - Delta-based metrics: ops, bytes, IOPS, throughput per interval
  - Latency percentiles: p50, p90, p99 for GET, PUT, and META operations
  - CPU utilization: user, system, and I/O wait percentages
  - Agent identification for distributed workload analysis
  - Warmup period flagging (is_warmup column) for filtering pre-measurement data
  - Stage tracking (prepare, workload, cleanup, listing)
  - New `PerfLogWriter`, `PerfLogEntry`, `PerfLogDeltaTracker` types
  - See [docs/PERF_LOG_FORMAT.md](./PERF_LOG_FORMAT.md) for complete specification

- **Extended LiveStatsSnapshot percentiles**
  - Added p90 and p99 percentiles to GET and PUT operations (kept p95 for compatibility)
  - Added p50, p90, p95, p99 percentiles to META operations
  - Enables finer-grained latency analysis in gRPC streaming stats

- **New configuration options**
  - `warmup_period: Duration` - Mark initial data as warmup (e.g., "10s")
  - `perf_log.path: String` - Output path for perf-log file
  - `perf_log.interval: Duration` - Sampling interval (default "1s")

- **New constants in `src/constants.rs`**
  - `DEFAULT_PERF_LOG_INTERVAL_SECS` = 1
  - `PERF_LOG_HEADER` - 28-column TSV header with full documentation

### Changed

- Moved `PERF_LOG_HEADER` constant to `constants.rs` for centralized configuration
- Updated module header comments to reference new documentation

---

## [0.8.14] - 2025-12-10

### Added

- **Distributed listing stage with progress updates**
  - New `STAGE_LISTING = 4` in proto and `WorkloadStage::Listing` enum variant
  - Listing phase now reports progress via LiveStatsTracker every 1000 files
  - Each agent lists only its assigned directories (distributed by tree manifest)
  - Uses streaming `list_stream()` API for real-time progress instead of blocking `list()`
  - Progress shows files found and rate (files/second)
  
- **New `list_existing_objects_distributed()` function**
  - Streaming list with progress callbacks
  - Distributes listing work across agents using tree manifest directory assignments
  - Each agent lists their assigned top-level directories depth-first
  - For 8 agents with 64 top-level directories, each agent lists 8 directories
  - Parses file indices during streaming for gap-aware creation
  
- **Robust error handling for LIST operations**
  - New `ListingErrorTracker` with same pattern as `PrepareErrorTracker`
  - Tracks total errors and consecutive errors with configurable thresholds
  - `DEFAULT_LISTING_MAX_ERRORS` = 50 total errors before abort
  - `DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS` = 5 consecutive errors before abort
  - Consecutive error counter resets on success (handles transient network issues)
  - Error messages collected for debugging (first 20 errors)
  - Listing aborts with detailed error report if thresholds exceeded
  - 12 new unit tests for ListingErrorTracker
  
- **New constants in `src/constants.rs`**
  - `DEFAULT_LISTING_MAX_ERRORS` = 50
  - `DEFAULT_LISTING_MAX_CONSECUTIVE_ERRORS` = 5
  - `LISTING_PROGRESS_INTERVAL` = 1000 (files between progress updates)

- **Total threads display in controller live stats**
  - Shows total concurrent workers across all agents during all stages
  - Added `concurrency` field to proto `LiveStats` message (field 35)
  - Added `concurrency: u32` to `LiveStatsTracker` and `LiveStatsSnapshot`
  - New `LiveStatsTracker::new_with_concurrency(u32)` constructor
  - Controller aggregates concurrency from all agents into `total_concurrency`
  - Display format: "8 of 8 Agents (512 threads)" when total_concurrency > 0
  - Visible during prepare, workload, cleanup, and listing stages

### Changed

- **Replaced blocking list calls in prepare phase**
  - `prepare_sequential` and `prepare_parallel` now use distributed listing
  - Progress visible in controller during long listing operations (can take 1+ hour)
  - Listing rate displayed in logs: "Listing progress: N files found (X/s)"

### Fixed

- **Agent state machine race condition on workload completion**
  - Fixed race condition between stats writer and control reader tasks
  - When workload completes, stats writer sends COMPLETED message and waits 3s for flush
  - Controller receives COMPLETED, closes stream (normal disconnect)
  - Control reader previously detected disconnect while state still "Running" → treated as abnormal
  - **Root cause**: Two concurrent tasks both try to transition state after disconnect
  - **Fix 1**: Added `completion_sent` flag to track when COMPLETED message was sent
  - Control reader now checks flag: if completion_sent && state==Running → normal disconnect
  - **Fix 2**: Same-state transitions are now valid no-ops (defensive programming)
  - `transition_to()` returns Ok() early if `*state == new_state`
  - Added `Idle → Idle` and `Failed → Failed` as valid transitions in `can_transition()`
  - New methods: `mark_completion_sent()`, `is_completion_sent()`, `reset_completion_sent()`
  - Eliminates spurious ERROR logs: "Invalid state transition: Idle → Idle"
  - Eliminates spurious WARN logs: "Abnormal disconnect during Running state"

- **Critical: Distributed cleanup double-filtering bug**
  - **Symptom**: Cleanup running at ~900 ops/s instead of expected ~20,000 ops/s (8 agents × 2,500 each)
  - **Root cause**: Double-filtering in cleanup phase
    - `prepare_objects()` returns only THIS agent's prepared objects (correctly filtered)
    - `cleanup_prepared_objects()` then re-filtered by list index % num_agents
    - Result: Each agent only deleted 1/N² of their objects instead of 1/N
  - **Fix**: Removed modulo filtering from `cleanup_prepared_objects()` in `cleanup.rs`
    - Caller is now responsible for passing the correct subset of objects
    - In distributed mode: `prepare_objects()` already filters
    - In cleanup-only mode: `list_existing_objects()` now filters by file index
  - **Updated `list_existing_objects()`** to filter by file index modulo
    - Tree mode: Parses `file_NNNNNNNN.dat` and filters by `file_idx % num_agents == agent_id`
    - Flat mode: Parses `prepared-NNNNNNNN.dat` and filters similarly
  - Cleanup performance should now scale linearly with number of agents

- **Directory structure cleanup for tree mode**
  - After file cleanup, agent 0 now cleans up any remaining directory markers
  - Handles GCS folder objects, `.keep` files, and other placeholder objects
  - Uses tree manifest to determine the tree root URI
  - Lists remaining objects under the tree prefix and deletes them
  - Only agent 0 performs this to avoid race conditions between agents
  - Ensures complete cleanup leaving no empty directories or markers

### Testing

- **New unit tests for ListingErrorTracker (12 tests)**
  - `test_listing_error_tracker_new` - Initial state
  - `test_listing_error_tracker_with_thresholds` - Custom thresholds
  - `test_listing_error_tracker_record_error` - Error recording
  - `test_listing_error_tracker_record_success_resets_consecutive` - Reset behavior
  - `test_listing_error_tracker_total_threshold` - Total error abort
  - `test_listing_error_tracker_consecutive_threshold` - Consecutive error abort
  - `test_listing_error_tracker_consecutive_reset_prevents_abort` - Recovery from errors
  - `test_listing_error_tracker_get_error_messages` - Error message collection
  - `test_listing_error_tracker_clone` - Arc-based cloning
  - `test_listing_error_tracker_thread_safety` - Multi-threaded safety
  - `test_listing_error_tracker_default` - Default trait implementation
  - `test_listing_result_default` - ListingResult default

- **New unit tests for Agent state machine (17 tests)**
  - State transition validation (6 tests):
    - `test_can_transition_valid_normal_flow` - Idle→Ready→Running→Idle
    - `test_can_transition_valid_error_flow` - Error transitions
    - `test_can_transition_valid_abort_flow` - Abort transitions
    - `test_can_transition_same_state_noop` - Idle→Idle, Failed→Failed
    - `test_can_transition_invalid_transitions` - Invalid transitions return false
    - `test_can_transition_all_same_state_except_running_ready_aborting` - Explicit no-ops
  - Async transition_to() tests (4 tests):
    - `test_transition_to_valid_transition` - Valid transition succeeds
    - `test_transition_to_invalid_transition` - Invalid transition fails
    - `test_transition_to_same_state_noop` - Same-state is no-op
    - `test_transition_to_failed_same_state_noop` - Failed→Failed is no-op
  - Completion sent flag tests (4 tests):
    - `test_completion_sent_initial_state` - Starts false
    - `test_completion_sent_mark_and_check` - Mark and check
    - `test_completion_sent_reset` - Reset clears flag
    - `test_completion_sent_shared_across_clones` - Arc sharing
  - Race condition scenario tests (3 tests):
    - `test_race_condition_scenario_success_completion` - completion_sent prevents false abort
    - `test_race_condition_scenario_control_reader_wins` - Idle→Idle no-op
    - `test_race_condition_scenario_error_path` - Failed→Failed no-op

---

## [0.8.13] - 2025-12-09

### Fixed

- **Critical: Controller error detection for distributed mode**
  - Controller now checks `status == 3` (ERROR) BEFORE `completed` flag
  - Previously, agent errors during prepare phase were never detected because agents send `completed: false` with ERROR status
  - Added handling for `status == 5` (ABORTED) in workload loop
  - Added `status == 5` handling in startup phase (edge case)
  - Added defensive check for `status == 4` (COMPLETED) with mismatched `completed` flag
  
- **Missing state transition: Preparing → Failed**
  - Controller can now properly transition agents from Preparing to Failed on errors
  - Previously this transition was undefined, causing state machine errors
  
- **Added Disconnected → Failed state transition**
  - Allows proper error handling when agents reconnect with error status

### Added

- **Resilient prepare phase with error thresholds**
  - New `PrepareErrorTracker` in `src/prepare.rs`
  - Agents continue on I/O errors until threshold exceeded
  - `DEFAULT_PREPARE_MAX_ERRORS` = 100 total errors before abort
  - `DEFAULT_PREPARE_MAX_CONSECUTIVE_ERRORS` = 10 consecutive errors before abort
  - Failed objects logged at debug level, summary at warn level
  - Applied to both `prepare_sequential` and `prepare_parallel` functions

- **Exponential backoff retry for all operations**
  - New `retry_with_backoff()` function in `src/workload.rs`
  - Configurable: initial delay (100ms), max delay (10s), multiplier (2.0)
  - 10% jitter to prevent thundering herd on recovery
  - Backoff resets on success - doesn't stay in backed-off state
  - Applied to PUT operations in prepare phase
  - New `ErrorHandlingConfig` fields: `initial_retry_delay`, `max_retry_delay`, `retry_backoff_multiplier`
  
- **Workload consecutive error threshold**
  - New `DEFAULT_MAX_CONSECUTIVE_ERRORS` = 10 in `constants.rs`
  - Prevents runaway failures when backend is completely unreachable

- **Comprehensive error handling tests** (23 new tests)
  - 12 tests for `PrepareErrorTracker` (consecutive reset, thresholds, thread safety)
  - 11 tests for retry logic (exponential backoff, jitter, success after failures)

### Changed

- **Downgraded h2 protocol error logging**
  - Stream errors after workload completion logged at `debug` instead of `info`
  - These are normal gRPC stream closures, not actual errors

---

## [0.8.12] - 2025-12-09

### Added

- **Custom endpoint documentation** in `docs/USAGE.md`
  - Environment variables for S3, Azure, and GCS custom endpoints
  - Usage examples for MinIO, Azurite, fake-gcs-server
  - Multi-protocol proxy configuration

### Fixed

- **Distributed error transmission race condition** - Agent now waits before closing stream
  - Added `AGENT_ERROR_FLUSH_DELAY_SECS` constant (5 seconds default)
  - Delay applied after sending ERROR, COMPLETED, or ABORTED status
  - Prevents controller from missing critical status messages

### Changed

- **Updated s3dlio to v0.9.25** with Azure/GCS custom endpoint support
  - `AZURE_STORAGE_ENDPOINT` / `AZURE_BLOB_ENDPOINT_URL` for Azure
  - `GCS_ENDPOINT_URL` / `STORAGE_EMULATOR_HOST` for GCS
  - S3 `force_path_style` for S3-compatible endpoints

---

## [0.8.11] - 2025-12-05

### Added

- **Agent progress bars** for distributed mode visibility
  - Prepare phase: Progress bar showing object count and rate
  - Workload phase: Spinner with GET/PUT operation stats
  - Proper cleanup on completion, error, disconnect, or abort

- **Azure/GCS custom endpoint support** with new test infrastructure
  - New test modules: `azure_tests.rs`, `gcs_tests.rs`, `file_tests.rs`, `s3_tests.rs`
  - Environment-driven endpoint configuration for private clouds
  - RangeEngine enable/disable testing for both backends

### Fixed

- **Distributed abort handling** - Agent now properly responds to controller abort signals
  - Wrapped all execution phases in `tokio::select!` with abort check
  - Stats writer task checks abort channel and exits cleanly
  - Agents send proper error status when aborted

- **SIGINT handler panic** - Fixed double-await bug in controller shutdown
  - Changed `shutdown_signal.await` to `sig = &mut shutdown_signal` pattern
  - Prevents panic when user presses Ctrl+C during distributed execution

### Changed

- **Removed legacy gRPC RPCs** (~824 lines removed)
  - Removed: `RunPrepare`, `RunWorkload`, `GetPrepareStats`, `GetWorkloadStats`, `Cleanup`, `Abort`
  - Single bidirectional stream `ExecuteWorkload` now handles all functionality
  - Cleaner codebase with reduced maintenance burden

### Code Quality

- **Zero clippy warnings** across entire codebase
  - Fixed needless_range_loop warnings with enumerate()/iter()
  - Fixed empty_line_after_doc_comments in tests
  - Fixed collapsible_match patterns
  - Fixed bool_assert_comparison in tests
  - Added `#[allow(clippy::too_many_arguments)]` where appropriate
  - Renamed `default()` to `with_default_config()` for clarity

### Documentation

- **Updated copilot-instructions.md** with critical guidelines:
  - NEVER create YAML test configs from scratch
  - ALWAYS use `./scripts/start_local_agents.sh` for agent testing
  - ALWAYS validate configs with `--dry-run` before running

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
