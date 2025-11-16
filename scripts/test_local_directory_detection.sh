#!/bin/bash
# Test script for directory tree detection and skip_verification
set -e

cd "$(dirname "$0")/../.."

echo "========================================="
echo "Local File:// Testing Suite"
echo "========================================="
echo ""

# Clean up any previous test data
echo "🧹 Cleaning previous test data..."
rm -rf /tmp/sai3-test/*
echo ""

# Kill any existing agents
echo "🔪 Killing any existing agents..."
pkill -9 sai3bench-agent 2>/dev/null || true
sleep 1
echo ""

# Start agents in background
echo "🚀 Starting 2 local agents..."
./target/release/sai3bench-agent --port 7761 --id test-agent-1 > /tmp/agent1.log 2>&1 &
AGENT1_PID=$!
./target/release/sai3bench-agent --port 7762 --id test-agent-2 > /tmp/agent2.log 2>&1 &
AGENT2_PID=$!

# Wait for agents to start
echo "⏳ Waiting for agents to start..."
sleep 3

# Function to check if agents are alive
check_agents() {
    if ! kill -0 $AGENT1_PID 2>/dev/null || ! kill -0 $AGENT2_PID 2>/dev/null; then
        echo "❌ ERROR: One or more agents died!"
        cat /tmp/agent1.log
        cat /tmp/agent2.log
        exit 1
    fi
}

# Test 1: First run - should create files and detect 0 existing
echo "========================================="
echo "TEST 1: First Run (skip_verification: false)"
echo "========================================="
echo "Expected: Create 320 files, detect 0 existing"
echo ""

check_agents
./target/release/sai3bench-ctl --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/local_test_2agents.yaml

echo ""
echo "✅ Test 1 completed"
echo "Files created: $(find /tmp/sai3-test -type f | wc -l)"
echo ""

# Test 2: Second run - should detect 320 existing, create 0 new
echo "========================================="
echo "TEST 2: Second Run (skip_verification: false)"
echo "========================================="
echo "Expected: Detect 320 existing files, create 0 new"
echo ""

check_agents
./target/release/sai3bench-ctl --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/local_test_2agents.yaml

echo ""
echo "✅ Test 2 completed"
echo "Files in directory: $(find /tmp/sai3-test -type f | wc -l)"
echo ""

# Test 3: Third run with skip_verification - should skip LIST entirely
echo "========================================="
echo "TEST 3: Third Run (skip_verification: true)"
echo "========================================="
echo "Expected: Skip LIST entirely, proceed directly to workload"
echo ""

check_agents
./target/release/sai3bench-ctl --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/local_test_2agents_skip.yaml

echo ""
echo "✅ Test 3 completed"
echo ""

# Verify file structure
echo "========================================="
echo "Verification"
echo "========================================="
TOTAL_FILES=$(find /tmp/sai3-test -type f | wc -l)
TOTAL_DIRS=$(find /tmp/sai3-test -type d -name "test.d*_w*.dir" | wc -l)

echo "📊 Final Statistics:"
echo "  Total directories: $TOTAL_DIRS (expected: 16)"
echo "  Total files: $TOTAL_FILES (expected: 320)"
echo ""

if [ "$TOTAL_DIRS" -eq 16 ] && [ "$TOTAL_FILES" -eq 320 ]; then
    echo "✅ ALL TESTS PASSED!"
else
    echo "❌ TEST FAILED: File counts don't match expected values"
    exit 1
fi

# Show sample directory structure
echo ""
echo "📁 Sample directory structure:"
find /tmp/sai3-test -type d -name "test.d*_w*.dir" | head -5
echo ""
echo "📄 Sample files:"
find /tmp/sai3-test -type f -name "prepared-*" | head -5
echo ""

# Cleanup
echo "🧹 Cleaning up agents..."
kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
wait $AGENT1_PID $AGENT2_PID 2>/dev/null || true

echo ""
echo "🎉 All tests completed successfully!"
