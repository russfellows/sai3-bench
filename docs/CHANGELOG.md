# Changelog

All notable changes to sai3-bench are documented in this file.

**For historical changes (v0.1.0 - v0.8.4)**, see [archive/CHANGELOG_v0.1.0-v0.8.4.md](archive/CHANGELOG_v0.1.0-v0.8.4.md).

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
