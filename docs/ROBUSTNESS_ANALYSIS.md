# Distributed Protocol Robustness Analysis

**Date**: November 24, 2025  
**Context**: Before implementing Phase 4-5, analyze ALL failure modes to ensure production-ready reliability  
**Goal**: Zero false failures in 30-minute real-world tests

---

## Problem Statement

Production tests require ~30 minutes to execute. ANY protocol bug causes complete test failure and restart. We need bulletproof implementation that handles:
- Network issues (latency, packet loss, temporary disconnects)
- Agent failures (crashes, hangs, resource exhaustion)
- Controller failures (user abort, system issues)
- Timing edge cases (clock skew, race conditions)
- Resource contention (backpressure, memory limits)

---

## Current Implementation Status

### ✅ Working (Tested)
1. **Phase 1-2**: Agent control reader and stats writer tasks
2. **Phase 3**: Coordinated start with clock skew handling
3. **Coordinated start test**: 3 agents with ±3.5s clock skew all start synchronously

### ❌ Observed Issues
1. **Repeated READY messages**: Agents send READY every 1s after initial READY
   - Causes: "Invalid state transition: Ready → Ready" errors
   - Impact: Log spam, potential controller confusion
   
2. **Agent-3 missing from controller**: Controller doesn't connect to 3rd agent
   - Root cause: Not yet debugged (was port binding issue in test)
   - Impact: Silent failure - no error reported

3. **No controller-side implementation**: Phase 4 not started
   - Controller can't send control messages to agents
   - Controller can't handle agent streams properly

---

## Failure Mode Analysis

### Category 1: Network Failures

#### 1.1 Agent Can't Connect to Controller
**Scenario**: Agent unreachable (firewall, network down, wrong address)

**Current Behavior**: 
- Controller: Connection timeout after gRPC default (unknown duration)
- Impact: Controller hangs during agent discovery

**Required Behavior**:
- Controller: Fail fast with clear error message (5s timeout)
- Show: "Agent 127.0.0.1:7761 unreachable after 5s"
- Decision: Abort all agents and exit (user must fix config)

**Implementation**:
```rust
// In controller agent connection loop
let connect_timeout = Duration::from_secs(5);
match tokio::time::timeout(connect_timeout, agent_client.connect()).await {
    Ok(Ok(conn)) => /* success */,
    Ok(Err(e)) => return Err(format!("Agent {}: connection failed: {}", addr, e)),
    Err(_) => return Err(format!("Agent {}: connection timeout after 5s", addr)),
}
```

#### 1.2 Stream Fails Mid-Execution
**Scenario**: Network blip, agent crash, resource exhaustion

**Current Behavior**:
- Agent: Stream error → task exits
- Controller: Unknown (not implemented)

**Required Behavior**:
- Agent: Log error, attempt graceful cleanup, exit cleanly
- Controller: Detect stream closure → mark agent FAILED → continue with remaining agents
- Show: "Agent agent-1 disconnected after 450s (completed 89% of work)"
- Decision: Continue test with remaining agents if majority still alive

**Implementation**:
```rust
// Agent stream handling
loop {
    tokio::select! {
        msg = stream.next() => {
            match msg {
                Some(Ok(control_msg)) => /* process */,
                Some(Err(e)) => {
                    error!("Stream error: {}", e);
                    break; // Exit gracefully
                },
                None => {
                    info!("Stream closed by controller");
                    break; // Normal shutdown
                }
            }
        }
    }
}
```

```rust
// Controller stream handling
for agent in agents {
    tokio::spawn(async move {
        let mut stream = agent.stream;
        while let Some(result) = stream.next().await {
            match result {
                Ok(stats) => aggregator_tx.send((agent_id, stats)).await?,
                Err(e) => {
                    error!("Agent {} stream error: {}", agent_id, e);
                    aggregator_tx.send((agent_id, AgentEvent::Failed(e))).await?;
                    break;
                }
            }
        }
        // Send final event
        aggregator_tx.send((agent_id, AgentEvent::Disconnected)).await?;
    });
}
```

#### 1.3 High Latency (>1s)
**Scenario**: WAN links, congested network, cloud routing issues

**Current Behavior**:
- Agent: Stats sent every 1s regardless of latency
- Controller: Unknown (not implemented)

**Required Behavior**:
- Agent: Continue sending stats (gRPC handles backpressure)
- Controller: Track latency, warn if >1s sustained
- Show: "WARNING: Agent agent-3 message latency: 2.4s (network issue?)"
- Decision: Continue test (high latency OK if messages still flow)

**Implementation**:
```rust
// Controller tracks message receive times
struct AgentTracker {
    last_msg_time: Instant,
    last_sequence: u64,
}

// In stats receiver:
let latency = now - tracker.last_msg_time;
if latency > Duration::from_secs(2) {
    warn!("Agent {} high latency: {}ms", agent_id, latency.as_millis());
}
```

---

### Category 2: Agent Failures

#### 2.1 Agent Crashes During Prepare
**Scenario**: OOM, segfault, panic in s3dlio

**Current Behavior**:
- Agent: Process dies
- Controller: Unknown (not implemented)

**Required Behavior**:
- Agent: Cannot prevent (OS-level crash)
- Controller: Detect stream closure → mark agent CRASHED → abort all (prepare is critical)
- Show: "Agent agent-2 crashed during prepare phase (no response for 30s)"
- Decision: ABORT ALL (prepare must succeed for all agents)

**Implementation**:
```rust
// Controller prepare phase timeout
let prepare_timeout = Duration::from_secs(30);
let ready_count = tokio::time::timeout(prepare_timeout, wait_for_all_ready()).await?;

if ready_count < total_agents {
    error!("Only {}/{} agents ready after 30s", ready_count, total_agents);
    send_abort_to_all().await;
    return Err("Prepare phase failed");
}
```

#### 2.2 Agent Hangs During Workload
**Scenario**: Deadlock, infinite loop, stuck I/O operation

**Current Behavior**:
- Agent: No stats sent (stats task blocked)
- Controller: Unknown (not implemented)

**Required Behavior**:
- Controller: Detect no stats for 60s → send PING on control channel
- If no response to PING for 60s → mark agent HUNG
- Show: "Agent agent-1 not responding (no stats for 120s) - marking as hung"
- Decision: Continue with remaining agents

**Implementation**:
```rust
// Controller PING logic
let stats_timeout = Duration::from_secs(60);
let ping_timeout = Duration::from_secs(60);

if now - last_stats_time > stats_timeout {
    warn!("Agent {} no stats for 60s, sending PING", agent_id);
    send_control_message(agent_id, ControlMessage::Ping).await?;
    
    let response_time = wait_for_next_message(agent_id, ping_timeout).await;
    if response_time.is_none() {
        error!("Agent {} not responding to PING - marking HUNG", agent_id);
        mark_agent_hung(agent_id);
    }
}
```

#### 2.3 Agent Sends Invalid Messages
**Scenario**: Protocol bug, memory corruption, version mismatch

**Current Behavior**:
- Agent: Sends malformed protobuf
- Controller: Unknown (likely crash or ignore)

**Required Behavior**:
- Controller: Catch protobuf decode error → log warning → ignore message
- Show: "Agent agent-3: ignoring invalid message (sequence 42): decode error"
- Decision: Continue (one bad message shouldn't kill entire test)

**Implementation**:
```rust
// Controller message handler
match stats_msg {
    Ok(stats) => process_stats(stats),
    Err(DecodeError(e)) => {
        warn!("Agent {}: invalid message: {}", agent_id, e);
        // Don't increment expected sequence (message lost)
    }
}
```

---

### Category 3: Controller Failures

#### 3.1 User Presses Ctrl-C
**Scenario**: User aborts test

**Current Behavior**:
- Controller: SIGINT handler installed (existing code)
- Agent: Unknown how controller signals abort

**Required Behavior**:
- Controller: Send ABORT to all agents → wait 5s for cleanup → force exit
- Agent: Receive ABORT → cancel workload → send final stats → exit
- Show: "Aborting... Sent ABORT to 3 agents... Waiting for cleanup..."
- Decision: Graceful shutdown with partial results if possible

**Implementation**:
```rust
// Controller SIGINT handler
tokio::spawn(async move {
    tokio::signal::ctrl_c().await?;
    info!("Received Ctrl-C - aborting all agents");
    
    // Send ABORT to all
    for agent in &agents {
        send_control_message(agent, ControlMessage::Abort).await.ok();
    }
    
    // Wait for final messages (5s timeout)
    let final_stats = tokio::time::timeout(
        Duration::from_secs(5),
        collect_final_stats()
    ).await;
    
    // Write partial results
    write_results(final_stats);
    
    std::process::exit(0);
});
```

#### 3.2 Controller Crashes
**Scenario**: Panic, OOM, system kill

**Current Behavior**:
- Controller: Process dies
- Agent: Stream closed → exit

**Required Behavior**:
- Agent: Detect stream closure → log warning → exit cleanly
- Show: "Controller disconnected - exiting"
- Decision: No recovery (user must restart test)

**Implementation**: Already handled by stream closure detection

#### 3.3 Controller Hangs (Deadlock)
**Scenario**: Lock contention, blocking I/O, infinite loop

**Current Behavior**: 
- Unknown (not implemented)

**Required Behavior**:
- Prevention: Use tokio async everywhere (no blocking code)
- Detection: Watchdog timer (external monitoring)
- Show: Progress bar must update every second (proves controller alive)
- Decision: If controller hangs, user must kill process

**Implementation**:
```rust
// Progress bar update proves controller alive
loop {
    tokio::select! {
        stats = rx.recv() => process_stats(stats),
        _ = tokio::time::sleep(Duration::from_secs(1)) => {
            // Update progress bar (proves we're not deadlocked)
            update_progress_bar();
        }
    }
}
```

---

### Category 4: Timing Edge Cases

#### 4.1 Race: ABORT Arrives During START Wait
**Scenario**: User Ctrl-C immediately after validation, before workload starts

**Current Behavior**:
- Agent control task: Sleeping until start_timestamp
- Agent stats task: Sending READY stats every 1s
- Unknown if ABORT wakes up sleeper

**Required Behavior**:
- Agent control task: Use interruptible sleep with cancellation token
- On ABORT: Cancel sleep → exit immediately
- Stats task: Detect cancellation → send final ABORTED status
- Decision: Clean abort (no workload started, nothing to clean up)

**Implementation**:
```rust
// Agent coordinated start wait (MUST be interruptible)
let wait_duration = start_timestamp_ns - now_ns;
tokio::select! {
    _ = tokio::time::sleep(Duration::from_nanos(wait_duration as u64)) => {
        // Normal: start time reached
        start_workload();
    }
    _ = cancel_token.cancelled() => {
        // ABORT received during wait
        info!("Coordinated start cancelled by ABORT");
        return;
    }
}
```

#### 4.2 Race: Agent Completes Before Controller Sends START
**Scenario**: Very short workload, agent starts before controller sends second START

**Current Behavior**:
- Phase 2 START (validation): Agent validates config, sends READY
- Controller: Waits for all READY, calculates start time
- Controller: Sends Phase 2 START (coordinated) with timestamp
- Agent: Unknown if second START causes issues

**Required Behavior**:
- Agent: If already running, ignore second START (idempotent)
- Show: "Ignoring duplicate START command (already RUNNING)"
- Decision: Continue (second START is optimization for coordination)

**Implementation**:
```rust
// Agent control message handler
ControlMessage::Start { start_timestamp_ns, .. } => {
    let current_state = state.lock().unwrap().phase;
    match current_state {
        AgentPhase::Idle => {
            // First START: validate config
            validate_and_ready();
        }
        AgentPhase::Ready => {
            // Second START: coordinated start
            coordinated_start(start_timestamp_ns);
        }
        AgentPhase::Running => {
            // Already running: ignore
            warn!("Ignoring START command (already RUNNING)");
        }
        _ => {
            warn!("Ignoring START command in state {:?}", current_state);
        }
    }
}
```

#### 4.3 Sequence Number Overflow
**Scenario**: Very long test (>1 week), sequence number wraps u64

**Current Behavior**:
- Sequence is u64 (18 quintillion messages)
- At 1 msg/sec = 584 million years to overflow

**Required Behavior**:
- No action needed (u64 is sufficient)
- Document assumption: tests last < 1 year

---

### Category 5: Resource Exhaustion

#### 5.1 Agent Runs Out of Memory
**Scenario**: Too many concurrent operations, memory leak

**Current Behavior**:
- Agent: OOM killer terminates process
- Controller: Detects stream closure

**Required Behavior**:
- Prevention: Limit concurrency in config validation
- Detection: Monitor RSS memory growth (future enhancement)
- Decision: Agent crashes are handled by stream closure logic

**Implementation**:
```rust
// Config validation
if config.concurrency > MAX_CONCURRENCY {
    return Err("Concurrency too high (limit: 1024)");
}
```

#### 5.2 Controller Overwhelmed by Stats Messages
**Scenario**: 100 agents × 1 msg/sec = 100 msg/sec to process

**Current Behavior**:
- Unknown (not implemented)

**Required Behavior**:
- Use bounded channel (1000 message buffer)
- If buffer full: drop oldest messages (not newest!)
- Log: "Warning: Dropping old stats messages (processing backlog)"
- Decision: Continue (losing 1s of stats OK, not cumulative)

**Implementation**:
```rust
// Bounded channel with oldest-drop policy
let (tx, mut rx) = mpsc::channel(1000);

// Stats receiver task
while let Some(msg) = rx.recv().await {
    // Try to process within 1s
    tokio::time::timeout(Duration::from_secs(1), process_stats(msg)).await.ok();
}
```

#### 5.3 Disk Full During Results Write
**Scenario**: Large results file, /tmp full

**Current Behavior**:
- Unknown (likely panic or unhandled error)

**Required Behavior**:
- Catch write errors → log error → show summary to stdout anyway
- Show: "ERROR: Cannot write results to disk (disk full)"
- Decision: Print summary to console, exit with error code

**Implementation**:
```rust
// Results writer
match write_results_to_disk(&results) {
    Ok(_) => info!("Results written to {}", path),
    Err(e) => {
        error!("Cannot write results: {}", e);
        println!("=== RESULTS (DISK WRITE FAILED) ===");
        print_summary_to_stdout(&results);
    }
}
```

---

## Critical Fixes Required Before Phase 4

### Fix 1: Stop Repeated READY Messages ❗
**Problem**: Agent sends READY every 1s after initial READY

**Root Cause**: Stats writer loop doesn't distinguish between READY and RUNNING phases

**Solution**:
```rust
// Stats writer task state machine
enum StatsPhase {
    Initial,        // Haven't sent anything yet
    SentReady,      // Sent READY, waiting for workload start
    Running,        // Workload running, send stats every 1s
    Completed,      // Sent COMPLETED, exit
}

let mut phase = StatsPhase::Initial;

loop {
    match phase {
        StatsPhase::Initial => {
            // Send READY once
            send_stats(LiveStats { status: Ready, ... });
            phase = StatsPhase::SentReady;
        }
        StatsPhase::SentReady => {
            // Wait for workload start signal (blocking)
            workload_start_rx.recv().await;
            phase = StatsPhase::Running;
        }
        StatsPhase::Running => {
            // Send stats every 1s
            tokio::time::sleep(Duration::from_secs(1)).await;
            send_stats(LiveStats { status: Running, ... });
        }
        StatsPhase::Completed => {
            // Exit
            break;
        }
    }
}
```

### Fix 2: Proper State Transitions ❗
**Problem**: Invalid state transition errors

**Root Cause**: Agent sends multiple READY messages, controller tries to transition Ready→Ready

**Solution**: Controller must be idempotent
```rust
// Controller agent state handler
match (current_state, new_state) {
    (Ready, Ready) => {
        // Duplicate READY: ignore (agent hasn't fixed repeated messages yet)
        warn!("Agent {} sent duplicate READY (ignoring)", agent_id);
    }
    (Ready, Running) => {
        // Valid transition
        transition_state(agent_id, Running);
    }
    (s1, s2) if s1 == s2 => {
        // Same state: idempotent (no-op)
    }
    (s1, s2) => {
        // Invalid transition: error
        error!("Invalid transition: {:?} -> {:?}", s1, s2);
    }
}
```

### Fix 3: Controller Connection Logic ❗
**Problem**: Controller doesn't connect to all agents

**Root Cause**: Not implemented yet (Phase 4)

**Solution**: Connect to each agent sequentially with timeout
```rust
// Controller startup
for agent_config in config.agents {
    let addr = agent_config.address;
    info!("Connecting to agent {}", addr);
    
    let client = tokio::time::timeout(
        Duration::from_secs(5),
        AgentClient::connect(format!("http://{}", addr))
    ).await??;
    
    agents.push(AgentConnection {
        id: agent_config.id,
        client,
        state: AgentState::Idle,
    });
}
```

---

## Implementation Priority

### Must-Have for Phase 4 (Controller Implementation)
1. ✅ Connection timeout (5s)
2. ✅ Stream error handling (mark FAILED, continue)
3. ✅ Prepare phase timeout (30s, abort all)
4. ✅ Graceful abort (Ctrl-C handler)
5. ✅ Progress bar updates (proves alive)

### Must-Have for Phase 5 (Timeout Logic)
1. ✅ Workload hang detection (60s no stats → PING)
2. ✅ PING timeout (60s no response → HUNG)
3. ✅ Majority-alive decision (continue if >50% OK)

### Nice-to-Have (Future)
1. ⏳ Latency monitoring
2. ⏳ Message retransmission
3. ⏳ Stream reconnection
4. ⏳ Memory monitoring

---

## Testing Plan for Robustness

### Test 1: Network Failures
- **Setup**: 3 agents, kill agent-2 network at 50% completion
- **Expected**: Controller detects disconnect, continues with agents 1 and 3
- **Verify**: Results file has 2 agents, clear error message for agent-2

### Test 2: Agent Crash
- **Setup**: 3 agents, `kill -9` agent-3 during prepare
- **Expected**: Controller detects crash within 30s, aborts all
- **Verify**: Clean abort, no hung controller, clear error message

### Test 3: High Latency
- **Setup**: 2 agents, add 2s latency with `tc` on agent-2
- **Expected**: Test completes, warning logged about latency
- **Verify**: Both agents complete, no false timeout

### Test 4: Controller Abort
- **Setup**: 3 agents, Ctrl-C at 30% completion
- **Expected**: All agents receive ABORT, clean exit within 5s
- **Verify**: Partial results written, all processes exit

### Test 5: Repeated READY Bug
- **Setup**: 3 agents, monitor controller logs
- **Expected**: Each agent sends READY once only
- **Verify**: No "Invalid state transition" errors

### Test 6: Race Conditions
- **Setup**: Very short workload (1s duration)
- **Expected**: No crashes, clean completion
- **Verify**: All agents complete, correct results

---

## Decision Matrix: When to Continue vs Abort

| Scenario | Phase | Decision | Rationale |
|----------|-------|----------|-----------|
| 1 agent unreachable | Startup | ABORT ALL | Can't validate config |
| 1 agent crashes | Prepare | ABORT ALL | Prepare must succeed for all |
| 1 agent crashes | Workload | CONTINUE | Partial results useful |
| Majority agents crash | Workload | ABORT | Results not representative |
| 1 agent hangs | Workload | CONTINUE | Other agents still working |
| Controller Ctrl-C | Any | ABORT ALL | User requested |
| High latency | Workload | CONTINUE | Slow OK, broken NOT OK |
| Invalid message | Workload | IGNORE MSG | One bad message survivable |
| Stream error | Workload | MARK FAILED | Network issue, not test failure |

---

## Success Criteria for Phase 4-5

### Before Merging to Main
- [ ] All 6 robustness tests pass
- [ ] No false timeout positives in 30min test
- [ ] Clean abort works in all phases
- [ ] Progress bar never freezes
- [ ] Clear error messages for all failure modes
- [ ] No "Invalid state transition" errors
- [ ] Controller handles stream errors gracefully

### Documentation Updates
- [ ] Update user guide with troubleshooting section
- [ ] Document network requirements (latency limits)
- [ ] Add runbook for common failure modes
- [ ] Update TWO_CHANNEL_IMPLEMENTATION_PLAN.md with lessons learned

---

## Conclusion

Current implementation (Phase 1-3) has working coordinated start but lacks robustness for production use. Phase 4-5 implementation MUST include all error handling from this document to avoid costly test failures.

**Key Principle**: Every error case should be tested before declaring Phase 4-5 complete.

---

**Status**: Analysis complete, ready to implement Phase 4 with full robustness
