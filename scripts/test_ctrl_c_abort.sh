#!/bin/bash
# Test Ctrl+C abort mechanism during workload execution (v0.7.12)
# Verifies that:
# 1. Controller sends abort to all agents when user presses Ctrl+C
# 2. Agents receive abort signal and stop workload
# 3. Agents reset to Idle state and are ready for next workload

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

echo "=== sai3-bench Ctrl+C Abort Test ==="
echo ""

# Build binaries
echo "1. Building binaries..."
cargo build --release --bin sai3bench-agent --bin sai3bench-ctl
echo ""

# Start 2 test agents with verbose logging
echo "2. Starting 2 test agents on ports 7761, 7762 with verbose logging..."
./target/release/sai3bench-agent --listen 0.0.0.0:7761 -vv > /tmp/agent1_ctrlc.log 2>&1 &
AGENT1_PID=$!
./target/release/sai3bench-agent --listen 0.0.0.0:7762 -vv > /tmp/agent2_ctrlc.log 2>&1 &
AGENT2_PID=$!

# Wait for agents to start
sleep 2

echo "   Agent 1 PID: $AGENT1_PID"
echo "   Agent 2 PID: $AGENT2_PID"
echo ""

# Function to kill agents
cleanup() {
    echo ""
    echo "Cleaning up agents..."
    kill -9 $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    wait $AGENT1_PID 2>/dev/null || true
    wait $AGENT2_PID 2>/dev/null || true
    echo "Agents killed"
}

trap cleanup EXIT

# Create test data
echo "3. Creating test directory with 20 files (1MB each)..."
mkdir -p /tmp/sai3-abort-test
for i in {1..20}; do
    dd if=/dev/zero of=/tmp/sai3-abort-test/file$i bs=1M count=1 2>/dev/null
done
echo ""

# Test 1: Run workload and interrupt with Ctrl+C after a few seconds
echo "4. Starting workload (will send Ctrl+C after 5 seconds)..."
echo "   This tests abort during active workload execution"
echo ""

# Run controller in background and capture PID
./target/release/sai3bench-ctl \
    --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/test_abort_2agents.yaml > /tmp/controller_ctrlc.log 2>&1 &
CONTROLLER_PID=$!

echo "   Controller PID: $CONTROLLER_PID"
echo "   Waiting 5 seconds for workload to start..."
sleep 5

echo "   Sending Ctrl+C (SIGINT) to controller..."
kill -INT $CONTROLLER_PID

# Wait for controller to finish
echo "   Waiting for controller to abort agents..."
wait $CONTROLLER_PID 2>/dev/null || true

echo "   Waiting 5 seconds for agents to reset to Idle state..."
sleep 5

echo ""
echo "5. Checking agent logs for abort and reset..."

if grep -q "Abort" /tmp/agent1_ctrlc.log /tmp/agent2_ctrlc.log 2>/dev/null; then
    echo "   ✅ Agents received abort signal"
else
    echo "   ✗ Agents did NOT receive abort signal"
fi

if grep -q "Aborting" /tmp/agent1_ctrlc.log /tmp/agent2_ctrlc.log 2>/dev/null; then
    echo "   ✅ Agents entered Aborting state"
else
    echo "   ⚠ Agents may not have entered Aborting state"
fi

if grep -q "reset to Idle" /tmp/agent1_ctrlc.log /tmp/agent2_ctrlc.log 2>/dev/null; then
    echo "   ✅ Agents reset to Idle state"
else
    echo "   ⚠ Agents may not have reset to Idle"
fi

echo ""

# Test 2: Verify agents can accept new workload after abort
echo "6. Test 2: Running new workload to verify agents recovered..."
timeout 30s ./target/release/sai3bench-ctl \
    --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/test_abort_2agents.yaml 2>&1 | tail -20 || true

echo ""
echo "7. Checking if second workload completed..."
if grep -q "completed" /tmp/agent1_ctrlc.log /tmp/agent2_ctrlc.log 2>/dev/null; then
    echo "   ✅ Agents successfully executed second workload after abort"
else
    echo "   ⚠ Second workload may not have completed (check logs)"
fi

echo ""
echo "8. Agent logs summary:"
echo ""
echo "=== Agent 1 state transitions ==="
grep -E "state transition|Abort|reset to Idle|Aborting" /tmp/agent1_ctrlc.log 2>/dev/null || echo "No state transitions found"
echo ""
echo "=== Agent 2 state transitions ==="
grep -E "state transition|Abort|reset to Idle|Aborting" /tmp/agent2_ctrlc.log 2>/dev/null || echo "No state transitions found"
echo ""

echo "=== Controller output (last 30 lines) ==="
tail -30 /tmp/controller_ctrlc.log
echo ""

echo "=== Test Complete ==="
echo ""
echo "Full logs:"
echo "  Agent 1: /tmp/agent1_ctrlc.log"
echo "  Agent 2: /tmp/agent2_ctrlc.log"
echo "  Controller: /tmp/controller_ctrlc.log"
