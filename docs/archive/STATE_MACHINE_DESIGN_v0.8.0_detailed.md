# Optimal State Machine Design (5 States)

## Analysis of Current Flow

**Actual execution timeline:**
1. RPC arrives → validation (sync, ~50ms)
2. Send READY status → controller knows validation passed
3. Wait for coordinated start time (0-60 seconds) ← **Critical: can be aborted**
4. Spawn workload task
5. Execute workload (0-3600 seconds) ← **Critical: can be aborted**
6. Send final stats → return to Idle

**Key insight:** There are **TWO distinct waiting periods** where abort must work differently:
- **Coordinated start delay** (0-60s): Waiting to begin, no cleanup needed, just exit stream
- **Workload execution** (0-3600s): Active work, need cleanup (cancel tasks, close files)

## Proposed 5-State Model

```
┌──────┐  RPC arrives
│ IDLE │  (validation happens here, <100ms)
└───┬──┘  
    │
    ▼  validation success → send READY(1)
┌───────┐
│ READY │  Waiting for coordinated start time
└───┬───┘  (0-60 seconds, interruptible)
    │
    ▼  start_timestamp reached → spawn workload
┌──────────┐
│ RUNNING  │  Workload executing (may include prepare phase)
└────┬─────┘  
     │
     ├──► duration elapsed ──────► IDLE (auto-reset after sending COMPLETED)
     │
     ├──► workload error ────────► FAILED ──► IDLE (auto-reset after sending ERROR)
     │
     └──► abort signal ──────────► ABORTING ──► IDLE (after 5-15s timeout)
```

### State Definitions

**1. IDLE**
- Ready to accept new workload RPC
- Validation happens in this state (fast, <100ms)
- Transition: RPC arrives → immediate inline validation → READY or FAILED

**2. READY** 
- Validation passed, READY status sent to controller
- Waiting for coordinated start timestamp
- Can wait 0-60 seconds
- **Abort behavior**: Exit stream immediately, no cleanup needed
- Transition: start_timestamp reached → RUNNING

**3. RUNNING**
- Workload task spawned and executing
- May be in prepare phase (tracked by `in_prepare_phase` flag)
- Sending RUNNING(2) status every 1 second
- **Abort behavior**: Signal workload task, wait for cleanup (5-15s)
- Transition: completion → IDLE, error → FAILED, abort → ABORTING

**4. FAILED**
- Error occurred during validation, prepare, or execution
- ERROR(3) status sent to controller with error_message
- Auto-transition to IDLE after sending error (no lingering)
- Transition: (automatic) → IDLE

**5. ABORTING**
- Abort signal received (from AbortWorkload RPC or controller disconnect)
- Workload task cancelling, cleaning up resources
- Wait up to 5s for graceful cleanup, then force to IDLE
- Transition: (timeout 5-15s) → IDLE

## State Transition Matrix

| From     | Event                  | Guard               | Action                          | To       |
|----------|------------------------|---------------------|---------------------------------|----------|
| IDLE     | RPC_RECEIVED           | validation passes   | Send READY(1), start timer      | READY    |
| IDLE     | RPC_RECEIVED           | validation fails    | Send ERROR(3), log error        | FAILED   |
| IDLE     | RPC_RECEIVED           | state != IDLE       | Reject: "agent busy"            | IDLE     |
| READY    | START_TIME_REACHED     | -                   | Spawn workload task             | RUNNING  |
| READY    | ABORT_RECEIVED         | -                   | Exit stream, no cleanup         | IDLE     |
| READY    | CONTROLLER_DISCONNECT  | -                   | Exit stream, no cleanup         | IDLE     |
| RUNNING  | DURATION_ELAPSED       | no errors           | Send COMPLETED(4), finalize     | IDLE     |
| RUNNING  | WORKLOAD_ERROR         | -                   | Send ERROR(3), cleanup          | FAILED   |
| RUNNING  | ABORT_RECEIVED         | -                   | Signal task cancel              | ABORTING |
| RUNNING  | CONTROLLER_DISCONNECT  | -                   | Signal task cancel              | ABORTING |
| ABORTING | TIMEOUT_5S             | task not done       | Log warning, reset              | IDLE     |
| ABORTING | TIMEOUT_15S            | task still running  | Force kill, reset               | IDLE     |
| ABORTING | TASK_CANCELLED         | -                   | Cleanup complete                | IDLE     |
| FAILED   | (automatic)            | after ERROR sent    | Clear error, reset              | IDLE     |

## Why 5 States (Not 4, Not 7)?

**Why not 4 states?** (combining READY into RUNNING)
- ❌ Can't distinguish "waiting to start" vs "actively executing"
- ❌ Abort behavior is different: READY needs no cleanup, RUNNING needs 5-15s
- ❌ Controller can't tell if agent is stuck in coordinated start vs stuck in workload
- ❌ Timeout detection unclear: is 10s delay during start or during execution?

**Why not 7 states?** (adding Validating, Preparing, Completed)
- ❌ **Validating**: happens inline in <100ms, not worth separate state
- ❌ **Preparing**: tracked by `in_prepare_phase` flag, doesn't change abort behavior
- ❌ **Completed**: just sends status and immediately returns to IDLE, no intermediate state needed

**5 states is optimal:**
- ✅ Clear abort semantics (READY vs RUNNING)
- ✅ Matches protocol status values (READY, RUNNING, ERROR)
- ✅ Simple to implement and reason about
- ✅ Logs show clear state transitions

## Diagram: Complete State Flow

```
                    ┌─────────────────────────────────────┐
                    │         RPC Arrives                 │
                    │  RunWorkloadWithLiveStats()         │
                    └──────────────┬──────────────────────┘
                                   │
                                   ▼
                    ┏━━━━━━━━━━━━━━━━━━━━━━━━┓
                    ┃   IDLE (+ validation)  ┃  ◄─── Start here
                    ┃   Duration: ~50ms      ┃
                    ┗━━━━━━━━┳━━━━━━┳━━━━━━━━┛
                             │      │
              validation OK  │      │  validation failed
              send READY(1)  │      │  send ERROR(3)
                             │      │
                             ▼      ▼
                    ┏━━━━━━━━━━┓  ┏━━━━━━━━┓
                    ┃  READY   ┃  ┃ FAILED ┃───┐
                    ┃  Wait    ┃  ┗━━━━━━━━┛   │ auto-reset
                    ┃  0-60s   ┃                │ after ERROR sent
                    ┗━━━┳━┳━┳━━┛                │
                        │ │ │                   │
     start_timestamp    │ │ │ abort/disconnect  │
     reached            │ │ └───────────────────┤
     spawn workload     │ │                     │
                        │ │                     │
                        ▼ │                     │
                ┏━━━━━━━━━━━━━┓                 │
                ┃   RUNNING   ┃                 │
                ┃  0-3600s    ┃                 │
                ┃  send stats ┃                 │
                ┃  every 1s   ┃                 │
                ┗━┳━━━┳━━━┳━━━┛                 │
                  │   │   │                     │
    duration      │   │   │  abort             │
    elapsed       │   │   │  signal            │
    send          │   │   │                     │
    COMPLETED(4)  │   │   └──────┐              │
                  │   │          │              │
                  │   │ workload │              │
                  │   │ error    │              │
                  │   │ send     │              │
                  │   │ ERROR(3) │              │
                  │   │          │              │
                  │   └────┐     ▼              │
                  │        │  ┏━━━━━━━━━━━┓    │
                  │        └─►┃ ABORTING  ┃    │
                  │           ┃  cleanup  ┃    │
                  │           ┃  5-15s    ┃    │
                  │           ┗━━━━━┳━━━━━┛    │
                  │                 │          │
                  │                 │ timeout  │
                  └─────────────────┴──────────┘
                                    │
                                    ▼
                          ┏━━━━━━━━━━━━━━┓
                          ┃     IDLE     ┃  ◄─── Back to start
                          ┃  (ready for  ┃
                          ┃   new work)  ┃
                          ┗━━━━━━━━━━━━━━┛
```

## Implementation Notes

### State Storage
```rust
enum WorkloadState {
    Idle,
    Ready,        // NEW: separate from Running
    Running,
    Failed,
    Aborting,
}

struct AgentState {
    state: Arc<Mutex<WorkloadState>>,
    error_message: Arc<Mutex<Option<String>>>,
    abort_tx: broadcast::Sender<()>,
}
```

### Transition Validation
```rust
fn can_transition(from: &WorkloadState, to: &WorkloadState) -> bool {
    use WorkloadState::*;
    matches!(
        (from, to),
        (Idle, Ready)           // RPC arrives, validation passes
        | (Idle, Failed)        // RPC arrives, validation fails
        | (Ready, Running)      // Start time reached
        | (Ready, Idle)         // Abort during wait
        | (Ready, Aborting)     // Abort during wait (alternative)
        | (Running, Idle)       // Completed successfully
        | (Running, Failed)     // Workload error
        | (Running, Aborting)   // Abort signal
        | (Aborting, Idle)      // Cleanup complete
        | (Failed, Idle)        // Auto-reset
    )
}
```

### Key Transition Points in Code

**1. IDLE → READY** (line ~488):
```rust
// After validation passes
agent_state.transition_to(WorkloadState::Ready, "validation passed").await?;
// Send READY(1) status to controller
yield Ok(ready_msg);
```

**2. READY → RUNNING** (line ~614):
```rust
// After coordinated start delay
agent_state.transition_to(WorkloadState::Running, "start time reached").await?;
tokio::spawn(async move { /* workload task */ });
```

**3. READY → IDLE** (line ~602 abort case):
```rust
// Abort during coordinated start
_ = abort_rx.recv() => {
    agent_state.transition_to(WorkloadState::Idle, "aborted during wait").await?;
    yield Err(Status::aborted("Aborted during coordinated start"));
    return;
}
```

**4. RUNNING → FAILED** (line ~692 workload error):
```rust
Some(Err(e)) => {
    agent_state.transition_to(WorkloadState::Failed, &e).await?;
    yield Err(Status::internal(e));
    break;
}
```

**5. RUNNING → ABORTING** (abort RPC):
```rust
// In AbortWorkload handler
if current == WorkloadState::Running {
    self.state.transition_to(WorkloadState::Aborting, "abort RPC").await?;
    self.state.send_abort();  // Signal workload task
    // Wait 5-15s for cleanup
}
```

## Comparison to Protocol Status

| Agent State | Protocol Status | Meaning |
|-------------|-----------------|---------|
| Idle        | (none)          | Not connected to controller |
| Ready       | READY (1)       | Validated, waiting to start |
| Running     | RUNNING (2)     | Executing workload |
| Failed      | ERROR (3)       | Validation or execution error |
| Aborting    | RUNNING (2)     | Still sending stats during cleanup |

Note: `completed` field in LiveStats indicates final message, not a separate state.

## Benefits of This Design

1. **Clear abort semantics**: READY exits immediately, RUNNING needs cleanup
2. **Matches reality**: Coordinated start is a distinct waiting period
3. **Debuggable**: State transitions logged, easy to see where agent is stuck
4. **Simple**: 5 states is minimal for the actual behavior needed
5. **Safe**: Validated transitions prevent impossible sequences

---

# Controller State Machine (9 States)

## Why Controller Has More States

The controller tracks the **full lifecycle** of each agent from the moment the gRPC stream opens until it completes or fails. This is different from the agent's internal states because:

1. **Controller sees network layer**: Stream connection before first message
2. **Controller tracks validation**: Period between first message and READY status
3. **Controller tracks prepare separately**: Can distinguish preparing vs running
4. **Controller sees completion**: Agent may disconnect after completing

The controller's view is **"what is this agent doing right now?"** which requires more granularity than the agent's internal state machine.

## Controller's 9-State Model (v0.8.0)

```
┌─────────────┐  gRPC stream opens
│ CONNECTING  │  (stream opened, waiting for first message)
└──────┬──────┘
       │
       ▼  first message arrives
┌─────────────┐
│ VALIDATING  │  (validation in progress, ~50ms)
└──┬─────┬────┘
   │     │
   │     └──► validation fails ──► FAILED
   │
   ▼  send READY(1)
┌─────────────┐
│   READY     │  (waiting for coordinated start, 0-60s)
└──┬─────┬────┘
   │     │
   │     └──► abort/disconnect ──► ABORTING / DISCONNECTED
   │
   ├──► prepare phase starts ──► PREPARING
   │
   └──► start_timestamp reached (no prepare) ──► RUNNING
        
┌─────────────┐
│  PREPARING  │  (in_prepare_phase=true)
└──────┬──────┘
       │
       ▼  prepare complete
┌─────────────┐
│   RUNNING   │  (RUNNING(2) status, executing workload)
└──┬─────┬────┘
   │     │
   │     └──► abort ──► ABORTING
   │
   ├──► duration elapsed ──► COMPLETED
   │
   └──► error ──► FAILED

Terminal states:
┌─────────────┐
│  COMPLETED  │  (workload finished successfully)
└─────────────┘

┌─────────────┐
│   FAILED    │  (error during validation/prepare/execution)
└─────────────┘

┌──────────────┐
│ DISCONNECTED │  (stream closed unexpectedly or timeout)
└──────────────┘

┌─────────────┐
│  ABORTING   │  (abort RPC sent, waiting for cleanup)
└──────┬──────┘
       │
       ├──► cleanup done ──► COMPLETED
       ├──► abort failed ──► FAILED
       └──► lost connection ──► DISCONNECTED
```

## Controller State Definitions

**1. CONNECTING**
- gRPC stream opened, waiting for first message
- Agent may be validating config, but controller hasn't received anything yet
- Timeout: 60 seconds (startup timeout)
- Transition: First message → VALIDATING

**2. VALIDATING**
- First message received from agent
- Agent is validating config (backend credentials, file paths, etc.)
- Controller waiting for READY(1) or ERROR(3) status
- Duration: ~50ms typical
- Transition: READY status → READY, ERROR status → FAILED

**3. READY**
- Agent sent READY(1) status - validation passed
- Waiting for coordinated start_timestamp
- Can wait 0-60 seconds
- Abort: Transitions to ABORTING (agent will exit stream cleanly)
- Transition: Prepare starts → PREPARING, Start time reached → RUNNING

**4. PREPARING**
- Agent reported in_prepare_phase=true
- Creating directories, generating files, pre-warming storage
- Still sending RUNNING(2) status (not a separate protocol status)
- Controller distinguishes this via in_prepare_phase flag
- Transition: in_prepare_phase=false → RUNNING

**5. RUNNING**
- Agent executing workload operations (GET/PUT/DELETE)
- Sending RUNNING(2) status every 1 second with metrics
- in_prepare_phase=false
- Abort: Transitions to ABORTING (5-15s cleanup)
- Transition: Duration elapsed → COMPLETED, Error → FAILED

**6. COMPLETED**
- Workload finished successfully
- Agent sent final LiveStats with completed=true
- Terminal state (no further transitions)
- Stream typically closes after this

**7. FAILED**
- Agent sent ERROR(3) status or validation failed
- error_message contains details
- Terminal state (no further transitions)
- Controller will abort remaining agents

**8. DISCONNECTED**
- gRPC stream closed unexpectedly
- No ERROR status received (just connection loss)
- Can happen during any active state (CONNECTING, VALIDATING, READY, PREPARING, RUNNING)
- Terminal state (no reconnect support in v0.8.0)

**9. ABORTING**
- Controller sent AbortWorkload RPC to agent
- Waiting for agent cleanup (5-15s timeout)
- Agent still sending stats during cleanup
- Transition: Cleanup complete → COMPLETED, Failed → FAILED, Disconnect → DISCONNECTED

## Controller State Transition Matrix

| From        | Event                  | To           | Notes |
|-------------|------------------------|--------------|-------|
| CONNECTING  | First message          | VALIDATING   | Agent alive |
| CONNECTING  | Timeout (60s)          | DISCONNECTED | Agent never responded |
| CONNECTING  | Stream closed          | DISCONNECTED | Agent crashed during startup |
| VALIDATING  | READY(1) received      | READY        | Validation passed |
| VALIDATING  | ERROR(3) received      | FAILED       | Validation failed |
| VALIDATING  | Stream closed          | DISCONNECTED | Agent crashed |
| READY       | in_prepare_phase=true  | PREPARING    | Prepare starts |
| READY       | RUNNING(2), no prepare | RUNNING      | Direct to workload |
| READY       | Abort sent             | ABORTING     | Cancel coordinated start |
| READY       | Stream closed          | DISCONNECTED | Lost connection |
| PREPARING   | in_prepare_phase=false | RUNNING      | Prepare complete |
| PREPARING   | Abort sent             | ABORTING     | Cancel prepare |
| PREPARING   | Stream closed          | DISCONNECTED | Lost connection |
| RUNNING     | completed=true         | COMPLETED    | Success |
| RUNNING     | ERROR(3) received      | FAILED       | Workload error |
| RUNNING     | Abort sent             | ABORTING     | User abort |
| RUNNING     | Stream closed          | DISCONNECTED | Lost connection |
| ABORTING    | completed=true         | COMPLETED    | Cleanup done |
| ABORTING    | ERROR(3) received      | FAILED       | Abort failed |
| ABORTING    | Stream closed          | DISCONNECTED | Lost during abort |
| COMPLETED   | (none)                 | COMPLETED    | Terminal state |
| FAILED      | (none)                 | FAILED       | Terminal state |
| DISCONNECTED| (none)                 | DISCONNECTED | Terminal state |

## Key Differences: Agent vs Controller States

| Agent State | Controller State(s) | Why Different? |
|-------------|---------------------|----------------|
| Idle        | (not tracked)       | Controller only tracks active streams |
| Ready       | CONNECTING → VALIDATING → READY | Controller sees network + validation separately |
| Running     | PREPARING → RUNNING | Controller distinguishes prepare vs workload |
| Failed      | FAILED              | Same meaning |
| Aborting    | ABORTING            | Same meaning |
| (none)      | COMPLETED           | Agent returns to Idle, controller tracks completion |
| (none)      | DISCONNECTED        | Agent doesn't distinguish "ended" vs "lost connection" |

## Implementation in controller.rs (v0.8.0)

```rust
enum ControllerAgentState {
    Connecting,   // Stream opened
    Validating,   // First message, validation in progress
    Ready,        // READY(1) received
    Preparing,    // in_prepare_phase=true
    Running,      // RUNNING(2) received
    Completed,    // Workload done
    Failed,       // ERROR(3) received
    Disconnected, // Stream closed or timeout
    Aborting,     // Abort sent
}

struct AgentTracker {
    agent_id: String,
    state: ControllerAgentState,
    last_seen: std::time::Instant,
    error_message: Option<String>,
    latest_stats: Option<LiveStats>,
}
```

## Why This Design?

**More states enable better debugging:**
- "Agent stuck at CONNECTING" → network issue, agent never started
- "Agent stuck at VALIDATING" → config validation taking too long
- "Agent stuck at READY" → coordinated start delay or clock skew
- "Agent stuck at PREPARING" → prepare phase taking too long
- "Agent stuck at RUNNING" → workload hung or extremely slow

**Terminal states enable proper cleanup:**
- Controller knows when all agents are COMPLETED/FAILED/DISCONNECTED
- Can aggregate final results without missing data
- Can detect partial failures (some COMPLETED, some FAILED)

**Aborting state enables tracking:**
- Controller knows abort is in progress
- Can wait for cleanup instead of immediately killing
- Can detect hung aborts (stuck in ABORTING > 15s)

---

## Summary: Two State Machines Working Together

**Agent (5 states)**: Internal state tracking for workload lifecycle
- Idle → Ready → Running → (Completed/Failed/Aborting) → Idle
- Focus: "What am I doing right now?"

**Controller (9 states)**: External view of agent lifecycle from connection to completion
- Connecting → Validating → Ready → Preparing → Running → (Completed/Failed/Disconnected/Aborting)
- Focus: "What is this agent doing right now, and can I rely on it?"

**Both use validated state transitions** with `can_transition()` checks and logging.

## Recommendation

**✅ Current implementation (v0.8.0) is correct:**
- Agent: 5-state model (Idle, Ready, Running, Failed, Aborting)
- Controller: 9-state model (Connecting, Validating, Ready, Preparing, Running, Completed, Failed, Disconnected, Aborting)

This is the right balance: agent states match internal behavior, controller states match external visibility needs.
