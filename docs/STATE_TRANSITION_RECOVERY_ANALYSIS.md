# State Transition and Recovery Analysis

**Date**: November 24, 2025  
**Version**: v0.8.4  
**Author**: Two-channel bidirectional streaming architecture

---

## Executive Summary

This document describes the evolution from single-stream to **two-channel bidirectional streaming** architecture to solve fundamental communication reliability issues in distributed execution.

### Issues Addressed in v0.8.2 (Single-Stream Era)

1. **Invalid state transitions** preventing normal operation → FIXED
2. **Insufficient timeout values** causing false positives during gRPC backpressure → WORKAROUND (60s timeout)
3. **No recovery mechanism** when agents incorrectly marked disconnected → FIXED
4. **Incomplete "back on" logic** verification → VERIFIED

### Remaining Issues (Single-Stream Limitations)

5. **Stream blocking during coordinated start** → Controller frozen while agents wait
6. **gRPC backpressure prevents keepalives** → False positive timeouts inevitable
7. **No failsafe control channel** → Cannot abort/ping when stats channel congested

### v0.8.4 Solution: Two-Channel Architecture

All remaining issues solved by switching to **bidirectional streaming with logical channel separation**:

```
Old (v0.8.2):  Agent ──stats→ Controller  (server streaming, one-way)

New (v0.8.4):  Agent ←──stats──→ Controller  (bidirectional)
                    ←─control─→
```

**Key Innovation**: Same gRPC stream, but logically separated message types:
- **Stats channel** (agent→controller): LiveStats messages with sequence numbers
- **Control channel** (controller→agent): ControlMessage (PING, START, ABORT, ACK)

---

## Root Cause Analysis: Why Single-Stream Failed

### Problem 1: Blocking During Coordinated Start

**Scenario**:
```
1. Agent validates config
2. Agent sends READY status
3. Agent calls tokio::time::sleep(start_delay) // BLOCKS HERE
4. Stream cannot yield any messages while sleeping
5. Controller sees no updates for 2+ seconds
6. Controller appears frozen ("0 ops/s" forever)
```

**Why This Happens**: async-stream macro generates a state machine where `yield` points are await points. Any other await (like `sleep`) blocks the entire stream generator.

**v0.8.2 Attempted Fix**: Send keepalive stats every 1s during wait
- **Problem**: Still blocks between keepalives, adds complexity
- **Problem**: What if start_delay is 60s? Send 60 keepalives?

**v0.8.4 Solution**: Controller sends START command when ready
```
1. Agent validates config
2. Agent sends READY status  
3. Agent awaits START command on control channel (non-blocking)
4. Meanwhile, agent sends keepalive stats every 1s
5. Controller receives READY from all agents
6. Controller sends START to all agents simultaneously
7. All agents begin workload at exact same moment
```

### Problem 2: gRPC Backpressure Blocks Everything

**Scenario**:
```
1. Agent processes 400K objects in prepare phase
2. Agent sends stats every 1s (400+ messages total)
3. Controller slow to process messages (busy rendering progress bar)
4. gRPC stream buffer fills up (default 256KB)
5. Agent's yield Ok(stats) BLOCKS waiting for buffer space
6. Agent cannot send ANY messages while blocked (even keepalives!)
7. Controller sees no updates for 60+ seconds
8. Controller marks agent as DISCONNECTED (false positive)
```

**Why This Happens**: Single stream means **stats and keepalives compete for same buffer**. If stats fill buffer, keepalives cannot be sent.

**v0.8.2 Attempted Fix**: Increase timeout to 60s
- **Problem**: Band-aid over root cause
- **Problem**: Truly dead agents now take 60s to detect
- **Problem**: Doesn't solve the blocking, just tolerates it longer

**v0.8.4 Solution**: Separate control channel never blocks on stats
```
1. Stats channel fills up due to backpressure
2. Controller sends PING on control channel (different buffer)
3. Agent receives PING, responds on control channel
4. Controller knows agent is alive even if stats delayed
5. When stats buffer clears, agent resumes sending stats
6. No false positive timeout
```

### Problem 3: No Failsafe Abort Mechanism

**Scenario**:
```
1. User presses Ctrl-C on controller
2. Controller wants to send ABORT to agents
3. But agents only listen on incoming RPC stream (server-side streaming)
4. Controller cannot send anything to agents after stream started!
5. Controller forced to close stream abruptly
6. Agents detect disconnect, abort via rx_cancel channel
7. Ugly error handling, not graceful
```

**v0.8.2 Limitation**: Separate `AbortWorkload()` RPC
- **Problem**: Requires new gRPC connection while workload running
- **Problem**: May fail if agent process overloaded
- **Problem**: Two separate communication paths = race conditions

**v0.8.4 Solution**: ABORT command on existing control channel
```
1. User presses Ctrl-C on controller
2. Controller sends ControlMessage(ABORT) on bidirectional stream
3. Agent receives ABORT immediately (same connection)
4. Agent cancels workload gracefully
5. Agent sends final LiveStats with error status
6. Clean shutdown, all messages delivered
```

---

## v0.8.4 Architecture: Two Logical Channels

### Implementation Strategy

**Single gRPC stream, two message types**:
```rust
// Agent side
let (mut tx_control, mut rx_control) = /* bidirectional stream */;

tokio::spawn(async move {
    // Control channel reader (controller→agent)
    while let Some(msg) = rx_control.next().await {
        match msg.command {
            PING => send_pong(),
            START => start_workload(),
            ABORT => cancel_workload(),
            ACKNOWLEDGE => mark_received(msg.ack_sequence),
        }
    }
});

// Stats channel writer (agent→controller) - separate task
tokio::spawn(async move {
    let mut seq = 0;
    loop {
        let stats = tracker.snapshot();
        seq += 1;
        tx_control.send(LiveStats { sequence: seq, ...stats }).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
});
```

**Key Design Points**:
1. **Two separate tasks**: Control reader and stats writer don't block each other
2. **Sequence numbers**: Enable ACK/NACK and gap detection
3. **Bounded buffers**: Control channel uses separate buffer from stats
4. **Graceful degradation**: If stats blocked, control still works

### Message Flow Diagram

```
Controller                                    Agent
  │                                             │
  ├─────── ControlMessage(START) ─────────────→│
  │                                             ├─ Validate config
  │                                             │
  │←──────── LiveStats(READY, seq=1) ──────────┤
  ├─────── ControlMessage(ACK, seq=1) ────────→│
  │                                             │
  │                                             ├─ Wait for START
  │←──────── LiveStats(keepalive) ─────────────┤  (non-blocking)
  │←──────── LiveStats(keepalive) ─────────────┤
  │                                             │
  ├─────── ControlMessage(START, final_ts) ───→│
  │                                             ├─ Begin workload
  │                                             │
  │←──────── LiveStats(RUNNING, seq=2) ────────┤
  │←──────── LiveStats(RUNNING, seq=3) ────────┤
  │                                             │
  │         [gRPC backpressure - stats blocked] │
  ├─────── ControlMessage(PING) ──────────────→│  (still works!)
  │←──────── LiveStats(PONG, seq=4) ───────────┤
  │                                             │
  │         [backpressure clears]               │
  │←──────── LiveStats(RUNNING, seq=5) ────────┤
  │←──────── LiveStats(RUNNING, seq=6) ────────┤
  │                                             │
  │←──────── LiveStats(COMPLETED) ─────────────┤
  ├─────── ControlMessage(ACK) ───────────────→│
  │                                             │
 END                                          END
```

---

## Issues Discovered

### Issue #1: Invalid Idle→Idle State Transition (Agent)

**Location**: `src/bin/agent.rs:1024-1036`

**Symptom**: 
```
ERROR Invalid state transition: Idle → Idle (reason: stream ended)
```

**Root Cause**:
1. Workload completes successfully: `Running → Idle` (line 984)
2. Stream generator ends, tries unconditional transition to Idle (line 1024)
3. State machine rejects `Idle → Idle` transition (not in allowed list)

**Fix Applied** (lines 1030-1036):
```rust
// v0.8.2: Reset agent state to Idle after stream completes (if not already Idle)
let current_state = agent_state_stream.get_state().await;
if current_state != WorkloadState::Idle {
    let _ = agent_state_stream.transition_to(WorkloadState::Idle, "stream ended").await;
    info!("Agent {} reset to Idle state from {:?}, ready for next workload", agent_id_stream, current_state);
} else {
    debug!("Agent {} already in Idle state, no transition needed", agent_id_stream);
}
```

**Why This Works**: Check current state before attempting transition. Only transition if not already Idle.

---

### Issue #2: Insufficient Timeout During Long Prepare Phase (Controller)

**Location**: `src/bin/controller.rs:1445-1446`

**Symptom**:
```
ERROR ❌ Agent agent-6 STALLED (no updates for 379.7s) - marking as DISCONNECTED
```

**Root Cause**:
1. Prepare phase with 400K+ objects takes extended time
2. Agent's `yield Ok(stats)` blocks when gRPC stream buffer full (backpressure)
3. While blocked, agent cannot send ANY updates (stuck in yield)
4. Controller sees no updates for >10s → marks agent as STALLED/DISCONNECTED
5. But agent is actually working fine, just blocked on network I/O

**Original Values**:
- `timeout_warn_secs = 5.0`
- `timeout_dead_secs = 10.0`

**Fix Applied** (lines 1445-1446):
```rust
// v0.8.2: Increased timeouts for long prepare phases (400K+ objects)
// Agents can block on yield if gRPC stream backpressures
let timeout_warn_secs = 30.0;  // Warn after 30s
let timeout_dead_secs = 60.0;  // Mark dead after 60s
```

**Why 60s?**: Testing showed 379s blockage during prepare. 60s is a reasonable compromise between:
- Detecting truly dead agents quickly
- Tolerating gRPC backpressure during large prepare phases
- Not waiting too long for failed agents

**Alternative Considered**: Dynamic timeout based on prepare size. Rejected because complexity doesn't justify benefit for this use case.

---

### Issue #3: No Recovery from Disconnected State (Controller)

**Location**: `src/bin/controller.rs:54-91` (state machine)

**Symptom**: Agents stay Disconnected even after sending completion messages. Final status shows:
```
✅ All 8 agents completed  (from results)
❌ Completed: 0/8, Disconnected: 8  (from state tracking)
```

**Root Cause**: State machine did not allow transitions FROM Disconnected state. Original allowed transitions:
```rust
(Running, Disconnected)        // Can go TO Disconnected
(Disconnected, Disconnected)   // Can stay Disconnected
// NO transitions FROM Disconnected!
```

**Fix Applied** (lines 84-86):
```rust
// v0.8.2: Recovery from Disconnected (gRPC stream backpressure, network issues)
| (Disconnected, Preparing)  // Reconnect during prepare
| (Disconnected, Running)    // Reconnect during workload
| (Disconnected, Completed)  // Reconnect with completion message
```

**Why These Transitions**: Agent can timeout during any active phase (Preparing/Running) or even complete while controller thinks it's disconnected. Need to allow recovery to any valid state based on received message.

---

### Issue #4: Disconnected Treated as Terminal State (Controller)

**Location**: `src/bin/controller.rs:145-147`

**Symptom**: Messages from recovering agents were ignored because `is_terminal()` included Disconnected.

**Root Cause**: The timeout detection loop (line 1462) checks:
```rust
if tracker.is_terminal() {
    continue;  // Skip processing - BUG!
}
```

Original `is_terminal()` included Disconnected, so recovery messages were never processed.

**Fix Applied** (lines 145-147):
```rust
/// Check if agent is in a terminal state (won't send more updates)
/// v0.8.2: Disconnected is NOT terminal - agents can recover
fn is_terminal(&self) -> bool {
    matches!(
        self.state,
        ControllerAgentState::Completed | ControllerAgentState::Failed
    )
}
```

**Why This Works**: Only Completed and Failed are truly terminal. Disconnected agents can recover, so they must not be skipped in message processing.

---

### Issue #5: Recovery Code Ignored Transition Failures (Controller)

**Location**: `src/bin/controller.rs:1505-1520`

**Symptom**: Recovery attempts failed silently due to `let _ =` ignoring errors.

**Original Code**:
```rust
if tracker.state == ControllerAgentState::Disconnected {
    // Recovery logic...
    let _ = tracker.transition_to(new_state, "reconnected");  // Ignores failure!
}
```

**Fix Applied** (lines 1505-1520):
```rust
// v0.8.2: If agent was marked Disconnected but sent message, recover state
if tracker.state == ControllerAgentState::Disconnected {
    let new_state = if stats.completed {
        ControllerAgentState::Completed
    } else if stats.in_prepare_phase {
        ControllerAgentState::Preparing
    } else {
        ControllerAgentState::Running
    };
    
    warn!("🔄 Agent {} RECOVERED from DISCONNECTED → {:?}", stats.agent_id, new_state);
    
    // Use transition_to with proper validation (now allowed in state machine)
    if let Err(e) = tracker.transition_to(new_state, "recovered from timeout") {
        error!("Failed to recover agent {}: {}", stats.agent_id, e);
    }
}
```

**Why This Works**: 
1. Determine correct target state from message content
2. Log recovery attempt with clear message
3. Check for errors (though shouldn't fail with Issue #3 fixed)
4. Provides visibility into recovery process

---

### Issue #6: Invalid Running→Preparing Transition (Controller)

**Location**: `src/bin/controller.rs:1529`

**Symptom**: Latent bug - state machine doesn't allow `(Running, Preparing)` but code attempted it.

**Root Cause**: Code checked `if in_prepare && (state == Ready || state == Running)`. Running→Preparing is invalid (you can't go backwards).

**Fix Applied** (line 1529):
```rust
// v0.7.13: Update agent state based on prepare phase
// v0.8.2: Only transition Ready→Preparing (not Running→Preparing, that's invalid)
if in_prepare && tracker.state == ControllerAgentState::Ready {
    let _ = tracker.transition_to(ControllerAgentState::Preparing, "prepare phase started");
}
```

**Why This Works**: Removed `Running` from the condition. Only `Ready → Preparing` is valid. If we're already Running, we shouldn't go back to Preparing.

---

## Recovery Mechanism Verification ("Back On" Logic)

A comprehensive check was performed to ensure no degraded state persists after recovery:

### ✅ Timeouts (Immutable)
**Location**: `src/bin/controller.rs:1445-1446`
```rust
let timeout_warn_secs = 30.0;
let timeout_dead_secs = 60.0;
```
- **Status**: Immutable `let` bindings, never modified
- **Recovery**: N/A - no backoff logic exists
- **Conclusion**: No persistent degraded state

### ✅ State Machine Transitions
**Location**: `src/bin/controller.rs:119-136`
```rust
fn transition_to(&mut self, new_state: ControllerAgentState, reason: &str) -> Result<()> {
    // Validation...
    self.state = new_state;
    self.last_seen = std::time::Instant::now();  // RESETS TIMESTAMP
    Ok(())
}
```
- **Recovery**: Full state transition via `transition_to()`
- **Side Effect**: Resets `last_seen` timestamp (line 133)
- **Conclusion**: Complete state reset, no partial transitions

### ✅ Timestamp Reset (Dual Path)
**Location**: `src/bin/controller.rs:1502, 1517`
1. Line 1502: `tracker.touch()` - Updates `last_seen`
2. Line 1517: `tracker.transition_to()` - Also updates `last_seen` (line 133)

- **Recovery**: Both paths reset timestamp
- **Redundancy**: Timestamp updated twice (safe, intentional for reliability)
- **Conclusion**: Timeout detection fully reset after recovery

### ✅ Progress Bar Display (Dynamic)
**Location**: `src/bin/controller.rs:1611, 1617-1627`
```rust
let dead_count = agent_trackers.values()
    .filter(|t| t.state == ControllerAgentState::Disconnected)
    .count();
```
- **Recovery**: `dead_count` recalculated every display update (100ms)
- **Behavior**: "⚠️ X dead" message automatically updates
- **Conclusion**: Display reflects recovery immediately

### ✅ Aggregator State (Entry Replacement)
**Location**: `src/bin/controller.rs:185-186`
```rust
fn update(&mut self, stats: LiveStats) {
    self.agent_stats.insert(stats.agent_id.clone(), stats);  // REPLACES entire entry
}
```
- **Recovery**: `insert()` replaces entire `LiveStats` entry
- **Side Effect**: Resets `completed` flag from recovered agent's message
- **Conclusion**: No stale state from Disconnected period

### ✅ Cumulative Stats (No Delta Loss)
**Critical Design**: Agents send **cumulative totals**, not deltas!

**Agent Side** (`src/live_stats.rs:80-82, 154-158`):
```rust
pub fn record_get(&self, bytes: usize, latency: Duration) {
    self.get_ops.fetch_add(1, Ordering::Relaxed);  // INCREMENTS counter
}

pub fn snapshot(&self) -> LiveStatsSnapshot {
    let get_ops = self.get_ops.load(Ordering::Relaxed);  // LOADS TOTAL
}
```

**Controller Side** (`src/bin/controller.rs:196, 225+`):
```rust
fn update(&mut self, stats: LiveStats) {
    self.agent_stats.insert(stats.agent_id.clone(), stats);  // Store latest totals
}

fn aggregate(&mut self) -> AggregateStats {
    let mut total_get_ops = 0u64;
    // Sum current totals across all agents
}
```

**Why This Matters for Recovery**:
- If agent disconnects for 30 seconds, it misses ~30 stat updates
- When it reconnects, it sends **current total** (e.g., 1,500,000 ops)
- Controller replaces old entry with new total
- **No operations lost** - controller always has accurate cumulative count
- This is the **correct design** for unreliable networks

**Alternative (Bad) Design**: If agents sent deltas ("+1000 ops per second"):
- ❌ Missed updates = lost operations in controller's count
- ❌ Controller would need complex replay/recovery logic
- ❌ Final totals could be wrong

**Conclusion**: Cumulative totals make the system **inherently resilient** to communication gaps

### ✅ Exit Logic (Dynamic Check)
**Location**: `src/bin/controller.rs:1674, 203`
```rust
if aggregator.all_completed() {  // Line 1674
    break;
}

fn all_completed(&self) -> bool {  // Line 203
    !self.agent_stats.is_empty() && 
    self.agent_stats.values().all(|s| s.completed)
}
```
- **Recovery**: When recovered agent sends `completed=false`, `all_completed()` returns false
- **Behavior**: Controller continues waiting for recovered agent
- **Conclusion**: Exit logic responds correctly to recovery

---

## Testing Recommendations

### Unit Tests Needed
1. **State machine transitions**: Verify all recovery paths allowed
2. **Timeout logic**: Mock time to test 60s timeout behavior
3. **Recovery flow**: Test Disconnected → Running → Completed sequence

### Integration Tests Needed
1. **Backpressure simulation**: Slow controller receiver to trigger timeout
2. **Recovery verification**: Confirm agent re-integration after timeout
3. **Final status check**: Verify test passes when all agents complete after recovery

### Production Testing
1. **Large prepare phase**: 400K+ objects to stress gRPC streaming
2. **Multiple agents**: 8+ agents to expose coordination issues
3. **Network latency**: Cloud VMs across regions to trigger realistic delays

---

## Code Review Checklist

When reviewing state transition code, verify:

- [ ] All state transitions are in `can_transition()` allowed list
- [ ] No `let _ =` on critical transitions (check errors!)
- [ ] Recovery paths exist for all timeout scenarios
- [ ] Timeouts are appropriate for workload characteristics
- [ ] `is_terminal()` only includes truly terminal states
- [ ] Display/aggregator state recalculated dynamically
- [ ] No persistent degraded state after recovery

---

## Related State Transition Bugs (Potential Future Issues)

### Areas to Monitor
1. **Abort handling**: Does Aborting state have proper recovery paths?
2. **Failed state**: Should Failed agents ever recover? (Currently: no)
3. **Validation failures**: Early validation failures before workload starts
4. **Multiple disconnections**: Agent disconnects, recovers, disconnects again

### Known Limitations
1. **No retry limit**: Agent can disconnect/recover indefinitely
2. **No degradation tracking**: Don't track how many times agent disconnected
3. **No partial results warning**: User not warned if some agents disconnected during test

---

## Performance Implications

### Timeout Increase (10s → 60s)
- **Impact**: Truly dead agents take longer to detect
- **Mitigation**: Warning at 30s provides early visibility
- **Trade-off**: Acceptable given false positive avoidance

### Recovery Overhead
- **State checks**: Minimal - single comparison per message
- **Logging**: WARN level for recovery (visible but not excessive)
- **Progress bar**: Recalculated every 100ms (already existing overhead)

---

## Conclusion

All identified state transition bugs have been fixed with comprehensive verification of recovery mechanisms. The system now supports:

✅ **Automatic recovery** from transient disconnections  
✅ **Full state restoration** without degraded performance  
✅ **Clear visibility** into recovery events via logging  
✅ **Correct final status** reflecting actual agent completion  

**No persistent degraded state remains after recovery.**

---

## Appendix: State Transition Diagram

```
[Connecting] 
    ↓
[Validating] ──→ [Failed]
    ↓
[Ready] ──────→ [Preparing] ──→ [Running] ──→ [Completed]
    ↓              ↓              ↓
    └──────────────┴──────────────┴──→ [Disconnected]
                                            ↓
                    (RECOVERY - NEW IN v0.8.2)
                                            ↓
                        ┌───────────────────┴───────────────────┐
                        ↓                   ↓                   ↓
                  [Preparing]          [Running]          [Completed]

Any active state → [Aborting] → [Completed]/[Failed]/[Disconnected]
```

**Key**: All states can transition to Disconnected. **v0.8.2**: Disconnected can transition back to active states (recovery). **v0.8.4**: Two-channel design eliminates most disconnect scenarios.

---

**Document Version**: 2.0  
**Last Updated**: November 24, 2025  
**Status**: v0.8.4 two-channel architecture design complete, implementation in progress

---

## v0.8.4 Implementation Plan

### Phase 1: Protobuf and Code Generation ✅
- [x] Add `ExecuteWorkload` bidirectional RPC to proto
- [x] Add `ControlMessage` message type  
- [x] Add `sequence` field to `LiveStats`
- [x] Regenerate Rust bindings

### Phase 2: Agent Implementation
- [ ] Implement `execute_workload()` RPC handler
- [ ] Split into two tasks: control reader + stats writer
- [ ] Handle START command with config validation
- [ ] Handle PING command with PONG response
- [ ] Handle ABORT command with graceful cancellation
- [ ] Increment sequence numbers on all LiveStats messages
- [ ] Keep old `run_workload_with_live_stats()` for compatibility

### Phase 3: Controller Implementation
- [ ] Implement `ExecuteWorkload` client
- [ ] Send START command with config
- [ ] Wait for READY from all agents
- [ ] Calculate clock offsets from READY timestamps
- [ ] Send final START with coordinated timestamp
- [ ] Process LiveStats stream (existing aggregation logic)
- [ ] Send periodic PING keepalives
- [ ] Send ABORT on Ctrl-C
- [ ] Acknowledge critical messages (READY, COMPLETED)

### Phase 4: Testing
- [ ] Unit tests for message sequencing
- [ ] Integration tests with simulated backpressure
- [ ] Cloud VM testing with 8+ agents
- [ ] Coordinated start timing verification
- [ ] Abort handling verification
- [ ] Migration from old RPC (compatibility test)

### Phase 5: Documentation
- [ ] Update user guide with new architecture
- [ ] Document migration from v0.8.3
- [ ] Add troubleshooting guide for two-channel issues
- [ ] Update CHANGELOG.md

---

## Expected Benefits

### Reliability
- ✅ No false positive timeouts (control channel never blocked)
- ✅ Graceful abort even during backpressure
- ✅ Explicit acknowledgment of critical messages
- ✅ Gap detection via sequence numbers

### Performance
- ✅ No blocking during coordinated start
- ✅ Stats can flow at full rate without control overhead
- ✅ Controller can ping without disrupting stats stream
- ✅ Lower latency for control operations

### Maintainability
- ✅ Clear separation of concerns (control vs stats)
- ✅ Easier to debug (separate logs for each channel)
- ✅ Future-proof for new control commands
- ✅ Testable in isolation (mock each channel)

---

## Backward Compatibility Strategy

### Transition Period (v0.8.4 - v0.8.6)
Both RPCs available:
- **Old**: `RunWorkloadWithLiveStats` (deprecated, maintained for compatibility)
- **New**: `ExecuteWorkload` (recommended for all new deployments)

### Deprecation (v0.9.0)
Remove `RunWorkloadWithLiveStats`:
- All users migrated to `ExecuteWorkload`
- Simplify codebase by removing old RPC
- Focus maintenance on single architecture

### Auto-Detection
Controller attempts `ExecuteWorkload` first, falls back to `RunWorkloadWithLiveStats` if not supported:
```rust
let result = client.execute_workload(config).await;
if result.is_err() && result.code() == Code::Unimplemented {
    warn!("Agent doesn't support ExecuteWorkload, falling back to old RPC");
    client.run_workload_with_live_stats(config).await?;
}
```
