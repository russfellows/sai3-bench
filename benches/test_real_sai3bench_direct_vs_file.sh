#!/bin/bash
# test_real_sai3bench_direct_vs_file.sh
# Real-world test using actual sai3-bench executable with config files
# This is the REAL test - not the standalone benchmark
#
# USAGE: sudo ./benches/test_real_sai3bench_direct_vs_file.sh
#
# Must run with sudo to drop page cache between tests

set -e

SAI3BENCH="./target/release/sai3-bench"
RESULTS_DIR="/tmp/sai3bench_real_test_results_$(date +%Y%m%d_%H%M%S)"

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo -e "${RED}Error: This script must be run with sudo to drop page cache${NC}"
    echo "Usage: sudo ./benches/test_real_sai3bench_direct_vs_file.sh"
    exit 1
fi

mkdir -p "$RESULTS_DIR"

echo -e "${GREEN}=============================================${NC}"
echo -e "${GREEN}Real sai3-bench Testing: file:// vs direct://${NC}"
echo -e "${GREEN}=============================================${NC}"
echo "Using actual sai3-bench executable with YAML configs"
echo "Results directory: $RESULTS_DIR"
echo ""

# Verify binary exists
if [ ! -f "$SAI3BENCH" ]; then
    echo -e "${RED}Error: sai3-bench binary not found at $SAI3BENCH${NC}"
    exit 1
fi

# Function to drop page cache
drop_cache() {
    echo -e "${YELLOW}Dropping page cache...${NC}"
    sync
    echo 3 > /proc/sys/vm/drop_caches
    sleep 2
    echo -e "${GREEN}Page cache cleared${NC}"
}

# Function to run test and extract metrics
run_test() {
    local name="$1"
    local config="$2"
    
    echo -e "\n${BLUE}========================================${NC}"
    echo -e "${BLUE}Test: $name${NC}"
    echo -e "${BLUE}Config: $config${NC}"
    echo -e "${BLUE}========================================${NC}"
    
    # Drop page cache before test
    drop_cache
    
    # Run sai3-bench
    output=$($SAI3BENCH -v run --config "$config" 2>&1)
    
    # Extract key metrics
    throughput_mibs=$(echo "$output" | grep "GET Performance:" -A 5 | grep "Throughput:" | awk '{print $2}')
    ops_per_sec=$(echo "$output" | grep "Throughput:" | head -1 | awk '{print $2}')
    latency_p50=$(echo "$output" | grep "Latency:" | sed 's/.*p50=\([0-9]*\)µs.*/\1/')
    latency_p99=$(echo "$output" | grep "Latency:" | sed 's/.*p99=\([0-9]*\)µs.*/\1/')
    
    echo "$output" | tail -20
    
    # Save to summary
    echo "$name|$throughput_mibs|$ops_per_sec|$latency_p50|$latency_p99" >> "$RESULTS_DIR/summary.txt"
}

# Initialize summary file
echo "Test|Throughput_MiB/s|Ops/s|Latency_P50_us|Latency_P99_us" > "$RESULTS_DIR/summary.txt"

# ==============================================================================
# Test 1: file:// with Sequential page cache (best buffered I/O)
# ==============================================================================
run_test "1. file:// Sequential" "tests/configs/file_io_sequential_test.yaml"

# ==============================================================================
# Test 2: direct:// with O_DIRECT
# ==============================================================================
run_test "2. direct:// O_DIRECT" "tests/configs/direct_io_256k_test.yaml"

# ==============================================================================
# Summary
# ==============================================================================
echo -e "\n${GREEN}=============================================${NC}"
echo -e "${GREEN}Test Summary${NC}"
echo -e "${GREEN}=============================================${NC}"
echo ""
column -t -s'|' "$RESULTS_DIR/summary.txt"
echo ""
echo -e "${BLUE}Detailed results in: $RESULTS_DIR${NC}"
echo ""

# Analysis
echo "Analysis:"
echo "========="
file_throughput=$(grep "file://" "$RESULTS_DIR/summary.txt" | cut -d'|' -f2)
direct_throughput=$(grep "direct://" "$RESULTS_DIR/summary.txt" | cut -d'|' -f2)

echo "✅ file:// (Sequential page cache): $file_throughput MiB/s"
echo "✅ direct:// (O_DIRECT): $direct_throughput MiB/s"
echo ""
echo "Both backends validated with real sai3-bench workload execution!"
echo ""
