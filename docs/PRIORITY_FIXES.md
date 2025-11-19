# Priority Fixes - 2-3 Day Sprint

**Date**: November 19, 2025  
**Timeline**: 2-3 days maximum  
**Goal**: Fix critical bugs, defer nice-to-have features

---

## Critical Path (Must-Have)

### Day 1: Core State Machine + Error Detection

#### 1. Agent State Machine (4 hours)
- [x] Current: 3 states (Idle, Running, Aborting)
- [ ] Add: Validating, Ready, Preparing, Completed, Failed
- [ ] Add: `can_transition()` validation
- [ ] Add: `transition_to()` with logging
- [ ] Add: error_message field in AgentState

**Files**: `src/bin/agent.rs` (~200 lines)

#### 2. Controller Agent Tracking (4 hours)
- [x] Current: HashSets (completed, dead, ready)
- [ ] Replace with: `AgentTracker` struct with state enum
- [ ] Add: proper state transitions
- [ ] Add: last_seen timestamp tracking
- [ ] Add: error_message storage

**Files**: `src/bin/controller.rs` (~300 lines)

#### 3. Error Detection & Propagation (2 hours)
- [x] Stream error handling (done today)
- [ ] Validation error handling
- [ ] Workload error handling with proper abort
- [ ] Clear error messages in controller output

**Files**: `src/bin/agent.rs`, `src/bin/controller.rs` (~100 lines)

**Day 1 Goal**: Agents and controller properly track state, errors propagate cleanly

---

### Day 2: Signal Handling + Keepalive

#### 4. Signal Handling (3 hours)
- [ ] SIGHUP handler (agent): abort workload, reset to Idle
- [ ] SIGHUP handler (controller): abort all agents, stay running
- [ ] SIGTERM handler (agent): graceful shutdown (5s max)
- [ ] SIGTERM handler (controller): abort all, exit cleanly
- [ ] Test script: verify SIGHUP doesn't kill processes

**Files**: `src/bin/agent.rs`, `src/bin/controller.rs` (~150 lines)
**New**: `scripts/test_signal_handling.sh`

#### 5. gRPC Keepalive (2 hours)
- [ ] Controller: add keepalive config (5s interval, 20s timeout)
- [ ] Agent: detect disconnect within 30s
- [ ] Agent: cleanup on disconnect (cancel workload, reset to Idle)
- [ ] Test: kill controller, verify agent cleans up

**Files**: `src/bin/controller.rs`, `src/bin/agent.rs` (~50 lines)

#### 6. Timeout Detection Improvements (2 hours)
- [x] Current: 10s timeout marks DEAD
- [ ] Add: warn at 5s, mark dead at 10s
- [ ] Add: heartbeat detection in logs
- [ ] Test: freeze agent (SIGSTOP), verify controller detects

**Files**: `src/bin/controller.rs` (~50 lines)

**Day 2 Goal**: Robust signal handling, quick disconnect detection

---

### Day 3: Testing + Documentation

#### 7. Integration Testing (4 hours)
- [ ] Script: test_state_transitions.sh
  - Start agents, run workload, verify COMPLETED
  - Start agents, send SIGHUP mid-run, verify reset
  - Start agents, kill one agent, verify controller detects
  - Start agents, invalid config, verify FAILED
- [ ] Script: test_error_recovery.sh
  - Invalid S3 credentials → controller exits with error
  - Mid-run S3 error → controller detects and aborts all
- [ ] Script: test_signal_handling.sh (from Day 2)
- [ ] Run all tests, fix bugs

**New files**: `scripts/test_state_transitions.sh`, `scripts/test_error_recovery.sh`

#### 8. Documentation Updates (2 hours)
- [ ] Update README.md with signal handling
- [ ] Update USAGE.md with error recovery behavior
- [ ] Update CHANGELOG.md with all fixes
- [ ] Add troubleshooting section (common errors)

**Files**: `README.md`, `docs/USAGE.md`, `docs/CHANGELOG.md`

#### 9. Final Validation (2 hours)
- [ ] Build release binaries
- [ ] Run full test suite
- [ ] Manual cloud testing (if time permits)
- [ ] Version bump to 0.7.13
- [ ] Tag release

**Day 3 Goal**: Tested, documented, ready to deploy

---

## Deferred (Nice-to-Have, Future Releases)

### Won't Implement This Sprint
1. **Controller reconnect** - Too complex, not critical
2. **Config-driven failure modes** - Can add later if needed
3. **Agent retry logic** - Not needed for current use case
4. **Live stats streaming improvements** - Working well enough
5. **Chaos testing** - Good for CI/CD, not blocking

### Simplifications
- **State machine**: Only add states needed for error handling (skip Preparing state if not used)
- **Keepalive**: Use gRPC built-in, don't build custom heartbeat
- **Error messages**: Simple strings, not structured error types

---

## Risk Mitigation

### If Behind Schedule
**End of Day 1**:
- Must have: State machines formalized
- Can defer: Error message improvements

**End of Day 2**:
- Must have: SIGHUP handling (critical for reload)
- Can defer: SIGTERM graceful shutdown (SIGKILL works)
- Can defer: Keepalive improvements (10s timeout works)

**End of Day 3**:
- Must have: Basic integration tests
- Can defer: Comprehensive test coverage
- Can defer: Cloud testing (can do after release)

### Rollback Plan
- Keep v0.7.12 tagged as stable
- New work on feature/v0.7.13-state-machine branch
- If not ready by Day 3 EOD, user can stay on v0.7.12

---

## Success Criteria

### Minimum Viable (Must Pass)
1. ✅ Agent errors propagate to controller immediately
2. ✅ Controller exits with non-zero code on agent failure
3. ✅ SIGHUP resets agents without killing them
4. ✅ State transitions are logged and valid
5. ✅ Timeout detection works reliably

### Stretch Goals (Nice to Have)
1. ⭐ All integration tests pass
2. ⭐ Controller detects disconnect within 30s
3. ⭐ Graceful shutdown on SIGTERM
4. ⭐ Comprehensive documentation

---

## Daily Checkpoints

### Day 1 EOD
- [ ] State machines implemented
- [ ] Code compiles without warnings
- [ ] Basic manual test: agent error → controller exits

### Day 2 EOD
- [ ] Signal handlers implemented
- [ ] Keepalive configured
- [ ] Manual test: SIGHUP → agent resets, controller continues

### Day 3 EOD
- [ ] Integration tests pass
- [ ] Documentation updated
- [ ] Release tagged

---

## Let's Start: Day 1, Task 1

**Next action**: Implement formalized agent state machine with 7 states and transition validation.

**Estimated time**: 4 hours  
**Files**: `src/bin/agent.rs`  
**Strategy**: Extend existing `WorkloadState` enum, add validation, update all transition points
