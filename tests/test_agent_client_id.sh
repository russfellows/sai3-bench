#!/bin/bash
# Test script to verify agent client_id appears in operation logs

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$PROJECT_DIR"

echo "=== Testing Agent client_id in Operation Logs ==="
echo

# Cleanup any previous test artifacts
rm -f /tmp/agent-1-oplog.tsv.zst /tmp/agent-2-oplog.tsv.zst
pkill -9 sai3bench-agent 2>/dev/null || true
sleep 1

# Create test data directory
TEST_DIR="/mnt/test/sai3bench-client-id-test"
mkdir -p "$TEST_DIR/data"

# Create some test files
echo "Creating test data..."
for i in {1..10}; do
    dd if=/dev/urandom of="$TEST_DIR/data/file-$i.dat" bs=1024 count=4 2>/dev/null
done
echo

# Start two agents with operation logging
echo "Starting agent-1 on port 7761..."
./target/release/sai3bench-agent \
    --listen 0.0.0.0:7761 \
    --op-log /tmp/agent-1-oplog.tsv.zst \
    > /tmp/agent-1.log 2>&1 &
AGENT1_PID=$!

echo "Starting agent-2 on port 7762..."
./target/release/sai3bench-agent \
    --listen 0.0.0.0:7762 \
    --op-log /tmp/agent-2-oplog.tsv.zst \
    > /tmp/agent-2.log 2>&1 &
AGENT2_PID=$!

sleep 2

# Create simple test config
cat > /tmp/test-client-id-config.yaml <<EOF
target: "file://$TEST_DIR/"
duration: "5s"
concurrency: 4
op_log_path: "/tmp/test-oplog.tsv.zst"

workload:
  - op: get
    path: "data/*"
    weight: 100
EOF

echo "Running distributed workload..."
./target/release/sai3bench-ctl \
    --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config /tmp/test-client-id-config.yaml \
    --start-delay 0
echo

# Give agents a moment to finish writing oplogs
sleep 2

# Stop agents
echo "Stopping agents..."
kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
sleep 1

# Verify oplogs exist
echo "=== Verification ==="
echo

if [ ! -f /tmp/agent-1-oplog.tsv.zst ]; then
    echo "❌ FAIL: agent-1 oplog not found"
    exit 1
fi

if [ ! -f /tmp/agent-2-oplog.tsv.zst ]; then
    echo "❌ FAIL: agent-2 oplog not found"
    exit 1
fi

echo "✓ Both agent oplogs exist"
echo

# Check agent-1 oplog for client_id="agent-1"
echo "Checking agent-1 oplog (first 5 operations):"
AGENT1_SAMPLE=$(zstd -d < /tmp/agent-1-oplog.tsv.zst | head -6 | tail -5)
echo "$AGENT1_SAMPLE"
echo

AGENT1_CLIENT_IDS=$(zstd -d < /tmp/agent-1-oplog.tsv.zst | tail -n +2 | cut -f4 | sort -u)
echo "Unique client_ids in agent-1 oplog: $AGENT1_CLIENT_IDS"

if echo "$AGENT1_CLIENT_IDS" | grep -q "agent-1"; then
    echo "✓ agent-1 oplog contains client_id='agent-1'"
else
    echo "❌ FAIL: agent-1 oplog does NOT contain client_id='agent-1'"
    echo "Found: $AGENT1_CLIENT_IDS"
    exit 1
fi
echo

# Check agent-2 oplog for client_id="agent-2"
echo "Checking agent-2 oplog (first 5 operations):"
AGENT2_SAMPLE=$(zstd -d < /tmp/agent-2-oplog.tsv.zst | head -6 | tail -5)
echo "$AGENT2_SAMPLE"
echo

AGENT2_CLIENT_IDS=$(zstd -d < /tmp/agent-2-oplog.tsv.zst | tail -n +2 | cut -f4 | sort -u)
echo "Unique client_ids in agent-2 oplog: $AGENT2_CLIENT_IDS"

if echo "$AGENT2_CLIENT_IDS" | grep -q "agent-2"; then
    echo "✓ agent-2 oplog contains client_id='agent-2'"
else
    echo "❌ FAIL: agent-2 oplog does NOT contain client_id='agent-2'"
    echo "Found: $AGENT2_CLIENT_IDS"
    exit 1
fi
echo

# Count operations per agent
AGENT1_OPS=$(zstd -d < /tmp/agent-1-oplog.tsv.zst | tail -n +2 | wc -l)
AGENT2_OPS=$(zstd -d < /tmp/agent-2-oplog.tsv.zst | tail -n +2 | wc -l)

echo "=== Results ==="
echo "Agent-1 operations: $AGENT1_OPS"
echo "Agent-2 operations: $AGENT2_OPS"
echo "Total operations: $((AGENT1_OPS + AGENT2_OPS))"
echo

echo "✅ SUCCESS: All agents correctly set client_id in operation logs!"
echo

# Cleanup
rm -rf "$TEST_DIR"
echo "Cleaned up test directory: $TEST_DIR"
