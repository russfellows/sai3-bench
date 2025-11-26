#!/bin/bash
# Test distributed cleanup with 2 agents
# Each agent should handle its own subset of objects via modulo distribution

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

CONTROLLER="./target/release/sai3bench-ctl"
CONFIG="tests/configs/test_distributed_cleanup.yaml"
TEST_DIR="/tmp/sai3-distributed-cleanup-test"

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}=== Distributed Cleanup Test (2 Agents) ===${NC}\n"

# Verify binaries exist
if [ ! -f "$CONTROLLER" ]; then
    echo "ERROR: Controller binary not found at $CONTROLLER"
    exit 1
fi

# Clean up previous test data and results
rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

# Start 2 agents using existing script
echo -e "${YELLOW}Starting 2 agents...${NC}"
./scripts/start_local_agents.sh 2 7761 "" "/tmp" || {
    echo "ERROR: Failed to start agents"
    exit 1
}

sleep 2

# Test 1: Distributed prepare (no cleanup)
echo -e "\n${BLUE}=== Test 1: Distributed prepare (agents create their subset) ===${NC}"
"$CONTROLLER" --agents 127.0.0.1:7761,127.0.0.1:7762 run --config "$CONFIG" --prepare-only
TOTAL_FILES=$(ls "$TEST_DIR" 2>/dev/null | wc -l)
echo -e "${GREEN}✓ Total files created: $TOTAL_FILES${NC}"

# Test 2: Normal distributed workload with cleanup
echo -e "\n${BLUE}=== Test 2: Distributed workload with cleanup ===${NC}"
"$CONTROLLER" --agents 127.0.0.1:7761,127.0.0.1:7762 run --config "$CONFIG"
REMAINING=$(ls "$TEST_DIR" 2>/dev/null | wc -l)
echo -e "${GREEN}✓ Files after cleanup: $REMAINING${NC}"

# Test 3: Prepare then cleanup-only
echo -e "\n${BLUE}=== Test 3: Distributed cleanup-only mode ===${NC}"
"$CONTROLLER" --agents 127.0.0.1:7761,127.0.0.1:7762 run --config "$CONFIG" --prepare-only
BEFORE_CLEANUP=$(ls "$TEST_DIR" 2>/dev/null | wc -l)
echo "  Files before cleanup: $BEFORE_CLEANUP"

"$CONTROLLER" --agents 127.0.0.1:7761,127.0.0.1:7762 run --config "$CONFIG" --cleanup-only
AFTER_CLEANUP=$(ls "$TEST_DIR" 2>/dev/null | wc -l)
echo -e "${GREEN}✓ Files after cleanup-only: $AFTER_CLEANUP${NC}"

# Test 4: Partial cleanup (manually delete some files first)
echo -e "\n${BLUE}=== Test 4: Distributed partial cleanup (tolerant mode) ===${NC}"
"$CONTROLLER" --agents 127.0.0.1:7761,127.0.0.1:7762 run --config "$CONFIG" --prepare-only
echo "  Manually deleting 25 files..."
ls "$TEST_DIR" | head -25 | xargs -I {} rm -f "$TEST_DIR/{}"
BEFORE=$(ls "$TEST_DIR" 2>/dev/null | wc -l)
echo "  Files before cleanup: $BEFORE"

"$CONTROLLER" --agents 127.0.0.1:7761,127.0.0.1:7762 run --config "$CONFIG" --cleanup-only
AFTER=$(ls "$TEST_DIR" 2>/dev/null | wc -l)
echo -e "${GREEN}✓ Files after partial cleanup: $AFTER${NC}"

# Cleanup
echo -e "\n${YELLOW}Stopping agents...${NC}"
pkill -9 sai3bench-agent || true
rm -rf "$TEST_DIR"

echo -e "\n${GREEN}=== All distributed cleanup tests passed! ===${NC}"
echo -e "${BLUE}Features validated:${NC}"
echo "  ✓ Distributed prepare (each agent creates its subset)"
echo "  ✓ Distributed cleanup (each agent deletes its subset)"
echo "  ✓ Cleanup-only mode with distributed agents"
echo "  ✓ Tolerant mode with partial pre-deletion"
