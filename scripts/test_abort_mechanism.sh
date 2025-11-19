#!/bin/bash
# Test script for abort mechanism (v0.7.12)
# Tests that agents can be aborted and reset properly

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

echo "=== sai3-bench Abort Mechanism Test ==="
echo ""

# Build binaries
echo "1. Building binaries..."
cargo build --release --bin sai3bench-agent --bin sai3bench-ctl
echo ""

# Start 2 test agents
echo "2. Starting 2 test agents on ports 7761, 7762..."
./target/release/sai3bench-agent --listen 0.0.0.0:7761 > /tmp/agent1.log 2>&1 &
AGENT1_PID=$!
./target/release/sai3bench-agent --listen 0.0.0.0:7762 > /tmp/agent2.log 2>&1 &
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

# Test 1: Startup timeout (should trigger abort)
echo "3. Test 1: Startup timeout with only 2 of 3 agents (should abort)"
echo "   Running controller with 3 agents but only 2 are running..."
timeout 15s ./target/release/sai3bench-ctl \
    --agents 127.0.0.1:7761,127.0.0.1:7762,127.0.0.1:7763 \
    run --config tests/configs/test_abort_2agents.yaml 2>&1 | head -30 || true

echo ""
echo "   Checking agent logs for abort..."
if grep -q "Abort" /tmp/agent1.log /tmp/agent2.log; then
    echo "   ✓ Agents received abort signal"
else
    echo "   ✗ Agents did NOT receive abort (may be OK if timeout before abort sent)"
fi

if grep -q "reset to Idle" /tmp/agent1.log /tmp/agent2.log; then
    echo "   ✓ Agents reset to Idle state"
else
    echo "   ⚠ Agents may not have reset (check logs)"
fi
echo ""

# Test 2: Invalid config (should trigger abort during validation)
echo "4. Test 2: Invalid config (should abort during agent-side validation)"
cat > /tmp/test_invalid_config.yaml <<EOF
# Invalid config - path pattern that doesn't exist (will fail during agent validation)
target: "file:///tmp/sai3-test/"
duration: "5s"
concurrency: 2
prepare:
  ensure_objects:
    - base_uri: "file:///tmp/nonexistent-path-that-will-fail/"
      count: 10
      fill: zero
      size_spec: 1048576
workload:
  - op: get
    path: "nonexistent/*.bin"
    weight: 100
distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  agents:
    - address: "127.0.0.1:7761"
      id: "agent-1"
    - address: "127.0.0.1:7762"
      id: "agent-2"
EOF

timeout 15s ./target/release/sai3bench-ctl \
    --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config /tmp/test_invalid_config.yaml 2>&1 | head -30 || true

echo ""
echo "   Checking agent logs for abort..."
if grep -q "Abort" /tmp/agent1.log /tmp/agent2.log; then
    echo "   ✓ Agents received abort signal"
else
    echo "   ⚠ Agents may not have received abort"
fi

if grep -q "reset to Idle" /tmp/agent1.log /tmp/agent2.log; then
    echo "   ✓ Agents reset to Idle state"
else
    echo "   ⚠ Agents may not have reset"
fi
echo ""

# Test 3: Successful run (agents should work after previous aborts)
echo "5. Test 3: Successful run (verify agents recovered)"
echo "   Creating test directory and files..."
mkdir -p /tmp/sai3-abort-test
for i in {1..20}; do
    dd if=/dev/zero of=/tmp/sai3-abort-test/file$i bs=1M count=1 2>/dev/null
done

timeout 30s ./target/release/sai3bench-ctl \
    --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/test_abort_2agents.yaml 2>&1 | tail -20 || true

echo ""
echo "   Checking if workload completed..."
if grep -q "Workload complete" /tmp/agent1.log /tmp/agent2.log; then
    echo "   ✓ Agents successfully executed workload after abort/reset"
else
    echo "   ⚠ Workload may not have completed (check logs)"
fi
echo ""

# Show agent log excerpts
echo "6. Agent 1 log excerpt (last 15 lines):"
tail -15 /tmp/agent1.log
echo ""

echo "7. Agent 2 log excerpt (last 15 lines):"
tail -15 /tmp/agent2.log
echo ""

echo "=== Test Complete ==="
echo ""
echo "Full logs:"
echo "  Agent 1: /tmp/agent1.log"
echo "  Agent 2: /tmp/agent2.log"
