# Bidirectional Streaming Architecture (v0.8.4+)

**Date**: November 24, 2025  
**Status**: ✅ Production (implemented and tested)

---

## Overview

sai3-bench v0.8.4+ uses **bidirectional streaming** for reliable distributed execution between controller and agents. This architecture solved critical issues with the previous unidirectional streaming approach.

## Architecture

### Communication Model

```
Controller ←──────── Stats Channel ←────────── Agent
           (LiveStats: progress, metrics)

Controller ───────→ Control Channel ──────→ Agent
           (ControlMessage: PING, START, ABORT)
```

**Key Features:**
- Single gRPC bidirectional stream (`ExecuteWorkload`)
- Logically separated message types (stats vs control)
- Non-blocking: Agent can send stats while waiting for control messages
- Controller can send commands even during heavy stats traffic

### RPC Definition

```protobuf
service Agent {
  rpc ExecuteWorkload(stream ControlMessage) returns (stream LiveStats);
}
```

---

## Agent State Machine

**States:** Idle → Ready → Running → Idle

### State Transitions

1. **Idle → Ready** (Phase 1: Configuration)
   - Controller sends: `START(config=..., timestamp=0)`
   - Agent validates config, runs prepare phase if needed
   - Agent sends: `LiveStats{status=READY, timestamp=...}`
   - **Agent waits silently** (no stats while waiting for coordinated start)

2. **Ready → Running** (Phase 2: Coordinated Start)
   - Controller sends: `START(timestamp=T)` when all agents ready
   - Agent starts workload at timestamp T
   - Agent sends: `LiveStats{status=RUNNING, ...}` every 1 second

3. **Running → Idle** (Completion)
   - Workload completes or times out
   - Agent sends: `LiveStats{status=COMPLETED}` (final message)
   - Stream closes

### Control Message Handling

Agent continuously listens for control messages:

- **PING**: Respond with `ACKNOWLEDGE` status (keepalive)
- **START**: Transition to next phase or abort if invalid
- **ABORT**: Cancel workload immediately, send `ABORTED` status

---

## Controller State Machine

**States:** Connecting → ConfigSent → Ready → StartSent → CollectingStats → Completed

### Phase 1: Configuration and Validation

1. **Send config** via `START(config=..., timestamp=0)`
2. **Wait for READY** status from all agents (40s timeout)
3. **Calculate coordinated start time** (now + 2s default)

### Phase 2: Coordinated Start

1. **Send start timestamp** via `START(timestamp=T)` to all agents
2. **Wait for first RUNNING** status from all agents (30s timeout)
3. **Collect live stats** every 1 second from all agents

### Phase 3: Stats Collection

- Receive `LiveStats` messages from all agents
- Aggregate metrics in real-time
- Display progress bar and live statistics
- Handle agent disconnections (mark failed, continue with others)

### Phase 4: Completion

- Wait for `COMPLETED` status from all agents
- Aggregate final results
- Write TSV files (workload_results.tsv, prepare_results.tsv)

---

## Timeout Management

### Agent Timeouts

- **IDLE timeout**: 30 seconds (waiting for initial config)
- **READY timeout**: 60 seconds (during prepare phase, may take time)
- **Check interval**: Every 5 seconds

**Recovery:** Agent self-terminates on timeout, controller marks as failed.

### Controller Timeouts

- **Phase 1 (READY)**: 40 seconds (agents validate config + prepare)
- **Phase 2 (START)**: 30 seconds (agents begin execution)
- **Phase 3 (RUNNING)**: Duration + 10 seconds grace period

**Recovery:** Controller marks agent as failed, continues with remaining agents.

---

## Key Improvements Over Unidirectional Streaming

### Problems Solved

1. **Repeated READY messages** (old bug)
   - **Old**: Keepalive sent READY every 1s → controller confused
   - **New**: Agent sends READY once, waits silently for START

2. **Blocking during coordinated start** (old bug)
   - **Old**: Agent blocked sending stats → controller frozen
   - **New**: Agent silent in Ready state → non-blocking

3. **No failsafe control** (old limitation)
   - **Old**: Controller couldn't send commands during stats streaming
   - **New**: Control channel always available (PING, ABORT)

4. **Clock synchronization** (new feature)
   - Agent calculates clock offset during Phase 1
   - Coordinated start uses controller's reference time
   - All agents start within milliseconds

---

## Testing Results

**Environment:** 2 local agents, file:// backend, 320-file prepare phase

**Synchronization:**
- READY phase: Agents sent READY within **0.049 ms** of each other
- START phase: Agents began workload within **0.55 ms** of each other

**Performance:**
- 105,729 GET operations in 30 seconds
- 10.1 GiB/s aggregate throughput
- Prepare metrics collected correctly (640 PUTs)

**Reliability:**
- ✅ All 148 tests pass (55 unit + 93 integration)
- ✅ Zero compiler warnings
- ✅ READY sent exactly once per agent
- ✅ Clean state transitions (Idle → Ready → Running → Idle)

---

## Implementation Files

**Core Implementation:**
- `src/bin/agent.rs`: Lines 1463-2726 (execute_workload RPC)
- `src/bin/controller.rs`: Full bidirectional streaming controller
- `proto/iobench.proto`: ControlMessage and status codes

**Documentation:**
- This file: High-level architecture overview
- `DISTRIBUTED_TESTING_GUIDE.md`: Usage guide for distributed testing
- `CHANGELOG.md`: Version history and release notes

---

## Usage Example

### Start Agents

```bash
# Start 2 local agents with verbose logging
bash scripts/start_local_agents.sh 2 7761 -v
```

### Run Controller

```bash
# Run distributed test
./target/release/sai3bench-ctl \
  --agents 127.0.0.1:7761,127.0.0.1:7762 \
  run --config tests/configs/local_test_2agents.yaml
```

### Expected Output

```
⏳ Waiting for agents to validate configuration...
  ✅ agent-1 ready
  ✅ agent-2 ready
✅ All 2 agents ready - starting workload execution

⏳ Starting in 0s...   
✅ Starting workload now!

[Progress bar and live stats...]

✅ Distributed workload complete!
```

---

## Future Enhancements

**Potential improvements** (not currently needed):

1. **Streaming progress updates**: Send live stats during prepare phase
2. **Dynamic agent addition**: Add agents mid-execution (complex, low priority)
3. **Checkpoint/resume**: Save state and resume after controller restart
4. **Multi-controller**: Active-passive failover for high availability

**Current status**: Production-ready for all common use cases.

---

## Troubleshooting

### Agent stuck in READY state

**Symptom:** Agent sends READY but never receives START  
**Cause:** Controller didn't receive READY (network issue, controller crashed)  
**Fix:** Agent will timeout after 60s and self-terminate

### Controller shows "0/N agents ready"

**Symptom:** Controller waiting indefinitely for agents  
**Cause:** Agents not running, wrong addresses, firewall blocking  
**Fix:** Check `ps aux | grep sai3bench-agent` and network connectivity

### False positive timeouts

**Symptom:** Agent marked failed but actually running  
**Cause:** gRPC backpressure (large prepare, slow network)  
**Fix:** Increase timeouts via constants in `src/constants.rs`

### READY sent multiple times (old bug)

**Symptom:** Controller receives READY every second  
**Cause:** Using old unidirectional streaming RPC  
**Fix:** Upgrade to v0.8.4+ (this is now fixed)

---

**Last Updated:** November 24, 2025  
**Version:** v0.8.4  
**Status:** Production-ready, fully tested
