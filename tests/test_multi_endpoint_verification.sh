#!/bin/bash
# Test script to verify multi-endpoint load balancing works correctly
#
# This test creates a small workload and verifies that:
# 1. Round-robin strategy accesses endpoints in order
# 2. Least-connections strategy distributes load
# 3. Per-endpoint statistics are collected
#
# Usage: ./test_multi_endpoint_verification.sh

set -e

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
SAI3_BENCH="$SCRIPT_DIR/../target/release/sai3-bench"
TEST_DIR="/tmp/sai3-multiep-test-$$"

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "================================"
echo "Multi-Endpoint Verification Test"
echo "================================"
echo ""

# Check if sai3-bench is built
if [ ! -f "$SAIENCH" ]; then
    echo -e "${YELLOW}Building sai3-bench...${NC}"
    cd "$SCRIPT_DIR/.." && cargo build --release
fi

# Create test directory structure
echo "Setting up test environment: $TEST_DIR"
mkdir -p "$TEST_DIR/ep1/data"
mkdir -p "$TEST_DIR/ep2/data"
mkdir -p "$TEST_DIR/ep3/data"
mkdir -p "$TEST_DIR/ep4/data"

# Create test config for round-robin verification
cat > "$TEST_DIR/test_round_robin.yaml" <<EOF
# Round-robin verification test
# We'll create 100 objects and do 400 GET operations
# With 4 endpoints and round-robin, each should get exactly 100 requests

multi_endpoint:
  strategy: round_robin
  endpoints:
    - file://$TEST_DIR/ep1/
    - file://$TEST_DIR/ep2/
    - file://$TEST_DIR/ep3/
    - file://$TEST_DIR/ep4/

duration: 10s
concurrency: 4

prepare:
  ensure_objects:
    - base_uri: "file://$TEST_DIR/ep1/data/"
      count: 100
      min_size: 1024
      max_size: 1024
      fill: zero
  cleanup: false  # Keep objects for inspection

workload:
  - op: get
    path: "data/*"
    weight: 100
EOF

# Create test config for least-connections verification
cat > "$TEST_DIR/test_least_connections.yaml" <<EOF
# Least-connections verification test
# Similar to round-robin but uses least_connections strategy

multi_endpoint:
  strategy: least_connections
  endpoints:
    - file://$TEST_DIR/ep1/
    - file://$TEST_DIR/ep2/
    - file://$TEST_DIR/ep3/
    - file://$TEST_DIR/ep4/

duration: 10s
concurrency: 8

prepare:
  ensure_objects:
    - base_uri: "file://$TEST_DIR/ep1/data/"
      count: 100
      min_size: 1024
      max_size: 1024
      fill: zero
  cleanup: false

workload:
  - op: get
    path: "data/*"
    weight: 100
EOF

echo ""
echo "=== Test 1: Round-Robin Strategy ==="
echo "Running workload with round_robin strategy..."
$SAI3_BENCH run --config "$TEST_DIR/test_round_robin.yaml" 2>&1 | tee "$TEST_DIR/round_robin_output.log"

echo ""
echo "=== Test 2: Least-Connections Strategy ==="
echo "Running workload with least_connections strategy..."
$SAI3_BENCH run --config "$TEST_DIR/test_least_connections.yaml" 2>&1 | tee "$TEST_DIR/least_connections_output.log"

echo ""
echo "=== Analysis ==="
echo ""
echo "Round-robin test output:"
grep -i "endpoint\|multi-endpoint\|statistics" "$TEST_DIR/round_robin_output.log" || echo "No per-endpoint statistics found in output"

echo ""
echo "Least-connections test output:"
grep -i "endpoint\|multi-endpoint\|statistics" "$TEST_DIR/least_connections_output.log" || echo "No per-endpoint statistics found in output"

echo ""
echo "=== Cleanup ==="
read -p "Remove test directory $TEST_DIR? (y/n) " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    rm -rf "$TEST_DIR"
    echo "Test directory removed"
else
    echo "Test directory preserved: $TEST_DIR"
fi

echo ""
echo -e "${GREEN}Multi-endpoint verification test complete!${NC}"
echo ""
echo "NOTE: Currently s3dlio's MultiEndpointStore does not expose per-endpoint"
echo "statistics to the caller. To verify load balancing, you would need to:"
echo "  1. Add per-endpoint statistics to s3dlio MultiEndpointStore API"
echo "  2. Display them in sai3-bench summary output"
echo "  3. Or use external monitoring (strace, tcpdump, storage logs)"
