#!/bin/bash
# Integration test for distributed workload execution

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "=== Distributed Workload Integration Test ==="
echo

# Build if needed
cd "$PROJECT_ROOT"
if [ ! -f "target/release/sai3bench-agent" ] || [ ! -f "target/release/sai3bench-ctl" ]; then
    echo "Building release binaries..."
    cargo build --release
    echo
fi

# Cleanup previous test data
rm -rf /tmp/sai3bench-distributed-test
mkdir -p /tmp/sai3bench-distributed-test

# Start two agents in background
echo "Starting agent 1 on port 7761..."
./target/release/sai3bench-agent --listen 127.0.0.1:7761 &
AGENT1_PID=$!

echo "Starting agent 2 on port 7762..."
./target/release/sai3bench-agent --listen 127.0.0.1:7762 &
AGENT2_PID=$!

# Cleanup function
cleanup() {
    echo
    echo "Cleaning up agents..."
    kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    wait $AGENT1_PID $AGENT2_PID 2>/dev/null || true
}
trap cleanup EXIT

# Wait for agents to start
echo "Waiting for agents to start..."
sleep 2

# Test ping
echo
echo "Testing ping..."
./target/release/sai3bench-ctl --insecure --agents 127.0.0.1:7761,127.0.0.1:7762 ping

# Run distributed workload
echo
echo "Running distributed workload..."
./target/release/sai3bench-ctl --insecure \
    --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config /tmp/distributed-test.yaml \
    --start-delay 1

# Verify path isolation - check that each agent created its own subdirectory
echo
echo "Verifying path isolation..."
if [ -d "/tmp/sai3bench-distributed-test/agent-1" ]; then
    echo "✓ Agent 1 directory exists: /tmp/sai3bench-distributed-test/agent-1/"
    ls -la /tmp/sai3bench-distributed-test/agent-1/data/ | head -5
else
    echo "✗ Agent 1 directory not found!"
    exit 1
fi

if [ -d "/tmp/sai3bench-distributed-test/agent-2" ]; then
    echo "✓ Agent 2 directory exists: /tmp/sai3bench-distributed-test/agent-2/"
    ls -la /tmp/sai3bench-distributed-test/agent-2/data/ | head -5
else
    echo "✗ Agent 2 directory not found!"
    exit 1
fi

echo
echo "=== Test Passed! ==="
echo "Distributed workload executed successfully with path isolation."
