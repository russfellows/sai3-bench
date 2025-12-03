# Replay Backpressure Design

## Overview

Implement intelligent backpressure handling for replay workloads to gracefully handle situations where the target system cannot sustain the required I/O rate.

## Problem Statement

When replaying an op-log with `--speed` multiplier (or even at 1.0x), the target storage system may not be able to keep up with the required I/O rate. Currently:
- Operations queue up to `max_concurrent` (1000)
- We silently fall behind with no visibility
- No graceful failure mechanism

## Design Decisions

### Mode Switching
1. **Normal Mode**: Operations execute at scheduled times (timing-faithful replay)
2. **Best-Effort Mode**: Operations execute as fast as possible (no artificial delays)
3. **Transition**: Normal → Best-Effort when lag exceeds `lag_threshold`
4. **Recovery**: Best-Effort → Normal when lag drops below `recovery_threshold`

### Flap Detection
- Track mode transitions in a sliding 60-second window
- Exit gracefully if transitions exceed `max_flaps_per_minute`
- Indicates target system is borderline - can't sustain load consistently

### Exit Behavior
- Graceful drain of in-flight operations (up to `drain_timeout`)
- Clear error message with remediation suggestions

## YAML Configuration Schema

```yaml
# Root-level replay configuration
replay:
  # Lag threshold to enter best-effort mode
  # When we fall this far behind schedule, stop waiting and execute ASAP
  # Default: 5s
  lag_threshold: "5s"
  
  # Lag must drop below this to return to normal timed mode
  # Prevents rapid oscillation between modes (hysteresis)
  # Default: 1s
  recovery_threshold: "1s"
  
  # Maximum mode transitions (normal↔best-effort) per minute
  # Exit gracefully if exceeded - target system can't sustain load
  # Default: 3
  max_flaps_per_minute: 3
  
  # Timeout for graceful drain on flap-exit
  # Wait this long for in-flight operations before force-exit
  # Default: 10s
  drain_timeout: "10s"
  
  # Maximum concurrent operations (existing, moved here)
  # Default: 1000
  max_concurrent: 1000
```

## Rust Data Structures

### Configuration

```rust
/// Replay-specific configuration (v0.9.0+)
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ReplayConfig {
    /// Lag threshold to enter best-effort mode (default: 5s)
    #[serde(default = "default_lag_threshold", with = "humantime_serde")]
    pub lag_threshold: Duration,
    
    /// Lag must drop below this to return to normal mode (default: 1s)
    #[serde(default = "default_recovery_threshold", with = "humantime_serde")]
    pub recovery_threshold: Duration,
    
    /// Maximum mode transitions per minute before exit (default: 3)
    #[serde(default = "default_max_flaps")]
    pub max_flaps_per_minute: u32,
    
    /// Timeout for graceful drain on exit (default: 10s)
    #[serde(default = "default_drain_timeout", with = "humantime_serde")]
    pub drain_timeout: Duration,
    
    /// Maximum concurrent operations (default: 1000)
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
}

fn default_lag_threshold() -> Duration { Duration::from_secs(5) }
fn default_recovery_threshold() -> Duration { Duration::from_secs(1) }
fn default_max_flaps() -> u32 { 3 }
fn default_drain_timeout() -> Duration { Duration::from_secs(10) }
fn default_max_concurrent() -> usize { 1000 }
```

### Runtime State

```rust
/// Replay execution mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayMode {
    /// Timing-faithful execution (wait for scheduled time)
    Normal,
    /// Execute as fast as possible (no delays)
    BestEffort,
}

/// Tracks mode transitions for flap detection
struct FlappingTracker {
    /// Timestamps of recent mode transitions
    transitions: VecDeque<Instant>,
    /// Window size for counting (60 seconds)
    window: Duration,
    /// Maximum allowed transitions in window
    max_transitions: u32,
}

impl FlappingTracker {
    fn record_transition(&mut self) -> bool {
        let now = Instant::now();
        self.transitions.push_back(now);
        
        // Prune old transitions outside window
        let cutoff = now - self.window;
        while self.transitions.front().map_or(false, |&t| t < cutoff) {
            self.transitions.pop_front();
        }
        
        // Return true if we've exceeded the limit
        self.transitions.len() as u32 > self.max_transitions
    }
    
    fn count(&self) -> usize {
        self.transitions.len()
    }
}
```

### Enhanced Statistics

```rust
#[derive(Debug, Default)]
pub struct ReplayStats {
    // Existing fields
    pub total_operations: u64,
    pub completed_operations: u64,
    pub failed_operations: u64,
    pub skipped_operations: u64,
    
    // Backpressure metrics (v0.9.0+)
    pub max_lag_ms: u64,              // Peak lag observed during replay
    pub total_best_effort_ms: u64,    // Cumulative time spent in best-effort mode
    pub mode_transitions: u64,        // Total normal↔best-effort switches
    pub final_mode: ReplayMode,       // Mode at completion
    pub aborted_due_to_flapping: bool,// Did we exit due to flap limit?
}
```

## Implementation Flow

```
┌─────────────────────────────────────────────────────────────────┐
│                    REPLAY MAIN LOOP                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  for each operation in op-log:                                  │
│                                                                  │
│    1. Calculate scheduled_time = epoch + (op.start / speed)    │
│    2. Calculate current_lag = now - scheduled_time             │
│                                                                  │
│    3. MODE CHECK:                                                │
│       ┌──────────────────────────────────────────────────────┐  │
│       │ IF mode == Normal AND lag > lag_threshold:           │  │
│       │   → Switch to BestEffort                             │  │
│       │   → Log: "⚠️ Falling behind by {lag}s, switching     │  │
│       │          to best-effort mode"                        │  │
│       │   → Record transition, check flap limit              │  │
│       │   → If flap limit exceeded: initiate graceful exit   │  │
│       ├──────────────────────────────────────────────────────┤  │
│       │ IF mode == BestEffort AND lag < recovery_threshold:  │  │
│       │   → Switch to Normal                                 │  │
│       │   → Log: "✅ Caught up (lag={lag}ms), resuming       │  │
│       │          timed replay"                               │  │
│       │   → Record transition, check flap limit              │  │
│       └──────────────────────────────────────────────────────┘  │
│                                                                  │
│    4. EXECUTE:                                                   │
│       IF mode == Normal:                                         │
│         → Sleep until scheduled_time (if in future)             │
│       ELSE (BestEffort):                                         │
│         → Execute immediately (no sleep)                        │
│                                                                  │
│    5. Spawn operation task                                      │
│                                                                  │
│    6. Poll completed tasks if at max_concurrent                 │
│                                                                  │
│    7. Update stats (max_lag, best_effort_time, etc.)           │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

## Logging Examples

### Entering Best-Effort Mode
```
2025-12-02T15:30:45.123Z  WARN sai3_bench::replay: ⚠️ Falling behind schedule by 5.2s, switching to best-effort mode
2025-12-02T15:30:45.123Z  INFO sai3_bench::replay:    Target system may be unable to sustain required I/O rate
2025-12-02T15:30:45.123Z  INFO sai3_bench::replay:    Operations will execute as fast as possible until caught up
```

### Recovering to Normal Mode
```
2025-12-02T15:31:02.456Z  INFO sai3_bench::replay: ✅ Caught up (lag=0.8s), resuming timed replay
```

### Flap Warning
```
2025-12-02T15:32:15.789Z  WARN sai3_bench::replay: ⚠️ Mode transition 2/3 in last 60s (normal → best-effort)
2025-12-02T15:32:15.789Z  WARN sai3_bench::replay:    Target system is borderline - oscillating between modes
```

### Flap Exit
```
2025-12-02T15:33:30.012Z ERROR sai3_bench::replay: ❌ Exceeded maximum mode transitions (3/min)
2025-12-02T15:33:30.012Z ERROR sai3_bench::replay:    Target system cannot sustain the required I/O rate
2025-12-02T15:33:30.012Z ERROR sai3_bench::replay:    Suggestions:
2025-12-02T15:33:30.012Z ERROR sai3_bench::replay:      - Reduce replay speed (current: 2.0x)
2025-12-02T15:33:30.012Z ERROR sai3_bench::replay:      - Increase target system capacity
2025-12-02T15:33:30.012Z ERROR sai3_bench::replay:      - Check for resource contention on target
2025-12-02T15:33:30.012Z  INFO sai3_bench::replay: Draining 847 in-flight operations (timeout: 10s)...
```

## Final Stats Output

```
=== Replay Statistics ===
Total operations:     1,000,000
Completed:            998,523 (99.85%)
Failed:               1,477 (0.15%)
Skipped:              0

=== Backpressure Metrics ===
Max lag observed:     8.3s
Time in best-effort:  45.2s (7.5% of replay)
Mode transitions:     2
Final mode:           Normal
Aborted:              No
```

## CLI Integration

The `replay` subcommand will read the `replay:` section from:
1. A standalone YAML config file (if provided via `--config`)
2. Defaults if no config provided

```bash
# With config file containing replay settings
sai3-bench replay --op-log trace.tsv.zst --config replay-config.yaml

# Without config (uses defaults)
sai3-bench replay --op-log trace.tsv.zst --target s3://bucket/
```

## Files to Modify

1. **src/config.rs** - Add `ReplayConfig` struct and parsing
2. **src/replay_streaming.rs** - Implement backpressure logic
3. **src/main.rs** - Wire up config loading for replay command
4. **src/constants.rs** - Add default constants
5. **docs/CONFIG_SYNTAX.md** - Document new replay section
6. **docs/USAGE.md** - Update replay documentation

## Test Cases

1. **Normal operation** - Verify timing-faithful replay when system keeps up
2. **Transition to best-effort** - Verify switch when lag exceeds threshold
3. **Recovery to normal** - Verify return when lag drops below recovery threshold
4. **Flap detection** - Verify exit when transitions exceed limit
5. **Graceful drain** - Verify in-flight ops complete on exit
6. **Stats accuracy** - Verify all metrics captured correctly
7. **Config parsing** - Verify YAML schema works with all fields
8. **Default values** - Verify sensible defaults when config omitted

## Version Target

v0.9.0 - "Replay Backpressure"
