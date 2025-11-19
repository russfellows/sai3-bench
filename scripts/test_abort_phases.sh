#!/bin/bash
# Comprehensive abort testing: abort during different phases
# Tests that agents properly reset to Idle after abort in any phase

set -e

AGENT1_PORT=7761
AGENT2_PORT=7762
AGENT1_LOG=/tmp/agent1_phases.log
AGENT2_LOG=/tmp/agent2_phases.log
RESULTS_LOG=/tmp/abort_phases_results.log

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "=== sai3-bench Abort Phases Test ==="
echo "Testing abort during: validation, start delay, prepare, workload"
echo ""

# Cleanup from previous runs
pkill -9 sai3bench-agent 2>/dev/null || true
rm -f $AGENT1_LOG $AGENT2_LOG $RESULTS_LOG
rm -rf /tmp/sai3test_abort_*

# Build
echo "1. Building binaries..."
cargo build --release --bin sai3bench-agent --bin sai3bench-ctl 2>&1 | tail -1

# Start agents
echo ""
echo "2. Starting 2 test agents on ports $AGENT1_PORT, $AGENT2_PORT..."
./target/release/sai3bench-agent --listen 0.0.0.0:$AGENT1_PORT -vv > $AGENT1_LOG 2>&1 &
AGENT1_PID=$!
./target/release/sai3bench-agent --listen 0.0.0.0:$AGENT2_PORT -vv > $AGENT2_LOG 2>&1 &
AGENT2_PID=$!
echo "   Agent 1 PID: $AGENT1_PID"
echo "   Agent 2 PID: $AGENT2_PID"
sleep 2

# Helper function to check agent state
check_agent_state() {
    local test_name=$1
    local expected_state=$2
    
    # Check logs for state transitions
    local agent1_state=$(grep "Agent state transition:" $AGENT1_LOG | tail -1 | awk '{print $NF}')
    local agent2_state=$(grep "Agent state transition:" $AGENT2_LOG | tail -1 | awk '{print $NF}')
    
    echo "   Agent 1 state: $agent1_state"
    echo "   Agent 2 state: $agent2_state"
    
    if [[ "$agent1_state" == "$expected_state" && "$agent2_state" == "$expected_state" ]]; then
        echo -e "   ${GREEN}✅ Both agents in $expected_state state${NC}"
        echo "$test_name: PASS" >> $RESULTS_LOG
        return 0
    else
        echo -e "   ${RED}❌ Agents not in expected state $expected_state${NC}"
        echo "$test_name: FAIL (agent1=$agent1_state, agent2=$agent2_state)" >> $RESULTS_LOG
        return 1
    fi
}

# Test 1: Abort during validation (agents waiting to become ready)
echo ""
echo "=== Test 1: Abort during validation phase ==="
echo "   Starting workload and aborting immediately..."
./target/release/sai3bench-ctl --agents 127.0.0.1:$AGENT1_PORT,127.0.0.1:$AGENT2_PORT \
    run --config tests/configs/test_abort_2agents.yaml > /tmp/ctrl1.log 2>&1 &
CTRL_PID=$!
sleep 1  # Let validation start
kill -INT $CTRL_PID 2>/dev/null || true
wait $CTRL_PID 2>/dev/null || true
echo "   Waiting 5 seconds for agents to reset..."
sleep 5
check_agent_state "Test1-Validation" "Idle"

# Test 2: Abort during coordinated start delay
echo ""
echo "=== Test 2: Abort during start delay (after validation, before execution) ==="
echo "   Config has 2s start delay - will abort after agents are ready but before execution..."
./target/release/sai3bench-ctl --agents 127.0.0.1:$AGENT1_PORT,127.0.0.1:$AGENT2_PORT \
    run --config tests/configs/test_abort_2agents.yaml > /tmp/ctrl2.log 2>&1 &
CTRL_PID=$!
sleep 3  # Wait for agents to be ready (longer than validation, less than start_delay)
# Check that agents are ready but haven't started workload yet
if grep -q "All 2 agents ready" /tmp/ctrl2.log; then
    echo "   ✅ Agents validated and ready"
fi
kill -INT $CTRL_PID 2>/dev/null || true
wait $CTRL_PID 2>/dev/null || true
echo "   Waiting 5 seconds for agents to reset..."
sleep 5
check_agent_state "Test2-StartDelay" "Idle"

# Test 3: Abort during prepare phase
echo ""
echo "=== Test 3: Abort during prepare phase ==="
echo "   Config creates 30 objects (should take 3-5 seconds) - will abort mid-prepare..."
./target/release/sai3bench-ctl --agents 127.0.0.1:$AGENT1_PORT,127.0.0.1:$AGENT2_PORT \
    run --config tests/configs/test_abort_prepare.yaml > /tmp/ctrl3.log 2>&1 &
CTRL_PID=$!
sleep 6  # Wait for prepare to start (2s start_delay + ~3s into prepare)
# Check that prepare is running
if grep -q "in_prepare_phase.*true" $AGENT1_LOG; then
    echo "   ✅ Prepare phase started"
fi
kill -INT $CTRL_PID 2>/dev/null || true
wait $CTRL_PID 2>/dev/null || true
echo "   Waiting 5 seconds for agents to reset..."
sleep 5
check_agent_state "Test3-Prepare" "Idle"

# Test 4: Abort during workload execution
echo ""
echo "=== Test 4: Abort during workload execution ==="
echo "   Will abort after workload has been running for a few seconds..."
./target/release/sai3bench-ctl --agents 127.0.0.1:$AGENT1_PORT,127.0.0.1:$AGENT2_PORT \
    run --config tests/configs/test_abort_2agents.yaml > /tmp/ctrl4.log 2>&1 &
CTRL_PID=$!
sleep 7  # 2s start_delay + 5s into workload
# Check that workload is running
if grep -q "completed.*false" $AGENT1_LOG; then
    echo "   ✅ Workload executing"
fi
kill -INT $CTRL_PID 2>/dev/null || true
wait $CTRL_PID 2>/dev/null || true
echo "   Waiting 5 seconds for agents to reset..."
sleep 5
check_agent_state "Test4-Workload" "Idle"

# Test 5: Verify agents can run another workload after all aborts
echo ""
echo "=== Test 5: Final recovery test - run complete workload ==="
echo "   Running full 10-second workload to verify agents fully recovered..."
./target/release/sai3bench-ctl --agents 127.0.0.1:$AGENT1_PORT,127.0.0.1:$AGENT2_PORT \
    run --config tests/configs/test_abort_2agents.yaml > /tmp/ctrl5.log 2>&1 &
CTRL_PID=$!
sleep 15  # Let it complete fully (2s start + 10s duration + buffer)
wait $CTRL_PID 2>/dev/null || true

# Check if workload completed successfully
if grep -q "Workload completed successfully" $AGENT1_LOG | tail -1; then
    echo -e "   ${GREEN}✅ Final workload completed successfully${NC}"
    echo "Test5-Recovery: PASS" >> $RESULTS_LOG
else
    echo -e "   ${RED}❌ Final workload did not complete${NC}"
    echo "Test5-Recovery: FAIL" >> $RESULTS_LOG
fi
sleep 2
check_agent_state "Test5-Recovery-Final" "Idle"

# Summary
echo ""
echo "=== Test Results Summary ==="
cat $RESULTS_LOG
echo ""

PASS_COUNT=$(grep -c "PASS" $RESULTS_LOG 2>/dev/null) || PASS_COUNT=0
FAIL_COUNT=$(grep -c "FAIL" $RESULTS_LOG 2>/dev/null) || FAIL_COUNT=0
TOTAL=$((PASS_COUNT + FAIL_COUNT))

if [ $FAIL_COUNT -eq 0 ]; then
    echo -e "${GREEN}✅ All $TOTAL tests PASSED${NC}"
    EXIT_CODE=0
else
    echo -e "${RED}❌ $FAIL_COUNT of $TOTAL tests FAILED${NC}"
    EXIT_CODE=1
fi

echo ""
echo "Full logs:"
echo "  Agent 1: $AGENT1_LOG"
echo "  Agent 2: $AGENT2_LOG"
echo "  Results: $RESULTS_LOG"
echo ""
echo "Cleaning up agents..."
kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
echo "Done."

exit $EXIT_CODE
