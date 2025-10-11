#!/bin/bash
# Test script for distributed results directory functionality
# Tests controller with actual running agents

set -e

BINARY_CTL="./target/release/sai3bench-ctl"
BINARY_AGENT="./target/release/sai3bench-agent"
CONFIG="tests/configs/distributed_put_test.yaml"

# Cleanup function
cleanup() {
    echo "Cleaning up..."
    # Kill any running agents
    pkill -f sai3bench-agent || true
    sleep 1
    # Remove test data directories
    rm -rf /tmp/sai3bench-test/
    rm -rf /tmp/io-bench-*
    # Keep results directories for examination
    echo "Keeping results directories for examination"
    # Uncomment to auto-clean:
    # rm -rf sai3-*
}

trap cleanup EXIT

# Test 1: Basic distributed run with 2 agents
test_basic_distributed() {
    echo "=== Test 1: Basic distributed run with 2 agents ==="
    
    # Start 2 agents
    echo "Starting agents..."
    $BINARY_AGENT --listen 127.0.0.1:7761 &
    AGENT1_PID=$!
    $BINARY_AGENT --listen 127.0.0.1:7762 &
    AGENT2_PID=$!
    
    # Wait for agents to be ready
    sleep 2
    
    # Verify agents are responding
    echo "Pinging agents..."
    $BINARY_CTL --insecure --agents 127.0.0.1:7761,127.0.0.1:7762 ping || {
        echo "❌ Agent ping failed"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    }
    
    # Run distributed workload
    echo "Running distributed workload..."
    $BINARY_CTL --insecure --agents 127.0.0.1:7761,127.0.0.1:7762 \
        run --config $CONFIG --start-delay 3 || {
        echo "❌ Distributed run failed"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    }
    
    # Find the created results directory
    RESULT_DIR=$(ls -td sai3-* 2>/dev/null | head -1)
    
    if [ -z "$RESULT_DIR" ]; then
        echo "❌ No results directory created"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    echo "Results directory: $RESULT_DIR"
    
    # Verify directory structure
    if [ ! -f "$RESULT_DIR/config.yaml" ]; then
        echo "❌ Missing config.yaml"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    if [ ! -f "$RESULT_DIR/console.log" ]; then
        echo "❌ Missing console.log"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    if [ ! -f "$RESULT_DIR/metadata.json" ]; then
        echo "❌ Missing metadata.json"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    if [ ! -d "$RESULT_DIR/agents" ]; then
        echo "❌ Missing agents/ directory"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    # Verify console.log contains expected content
    if ! grep -q "Distributed Workload" "$RESULT_DIR/console.log"; then
        echo "❌ console.log missing 'Distributed Workload' header"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    if ! grep -q "Sending workload to 2 agents" "$RESULT_DIR/console.log"; then
        echo "❌ console.log missing agent count message"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    if ! grep -q "Aggregate Totals" "$RESULT_DIR/console.log"; then
        echo "❌ console.log missing aggregate results"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    # Verify metadata.json contains distributed flag and agents
    if ! grep -q '"distributed": true' "$RESULT_DIR/metadata.json"; then
        echo "❌ metadata.json missing distributed flag"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    if ! grep -q '"agents":' "$RESULT_DIR/metadata.json"; then
        echo "❌ metadata.json missing agents list"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    # Display metadata for verification
    echo "--- Metadata ---"
    cat "$RESULT_DIR/metadata.json"
    echo ""
    
    echo "--- Console Log (first 30 lines) ---"
    head -30 "$RESULT_DIR/console.log"
    echo ""
    
    # Kill agents
    kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    wait $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    
    echo "✅ Test 1 passed"
    return 0
}

# Run tests
echo "======================================"
echo "Distributed Results Directory Tests"
echo "======================================"
echo ""

test_basic_distributed

echo ""
echo "======================================"
echo "All tests completed successfully!"
echo "======================================"
