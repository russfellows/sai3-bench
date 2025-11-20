# State Machine Architecture (v0.8.0)

## Overview

sai3-bench uses formal state machines for reliable distributed execution:
- **Agent**: 5-state machine for workload lifecycle
- **Controller**: 9-state machine for tracking agent health

## Agent State Machine (5 States)

```
IDLE → READY → RUNNING → IDLE
         ↓        ↓
      FAILED   ABORTING
         ↓        ↓
       IDLE ← IDLE
```

### States

**IDLE**: Ready to accept new workload
- Validates incoming requests
- Transitions to READY (success) or FAILED (validation error)

**READY**: Validation passed, waiting for coordinated start
- Sent READY(1) status to controller
- Waits 0-60s for start_timestamp
- Can be aborted instantly (no cleanup needed)

**RUNNING**: Workload actively executing
- Sends RUNNING(2) status every 1 second
- May include prepare phase (tracked separately)
- Transitions to IDLE (success), FAILED (error), or ABORTING (abort signal)

**FAILED**: Error occurred during validation or execution
- Sends ERROR(3) status with error message
- **Auto-resets to IDLE** after sending error (agents accept new requests immediately)

**ABORTING**: Graceful shutdown in progress
- Workload task cancelling, cleaning up resources
- 5s timeout for graceful shutdown, 15s for forced shutdown
- Transitions to IDLE after cleanup

### Key Features

- **Auto-reset**: FAILED → IDLE transition happens automatically (no agent restart needed)
- **Thread-safe**: All state changes use Arc<Mutex<>> for concurrent access
- **Transition validation**: Invalid state changes are logged and rejected
- **Abort intelligence**: Different cleanup for READY (instant) vs RUNNING (5-15s)

## Controller Agent Tracking (9 States)

```
CONNECTING → VALIDATING → READY → RUNNING → COMPLETED
                ↓           ↓        ↓
              FAILED ← FAILED ← FAILED
                                 ↓
                            DISCONNECTED
                                 ↓
                             STALLED
                                 ↓
                             ABORTING
```

### States

**CONNECTING**: Stream opened, waiting for first message
**VALIDATING**: First message received, validation in progress
**READY**: Agent sent READY(1) status
**RUNNING**: Workload executing, receiving RUNNING(2) status
**COMPLETED**: Agent sent COMPLETED(4) status
**FAILED**: Agent sent ERROR(3) or validation failed
**DISCONNECTED**: Stream closed unexpectedly
**STALLED**: No updates for 10+ seconds (timeout detection)
**ABORTING**: Abort signal sent, waiting for acknowledgment

### Why More States Than Agent?

Controller sees network layer + validation phases that agents don't track internally:
- **CONNECTING**: Network handshake (agent already in IDLE when RPC arrives)
- **VALIDATING**: Controller distinguishes validation-in-progress from READY
- **DISCONNECTED**: Network failure vs clean completion
- **STALLED**: Timeout detection (agent doesn't track this about itself)

## Implementation Details

**Files**:
- Agent: `src/bin/agent.rs` (lines 75-125: WorkloadState enum)
- Controller: `src/bin/controller.rs` (lines 24-65: ControllerAgentState enum, AgentTracker struct)

**State Storage**:
```rust
// Agent
Arc<Mutex<WorkloadState>>

// Controller
HashMap<String, AgentTracker>  // agent_id -> tracker
```

**Transition Validation**:
```rust
impl WorkloadState {
    fn can_transition_to(&self, new_state: &WorkloadState) -> bool {
        // Returns true only for valid transitions
    }
}
```

## Error Handling

**Agent Side**:
1. Error occurs during workload execution
2. Transition Running → Failed
3. Send ERROR(3) status to controller
4. **Auto-reset**: Transition Failed → Idle
5. Ready for next workload

**Controller Side**:
1. Receives ERROR(3) status from agent
2. Update AgentTracker state to Failed
3. Store error_message for display
4. Mark agent as failed in final results
5. Agent auto-resets, available for next request

## Signal Handling (v0.8.0)

Both binaries handle SIGINT (Ctrl-C) and SIGTERM gracefully:

**Agent**:
- Transitions to ABORTING state
- Cancels workload task
- Cleans up resources (5-15s timeout)
- Exits with code 130 (SIGINT) or 143 (SIGTERM)

**Controller**:
- Sends abort to all Running agents
- Waits for acknowledgment (5s timeout)
- Exits cleanly with proper exit code

## Testing

State machines verified with 153 passing tests including:
- Multiple sequential workloads without agent restart
- Error recovery and auto-reset
- Abort handling in different states
- Timeout detection and stall handling
- Signal handling (SIGINT/SIGTERM)

See `tests/multi_process_tests.rs` and `tests/config_tests.rs` for state transition tests.

## Future Enhancements

Potential improvements for v0.8.1+:
- gRPC keepalive for faster disconnect detection
- Reconnect support (DISCONNECTED → CONNECTING)
- Pause/resume states for long-running workloads
- More granular abort reasons (user vs timeout vs disconnect)

## References

- Detailed design decisions: `docs/archive/STATE_MACHINE_DESIGN_v0.8.0_detailed.md`
- Implementation history: `docs/CHANGELOG.md#080`
- Priority fixes tracking: `docs/archive/PRIORITY_FIXES_v0.8.0.md`
