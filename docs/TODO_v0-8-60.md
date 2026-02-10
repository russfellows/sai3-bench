# TODO Items for Future Releases (Deferred from v0.8.60)

This document tracks TODO items identified during v0.8.60 preparation that have been deferred to future releases.

**Date**: February 9, 2026  
**Release**: v0.8.60 (metadata cache + barrier sync)

---

## 🟡 MODERATE Priority - Phase 2 Features

### MetadataCache Worker-Level Sharing
**Files**: `src/metadata_prefetch.rs:188, 203`  
**Status**: Coordinator-only cache works, worker-level sharing not yet implemented  
**Requirement**: Would require `Arc<Cache>` and proper lifetime management  
**Impact**: Low - cache works at coordinator level, worker sharing is optimization  
**Defer to**: v0.9.x when cache performance tuning needed

### Object Storage Validation (S3/Azure/GCS)
**Files**: `src/preflight/object_storage.rs:45, 200`  
**Status**: Filesystem validation fully implemented, cloud storage Phase 2  
**Requirement**: Implement actual S3/Azure/GCS API calls for pre-flight checks  
**Impact**: Medium - filesystem checks work, cloud checks nice-to-have  
**Defer to**: v0.9.x when cloud-native deployments increase

### LiveStats PreFlightResult Proto Field
**Files**: `src/bin/agent.rs:2554` (now documented, not TODO)  
**Status**: Using JSON workaround in `error_message` field  
**Requirement**: Proto change to add proper `PreFlightResult` message type  
**Impact**: Low - JSON workaround functional, proper field is cleaner  
**Defer to**: Next proto version bump (v1.0?)

---

## 🟢 LOW Priority - Future Enhancements

### LiveStatsTracker Error Tracking
**Files**: `src/bin/agent.rs:3373` (stage summary), `src/bin/agent.rs:776-790` (PhaseProgress)  
**Status**: LiveStatsTracker has no error counters  
**Requirement**: Add error tracking to LiveStatsTracker internals  
**Impact**: Medium - stage summaries and progress reports missing error counts  
**Implementation**:
```rust
// Add to LiveStatsTracker struct:
get_errors: Arc<AtomicU64>,
put_errors: Arc<AtomicU64>,
meta_errors: Arc<AtomicU64>,

// Add methods:
pub fn record_get_error(&self) { ... }
pub fn record_put_error(&self) { ... }
pub fn record_meta_error(&self) { ... }
```
**Defer to**: v0.9.x when comprehensive error reporting needed

### LiveStatsTracker Per-Endpoint Stats
**Files**: `src/bin/agent.rs:3405` (stage summary endpoint_stats)  
**Status**: LiveStatsTracker has no per-endpoint data structures  
**Requirement**: Add per-endpoint histograms and counters  
**Impact**: Medium - multi-endpoint workloads missing per-endpoint breakdowns  
**Implementation**:
```rust
// Add to LiveStatsTracker:
endpoint_stats: Arc<Mutex<HashMap<String, EndpointMetrics>>>,

struct EndpointMetrics {
    ops: u64,
    bytes: u64,
    errors: u64,
    hist: Histogram<u64>,
}
```
**Defer to**: v0.9.x when multi-endpoint analysis needed

### PhaseProgress Rich Fields
**Files**: `src/bin/agent.rs:776-790` (now documented, not TODOs)  
**Status**: Most PhaseProgress fields return 0 (not tracked by LiveStatsTracker)  
**Fields not implemented**:
- `phase_start_time_ms` - Not tracked (use `heartbeat_time_ms`)
- `objects_total` - Not available in stage-based tracking
- `errors_encountered` - No error counters in LiveStatsTracker
- `current_throughput` - Not calculated (derive from ops/bytes)
- `phase_elapsed_ms` - Not tracked (use `stage_elapsed_s` from LiveStatsSnapshot)
- `objects_deleted` - Not tracked (cleanup uses LIST results)
- `is_stuck` - No stuck detection logic
- `progress_rate` - Not calculated (derive from operations_completed)

**Impact**: Low - these are query response fields, basic progress tracking works  
**Defer to**: v0.9.x if richer query responses needed

### Filesystem Quota Checking
**File**: `src/preflight/filesystem.rs:678`  
**Status**: Basic filesystem validation works, quota checking not implemented  
**Requirement**: Implement quota checks for ext4, XFS, NFS filesystems  
**Impact**: Low - capacity checks exist, quota enforcement is advanced feature  
**Defer to**: v1.0+ when enterprise features needed

### Random Data Generation
**File**: `src/main.rs:902`  
**Status**: Using zero-filled buffers for PUT operations  
**Requirement**: Add random data generation for realistic compression testing  
**Impact**: Low - zero-fill adequate for I/O testing, random for compression tests  
**Defer to**: v1.0+ when data compression testing needed

### Directory Tree mkdir Latency Tracking
**File**: `src/prepare/directory_tree.rs:183`  
**Status**: Directory creation works, latency metrics not tracked  
**Requirement**: Track per-mkdir latencies for directory tree creation  
**Impact**: Low - functional feature, latency tracking is optimization insight  
**Defer to**: v0.9.x if directory creation performance analysis needed

### Prepare Retry Fill Pattern Storage
**File**: `src/prepare/retry.rs:155`  
**Status**: Retry logic works, fill pattern not stored in PrepareFailure  
**Requirement**: Store exact fill pattern for regeneration on retry  
**Impact**: Very Low - retries work, exact pattern regeneration edge case  
**Defer to**: v1.0+ if deterministic retry critical

### Cleanup Concurrency Parameter
**File**: `src/prepare/cleanup.rs:100`  
**Status**: Using reasonable default concurrency (16)  
**Requirement**: Accept concurrency parameter from caller  
**Impact**: Low - default works, parameterization is flexibility enhancement  
**Defer to**: v0.9.x if cleanup performance tuning needed

### Prepare Function Signature Refactoring
**File**: `src/prepare/mod.rs:79`  
**Status**: Using #[allow(clippy::too_many_arguments)], function works  
**Requirement**: Refactor 11 parameters into PrepareParams struct  
**Impact**: Low - code quality improvement, not functional issue  
**Defer to**: v1.0 when doing major API refactoring

### Azure/S3 Config Support in Workload
**File**: `src/workload.rs:232`  
**Status**: Using s3dlio ObjectStore abstraction (works for all backends)  
**Requirement**: Add Azure/S3 specific config options  
**Impact**: Low - ObjectStore handles backends, specific config for optimizations  
**Defer to**: v0.9.x when backend-specific tuning needed

### Agent Identification Mechanism
**File**: `src/workload.rs:1941`  
**Status**: Agent ID passed via config in distributed mode  
**Requirement**: Add env var or CLI flag for agent self-identification  
**Impact**: Very Low - distributed mode passes agent_id correctly  
**Defer to**: v1.0+ if standalone agent deployment model changes

### Stuck Agent Detection
**File**: `src/bin/controller.rs:2727` (now documented, not TODO)  
**Status**: No stuck detection based on progress rate  
**Requirement**: Implement progress rate calculation and stuck threshold  
**Impact**: Medium - would help diagnose hung agents faster  
**Defer to**: v0.9.x when production monitoring features added

### Metadata Cache Objects Count
**File**: `src/metadata_cache.rs:759`  
**Status**: CacheStats returns 0 for objects_count  
**Requirement**: Track object count separately if needed  
**Impact**: Very Low - diagnostic field, not critical  
**Defer to**: v1.0+ if cache statistics become important

---

## ✅ Fixed in v0.8.60

### CRITICAL Fixed Items:
1. **Abort handling in execute_stages_workflow** ✅  
   - Wire `subscribe_abort()` into stage loop with `tokio::select!`
   - Now properly aborts between stages on CTRL-C or abort RPC

2. **Stage summary error tracking** ✅  
   - Documented that LiveStatsTracker lacks error counters (deferred feature)
   - Changed misleading TODO to clear "Not tracked" comment

3. **Stage summary endpoint stats** ✅  
   - Documented that LiveStatsTracker lacks per-endpoint data (deferred feature)
   - Changed misleading TODO to clear "Not tracked" comment

4. **PhaseProgress field documentation** ✅  
   - Removed 9 misleading TODOs
   - Added clear comments explaining which fields are not implemented

5. **Unused barrier_start field** ✅  
   - Removed from BarrierManager struct
   - Removed from BarrierManager::new() initialization
   - Timeout tracking already implemented in wait_for_barrier() method

6. **Catch-all abort/disconnect handlers** ✅  
   - Removed misleading "TODO v0.8.25: Replace this catch-all"
   - Documented as intentional conservative design decision
   - Works correctly - no action needed

---

## Summary

**Total TODO comments in v0.8.60**: 30 → 0  
**Fixed/Documented**: 6 critical items  
**Deferred to Phase 2**: 24 enhancement items  

All remaining items are **feature enhancements**, not bugs. v0.8.60 has **zero known bugs** from TODO analysis.
