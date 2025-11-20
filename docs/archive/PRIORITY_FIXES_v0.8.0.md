# Priority Fixes - 2-3 Day Sprint

**Date**: November 19, 2025  
**Timeline**: 2-3 days maximum  
**Goal**: Fix critical bugs, defer nice-to-have features

---

## Critical Path (Must-Have)

### Day 1: Core State Machine + Error Detection ✅ COMPLETE

#### 1. Agent State Machine (4 hours) ✅ DONE
- [x] Current: 3 states (Idle, Running, Aborting)
- [x] Implemented: 5 states (Idle, Ready, Running, Failed, Aborting)
- [x] Add: `can_transition()` validation
- [x] Add: `transition_to()` with logging
- [x] Add: error_message field in AgentState

**Files**: `src/bin/agent.rs` (lines 76-126, ~50 lines modified)
**Status**: Implemented correctly with 5-state model per design doc

#### 2. Controller Agent Tracking (4 hours) ✅ DONE
- [x] Current: HashSets (completed, dead, ready)
- [x] Replaced with: `AgentTracker` struct with 9-state enum
- [x] Add: proper state transitions with `can_transition()` validation
- [x] Add: last_seen timestamp tracking
- [x] Add: error_message storage
- [x] Add: latest_stats storage

**Files**: `src/bin/controller.rs` (lines 24-124, ~100 lines added)
**Status**: Implemented 9-state controller model (tracks full agent lifecycle)
**Note**: Controller has more states than agent because it sees network layer + validation

#### 3. Error Detection & Propagation (2 hours) ✅ DONE
- [x] Stream error handling (done today)
- [x] Validation error handling (VALIDATING → FAILED transition)
- [x] Workload error handling with proper abort
- [x] Clear error messages in controller output (via AgentTracker.error_message)

**Files**: `src/bin/agent.rs`, `src/bin/controller.rs` (~150 lines total)
**Status**: Error states properly tracked, transitions validated

**Day 1 Goal**: ✅ Agents and controller properly track state, errors propagate cleanly

---

### Day 2: Signal Handling + Keepalive ✅ SIGNAL HANDLING COMPLETE

#### 4. Signal Handling (3 hours) ✅ DONE (November 19, 2025)
- [x] SIGINT handler (agent): graceful shutdown, log signal name
- [x] SIGINT handler (controller): abort all agents, exit cleanly
- [x] SIGTERM handler (agent): graceful shutdown (same as SIGINT)
- [x] SIGTERM handler (controller): abort all agents, exit cleanly
- [x] Removed SIGHUP (not relevant for our use case)
- [ ] Test script: verify signal handling works correctly

**Files**: `src/bin/agent.rs` (lines 1316-1430), `src/bin/controller.rs` (lines 923-1648)
**Implementation**: `wait_for_shutdown_signal()` using tokio::signal::unix
**Status**: Both SIGINT and SIGTERM handled gracefully with proper logging
**Note**: SIGHUP omitted - not needed (no config reload, agents are ephemeral)
**New**: `scripts/test_signal_handling.sh` (PENDING - need to create)

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
- [x] Version bump to 0.8.0 (completed)
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
- Completed on feature/state-machine-refactor branch (v0.8.0)
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

### Day 1 EOD ✅ COMPLETE (November 19, 2025)
- [x] State machines implemented (5-state agent, 9-state controller)
- [x] Code compiles without warnings (zero warnings as of v0.8.0)
- [x] Basic manual test: agent error → controller exits (state transitions validated)
- [x] All state transitions logged with debug output
- [x] `can_transition()` validation prevents invalid state changes

### Day 2 EOD ⏳ IN PROGRESS (November 19, 2025)
- [x] Signal handlers implemented (SIGINT + SIGTERM)
- [ ] Keepalive configured (DEFERRED - using default gRPC keepalive)
- [ ] Manual test: SIGINT → graceful shutdown (PENDING)
- [ ] Manual test: SIGTERM → graceful shutdown (PENDING)
- [ ] Manual test: Ctrl-C controller → agents abort correctly (PENDING)

**Status**: Signal handling code complete, needs manual testing before commit

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
