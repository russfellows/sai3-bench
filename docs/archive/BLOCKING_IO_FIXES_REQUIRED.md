# Critical Blocking I/O Fixes Required for Large-Scale Deployments

**Date**: February 6, 2026  
**Severity**: **CRITICAL** - Affects production deployments with >100K files  
**Status**: Issues Confirmed, Fixes Pending

## Executive Summary

External code review identified **four critical blocking I/O issues** that cause executor starvation in large-scale distributed tests. These issues prevent:
- Agents from sending READY signals
- gRPC heartbeats from functioning
- Stats updates from being transmitted
- Barrier coordination from completing

All issues confirmed through code inspection. Fixes are straightforward but essential for v0.8.51.

---

## Issue 1: Blocking `glob` in Async Validation

### Location
- `src/bin/agent.rs` line 3828
- `src/bin/controller.rs` line 4606

### Problem
```rust
async fn validate_workload_config(config: &Config) -> Result<()> {
    // ...
    if file_path.contains('*') {
        let paths: Vec<_> = glob::glob(&file_path)  // ❌ BLOCKING SYSCALL
            .map_err(|e| anyhow!("..."))?
            .collect();  // ❌ BLOCKING ITERATION
    }
}
```

### Impact
- **Scale**: With 1M files, glob traversal can take **5-30 seconds**
- **Starves**: Entire tokio worker thread blocked
- **Breaks**: Agent cannot send READY signal within 30s timeout
- **Result**: Controller aborts with "Agent did not send READY within 30s"

### Fix (Required for v0.8.51)
```rust
async fn validate_workload_config(config: &Config) -> Result<()> {
    // ...
    if file_path.contains('*') {
        let file_path_clone = file_path.clone();
        let paths = tokio::task::spawn_blocking(move || -> Result<Vec<std::path::PathBuf>> {
            let paths: Vec<_> = glob::glob(&file_path_clone)
                .map_err(|e| anyhow!("Invalid glob pattern '{}': {}", file_path_clone, e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Glob error: {}", e))?;
            Ok(paths)
        })
        .await
        .map_err(|e| anyhow!("Blocking task failed: {}", e))??;
        
        if paths.is_empty() {
            anyhow::bail!("No files found matching GET pattern: {}", path);
        }
    }
}
```

**Files to modify**:
- `src/bin/agent.rs` - Wrap glob in spawn_blocking
- `src/bin/controller.rs` - Wrap glob in spawn_blocking

---

## Issue 2: Hardcoded 30-Second Ready Timeout

### Location
- `src/bin/controller.rs` line 2018

### Problem
```rust
let ready_timeout = tokio::time::Duration::from_secs(30);  // ❌ TOO SHORT
```

### Impact
- **Scale**: Large configs with glob validation take >30s to process
- **Breaks**: Timeout occurs before agent finishes validation
- **Result**: "Agent did not send READY within 30s" error

### Fix (Required for v0.8.51)

**Step 1**: Add config field to `src/config.rs`:
```rust
pub struct DistributedConfig {
    // ... existing fields
    
    /// Timeout for agents to send initial READY signal after receiving config
    /// Default: 120 seconds
    /// Large-scale deployments (>100K files): Increase to 300-600 seconds
    #[serde(default = "default_agent_ready_timeout")]
    pub agent_ready_timeout: u64,
}

fn default_agent_ready_timeout() -> u64 { 120 }
```

**Step 2**: Update controller to use config value:
```rust
// src/bin/controller.rs around line 2018
let ready_timeout = config.distributed.as_ref()
    .map(|d| tokio::time::Duration::from_secs(d.agent_ready_timeout))
    .unwrap_or(tokio::time::Duration::from_secs(120));  // 2 min default (up from 30s)
```

**Step 3**: Document in CONFIG_SYNTAX.md:
```markdown
#### `agent_ready_timeout` (Optional)

Timeout in seconds for agents to send initial READY signal after receiving configuration.

**Default**: `120` (2 minutes)

**Recommendations by scale**:
- Small (<10K files): 60 seconds
- Medium (10K-100K files): 120 seconds (default)
- Large (100K-1M files): 300 seconds (5 minutes)
- Very large (>1M files): 600 seconds (10 minutes)

**Note**: Agents perform config validation (including glob pattern expansion) before sending READY.
Large directory structures require longer timeouts.
```

---

## Issue 3: No `yield_now()` in Prepare Phase

### Location
- `src/prepare.rs` - Multiple functions (prepare_parallel, prepare_sequential, create loops)

### Problem
```rust
// Current code (simplified):
for i in 0..object_count {
    store.put(...).await?;  // ✅ Async, but doesn't guarantee yield
    // ❌ NO YIELD - Can starve executor for minutes
}
```

### Impact
- **Scale**: Creating 1M files without yielding = **minutes of executor starvation**
- **Starves**: Stats writer task cannot send updates
- **Breaks**: Controller marks agent as DEAD after 60s of no stats
- **Result**: Distributed test aborts mid-prepare

### Fix (Required for v0.8.51)

**Add yield points in loops**:

```rust
// src/prepare.rs - In object creation loops
let mut counter = 0u64;
while let Some((index, obj_spec)) = stream.next().await {
    // ... PUT operation ...
    store.put(...).await?;
    
    counter += 1;
    // Yield every 100 operations to allow heartbeat/stats tasks to run
    if counter % 100 == 0 {
        tokio::task::yield_now().await;
    }
}
```

**Locations to add yields**:
1. `prepare_parallel()` - In the futures stream loop (~line 900-1000)
2. `prepare_sequential()` - In object creation loop (~line 800-850)
3. `finalize_tree_with_mkdir()` - In directory creation loop (~line 600-650)

**Tuning guidance**:
- Yield every **100 operations** for local filesystems (fast PUTs)
- Yield every **500 operations** for S3/cloud storage (slower PUTs, already yields internally)
- Monitor: Should yield at least once per second during prepare

---

## Issue 4: gRPC Keep-Alive Too Aggressive

### Location
- `src/bin/controller.rs` lines 818-819, 844-845
- Default: 30s interval, 10s timeout

### Problem
```rust
.http2_keep_alive_interval(Duration::from_secs(30))      // ⚠️ OK
.keep_alive_timeout(Duration::from_secs(10))             // ❌ TOO SHORT for blocking ops
```

### Impact
- **With blocking ops**: Agent cannot respond to gRPC PING within 10s
- **Result**: Controller closes TCP connection
- **Error**: "Connection reset by peer" or "Timeout"

### Fix (Partial - addressed in v0.8.27, needs documentation)

**Already configurable** via `grpc_keepalive_interval` and `grpc_keepalive_timeout` (v0.8.27).

**Document recommendations** in CONFIG_SYNTAX.md:
```markdown
#### gRPC Keep-Alive Settings

**Recommendations for large-scale deployments**:

```yaml
distributed:
  grpc_keepalive_interval: 30      # Default OK
  grpc_keepalive_timeout: 30       # Increase from 10 to 30 for large tests
```

**Why increase timeout?**
- Agents performing heavy I/O (millions of files) may not respond to PING within 10s
- Even with proper yields, GC pauses or network delays can exceed 10s
- 30s provides margin without impacting failure detection
```

---

## Testing Plan

### Test Case 1: Million-File Glob Validation
```yaml
# config: million_file_glob_test.yaml
distributed:
  agent_ready_timeout: 600  # 10 minutes for glob expansion
  grpc_keepalive_timeout: 30

workload:
  - op: get
    path: "file:///mnt/large-dataset/*.dat"  # 1M files
    weight: 100
```

**Expected**:
- ✅ Agent completes glob validation (spawn_blocking)
- ✅ Agent sends READY within 600s
- ✅ No executor starvation

### Test Case 2: Million-Object Prepare with Yields
```yaml
# config: million_object_prepare_test.yaml
prepare:
  ensure_objects:
    - count: 1000000
      object_size: 1MB
      base_uri: "file:///mnt/prepare-test/"

distributed:
  agent_ready_timeout: 300
  grpc_keepalive_timeout: 30
```

**Expected**:
- ✅ Prepare yields every 100 objects
- ✅ Stats updates continue throughout prepare
- ✅ gRPC heartbeats functional
- ✅ Controller does not mark agent as DEAD

### Test Case 3: Large Directory Structure
```yaml
# config: large_tree_test.yaml
prepare:
  directory_structure:
    width: 100
    depth: 4
    files_per_dir: 100  # 100M total files

distributed:
  agent_ready_timeout: 900  # 15 minutes
  grpc_keepalive_timeout: 60  # Extra margin
```

**Expected**:
- ✅ Tree manifest creation yields periodically
- ✅ Directory creation (finalize_tree_with_mkdir) yields
- ✅ Agent remains responsive throughout

---

## Implementation Checklist

### v0.8.51 Release (Critical Fixes)

- [ ] **Issue 1**: Wrap glob in spawn_blocking
  - [ ] `src/bin/agent.rs` line ~3828
  - [ ] `src/bin/controller.rs` line ~4606
  - [ ] Add unit test for glob with >1000 files
  
- [ ] **Issue 2**: Make ready_timeout configurable
  - [ ] Add `agent_ready_timeout` field to `DistributedConfig`
  - [ ] Update controller to use config value
  - [ ] Set default to 120s (up from 30s)
  - [ ] Document in CONFIG_SYNTAX.md
  
- [ ] **Issue 3**: Add yield_now() to prepare phase
  - [ ] `prepare_parallel()` - yield every 100 objects
  - [ ] `prepare_sequential()` - yield every 100 objects
  - [ ] `finalize_tree_with_mkdir()` - yield every 100 directories
  - [ ] Add integration test to verify no executor starvation
  
- [ ] **Issue 4**: Document keep-alive recommendations
  - [ ] Add "Large-Scale Deployment Tuning" section to CONFIG_SYNTAX.md
  - [ ] Recommend `grpc_keepalive_timeout: 30` for >100K file tests
  - [ ] Cross-reference from USAGE.md

### Testing
- [ ] Test 1: Million-file glob validation
- [ ] Test 2: Million-object prepare with stats monitoring
- [ ] Test 3: Large directory structure (100M files theoretical)
- [ ] Regression: Existing tests still pass
- [ ] Performance: Yields don't impact throughput (<1% overhead)

### Documentation
- [ ] Update CHANGELOG.md with v0.8.51 entry
- [ ] Add "Executor Starvation Prevention" section to ARCHITECTURE.md
- [ ] Document yield strategy in IMPLEMENTATION_NOTES.md
- [ ] Add troubleshooting guide for timeout errors

---

## Performance Impact

**Yields** (tokio::task::yield_now):
- **Overhead**: <1% throughput reduction
- **Frequency**: Once per 100 operations = ~10 yields/second for 1000 ops/s workload
- **Benefit**: Prevents multi-minute executor blocks

**spawn_blocking** (glob):
- **Overhead**: ~100-500μs spawn cost
- **Frequency**: Once per config load (startup only)
- **Benefit**: Prevents 5-30 second executor blocks

**Net impact**: Negligible (<1%) on throughput, massive improvement in reliability.

---

## References

- External code review: [Date: Feb 6, 2026]
- Tokio best practices: https://tokio.rs/tokio/tutorial/spawning#send-bound
- Executor starvation: https://ryhl.io/blog/async-what-is-blocking/

## Priority

**🔴 CRITICAL - Block v0.8.51 release until fixed**

These issues cause production failures at scale. All four fixes are straightforward and low-risk.
