# Large-Scale Timeout Analysis (300k+ Dirs, 64M+ Files)

**Created**: February 5, 2026  
**Status**: Active Documentation  
**Context**: 4-agent, 16-endpoint, 331k directory, 64M file test scenario

---

## Executive Summary

When running at extreme scale (331,776 directories, 64M files across 16 mount points), several operations can take **minutes to hours**. Our analysis reveals **3 critical timeout bottlenecks** that require configuration changes for large-scale deployments:

1. **Barrier timeout (30s hardcoded)** → Too short for tree generation (90s)
2. **gRPC keep-alive (40s total)** → Can drop connections during long LIST operations
3. **Default barrier sync config (110s)** → Barely sufficient, needs 300s+ for safety

**Recommendation**: Use custom barrier_sync config with extended timeouts for large-scale operations.

---

## Scale Analysis

### 3 Test Scales

| Scale | Width | Depth | Directories | Files | Est. Tree Gen | Est. LIST Time | Est. CREATE Time |
|-------|-------|-------|------------|-------|---------------|----------------|------------------|
| **Simple** | 24 | 2 | 576 | 111k | 1s | 11s | 111s (2 min) |
| **Medium** | 24 | 3 | 13,824 | 2.7M | 5s | 267s (4.5 min) | 2,668s (45 min) |
| **Large** | 24 | 4 | 331,776 | 64M | 90s | 6,403s (107 min) | 64,033s (18 hours) |

**Key Insight**: Tree generation time scales with `width^depth`, but remains manageable (90s for 331k dirs). The REAL problem is LIST and CREATE operations at scale.

---

## Critical Timeout Paths

### 1. Agent Barrier Timeout (HARDCODED) ⚠️

**Location**: `src/bin/agent.rs:3300`

```rust
let barrier_timeout = std::time::Duration::from_secs(30);  // TODO: Make configurable
let max_barrier_retries = 5;
```

**Problem**:   - Each barrier wait has 30s timeout
- With 5 retries: 30s × 5 = 150s max total
- Tree generation for 331k dirs can take 90s
- **If tree generation takes > 30s, first timeout triggers retry**
- **If tree generation takes > 150s, all retries exhausted → FAILURE**

**Impact at Scale**:
- ✅ Simple (1s tree gen): No problem
- ✅ Medium (5s tree gen): No problem  
- ⚠️ Large (90s tree gen): **Works but triggers 3 retries**, wastes time
- ❌ Large + slow hardware (120s tree gen): **FAILS after 5 retries**

**Solution**:
```yaml
distributed:
  barrier_sync:
    enabled: true
    prepare:
      heartbeat_interval: 120  # 2 minutes
      missed_threshold: 10     # Allow 10 missed = 20 minutes total
```

**Code Change Needed**:
```rust
// Make barrier_timeout configurable via StageConfig or BarrierSyncConfig
let barrier_timeout = stage.get_timeout()
    .unwrap_or(std::time::Duration::from_secs(30));
```

---

### 2. gRPC Keep-Alive Timeouts

**Location**: `src/bin/controller.rs:815-816`

```rust
.http2_keep_alive_interval(Duration::from_secs(30))  // Send PING every 30s
.keep_alive_timeout(Duration::from_secs(10))         // Wait 10s for PONG
```

**Total Timeout**: 30s (no PING) + 10s (PONG timeout) = **40 seconds**

**Problem**:
- If agent is busy for >40s without sending stats **→ gRPC connection drops**
- Tree generation (90s) sends progress updates every 5000 dirs (~1-2s intervals) → **OK**
- LIST operations send progress every 1000 files → **Depends on listing speed!**

**Listing Speed Analysis**:
- At 10k files/sec: 1000 files = 0.1s between updates → **OK**
- At 1k files/sec: 1000 files = 1s between updates → **OK**
- At 100 files/sec: 1000 files = 10s between updates → **OK**
- At 10 files/sec: 1000 files = 100s between updates → **FAILS (> 40s)**

**Recommendation**: LIST operations must maintain > 25 files/sec to avoid gRPC timeout. If listing is slower:
- Reduce `LISTING_PROGRESS_INTERVAL` (currently 1000) to 500 or 250
- Or increase gRPC keep-alive interval to 120s

---

### 3. Barrier Sync Default Config

**Location**: `src/config.rs:1682-1686`

```rust
fn default_heartbeat_interval() -> u64 { 30 }
fn default_missed_threshold() -> u32 { 3 }
fn default_query_timeout() -> u64 { 10 }
fn default_query_retries() -> u32 { 2 }
```

**Total Timeout Calculation**:
```
Heartbeat timeout = interval × threshold = 30s × 3 = 90s
Query timeout = timeout × retries = 10s × 2 = 20s
Total = 90s + 20s = 110 seconds
```

**Problem**:
- 331k tree generation takes ~90s
- Default 110s timeout gives only **20s margin** → **Too tight!**
- Any slowness (slow disk, CPU contention) → TIMEOUT

**Recommended Config for Large Scale**:
```yaml
distributed:
  barrier_sync:
    enabled: true
    
    # PREPARE phase: Can take > 90s for tree generation
    prepare:
      type: all_or_nothing
      heartbeat_interval: 120    # 2 minutes between heartbeats
      missed_threshold: 10       # 10 missed = 20 minutes total
      query_timeout: 60          # 1 minute per query
      query_retries: 5           # 5 retries = 5 minutes query time
    
    # EXECUTE phase: Standard config OK
    execute:
      type: all_or_nothing
      heartbeat_interval: 30
      missed_threshold: 3
      
    # CLEANUP phase: Can take hours for 64M files
    cleanup:
      type: best_effort          # Don't block on slow cleanup
      heartbeat_interval: 300    # 5 minutes
      missed_threshold: 12       # 60 minutes total
```

**Total Timeout with Recommended Config**:
- Prepare: 120s × 10 + 60s × 5 = **1,200s heartbeat + 300s query = 1,500s (25 min)**
- Cleanup: 300s × 12 + 60s × 5 = **3,600s heartbeat + 300s query = 3,900s (65 min)**

---

## Operation Timelines

### Prepare Phase (With 331k Tree, 64M Files)

```
T+0s:     Controller sends START command
T+1s:     Agents receive config, parse YAML
T+2s:     Agents start tree generation (331k directories)
T+10s:    Progress: 50,000/331,776 dirs (15%)
T+20s:    Progress: 100,000/331,776 dirs (30%)
T+30s:    Progress: 150,000/331,776 dirs (45%) ⚠️ First barrier timeout check
T+40s:    Progress: 200,000/331,776 dirs (60%)
T+50s:    Progress: 250,000/331,776 dirs (75%)
T+60s:    Progress: 300,000/331,776 dirs (90%) ⚠️ Second barrier timeout (retry 1)
T+90s:    Progress: 331,776/331,776 dirs (100%) ✅ Tree generation complete
T+92s:    Agent transitions to AtStage{ready_for_next: true}
T+93s:    Agent sends barrier notification with at_barrier=true
T+94s:    Controller receives barrier notification
T+95s:    Controller checks barrier (all agents ready)
T+96s:    Controller broadcasts RELEASE_BARRIER
T+97s:    Agents receive RELEASE_BARRIER, proceed to next stage
```

**With Default Config** (30s timeout, 5 retries):
- Timeouts at: T+30s, T+60s, T+90s (3 retries consumed)
- Succeeds at: T+92s (still has 2 retries left)
- **Result**: WORKS but inefficient (unnecessary retries)

**With Recommended Config** (120s timeout):
- No timeouts triggered
- Completes at: T+92s
- **Result**: EFFICIENT, no wasted retries

---

## Progress Reporting Intervals

All long-running operations send progress updates to avoid timeout:

| Operation | Progress Interval | Constant | Location |
|-----------|------------------|----------|----------|
| **Tree Generation** | Every 5,000 dirs | (hardcoded) | `src/directory_tree.rs:156` |
| **LIST Operations** | Every 1,000 files | `LISTING_PROGRESS_INTERVAL` | `src/constants.rs:92` |
| **CREATE Operations** | Every file (via tracker) | N/A | `src/prepare.rs` |
| **Agent Heartbeat** | Every 1 second | `DEFAULT_AGENT_HEARTBEAT_SECS` | `src/constants.rs:236` |

**Tree Generation Progress** (331k dirs):
- Updates: 331,776 / 5,000 = 66 progress reports
- Frequency: 90s / 66 = 1.36s per update
- **Result**: Frequent enough to keep gRPC alive (< 30s)

**LIST Operations Progress** (64M files at 10k/sec):
- Updates: 64,032,768 / 1,000 = 64,032 progress reports  
- Frequency: 6,403s / 64,032 = 0.1s per update
- **Result**: Very frequent, no risk of timeout

**LIST Operations at Slow Speed** (64M files at 10/sec):
- Frequency: 1,000 files / 10 files/sec = 100s per update
- **Result**: **EXCEEDS 40s gRPC keep-alive → CONNECTION DROP** ❌

---

## Test Results

From `cargo test --test test_large_scale_timeouts`:

### Tree Size Calculations ✅
```
✓ Tree 24^2 = 576 dirs × 193 files/dir = 111,168 total files
✓ Tree 24^3 = 13,824 dirs × 193 files/dir = 2,668,032 total files  
✓ Tree 24^4 = 331,776 dirs × 193 files/dir = 64,032,768 total files
```

### Default Timeout Analysis ⚠️
```
⚠ DEFAULT prepare timeout: 90s heartbeat + 20s query = 110s total
⚠ WARNING: Default 110s timeout may be insufficient for 331k tree (can take 90s+)
   Recommendation: Use custom barrier_sync config with longer timeouts
```

### Recommended Timeout Config ✅
```
✓ Prepare phase timeout: 1200s heartbeat + 300s query = 1500s total (25.0 min)
```

### Progress Reporting ✅
```
✓ Listing progress for 64M files:
  64,032 updates (every 1000 files)
  Update every 0.10s @ 10k files/sec
  Agent heartbeat: every 1s
```

### Multi-Agent Configuration ✅
```
✓ 4-agent distributed config validated:
  4 agents × 4 endpoints = 16 total mount points
  Concurrency per agent: 16
  Total concurrency: 64
```

---

## Recommended Actions

### 1. Immediate (Config Changes)

Create test configs with extended timeouts for large-scale testing:

**File**: `tests/configs/4host-test_LARGE_with_barriers.yaml`

```yaml
# Large-scale test with barrier sync enabled and extended timeouts
distributed:
  agents:
    # ... 4 agents with 16 endpoints ...
    
  barrier_sync:
    enabled: true
    
    # Extend prepare phase timeout for 331k tree generation
    prepare:
      type: all_or_nothing
      heartbeat_interval: 120
      missed_threshold: 10
      query_timeout: 60
      query_retries: 5
    
    # Standard execute timeout
    execute:
      type: all_or_nothing
      heartbeat_interval: 30
      missed_threshold: 3
    
    # Best-effort cleanup (don't block on slow agents)
    cleanup:
      type: best_effort
      heartbeat_interval: 300
      missed_threshold: 12

prepare:
  prepare_strategy: parallel
  skip_verification: false
  force_overwrite: true
  
  directory_structure:
    width: 24
    depth: 4
    files_per_dir: 193
    distribution: "bottom"
    dir_mask: "d%d_w%d.dir"
  
  ensure_objects:
    - count: 64032768
      fill: random
      size_distribution:
        type: lognormal
        mean: 8MiB
        std_dev: 1MiB
        min: 1MiB
        max: 16MiB
      use_multi_endpoint: true
```

### 2. Short-Term (Code Changes)

#### A. Make Agent Barrier Timeout Configurable

**Location**: `src/bin/agent.rs:3300`

**Current**:
```rust
let barrier_timeout = std::time::Duration::from_secs(30);  // TODO: Make configurable
```

**Change To**:
```rust
// Read from stage config, defaulting to barrier_sync config, then 30s
let barrier_timeout = stage.timeout_secs
    .map(std::time::Duration::from_secs)
    .unwrap_or_else(|| {
        // Get from barrier sync config for this stage
        let barrier_config = config.distributed.barrier_sync.get_phase_config(&stage.name);
        let timeout_secs = barrier_config.heartbeat_interval * barrier_config.missed_threshold as u64;
        std::time::Duration::from_secs(timeout_secs)
    });
```

#### B. Add Listing Speed Monitor

**Location**: `src/prepare.rs:428`

**Add After Progress Update**:
```rust
// Warn if listing speed is too slow (< 25 files/sec = risk of gRPC timeout)
let listing_speed = rate; // files/sec
if listing_speed < 25.0 {
    warn!(
        "  ⚠️ Listing speed {:.1} files/sec is dangerously slow (< 25/sec). \
         Risk of gRPC timeout after 40s. Consider reducing LISTING_PROGRESS_INTERVAL.",
        listing_speed
    );
}
```

#### C. Increase gRPC Keep-Alive for Large Deployments

**Location**: `src/bin/controller.rs:815`

**Current**:
```rust
.http2_keep_alive_interval(Duration::from_secs(30))
.keep_alive_timeout(Duration::from_secs(10))
```

**Add Configuration**:
```rust
// Make configurable via DistributedConfig
let keep_alive_interval = config.grpc_keep_alive_interval_secs
    .unwrap_or(30);
let keep_alive_timeout = config.grpc_keep_alive_timeout_secs
    .unwrap_or(10);
    
.http2_keep_alive_interval(Duration::from_secs(keep_alive_interval))
.keep_alive_timeout(Duration::from_secs(keep_alive_timeout))
```

### 3. Long-Term (Architecture)

1. **Adaptive Timeouts**: Auto-detect scale and adjust timeouts dynamically
2. **Progress Streaming**: Stream progress during ALL long operations (not just tree/list)
3. **Stage-Specific Timeouts**: Each stage can declare expected min/max duration
4. **Timeout Profiling**: Track actual operation times and recommend config adjustments

---

## Failure Scenarios at Scale

### Scenario 1: Tree Generation Timeout (Default Config)

**Setup**: 4 agents, 331k dirs, default barrier config (110s total)

**Timeline**:
```
T+0:   Agents start tree generation
T+90s: Tree generation completes
T+110s: ⚠️ BARRIER TIMEOUT (only 20s margin)
```

**Result**: ✅ PASSES (barely)
**Risk**: **MEDIUM** - Any slowness causes failure

---

### Scenario 2: Slow Listing Speed

**Setup**: 64M files, listing at 10 files/sec (very slow)

**Timeline**:
```
T+0:   Agent starts LIST operation
T+100s: Progress update (1000 files @ 10/sec)
T+40s: ⚠️ gRPC keep-alive timeout (no stats for 40s)
T+40s: ❌ Controller drops connection
```

**Result**: ❌ FAILS
**Risk**: **HIGH** on slow storage or network

---

### Scenario 3: Multi-Hour Cleanup

**Setup**: 64M files, delete at 1k/sec

**Timeline**:
```
T+0:      Start cleanup
T+3600s:  Deleted 3.6M files (5.6% complete)
T+18000s: Deleted 18M files (28% complete) 
T+64033s: Cleanup complete (17.8 hours)
```

**With default config**: ❌ FAILS after 110s
**With best_effort cleanup**: ✅ PASSES (fast agents proceed, slow agents ignored)

---

## Configuration Examples

### Simple Scale (576 dirs, 111k files)

```yaml
distributed:
  barrier_sync:
    enabled: true
    # Can use defaults - 110s is plenty
```

### Medium Scale (13k dirs, 2.7M files) 

```yaml
distributed:
  barrier_sync:
    enabled: true
    prepare:
      heartbeat_interval: 60    # 60s × 3 = 180s (3 min)
      missed_threshold: 3
```

### Large Scale (331k dirs, 64M files)

```yaml
distributed:
  barrier_sync:
    enabled: true
    prepare:
      heartbeat_interval: 120   # 120s × 10 = 1200s (20 min)
      missed_threshold: 10
    cleanup:
      type: best_effort         # Don't wait for slow cleanup
      heartbeat_interval: 300   # 5 min
      missed_threshold: 12      # 60 min total
```

---

## Monitoring & Debugging

### Log Messages to Watch For

**Tree Generation Progress** (every 5000 dirs):
```
INFO Tree generation progress: 50000/331776 (15.1%)
INFO Tree generation progress: 100000/331776 (30.1%)
```

**Barrier Timeout Warnings** (agent retrying):
```
WARN Barrier retry 1/5 for 'stage_prepare' (sequence 0)
WARN Barrier 'stage_prepare' timeout (30s), incrementing to sequence 1
```

**Listing Speed Warnings**:
```
WARN  ⚠️ Listing speed 8.5 files/sec is dangerously slow (< 25/sec)
WARN  Risk of gRPC timeout after 40s
```

**gRPC Connection Drops**:
```
ERROR Agent agent-1 disconnected during prepare phase
ERROR h2 protocol error: connection closed
```

### Debug Commands

```bash
# Monitor agent logs for timeout issues
tail -f /var/log/sai3bench-agent.log | grep -E "timeout|retry|barrier"

# Check listing speed in real-time
tail -f /var/log/sai3bench-agent.log | grep "Listing progress" | awk '{print $NF}'

# Monitor gRPC keep-alive
tail -f /var/log/sai3bench-ctl.log | grep -E "PING|PONG|keep-alive"
```

---

## Summary

| Component | Default Timeout | Large-Scale Recommended | Reason |
|-----------|----------------|------------------------|--------|
| **Agent barrier wait** | 30s | 120s+ | Tree generation takes 90s |
| **Barrier sync (prepare)** | 110s total | 1500s (25 min) | Buffer for slow hardware |
| **gRPC keep-alive** | 30s PING + 10s | 120s PING + 20s | Prevent drops during slow LIST |
| **Listing progress** | Every 1000 files | Every 250 files | Ensure frequent updates |

**Key Takeaway**: Default timeouts work for **simple** and **medium** scales, but **large-scale** (300k+ dirs, 64M+ files) requires custom configuration to avoid failures.

