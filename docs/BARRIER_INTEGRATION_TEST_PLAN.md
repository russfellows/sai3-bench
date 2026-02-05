# Barrier Integration Test Plan

**Created**: February 4, 2026  
**Purpose**: Define tests that will FAIL with current incomplete implementation

---

## Why Current Unit Tests Passed (Despite Incomplete Implementation)

Our 19 unit tests test `BarrierManager::check_barrier()` in isolation:
- ✅ Create BarrierManager
- ✅ Mock heartbeat data (agent readiness)
- ✅ Call check_barrier() and verify logic
- ✅ Tests pass because **barrier logic is correct**

But they DON'T test:
- ❌ Agent actually transitions through new states
- ❌ Agent reports readiness at phase boundaries
- ❌ Controller waits for barriers before proceeding
- ❌ End-to-end coordination works

**Result**: Tests passed, but feature is incomplete!

---

## Integration Tests We Need

### Test 1: Agent State Transitions (Unit Test)
**Location**: `src/bin/agent.rs` (agent-side unit test)  
**What it tests**: Agent execution flow uses new state machine

```rust
#[tokio::test]
async fn test_agent_transitions_through_barrier_states() {
    // Create agent with mock controller
    let agent = /* ... */;
    
    // Start workload
    agent.run_workload(request).await;
    
    // ASSERT: Agent transitions through new states
    // Expected flow: Idle → Validating → PrepareReady → Preparing → ...
    
    // Current behavior: Idle → Ready → Running (OLD STATES)
    // This test will FAIL until we fix implementation!
}
```

**Why it will fail**: Agent never calls `transition_to(Validating)`, goes straight to `Ready`

---

### Test 2: Controller Waits at Barriers (Integration Test)
**Location**: `src/bin/controller.rs` (controller-side integration test)  
**What it tests**: Controller doesn't proceed until barrier satisfied

```rust
#[tokio::test]
async fn test_controller_waits_for_prepare_barrier() {
    // Start 2 mock agents
    let agent1 = MockAgent::new();
    let agent2 = MockAgent::new();
    
    // Start workload with AllOrNothing barrier
    let controller = Controller::new(/* ... */);
    
    // Agent1 reports PrepareReady immediately
    agent1.report_ready(WorkloadPhase::Prepare);
    
    // ASSERT: Controller is WAITING (hasn't started execute phase)
    assert!(controller.phase() == WorkloadPhase::Prepare);
    
    // Agent2 reports PrepareReady (barrier satisfied)
    agent2.report_ready(WorkloadPhase::Prepare);
    
    // ASSERT: Controller proceeds to Execute phase
    assert!(controller.phase() == WorkloadPhase::Execute);
}
```

**Why it will fail**: Controller doesn't have barrier waiting logic yet

---

### Test 3: Agent Reports Readiness (Integration Test)
**Location**: `tests/barrier_integration.rs` (new integration test file)  
**What it tests**: Agent sends ReportBarrierReady RPC at phase boundaries

```rust
#[tokio::test]
async fn test_agent_reports_readiness_at_phase_boundaries() {
    // Start real agent
    let agent = start_test_agent().await;
    
    // Start workload
    agent.run_workload(config).await;
    
    // ASSERT: Agent sent ReportBarrierReady for Prepare phase
    let readiness_calls = mock_controller.get_readiness_calls();
    assert!(readiness_calls.contains(&WorkloadPhase::Prepare));
    
    // ASSERT: Agent sent ReportBarrierReady for Execute phase
    assert!(readiness_calls.contains(&WorkloadPhase::Execute));
}
```

**Why it will fail**: Agent doesn't call ReportBarrierReady RPC anywhere!

---

### Test 4: End-to-End with Real Agents (E2E Test)
**Location**: `tests/e2e_barrier_sync.rs` (new E2E test)  
**What it tests**: Full distributed coordination works

```rust
#[tokio::test]
async fn test_two_agents_synchronize_at_barriers() {
    // Start 2 real agents (in-memory, not separate processes)
    let agent1 = start_agent("agent1").await;
    let agent2 = start_agent("agent2").await;
    
    // Start controller with AllOrNothing barriers
    let controller = start_controller(vec![agent1, agent2]).await;
    
    // Run workload
    controller.run_workload(config).await;
    
    // ASSERT: Both agents finished prepare before ANY started execute
    let agent1_prepare_end = agent1.phase_end_time(Prepare);
    let agent2_prepare_end = agent2.phase_end_time(Prepare);
    let agent1_execute_start = agent1.phase_start_time(Execute);
    let agent2_execute_start = agent2.phase_start_time(Execute);
    
    assert!(agent1_prepare_end < agent1_execute_start);
    assert!(agent2_prepare_end < agent2_execute_start);
    assert!(agent1_prepare_end < agent2_execute_start);
    assert!(agent2_prepare_end < agent1_execute_start);
    
    // This proves barrier synchronization worked!
}
```

**Why it will fail**: No barrier coordination exists, agents work independently

---

## Implementation Order

1. **Write Test 3 first** (simplest to verify)
   - Shows agent doesn't report readiness
   - Easy to mock controller and verify RPC calls
   
2. **Fix agent to report readiness**
   - Add ReportBarrierReady calls at phase boundaries
   - Test 3 should now pass
   
3. **Write Test 1** (agent state transitions)
   - Shows agent doesn't use new states
   
4. **Fix agent state machine**
   - Update run_workload_with_live_stats() to use new states
   - Test 1 should now pass
   
5. **Write Test 2** (controller waits)
   - Shows controller doesn't wait at barriers
   
6. **Fix controller to wait**
   - Add barrier waiting logic before phase transitions
   - Test 2 should now pass
   
7. **Write Test 4** (E2E)
   - Validates everything works together
   - Should pass if Tests 1-3 pass

---

## Success Criteria

All 4 integration tests pass, proving:
- ✅ Agents transition through new states
- ✅ Agents report readiness at barriers
- ✅ Controller waits for barriers before proceeding
- ✅ End-to-end coordination works correctly

Then we can confidently say: **Barrier synchronization is FULLY IMPLEMENTED**
