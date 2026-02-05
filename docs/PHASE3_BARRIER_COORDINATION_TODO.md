# Phase 3: Barrier Coordination Implementation

**Status**: NOT IMPLEMENTED (Critical Missing Feature)  
**Priority**: HIGH (Required for true distributed execution)  
**Affects**: Both YAML-driven stages AND deprecated prepare→execute→cleanup flow

---

## Problem Statement

**Current behavior**: Agents execute stages independently without synchronization
- Fast agents race ahead through all stages
- Slow agents lag behind
- **Result**: Agents can be hours out of sync, executing completely different stages

**Required behavior**: Agents must wait at stage barriers until ALL agents (or majority, depending on barrier policy) are ready before proceeding together

---

## Current Implementation Status

### ✅ What Works
- [x] Agents parse stages from YAML config
- [x] Agents execute stages sequentially (per-agent)
- [x] Agents transition to `AtStage{ready_for_next: true}` at barriers
- [x] Controller receives agent state updates via bidirectional gRPC
- [x] Barrier configuration defined (AllOrNothing/Majority/BestEffort)

### ❌ What's Missing
- [ ] **Agent**: Send barrier ready notification to controller
- [ ] **Agent**: Block waiting for controller's barrier release signal
- [ ] **Controller**: Track which agents are ready at which barrier
- [ ] **Controller**: Implement barrier policy enforcement
- [ ] **Controller**: Send RELEASE command when barrier criteria met
- [ ] **Tests**: Verify multi-agent synchronization actually works

---

## Implementation Plan

### 1. Protocol Changes (gRPC)

**New ControlMessage fields** (proto/iobench.proto):
```protobuf
message ControlMessage {
  enum Command {
    START = 0;
    ABORT = 1;
    RELEASE_BARRIER = 2;  // NEW: Controller releases barrier
  }
  
  // Existing fields...
  
  // NEW: Barrier information
  optional string barrier_name = 10;      // e.g., "stage-3-execute"
  optional uint32 barrier_sequence = 11;  // Monotonic counter for ordering
}
```

**New StatsMessage fields**:
```protobuf
message StatsMessage {
  // Existing fields...
  
  // NEW: Barrier readiness
  optional bool at_barrier = 20;
  optional string barrier_name = 21;
  optional uint32 barrier_sequence = 22;
}
```

### 2. Agent-Side Changes

**In execute_stages_workflow()** (src/bin/agent.rs):

```rust
// After transitioning to AtStage{ready_for_next: true}
info!("Stage {} complete, reporting barrier ready", stage.name);

// Send barrier ready via stats channel
let barrier_ready_msg = StatsMessage {
    at_barrier: true,
    barrier_name: format!("stage-{}-{}", stage_index, stage.name),
    barrier_sequence: stage_index as u32,
    // ... other stats ...
};
tx_stats.send(barrier_ready_msg).await?;

// Block waiting for RELEASE command
info!("Waiting for controller to release barrier...");
let release_msg = rx_control.recv().await
    .ok_or("Controller disconnected while waiting at barrier")?;

if release_msg.command != Command::RELEASE_BARRIER {
    return Err("Expected RELEASE_BARRIER, got {:?}", release_msg.command);
}

if release_msg.barrier_sequence != stage_index as u32 {
    return Err("Barrier sequence mismatch: expected {}, got {}", 
               stage_index, release_msg.barrier_sequence);
}

info!("Barrier released by controller, proceeding to next stage");
```

### 3. Controller-Side Changes

**New BarrierManager struct** (src/bin/controller.rs):

```rust
struct BarrierManager {
    /// Current barrier we're coordinating
    current_barrier: Option<BarrierState>,
    
    /// Barrier policy from config
    policy: BarrierType,  // AllOrNothing/Majority/BestEffort
    
    /// Total number of agents
    num_agents: usize,
}

struct BarrierState {
    name: String,
    sequence: u32,
    ready_agents: HashSet<String>,  // agent_ids that are ready
    release_sent: bool,
}

impl BarrierManager {
    /// Report that an agent is ready at a barrier
    fn agent_ready(&mut self, agent_id: &str, barrier_name: &str, sequence: u32) -> Option<BarrierRelease> {
        // Track ready agent
        // Check if barrier criteria met (based on policy)
        // Return Some(release) if ready to proceed, None if still waiting
    }
    
    /// Check if barrier criteria are met
    fn is_barrier_ready(&self) -> bool {
        match self.policy {
            BarrierType::AllOrNothing => {
                self.current_barrier.ready_agents.len() == self.num_agents
            }
            BarrierType::Majority => {
                self.current_barrier.ready_agents.len() > self.num_agents / 2
            }
            BarrierType::BestEffort => {
                // Timeout-based: proceed after N seconds even if not all ready
                true  // Implementation needed
            }
        }
    }
}
```

**In stats receiver loop**:

```rust
// When receiving stats from agent
if stats_msg.at_barrier {
    info!("Agent {} ready at barrier: {}", agent_id, stats_msg.barrier_name);
    
    if let Some(release) = barrier_manager.agent_ready(
        &agent_id, 
        &stats_msg.barrier_name, 
        stats_msg.barrier_sequence
    ) {
        info!("Barrier {} ready - releasing all agents", release.barrier_name);
        
        // Send RELEASE to all agents
        for agent_tx in &agent_control_channels {
            let release_msg = ControlMessage {
                command: Command::RELEASE_BARRIER,
                barrier_name: release.barrier_name.clone(),
                barrier_sequence: release.sequence,
                ...
            };
            agent_tx.send(release_msg).await?;
        }
    }
}
```

### 4. Testing Requirements

**Unit tests** (tests/barrier_coordination.rs):
```rust
#[tokio::test]
async fn test_barrier_all_or_nothing() {
    // Start 3 agents with different execution speeds
    // Verify slow agent holds up fast agents at barrier
    // Verify all proceed together after slow agent ready
}

#[tokio::test]
async fn test_barrier_majority() {
    // Start 5 agents, 2 fail
    // Verify 3 agents proceed when majority (>50%) ready
    // Verify failed agents are excluded
}

#[tokio::test]
async fn test_barrier_sequence_mismatch() {
    // Verify agents reject out-of-order barrier releases
    // Verify error handling for sequence mismatches
}
```

**Integration tests** (examples/test_distributed_stages.sh):
```bash
#!/bin/bash
# Start 3 agents
# Run 5-stage config
# Insert artificial delays in agent-2
# Verify logs show synchronization:
#   - All agents reach stage 1 before any proceed to stage 2
#   - All agents reach stage 2 before any proceed to stage 3
#   - etc.
```

---

## Acceptance Criteria

- [ ] 3+ agents with different execution speeds stay synchronized
- [ ] Fast agents wait at barriers (verified in logs)
- [ ] Controller releases barriers when policy satisfied
- [ ] All barrier policies work (AllOrNothing/Majority/BestEffort)
- [ ] Agents handle controller disconnect while at barrier (graceful degradation)
- [ ] Sequence numbers prevent out-of-order releases
- [ ] Integration test with 5-stage workflow passes

---

## Estimated Effort

- Protocol changes: 2 hours
- Agent implementation: 4 hours
- Controller BarrierManager: 6 hours
- Unit tests: 3 hours
- Integration tests: 2 hours
- Documentation: 1 hour

**Total**: ~18 hours (2-3 days)

---

## Impact of Not Implementing

Without barrier coordination:
- ✅ Single-agent testing works fine
- ✅ Development and debugging can proceed
- ❌ Multi-agent distributed testing is broken
- ❌ Stage-driven workflows are not truly coordinated
- ❌ Performance measurements will be skewed (agents out of sync)
- ❌ Cannot test real-world distributed scenarios

**Workaround**: Use single agent for testing until Phase 3 implemented

---

## Related Files

- `src/bin/agent.rs`: Lines 3265-3290 (stage workflow), lines 2344-2354 (deprecated flow)
- `src/bin/controller.rs`: Stats receiver loop, control message sender
- `proto/iobench.proto`: Protocol definitions
- `tests/barrier_agent_states.rs`: Existing barrier state tests (unit level)
- `docs/DISTRIBUTED_BARRIER_SYNC_DESIGN.md`: Original design document

---

## Notes

This is a **critical missing feature** that affects both new (stage-driven) and old (deprecated) execution flows. The infrastructure exists (bidirectional gRPC, state tracking) but the coordination logic is not implemented.

Phase 2 focused on stage execution per-agent. Phase 3 must implement cross-agent coordination to make this production-ready for distributed testing.
