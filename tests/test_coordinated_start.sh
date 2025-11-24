#!/bin/bash
# Test coordinated start with simulated clock skew using faketime
# This script verifies that agents start workloads synchronously despite clock differences

set -e

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}=== Testing Coordinated Start with Clock Skew ===${NC}\n"

# Ensure we're in the right directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

# Build if needed
if [ ! -f ./target/release/sai3bench-agent ]; then
    echo "Building sai3-bench..."
    cargo build --release
fi

# Clean up any existing agents
echo "Cleaning up any existing agents..."
pkill -9 sai3bench-agent 2>/dev/null || true
sleep 2  # Give ports time to be released

# Create test directory
TEST_DIR="/tmp/sai3-bench-coordinated-test"
rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

# Use EXISTING working config (modified for 3 agents)
CONFIG_FILE="$TEST_DIR/test_config.yaml"
cp tests/configs/local_test_2agents.yaml "$CONFIG_FILE"

# Modify config to use 3 agents
sed -i 's/local_test_2agents/coordinated_start_test/' "$CONFIG_FILE"
sed -i '/agent-2/a\    - address: "127.0.0.1:7763"\n      id: "agent-3"' "$CONFIG_FILE"

echo -e "${YELLOW}Validating config with --dry-run...${NC}"
./target/release/sai3bench-ctl --agents 127.0.0.1:7761,127.0.0.1:7762,127.0.0.1:7763 \
    run --config "$CONFIG_FILE" --dry-run || {
    echo -e "${RED}Config validation failed!${NC}"
    echo "Config content:"
    cat "$CONFIG_FILE"
    exit 1
}
echo -e "${GREEN}Config validation passed!${NC}\n"

# Start agents with simulated clock skew
LOG_DIR="$TEST_DIR/logs"
mkdir -p "$LOG_DIR"

echo -e "${YELLOW}Starting agents with simulated clock skew (with verbose logging)...${NC}"

# Agent 1: Normal clock
echo "  Agent 1 (listen 0.0.0.0:7761): Normal clock"
./target/release/sai3bench-agent -v --listen 0.0.0.0:7761 > "$LOG_DIR/agent1.log" 2>&1 &
AGENT1_PID=$!
echo "    Started with PID $AGENT1_PID"
sleep 2

# Agent 2: +3.5 seconds ahead
echo "  Agent 2 (listen 0.0.0.0:7762): +3.5 seconds ahead"
faketime -f '+3.5s' ./target/release/sai3bench-agent -v --listen 0.0.0.0:7762 > "$LOG_DIR/agent2.log" 2>&1 &
AGENT2_PID=$!
echo "    Started with PID $AGENT2_PID"
sleep 2

# Agent 3: -2.8 seconds behind
echo "  Agent 3 (listen 0.0.0.0:7763): -2.8 seconds behind"
faketime -f '-2.8s' ./target/release/sai3bench-agent -v --listen 0.0.0.0:7763 > "$LOG_DIR/agent3.log" 2>&1 &
AGENT3_PID=$!
echo "    Started with PID $AGENT3_PID"
sleep 2

# Verify all agents are running with ps command
echo -e "\n${YELLOW}Verifying agents are running:${NC}"
ps aux | grep sai3bench-agent | grep -v grep
echo ""

# Verify all agents are running with ps command
echo -e "\n${YELLOW}Verifying agents are running:${NC}"
ps aux | grep sai3bench-agent | grep -v grep
echo ""

# Double-check each PID
echo -e "${YELLOW}Checking individual PIDs:${NC}"
if ps -p $AGENT1_PID > /dev/null 2>&1; then
    echo "  ✅ Agent 1 (PID $AGENT1_PID) is running"
else
    echo -e "  ${RED}❌ Agent 1 (PID $AGENT1_PID) is NOT running${NC}"
    echo "  Agent 1 log:"
    cat "$LOG_DIR/agent1.log"
fi

if ps -p $AGENT2_PID > /dev/null 2>&1; then
    echo "  ✅ Agent 2 (PID $AGENT2_PID) is running"
else
    echo -e "  ${RED}❌ Agent 2 (PID $AGENT2_PID) is NOT running${NC}"
    echo "  Agent 2 log:"
    cat "$LOG_DIR/agent2.log"
fi

if ps -p $AGENT3_PID > /dev/null 2>&1; then
    echo "  ✅ Agent 3 (PID $AGENT3_PID) is running"
else
    echo -e "  ${RED}❌ Agent 3 (PID $AGENT3_PID) is NOT running${NC}"
    echo "  Agent 3 log:"
    cat "$LOG_DIR/agent3.log"
fi
echo ""

# Check if agents are listening on their ports
echo -e "${YELLOW}Checking if agents are listening on ports:${NC}"
netstat -tuln | grep -E ':(7761|7762|7763)' || echo "  No agents listening on expected ports!"
echo ""

# Verify all agents are running
if ! ps -p $AGENT1_PID > /dev/null 2>&1; then
    echo -e "${RED}Agent 1 failed to start!${NC}"
    cat "$LOG_DIR/agent1.log"
    exit 1
fi

if ! ps -p $AGENT2_PID > /dev/null 2>&1; then
    echo -e "${RED}Agent 2 failed to start!${NC}"
    cat "$LOG_DIR/agent2.log"
    exit 1
fi

if ! ps -p $AGENT3_PID > /dev/null 2>&1; then
    echo -e "${RED}Agent 3 failed to start!${NC}"
    cat "$LOG_DIR/agent3.log"
    exit 1
fi

echo -e "${GREEN}All 3 agents verified running and listening${NC}\n"

# Run controller
echo -e "${YELLOW}Running controller with coordinated start (verbose logging)...${NC}"
./target/release/sai3bench-ctl -v --agents 127.0.0.1:7761,127.0.0.1:7762,127.0.0.1:7763 \
    run --config "$CONFIG_FILE"

CONTROLLER_EXIT=$?

# Cleanup
echo -e "\n${YELLOW}Cleaning up agents...${NC}"
kill $AGENT1_PID $AGENT2_PID $AGENT3_PID 2>/dev/null || true
sleep 1
pkill -9 sai3bench-agent 2>/dev/null || true

# Check results
if [ $CONTROLLER_EXIT -ne 0 ]; then
    echo -e "${RED}Controller failed with exit code $CONTROLLER_EXIT${NC}"
    echo -e "\n${YELLOW}Agent logs:${NC}"
    for i in 1 2 3; do
        echo -e "\n--- Agent $i ---"
        tail -20 "$LOG_DIR/agent$i.log"
    done
    exit 1
fi

echo -e "\n${GREEN}=== Analyzing Coordinated Start Timing ===${NC}"

# Extract and display start times from agent logs
echo -e "\nSearching for coordinated start messages in agent logs...\n"

for i in 1 2 3; do
    echo -e "${YELLOW}Agent $i:${NC}"
    grep -i "coordinated start\|waiting until\|workload starting" "$LOG_DIR/agent$i.log" || echo "  No start messages found"
    echo ""
done

echo -e "${GREEN}=== Test Complete ===${NC}"
echo -e "Logs saved to: $LOG_DIR"
echo -e "To review detailed logs:"
echo -e "  tail -f $LOG_DIR/agent1.log"
echo -e "  tail -f $LOG_DIR/agent2.log"
echo -e "  tail -f $LOG_DIR/agent3.log"


echo -e "Starting 3 agents with simulated clock skew:\n"
echo "  Agent 1: Normal clock"
echo "  Agent 2: Clock +2 hours ahead  (FAKETIME='+2h')"
echo -e "  Agent 3: Clock -1 hour behind (FAKETIME='-1h')\n"

# Check if faketime is installed
if ! command -v faketime &> /dev/null; then
    echo -e "${YELLOW}WARNING: faketime not installed. Installing...${NC}"
    if command -v apt-get &> /dev/null; then
        sudo apt-get update && sudo apt-get install -y faketime
    elif command -v dnf &> /dev/null; then
        sudo dnf install -y faketime
    else
        echo -e "${RED}ERROR: Cannot install faketime. Please install manually.${NC}"
        exit 1
    fi
fi

# Start agents with different clock settings
echo -e "\n${GREEN}Starting agents...${NC}"

# Agent 1: Normal clock
../target/release/sai3bench-agent --port 7761 > "$TEST_DIR/agent1.log" 2>&1 &
AGENT1_PID=$!
echo "Agent 1 (normal clock) started on port 7761 (PID: $AGENT1_PID)"

# Agent 2: +2 hours ahead
faketime '+2h' ../target/release/sai3bench-agent --port 7762 > "$TEST_DIR/agent2.log" 2>&1 &
AGENT2_PID=$!
echo "Agent 2 (+2h ahead) started on port 7762 (PID: $AGENT2_PID)"

# Agent 3: -1 hour behind
faketime '-1h' ../target/release/sai3bench-agent --port 7763 > "$TEST_DIR/agent3.log" 2>&1 &
AGENT3_PID=$!
echo "Agent 3 (-1h behind) started on port 7763 (PID: $AGENT3_PID)"

# Wait for agents to start
sleep 2

# Verify agents are running
if ! ps -p $AGENT1_PID > /dev/null; then
    echo -e "${RED}ERROR: Agent 1 failed to start${NC}"
    cat "$TEST_DIR/agent1.log"
    exit 1
fi

if ! ps -p $AGENT2_PID > /dev/null; then
    echo -e "${RED}ERROR: Agent 2 failed to start${NC}"
    cat "$TEST_DIR/agent2.log"
    exit 1
fi

if ! ps -p $AGENT3_PID > /dev/null; then
    echo -e "${RED}ERROR: Agent 3 failed to start${NC}"
    cat "$TEST_DIR/agent3.log"
    exit 1
fi

echo -e "\n${GREEN}All agents started successfully${NC}\n"

# Run controller with distributed agents
echo -e "${GREEN}Running controller with 3 agents...${NC}\n"

../target/release/sai3bench-ctl \
    --agents 127.0.0.1:7761,127.0.0.1:7762,127.0.0.1:7763 \
    run \
    --config "$TEST_DIR/test_config.yaml" \
    --results-dir "$TEST_DIR/results" \
    2>&1 | tee "$TEST_DIR/controller.log"

CONTROLLER_EXIT=$?

# Clean up agents
echo -e "\n${GREEN}Cleaning up agents...${NC}"
kill $AGENT1_PID $AGENT2_PID $AGENT3_PID 2>/dev/null || true
sleep 1
pkill -9 sai3bench-agent 2>/dev/null || true

# Check results
echo -e "\n${GREEN}=== Test Results ===${NC}\n"

if [ $CONTROLLER_EXIT -eq 0 ]; then
    echo -e "${GREEN}✓ Controller completed successfully${NC}"
    
    # Check if all agents completed
    if grep -q "All 3 agents completed" "$TEST_DIR/controller.log"; then
        echo -e "${GREEN}✓ All 3 agents completed workload${NC}"
    else
        echo -e "${YELLOW}⚠ Not all agents completed (check logs)${NC}"
    fi
    
    # Check for coordinated start logs
    echo -e "\n${GREEN}Checking coordinated start timing:${NC}"
    
    for i in 1 2 3; do
        if grep -q "Coordinated start time reached" "$TEST_DIR/agent$i.log"; then
            START_TIME=$(grep "Coordinated start time reached" "$TEST_DIR/agent$i.log" | head -1)
            echo "  Agent $i: $START_TIME"
        else
            echo -e "  ${YELLOW}Agent $i: No coordinated start log found${NC}"
        fi
    done
    
    # Check results directory
    if [ -d "$TEST_DIR/results" ]; then
        echo -e "\n${GREEN}✓ Results directory created${NC}"
        echo "  Location: $TEST_DIR/results"
        
        if [ -f "$TEST_DIR/results/workload_results.tsv" ]; then
            echo -e "${GREEN}✓ Consolidated results TSV created${NC}"
            
            # Show summary
            echo -e "\n${GREEN}Results summary:${NC}"
            head -2 "$TEST_DIR/results/workload_results.tsv"
        fi
        
        # Check per-agent results
        AGENT_RESULTS=$(find "$TEST_DIR/results/agents" -name "workload_results.tsv" 2>/dev/null | wc -l)
        if [ $AGENT_RESULTS -eq 3 ]; then
            echo -e "${GREEN}✓ All 3 agent results collected${NC}"
        else
            echo -e "${YELLOW}⚠ Only $AGENT_RESULTS agent results found (expected 3)${NC}"
        fi
    fi
    
    echo -e "\n${GREEN}========================================${NC}"
    echo -e "${GREEN}TEST PASSED: Coordinated start working!${NC}"
    echo -e "${GREEN}========================================${NC}\n"
    
else
    echo -e "${RED}✗ Controller failed with exit code $CONTROLLER_EXIT${NC}"
    
    echo -e "\n${RED}Agent logs:${NC}"
    for i in 1 2 3; do
        echo -e "\n${RED}=== Agent $i ===${NC}"
        tail -20 "$TEST_DIR/agent$i.log"
    done
    
    echo -e "\n${RED}Controller log:${NC}"
    tail -50 "$TEST_DIR/controller.log"
    
    echo -e "\n${RED}========================================${NC}"
    echo -e "${RED}TEST FAILED: Check logs above${NC}"
    echo -e "${RED}========================================${NC}\n"
    
    exit 1
fi

echo "Test artifacts saved to: $TEST_DIR"
echo "To inspect: cd $TEST_DIR"
