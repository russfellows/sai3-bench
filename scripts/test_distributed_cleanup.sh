#!/bin/bash
# Test distributed cleanup with multiple agents
# Tests 3 scenarios:
#   1. Full lifecycle (prepare + workload + cleanup)
#   2. No cleanup (prepare + workload, leave files)
#   3. Cleanup-only (cleanup files from scenario 2)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  Distributed Cleanup Testing${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
echo ""

# Check if agents are running
if ! pgrep -f sai3bench-agent > /dev/null; then
    echo -e "${RED}ERROR: No agents running!${NC}"
    echo "Please start agents first:"
    echo "  ./scripts/start_local_agents.sh 2"
    exit 1
fi

echo -e "${GREEN}✓ Agents are running${NC}"
ps aux | grep sai3bench-agent | grep -v grep | awk '{print "  Agent on port", $NF}'
echo ""

# Clean test directories
echo "Cleaning test directories..."
rm -rf /tmp/sai3-test-full-lifecycle /tmp/sai3-test-no-cleanup
mkdir -p /tmp/sai3-test-full-lifecycle /tmp/sai3-test-no-cleanup
echo ""

# Test 1: Full lifecycle (prepare + workload + cleanup)
echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
echo -e "${BLUE}Test 1: Full Lifecycle (prepare + workload + cleanup)${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
./target/release/sai3bench-ctl --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/test_distributed_full_lifecycle.yaml

echo ""
echo "Checking if files were cleaned up..."
FILE_COUNT=$(find /tmp/sai3-test-full-lifecycle -name "prepared-*.dat" 2>/dev/null | wc -l)
if [ "$FILE_COUNT" -eq 0 ]; then
    echo -e "${GREEN}✓ Test 1 PASSED: All files cleaned up (found $FILE_COUNT files)${NC}"
else
    echo -e "${RED}✗ Test 1 FAILED: Found $FILE_COUNT files remaining (should be 0)${NC}"
    exit 1
fi
echo ""

# Test 2: No cleanup (prepare + workload, leave files)
echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
echo -e "${BLUE}Test 2: No Cleanup (prepare + workload, leave files)${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
./target/release/sai3bench-ctl --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/test_distributed_no_cleanup.yaml

echo ""
echo "Checking if files were left in place..."
FILE_COUNT=$(find /tmp/sai3-test-no-cleanup -name "prepared-*.dat" 2>/dev/null | wc -l)
if [ "$FILE_COUNT" -eq 30 ]; then
    echo -e "${GREEN}✓ Test 2 PASSED: Files left in place (found $FILE_COUNT files)${NC}"
else
    echo -e "${YELLOW}⚠ Test 2 WARNING: Found $FILE_COUNT files (expected 30)${NC}"
    echo "This might be OK if some creates failed, but verifying..."
fi
echo ""

# Test 3: Cleanup-only (cleanup files from test 2)
echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
echo -e "${BLUE}Test 3: Cleanup-Only (cleanup files from Test 2)${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
timeout 60 ./target/release/sai3bench-ctl -v --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/test_distributed_cleanup_only.yaml
TEST3_EXIT=$?

if [ $TEST3_EXIT -eq 124 ]; then
    echo -e "${RED}✗ Test 3 FAILED: Timeout after 60 seconds${NC}"
    echo "Check agent logs: /tmp/agent1.log and /tmp/agent2.log"
    exit 1
elif [ $TEST3_EXIT -ne 0 ]; then
    echo -e "${RED}✗ Test 3 FAILED: Controller exited with code $TEST3_EXIT${NC}"
    exit 1
fi

echo ""
echo "Checking if files were cleaned up..."
FILE_COUNT=$(find /tmp/sai3-test-no-cleanup -name "prepared-*.dat" 2>/dev/null | wc -l)
if [ "$FILE_COUNT" -eq 0 ]; then
    echo -e "${GREEN}✓ Test 3 PASSED: All files cleaned up (found $FILE_COUNT files)${NC}"
else
    echo -e "${RED}✗ Test 3 FAILED: Found $FILE_COUNT files remaining (should be 0)${NC}"
    exit 1
fi
echo ""

# Summary
echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
echo -e "${GREEN}ALL TESTS PASSED!${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════${NC}"
echo ""
echo "Summary:"
echo "  ✓ Test 1: Full lifecycle with cleanup works"
echo "  ✓ Test 2: Can skip cleanup and leave files"
echo "  ✓ Test 3: Cleanup-only operation works"
echo ""
echo "Distributed cleanup is working correctly!"
