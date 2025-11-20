# Distributed Live Stats Implementation Guide

## Overview

This document describes the complete implementation of Phase 4.5 (Distributed Live Stats) and Phase 5 (Testing & Refinements) for sai3-bench. The implementation provides real-time progress visualization during distributed workload execution with proper startup handshaking.

**Version**: v0.7.6 (work in progress)  
**Date**: November 9, 2025

## Features Implemented

### 1. Startup Handshake Protocol (v0.7.6)
- **3-way handshake**: Controller sends config → Agents validate → Agents report READY/ERROR → Controller coordinates start
- **Synchronized start**: All agents begin workload execution at the exact same coordinated timestamp
- **Validation window**: 3 seconds for agents to validate configuration before workload starts
- **Error propagation**: Agents send ERROR status with descriptive messages if validation fails
- **Fast startup**: 5 seconds total (3s validation + 2s user-configurable delay) instead of previous 32s

### 2. Real-Time Progress Display (v0.7.6)
- **Progress bar**: Shows elapsed/total seconds with visual bar (e.g., `[=====>] 15/30s`)
- **Live stats**: Updates every 1 second with aggregate metrics across all agents
- **Multi-line display**: 3 lines showing agent count, GET stats, PUT stats
- **Preserved output**: Final stats remain visible after completion (not overwritten)
- **Consistent units**: All latency metrics in microseconds (µs) throughout

### 3. Microsecond Precision (v0.7.6)
- **Live stats display**: Shows latency in microseconds during execution
- **Console output**: Final summary uses microseconds
- **TSV files**: Column headers use `_us` suffix (mean_us, p50_us, p95_us, etc.)
- **Consistency**: Matches final per-agent results format

## Protocol Buffers Changes

### File: `proto/iobench.proto`

Added two new fields to the `LiveStats` message to support startup handshaking and error reporting:

```protobuf
message LiveStats {
  // ... existing fields ...
  
  // v0.7.6: Status enum for startup handshake
  enum Status {
    UNKNOWN = 0;    // Default/unset
    READY = 1;      // Agent validated config and is ready to start
    RUNNING = 2;    // Agent is executing workload
    ERROR = 3;      // Agent encountered error during validation
    COMPLETED = 4;  // Agent finished workload
  }
  
  Status status = 18;           // Current agent status
  string error_message = 19;    // Error details when status=ERROR
}
```

**Rationale**: 
- Enables bidirectional communication during startup phase
- Allows agents to signal readiness before workload execution begins
- Provides detailed error information when agents fail validation

## Agent Changes

### File: `src/bin/agent.rs`

#### 1. Configuration Validation Function

**Location**: Lines ~762-810

Added `validate_workload_config()` function called AFTER `apply_agent_prefix()` so validation uses per-agent paths:

```rust
async fn validate_workload_config(config: &sai3_bench::config::Config) -> Result<()> {
    // Check workload is not empty
    ensure!(!config.workload.is_empty(), "Workload is empty");
    
    for weighted_op in &config.workload {
        match weighted_op.op {
            Operation::Get | Operation::Head => {
                // Verify patterns/uris are configured
                // For file:// patterns, use glob to verify files exist
                if let Some(pattern) = &weighted_op.path {
                    if pattern.starts_with("file://") {
                        let local_path = pattern.strip_prefix("file://").unwrap();
                        let entries: Vec<_> = glob::glob(local_path)?.collect();
                        ensure!(!entries.is_empty(), 
                            "Pattern '{}' matches no files", pattern);
                    }
                }
            }
            Operation::Put => {
                // Verify object_size is configured
                ensure!(weighted_op.object_size.is_some(), 
                    "PUT operation requires object_size");
            }
            // ... other operations ...
        }
    }
    Ok(())
}
```

**Key Points**:
- Validates AFTER path prefixing (so agent-specific paths are checked)
- Checks file:// patterns using `glob` crate to ensure files exist
- Verifies PUT operations have `object_size` configured
- Returns descriptive error messages for validation failures

#### 2. Streaming Response with Startup Handshake

**Location**: Lines ~320-490

**Critical Fix**: Moved coordinated start wait and workload spawning INSIDE the stream (not before it).

**Before** (buggy):
```rust
// WRONG: Spawned before stream creation
tokio::spawn(async move { /* workload */ });

let stream = async_stream::stream! {
    // Stream only yields stats
};
```

**After** (correct):
```rust
let stream = async_stream::stream! {
    // 1. Send READY status immediately
    yield Ok(LiveStats { status: 1, ..ready_msg });
    
    // 2. Wait for coordinated start time
    if let Some(wait_dur) = wait_duration {
        tokio::time::sleep(wait_dur).await;
    }
    
    // 3. NOW spawn workload task (INSIDE stream, after READY)
    tokio::spawn(async move {
        // Execute prepare phase
        // Execute workload
        // Send result to channel
    });
    
    // 4. Stream live stats every 1s
    loop {
        select! {
            _ = interval.tick() => yield stats with status=2 (RUNNING)
            result = rx_done.recv() => yield final with status=4 (COMPLETED)
        }
    }
};
```

**Rationale**:
- READY message must be sent before workload starts
- Workload must not start until coordinated timestamp
- Stream must be created immediately (not blocked by 32s wait)

#### 3. Error Handling During Validation

**Location**: Lines ~332-368

If validation fails, agent sends ERROR status and exits stream:

```rust
match validate_workload_config(&config).await {
    Ok(_) => {
        info!("Configuration validated successfully");
        // Continue with READY message...
    }
    Err(e) => {
        let error_msg = format!("Configuration validation failed: {}", e);
        error!("{}", error_msg);
        
        // Send ERROR status to controller
        let error_stats = LiveStats {
            agent_id: agent_id.clone(),
            status: 3, // ERROR
            error_message: error_msg.clone(),
            ..Default::default()
        };
        yield Ok(error_stats);
        return; // Exit stream
    }
}
```

## Controller Changes

### File: `src/bin/controller.rs`

#### 1. Early Config Parsing for Progress Bar

**Location**: Lines ~683-700

**Change**: Parse config BEFORE progress bar creation (not twice):

```rust
// Read YAML configuration
let config_yaml = fs::read_to_string(config_path)?;

// Parse config early so we can use it for progress bar and storage detection
let config: sai3_bench::config::Config = serde_yaml::from_str(&config_yaml)?;

// Auto-detect shared storage
let is_shared_storage = if let Some(explicit) = shared_prepare {
    explicit
} else {
    detect_shared_storage(&config)
};
```

**Rationale**: Need `config.duration` to set progress bar length.

#### 2. Progress Bar with Duration (Not Spinner)

**Location**: Lines ~770-783

**Before** (spinner):
```rust
let progress_bar = multi_progress.add(ProgressBar::new_spinner());
progress_bar.set_style(
    ProgressStyle::default_spinner()
        .template("{spinner:.green} {msg}")
);
```

**After** (progress bar):
```rust
let duration_secs = config.duration.as_secs();
let progress_bar = multi_progress.add(ProgressBar::new(duration_secs));
progress_bar.set_style(
    ProgressStyle::default_bar()
        .template("{bar:40.cyan/blue} {pos:>3}/{len:3}s\n{msg}")
        .progress_chars("=>-")
);
```

**Key Points**:
- Uses `ProgressBar::new(duration_secs)` for determinate progress
- Template shows `[bar] pos/len` format (e.g., `[=====>] 15/30s`)
- `\n{msg}` puts multi-line stats below the progress bar
- Progress chars: `=>-` creates nice-looking bar

#### 3. Startup Timing Configuration

**Location**: Lines ~755-763

**Change**: Reduced validation window from 30s to 3s:

```rust
// v0.7.6: Add extra time for validation (3s) + user's start_delay
let validation_time_secs = 3;
let total_delay_secs = validation_time_secs + start_delay_secs;
let start_time = SystemTime::now() + Duration::from_secs(total_delay_secs);
let start_ns = start_time.duration_since(UNIX_EPOCH)?.as_nanos() as i64;
```

**Rationale**: 3 seconds is enough for agents to validate config and send READY.

#### 4. Channel Creation Before Task Spawning

**Location**: Lines ~785-843

**Critical Fix**: Create channel BEFORE spawning tasks (tasks need sender):

```rust
// v0.7.6: Create channel BEFORE spawning tasks
let (tx_stats, mut rx_stats) = tokio::sync::mpsc::channel::<LiveStats>(100);

for (idx, agent_addr) in agent_addrs.iter().enumerate() {
    let tx = tx_stats.clone();  // Clone sender for each task
    
    let handle = tokio::spawn(async move {
        let mut stream = client.run_workload_with_live_stats(...).await?.into_inner();
        
        // v0.7.6: Forward messages IMMEDIATELY (not after stream completes)
        while let Some(stats_result) = stream.message().await? {
            let _ = tx.send(stats_result).await;  // Send immediately
        }
        Ok::<(), anyhow::Error>(())
    });
    stream_handles.push(handle);
}

drop(tx_stats);  // Drop original so channel closes when all tasks done
```

**Before** (buggy):
- Created channel AFTER spawning tasks → tasks couldn't send messages
- Collected all messages in Vec → only forwarded after stream completion

**After** (correct):
- Create channel first, clone `tx` for each task
- Forward each message immediately as received
- Drop original `tx` so channel closes when all tasks finish

#### 5. Startup Handshake Loop

**Location**: Lines ~874-933

Added loop to collect READY/ERROR from all agents before proceeding:

```rust
let startup_timeout_secs = validation_time_secs - 2;  // 1s buffer
let startup_deadline = tokio::time::Instant::now() + startup_timeout;

'startup: loop {
    tokio::select! {
        Some(stats) = rx_stats.recv() => {
            match stats.status {
                1 => {  // READY
                    agent_status.insert(stats.agent_id.clone(), "READY");
                    ready_agents.insert(stats.agent_id.clone());
                    eprintln!("  ✅ {} ready", stats.agent_id);
                }
                3 => {  // ERROR
                    agent_status.insert(stats.agent_id.clone(), "ERROR");
                    error_agents.push((stats.agent_id, stats.error_message));
                    eprintln!("  ❌ {} error: {}", stats.agent_id, stats.error_message);
                }
                _ => { /* Treat as ready */ }
            }
            
            if agent_status.len() >= expected_agent_count {
                break 'startup;  // All agents responded
            }
        }
        _ = tokio::time::sleep_until(startup_deadline) => {
            bail!("Agent startup validation timeout");
        }
    }
}

// Check for errors
if !error_agents.is_empty() {
    // Display errors and exit
    bail!("{} agent(s) failed startup validation", error_agents.len());
}

eprintln!("✅ All {} agents ready - starting workload execution", ready_agents.len());
```

**Key Points**:
- Waits up to 1s (with 2s buffer) for all agents to report status
- Tracks which agents are READY vs ERROR
- If any ERROR: Reports details and exits
- If all READY: Proceeds to workload (agents start automatically at coordinated time)

#### 6. Workload Start Tracking for Progress Bar

**Location**: Lines ~938-942

Track when workload actually starts (after validation):

```rust
eprintln!("✅ All {} agents ready - starting workload execution\n", ready_agents.len());

// v0.7.6: Track workload start time for progress bar position
let workload_start = std::time::Instant::now();
```

#### 7. Progress Bar Position Updates

**Location**: Lines ~1006-1017

Update progress bar position based on elapsed time:

```rust
// Update display every 100ms (rate limiting)
if last_update.elapsed() > std::time::Duration::from_millis(100) {
    let agg = aggregator.aggregate();
    let msg = /* format stats */;
    progress_bar.set_message(msg.clone());
    
    // v0.7.6: Update progress bar position based on elapsed time
    let elapsed_secs = workload_start.elapsed().as_secs().min(duration_secs);
    progress_bar.set_position(elapsed_secs);
    
    last_update = std::time::Instant::now();
}
```

#### 8. Stats Display Format (Microseconds)

**Location**: Lines ~181-241 and ~1067-1083

**Change**: Display latencies in microseconds (not milliseconds):

**LiveStatsAggregator::format_progress()** (Lines ~181-241):
```rust
// Before:
format!("... (mean: {:.1}ms, p50: {:.1}ms, p95: {:.1}ms)",
    self.get_mean_us / 1000.0,
    self.get_p50_us / 1000.0,
    self.get_p95_us / 1000.0)

// After:
format!("... (mean: {:.0}µs, p50: {:.0}µs, p95: {:.0}µs)",
    self.get_mean_us,
    self.get_p50_us,
    self.get_p95_us)
```

**Final summary output** (Lines ~1067-1083):
```rust
// Before:
println!("GET: {:.0} ops/s, {} (mean: {:.1}ms, p50: {:.1}ms, p95: {:.1}ms)",
    /* ... */,
    final_stats.get_mean_us / 1000.0,
    final_stats.get_p50_us / 1000.0,
    final_stats.get_p95_us / 1000.0);

// After:
println!("GET: {:.0} ops/s, {} (mean: {:.0}µs, p50: {:.0}µs, p95: {:.0}µs)",
    /* ... */,
    final_stats.get_mean_us,
    final_stats.get_p50_us,
    final_stats.get_p95_us);
```

**Rationale**: Microseconds provide better precision for fast local operations (90-100µs range).

#### 9. Preserve Final Stats Display

**Location**: Lines ~1060-1066

Add newlines after progress bar finishes to preserve last stats:

```rust
// Final aggregation
let final_stats = aggregator.aggregate();
progress_bar.finish_with_message(format!("✓ All {} agents completed", final_stats.num_agents));

// v0.7.6: Add blank lines to preserve the last stats display before printing results
println!("\n\n");  // Preserve GET/PUT stats lines from being overwritten

// v0.7.5: Print live aggregate stats for immediate visibility
println!("=== Live Aggregate Stats (from streaming) ===");
```

**Rationale**: Without newlines, final results overwrite the last live stats display.

## Dependencies Added

### File: `Cargo.toml`

Added `glob` crate for file pattern validation:

```toml
[dependencies]
# ... existing dependencies ...
glob = "^0.3"  # For validating file:// patterns in agent
```

## Testing

### Test Configuration

**File**: `tests/configs/test_distributed_local.yaml`

```yaml
target: "file:///mnt/test/sai3bench-test/"
duration: "30s"
concurrency: 2

workload:
  - op: get
    path: "data/*"
    weight: 70
  - op: put
    path: "results/"
    object_size: 2048
    weight: 30
```

### Test Execution

```bash
# Start agents
./target/release/sai3bench-agent --listen 127.0.0.1:7761 -v &
./target/release/sai3bench-agent --listen 127.0.0.1:7762 -v &

# Run test
./target/release/sai3bench-ctl \
  --agents 127.0.0.1:7761,127.0.0.1:7762 \
  \
  run \
  --config tests/configs/test_distributed_local.yaml
```

### Expected Output

```
=== Distributed Workload ===
Config: tests/configs/test_distributed_local.yaml
Agents: 2
Start delay: 2s
Storage mode: local (per-agent)

Starting workload on 2 agents with live stats...

⏳ Waiting for agents to validate configuration...
  ✅ agent-1 ready
  ✅ agent-2 ready
✅ All 2 agents ready - starting workload execution

[========================================] 30/30s
2 agents
  GET: 19882 ops/s, 19.4 MiB/s (mean: 95µs, p50: 96µs, p95: 135µs)
  PUT: 8541 ops/s, 16.7 MiB/s (mean: 102µs, p50: 98µs, p95: 136µs)
✓ All 2 agents completed


=== Live Aggregate Stats (from streaming) ===
Total operations: 689656 GET, 296040 PUT, 0 META
GET: 19882 ops/s, 19.4 MiB/s (mean: 95µs, p50: 96µs, p95: 135µs)
PUT: 8541 ops/s, 16.7 MiB/s (mean: 102µs, p50: 98µs, p95: 136µs)
Elapsed: 35.00s

=== Distributed Results ===
[... per-agent results with µs latencies ...]
```

## Output Files Verification

All output files use microsecond units consistently:

### console.log
- Live stats: `mean: 95µs, p50: 96µs, p95: 135µs`
- Final results: `mean: 95µs, p95: 135µs`

### results.tsv (aggregated)
```
operation  size_bucket  mean_us  p50_us  p90_us  p95_us  p99_us  max_us  ...
GET        1B-8KiB      95.76    96.00   126.00  135.00  160.00  3079.00 ...
PUT        1B-8KiB      102.99   98.00   124.00  136.00  210.00  15143.00...
```

### agents/{agent-id}/results.tsv
Same format as aggregated results.tsv, all values in microseconds.

## Summary of Key Fixes

1. **Message Flow**: Create channel before tasks, forward immediately (not batched)
2. **Workload Timing**: Spawn workload INSIDE stream after READY (not before stream)
3. **Coordinated Start**: Wait happens inside stream, not blocking stream creation
4. **Fast Startup**: 5s total (3s validation + 2s delay) instead of 32s
5. **Progress Display**: Bar shows elapsed/total with live stats below
6. **Unit Consistency**: All latencies in microseconds everywhere
7. **Error Handling**: Agents validate and report errors before starting

## Applying to dl-driver

The same patterns should be applied to dl-driver:

1. Add `Status` enum to proto LiveStats message
2. Add validation function in agent (after path prefixing)
3. Move workload spawning inside stream (after READY sent)
4. Create channel before spawning tasks in controller
5. Add startup handshake loop in controller
6. Use progress bar instead of spinner
7. Change all latency displays from ms to µs
8. Add newlines after progress bar completes

Key differences for dl-driver:
- May need phase-aware validation (data gen vs training)
- Progress bar might need to handle multi-phase workloads
- Consider showing current phase in progress display
