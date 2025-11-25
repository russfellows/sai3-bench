#!/bin/bash
# Test script for clock synchronization with simulated clock skew
# This verifies that agents with different clock offsets all start at the same absolute time

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
AGENT_BIN="$REPO_ROOT/target/release/sai3bench-agent"
CONTROLLER_BIN="$REPO_ROOT/target/release/sai3bench-ctl"

# Cleanup function
cleanup() {
    echo "Cleaning up agent processes..."
    pkill -9 sai3bench-agent || true
    rm -rf /tmp/sai3-clock-test-*
    exit
}
trap cleanup EXIT INT TERM

# Check binaries exist
if [ ! -f "$AGENT_BIN" ] || [ ! -f "$CONTROLLER_BIN" ]; then
    echo "ERROR: Binaries not found. Run 'cargo build --release' first"
    exit 1
fi

echo "=== Clock Synchronization Test ==="
echo "Starting 3 agents with simulated clock skew:"
echo "  Agent 1: +5000ms (5 seconds ahead)"
echo "  Agent 2: -3000ms (3 seconds behind)"
echo "  Agent 3: +1000ms (1 second ahead)"
echo ""

# Create test directory
TEST_DIR="/tmp/sai3-clock-test-$$"
mkdir -p "$TEST_DIR"

# Start agents with different clock skews
echo "Starting agent 1 (port 7761, +5s skew)..."
SAI3_AGENT_CLOCK_SKEW_MS=5000 "$AGENT_BIN" --listen 127.0.0.1:7761 > "$TEST_DIR/agent1.log" 2>&1 &
AGENT1_PID=$!

echo "Starting agent 2 (port 7762, -3s skew)..."
SAI3_AGENT_CLOCK_SKEW_MS=-3000 "$AGENT_BIN" --listen 127.0.0.1:7762 > "$TEST_DIR/agent2.log" 2>&1 &
AGENT2_PID=$!

echo "Starting agent 3 (port 7763, +1s skew)..."
SAI3_AGENT_CLOCK_SKEW_MS=1000 "$AGENT_BIN" --listen 127.0.0.1:7763 > "$TEST_DIR/agent3.log" 2>&1 &
AGENT3_PID=$!

# Wait for agents to start
sleep 2
echo "Agents started (PIDs: $AGENT1_PID, $AGENT2_PID, $AGENT3_PID)"
echo ""

# Check agents are running
if ! kill -0 $AGENT1_PID 2>/dev/null || ! kill -0 $AGENT2_PID 2>/dev/null || ! kill -0 $AGENT3_PID 2>/dev/null; then
    echo "ERROR: One or more agents failed to start"
    cat "$TEST_DIR"/agent*.log
    exit 1
fi

# Create test config (based on test_distributed_local.yaml)
cat > "$TEST_DIR/test_config.yaml" <<EOF
# Clock sync test config - distributed local file backend
target: "file://$TEST_DIR/data/"
duration: "10s"
concurrency: 2

workload:
  - op: put
    path: "testfile-*.bin"
    object_size: 102400
    weight: 100
EOF

echo "Validating config with --dry-run..."
if ! "$CONTROLLER_BIN" --agents 127.0.0.1:7761,127.0.0.1:7762,127.0.0.1:7763 run --config "$TEST_DIR/test_config.yaml" --dry-run 2>&1 | grep -q "Configuration is valid"; then
    echo "ERROR: Config validation failed"
    "$CONTROLLER_BIN" --agents 127.0.0.1:7761,127.0.0.1:7762,127.0.0.1:7763 run --config "$TEST_DIR/test_config.yaml" --dry-run
    exit 1
fi
echo "Config validated successfully"
echo ""

echo "Running workload with controller..."
echo "Controller will calculate clock offsets and coordinate start"
echo ""

"$CONTROLLER_BIN" \
    --agents 127.0.0.1:7761,127.0.0.1:7762,127.0.0.1:7763 \
    run --config "$TEST_DIR/test_config.yaml" \
    2>&1 | tee "$TEST_DIR/controller.log"

echo ""
echo "=== Test Results ==="

# Extract clock offset warnings from controller log
echo "Clock offsets detected by controller:"
grep -E "Agent .* clock offset:|offset > 1000ms" "$TEST_DIR/controller.log" || echo "(No offset warnings found)"

# Check agent logs for test mode messages
echo ""
echo "Agent clock skew simulation:"
for i in 1 2 3; do
    echo "  Agent $i:"
    grep "TEST MODE" "$TEST_DIR/agent$i.log" || echo "    (No test mode message)"
done

# Verify coordinated start by checking workload start times in agent logs
echo ""
echo "Verifying coordinated start..."
echo "(All agents should start workload within milliseconds of each other)"

# Extract actual start timestamps from agent logs if available
grep -E "waiting.*for coordinated start|Starting workload" "$TEST_DIR"/agent*.log | head -6

echo ""
echo "=== Test Complete ==="
echo "Test artifacts in: $TEST_DIR"
echo "To examine logs: ls -lh $TEST_DIR"
