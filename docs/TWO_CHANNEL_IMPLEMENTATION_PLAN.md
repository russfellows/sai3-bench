# Two-Channel Bidirectional Streaming Implementation Plan

**Version**: v0.8.4  
**Date**: November 24, 2025  
**Status**: Design approved, implementation in progress

---

## Overview

Implement bidirectional streaming RPC with logical separation into control and stats channels to solve blocking and backpressure issues in distributed execution.

---

## Architecture Principles

### Core Concept
**One gRPC bidirectional stream, two logical message flows, two independent tokio tasks**

```
Agent Process
├── Control Reader Task
│   └── Receives: ControlMessage (PING, START, ABORT, ACK)
│       ├── Validates START config
│       ├── Spawns workload on START command
│       ├── Cancels workload on ABORT command
│       └── Responds to PING with stats
│
└── Stats Writer Task
    └── Sends: LiveStats (READY, RUNNING, COMPLETED)
        ├── READY: Config validated, awaiting START
        ├── RUNNING: Progress updates every 1s
        └── COMPLETED: Final results with summary
```

### Key Design Decisions

1. **Task Independence**: Control reader and stats writer run in separate tokio tasks
   - Stats writer never blocks on control messages
   - Control reader processes immediately (no queuing behind stats)
   
2. **Channel Communication**: Use tokio channels for inter-task coordination
   - Control task → Workload task: mpsc (start signal, abort signal)
   - Workload task → Stats task: watch (latest snapshot)
   - Stats task → gRPC stream: async (back-pressure handled by gRPC)

3. **Sequence Numbers**: Every LiveStats message has monotonic sequence
   - Controller can detect gaps (dropped messages)
   - Controller can ACK specific messages (READY, COMPLETED)
   - Agent can retransmit if ACK timeout

4. **State Synchronization**: Agent state machine shared via Arc<Mutex<>>
   - Control task transitions state on START/ABORT
   - Stats task reads state for status field
   - Workload task transitions state on completion/error

---

## Implementation Phases

### Phase 1: Agent - Control Reader Task ✅ (in progress)

**Purpose**: Process incoming control messages from controller

**Responsibilities**:
1. Receive ControlMessage from bidirectional stream
2. Handle START command:
   - Parse config_yaml
   - Validate configuration
   - Store start_timestamp_ns for coordinated start
   - Signal stats writer to send READY
3. Handle PING command:
   - Respond with current stats snapshot (lightweight)
4. Handle ABORT command:
   - Cancel workload via cancellation token
   - Transition state to ABORTING
5. Handle ACKNOWLEDGE command:
   - Mark message as received (for retransmit logic)

**Error Handling**:
- Invalid config → send LiveStats(ERROR) immediately
- Stream closed → transition to IDLE and exit gracefully
- Unexpected command → log warning, continue processing

**Exit Conditions**:
- Stream closed by controller (normal shutdown)
- ABORT received (cancel and exit)
- Agent process shutdown signal (SIGINT/SIGTERM)

---

### Phase 2: Agent - Stats Writer Task ✅ (in progress)

**Purpose**: Send live statistics to controller

**Responsibilities**:
1. Wait for START signal from control task
2. Send LiveStats(READY) with agent_timestamp_ns for clock sync
3. Wait for workload spawn signal from control task
4. Loop every 1 second:
   - Read snapshot from workload tracker
   - Increment sequence number
   - Send LiveStats(RUNNING) with current stats
5. When workload completes:
   - Send LiveStats(COMPLETED) with final_summary
6. On ABORT:
   - Send LiveStats(ERROR or COMPLETED) based on cleanup result

**Sequence Number Management**:
- Start at 1 (READY message)
- Increment by 1 for each message
- Never reset during stream lifetime
- Include in every LiveStats message

**Keepalive Strategy**:
- Send stats even if counters unchanged (proves agent alive)
- Include in_prepare_phase flag for controller visibility
- Send even during coordinated start wait (status=READY)

**Error Handling**:
- Stream send error → log and exit (controller disconnected)
- Tracker snapshot error → send zero stats, log warning
- gRPC backpressure → await naturally (no timeout, control channel handles keepalive)

---

### Phase 3: Agent - Workload Lifecycle Integration

**Purpose**: Connect new RPC to existing workload execution engine

**Key Integration Points**:

1. **Config Validation** (existing code in `run_workload_with_live_stats`):
   - Reuse parse_workload_config()
   - Reuse validation logic
   - Send ERROR status if validation fails

2. **Coordinated Start** (NEW logic):
   - Control task stores start_timestamp_ns
   - Stats task sends READY with agent_timestamp_ns
   - Control task waits for controller START command
   - On START: calculate wait_duration from start_timestamp_ns
   - Sleep until start time (no blocking on stream!)
   - Signal workload spawn

3. **Workload Execution** (existing LiveStatsTracker):
   - Spawn WorkloadRunner in separate tokio task
   - Use existing LiveStatsTracker for metrics
   - Stats task reads snapshots every 1s
   - No changes to core workload engine

4. **Prepare Phase** (existing logic):
   - Workload task sends prepare metrics via channel
   - Stats task includes in LiveStats messages
   - Controller receives in_prepare_phase=true stats

5. **Completion** (existing cleanup):
   - Workload completes → sends summary via channel
   - Stats task converts to WorkloadSummary proto
   - Send LiveStats(COMPLETED) with final_summary
   - Control task exits, stream closes

6. **Abort Handling** (NEW logic):
   - Control task receives ABORT command
   - Cancel workload via CancellationToken
   - Workload task cleans up (5s timeout)
   - Stats task sends final message
   - Both tasks exit

---

### Phase 4: Controller - Bidirectional Stream Client

**Purpose**: Open bidirectional stream and manage both channels

**Responsibilities**:

1. **Initial START Command**:
   - Connect to each agent
   - Open bidirectional stream
   - Send ControlMessage(START) with config
   - Store stream handle for each agent

2. **READY Collection** (startup handshake):
   - Receive LiveStats(READY) from each agent
   - Extract agent_timestamp_ns for clock sync
   - Calculate clock offsets
   - Track which agents are ready (state machine)
   - Timeout if agents don't respond (60s)

3. **Final START Command** (coordinated start):
   - Calculate final start_timestamp_ns (now + delay)
   - Send ControlMessage(START) with final timestamp to all agents
   - Agents begin workload at synchronized time

4. **Stats Collection Loop**:
   - Receive LiveStats(RUNNING) from all agents
   - Aggregate metrics across agents
   - Update progress bar
   - Store per-agent stats

5. **Keepalive PINGs**:
   - Send ControlMessage(PING) every 30s
   - Prevents false timeout during backpressure
   - Agents respond with next stats message (no special PONG needed)

6. **Abort Handling**:
   - User presses Ctrl-C
   - Send ControlMessage(ABORT) to all agents
   - Wait for final LiveStats from each agent (5s timeout)
   - Display abort summary

7. **Completion Detection**:
   - Receive LiveStats(COMPLETED) from all agents
   - Extract final_summary from each
   - Write per-agent results
   - Aggregate histograms
   - Write consolidated results

**Task Structure**:
```
For each agent:
  ├── Stream Writer Task
  │   └── Sends: ControlMessage (START, PING, ABORT, ACK)
  │
  └── Stream Reader Task
      └── Receives: LiveStats → forward to aggregator channel
```

**Error Handling**:
- Agent stream error → mark agent as FAILED, continue with others
- Timeout waiting for READY → abort all agents, exit
- Agent doesn't support ExecuteWorkload → fallback to old RPC
- Partial completion → report which agents succeeded/failed

---

### Phase 5: Controller - Timeout and Disconnect Logic

**Purpose**: Detect and handle agent failures gracefully

**Timeout Strategy** (REVISED for two-channel):

1. **Startup Timeout** (unchanged):
   - 30s base + 5s per agent for READY messages
   - Failure: abort all agents, exit with error

2. **Workload Timeout** (IMPROVED):
   - No fixed timeout for stats messages
   - Send PING every 30s on control channel
   - If agent doesn't respond to PING within 60s → mark STALLED
   - Distinction: stats backpressure OK, control channel failure NOT OK

3. **Completion Timeout** (unchanged):
   - After last agent sends COMPLETED, wait 5s for stragglers
   - Failure: log warning, use partial results

**Disconnect Detection**:
- Stream closed unexpectedly → mark DISCONNECTED
- But: immediately try to receive final message (may be in flight)
- If receive COMPLETED after disconnect → mark COMPLETED (recovery)

**Graceful Degradation**:
- If 1 agent fails: continue with remaining agents
- If majority fail: warn user, continue if possible
- If all fail: abort and report error

---

### Phase 6: Backward Compatibility Layer

**Purpose**: Support both old and new RPC during transition

**Strategy**:

1. **Agent Side**:
   - Keep `run_workload_with_live_stats()` implementation
   - Add `execute_workload()` implementation
   - Both share same workload engine (LiveStatsTracker)
   - No code duplication for core logic

2. **Controller Side**:
   - Try ExecuteWorkload first
   - If Code::Unimplemented error → fallback to RunWorkloadWithLiveStats
   - Log which RPC is used (info level)
   - Warn if using deprecated RPC

3. **Feature Flag** (optional):
   - Environment variable: FORCE_OLD_RPC=1
   - CLI flag: --use-old-rpc
   - Useful for debugging/comparison

**Deprecation Timeline**:
- v0.8.4: Both RPCs available, ExecuteWorkload recommended
- v0.8.5: Warn on old RPC usage
- v0.9.0: Remove RunWorkloadWithLiveStats entirely

---

## Testing Strategy

### Unit Tests

1. **Control Message Parsing**:
   - Valid START command → config extracted correctly
   - Invalid config → error returned
   - PING command → no-op (just ACK)
   - ABORT command → cancellation triggered

2. **Sequence Number Logic**:
   - Increment by 1 per message
   - No gaps in sequence
   - Starts at 1 (READY)

3. **State Transitions**:
   - IDLE → READY → RUNNING → IDLE
   - IDLE → READY → ABORTING → IDLE
   - Invalid transitions rejected

### Integration Tests

1. **Two-Channel Independence**:
   - Simulate stats backpressure
   - Verify control messages still flow
   - Verify PING received during backpressure

2. **Coordinated Start**:
   - 3 agents with simulated clock skew
   - Verify all start within 10ms of target time
   - Verify clock offset calculation correct

3. **Abort Handling**:
   - Send ABORT during prepare phase
   - Send ABORT during workload phase
   - Verify graceful cleanup in both cases

4. **Error Recovery**:
   - Agent sends ERROR during validation
   - Controller marks agent FAILED, continues with others
   - Verify partial results collected

### Cloud Testing

1. **8+ Agent Distributed Workload**:
   - Large prepare phase (400K objects)
   - Monitor for false timeout positives
   - Verify no controller freezing

2. **Network Latency**:
   - Agents in different regions
   - High latency (100ms+)
   - Verify coordination still works

3. **Graceful Shutdown**:
   - Ctrl-C on controller
   - Verify all agents receive ABORT
   - Verify clean exit on all nodes

---

## Success Criteria

### Functional Requirements
- ✅ Controller never freezes waiting for stats
- ✅ No false positive timeouts during backpressure
- ✅ Graceful abort works in all scenarios
- ✅ Coordinated start accurate within 100ms
- ✅ Backward compatible with old RPC

### Performance Requirements
- ✅ Stats throughput same as old RPC (1 msg/s per agent)
- ✅ Control message latency < 100ms
- ✅ Memory overhead < 1MB per agent
- ✅ No measurable workload performance impact

### Reliability Requirements
- ✅ Zero data loss (all stats delivered or detected as lost)
- ✅ No deadlocks or race conditions
- ✅ Clean shutdown on all signal types
- ✅ Works with 100+ agents (scalability)

---

## Risk Mitigation

### Risk: Bidirectional Streaming Complexity
**Mitigation**: Extensive unit tests, gradual rollout, keep old RPC available

### Risk: Sequence Number Gaps Not Detected
**Mitigation**: Log gaps at WARN level, add monitoring in Phase 5

### Risk: Control Channel Backpressure
**Mitigation**: Use bounded channel with small buffer (16 messages), drop oldest if full

### Risk: Clock Sync Inaccuracy
**Mitigation**: Log offsets > 1s at WARN level, document NTP requirement

### Risk: Backward Compatibility Bugs
**Mitigation**: Integration tests covering both RPCs, side-by-side comparison

---

## Implementation Timeline

- **Phase 1-2** (Agent tasks): 1-2 days
- **Phase 3** (Integration): 1 day  
- **Phase 4** (Controller): 1-2 days
- **Phase 5** (Timeout logic): 1 day
- **Phase 6** (Compatibility): 0.5 days
- **Testing**: 1-2 days

**Total Estimate**: 5-8 days for complete implementation and testing

---

## Future Enhancements

### Message Retransmission
- Agent retransmits if no ACK within 5s
- Controller deduplicates based on sequence number
- Ensures critical messages (READY, COMPLETED) never lost

### Adaptive Keepalive
- Increase PING frequency if detecting high latency
- Decrease if network stable (save bandwidth)
- Monitor round-trip time

### Stream Reconnection
- If stream fails, agent tries to reconnect
- Controller accepts reconnect and resumes
- Agent sends last known sequence number
- Controller sends missed messages

### Enhanced Diagnostics
- Track per-agent control message latency
- Dashboard showing channel health
- Alerts for abnormal patterns

---

**Document Status**: Design complete, implementation starting Phase 1-2
