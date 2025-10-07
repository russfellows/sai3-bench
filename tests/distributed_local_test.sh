#!/bin/bash
# Test distributed workload with LOCAL storage (file://)
# Each agent should prepare its own dataset

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
AGENT_BIN="$PROJECT_ROOT/target/release/sai3bench-agent"
CTL_BIN="$PROJECT_ROOT/target/release/sai3bench-ctl"
CONFIG="$SCRIPT_DIR/configs/file_test.yaml"

# Clean up any previous test data
TEST_DIR="/tmp/sai3bench-local-test"
rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

echo "=== Testing Distributed Workload with LOCAL storage (file://) ==="
echo "Expected behavior: Each agent prepares its own dataset in separate directories"
echo ""

# Update config to use test directory
TEST_CONFIG="/tmp/distributed_local_test.yaml"
cat > "$TEST_CONFIG" << 'EOF'
target: "file:///tmp/sai3bench-local-test/"
duration: "3s"
concurrency: 2

# Prepare phase - each agent will create its own dataset
prepare:
  ensure_objects:
    - base_uri: "file:///tmp/sai3bench-local-test/data/"
      count: 10
      min_size: 1024
      max_size: 1024
      fill: zero

workload:
  - op: get
    path: "data/*"
    weight: 70

  - op: put
    path: "results/"
    object_size: 512
    weight: 30
EOF

echo "Starting 2 agents..."

# Start agent 1
$AGENT_BIN -vv --listen 127.0.0.1:7761 > /tmp/agent1.log 2>&1 &
AGENT1_PID=$!
echo "Agent 1 (PID $AGENT1_PID) listening on 127.0.0.1:7761"

# Start agent 2
$AGENT_BIN -vv --listen 127.0.0.1:7762 > /tmp/agent2.log 2>&1 &
AGENT2_PID=$!
echo "Agent 2 (PID $AGENT2_PID) listening on 127.0.0.1:7762"

# Give agents time to start
sleep 2

# Cleanup function
cleanup() {
    echo ""
    echo "Cleaning up..."
    kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    wait $AGENT1_PID $AGENT2_PID 2>/dev/null || true
}
trap cleanup EXIT

# Verify agents are running
if ! $CTL_BIN --insecure --agents 127.0.0.1:7761,127.0.0.1:7762 ping > /dev/null 2>&1; then
    echo "ERROR: Agents not responding to ping"
    exit 1
fi
echo "Agents ready"
echo ""

# Run distributed workload
echo "Running distributed workload..."
$CTL_BIN -vv --insecure --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config "$TEST_CONFIG" --start-delay 1

echo ""
echo "=== Verifying Results ==="

# Check directory structure
echo "Directory structure:"
find "$TEST_DIR" -type d | sort

echo ""
echo "Files created:"
find "$TEST_DIR" -type f | wc -l

# Verify each agent created its own data directory
if [ -d "$TEST_DIR/agent-1/data" ]; then
    echo "✓ Agent 1 data directory exists"
    AGENT1_DATA=$(find "$TEST_DIR/agent-1/data" -type f | wc -l)
    echo "  - Files: $AGENT1_DATA"
else
    echo "✗ Agent 1 data directory missing"
fi

if [ -d "$TEST_DIR/agent-2/data" ]; then
    echo "✓ Agent 2 data directory exists"
    AGENT2_DATA=$(find "$TEST_DIR/agent-2/data" -type f | wc -l)
    echo "  - Files: $AGENT2_DATA"
else
    echo "✗ Agent 2 data directory missing"
fi

# Check results directories
if [ -d "$TEST_DIR/agent-1/results" ]; then
    echo "✓ Agent 1 results directory exists"
    AGENT1_RESULTS=$(find "$TEST_DIR/agent-1/results" -type f | wc -l)
    echo "  - Files: $AGENT1_RESULTS"
fi

if [ -d "$TEST_DIR/agent-2/results" ]; then
    echo "✓ Agent 2 results directory exists"
    AGENT2_RESULTS=$(find "$TEST_DIR/agent-2/results" -type f | wc -l)
    echo "  - Files: $AGENT2_RESULTS"
fi

echo ""
echo "=== Agent Logs ==="
echo "Agent 1 log (last 20 lines):"
tail -20 /tmp/agent1.log
echo ""
echo "Agent 2 log (last 20 lines):"
tail -20 /tmp/agent2.log

echo ""
echo "=== Test Complete ==="
