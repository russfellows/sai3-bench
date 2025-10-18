#!/bin/bash
# test_buffer_improvements.sh
# Comprehensive benchmark for s3dlio v0.9.9 buffer pool improvements
# Tests file:// and direct:// backends with various configurations

set -e

# Configuration
BENCH_DIR="/tmp/fsbench_data"
NUM_FILES=64
FILE_SIZE_MIB=8
BLOCK_SIZE=262144  # 256 KiB
PASSES=3
BENCH_BIN="./target/release/fs_read_bench"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to drop page cache (requires sudo)
drop_cache() {
    echo -e "${YELLOW}Dropping page cache...${NC}"
    sync
    if sudo -n true 2>/dev/null; then
        echo 3 | sudo tee /proc/sys/vm/drop_caches > /dev/null
        sleep 1
    else
        echo -e "${YELLOW}Warning: Cannot drop cache (no sudo access). Results may be affected by page cache.${NC}"
    fi
}

# Function to run benchmark and extract throughput
run_benchmark() {
    local desc="$1"
    shift
    echo -e "\n${BLUE}========================================${NC}"
    echo -e "${BLUE}Test: $desc${NC}"
    echo -e "${BLUE}========================================${NC}"
    
    drop_cache
    
    # Run benchmark and capture output
    local output=$("$BENCH_BIN" "$@" 2>&1)
    echo "$output"
    
    # Extract throughput (GiB/s) - format: "total: X.XXX GiB in Y.YYY s -> Z.ZZ GiB/s"
    local throughput=$(echo "$output" | grep "total:" | grep -oP '\d+\.\d+(?= GiB/s)')
    
    # Extract page faults (format: "page faults (delta): minor=XXX  major=YYY")
    local minor_faults=$(echo "$output" | grep "page faults" | sed 's/.*minor=\([0-9]*\).*/\1/')
    local major_faults=$(echo "$output" | grep "page faults" | sed 's/.*major=\([0-9]*\).*/\1/')
    
    echo -e "${GREEN}Result: ${throughput} GiB/s | minor_faults=${minor_faults} major_faults=${major_faults}${NC}"
    
    # Store result
    echo "$desc|$throughput|$minor_faults|$major_faults" >> "$RESULTS_FILE"
}

# Results file
RESULTS_FILE="/tmp/buffer_bench_results_$(date +%Y%m%d_%H%M%S).txt"
echo "Test|Throughput_GiB/s|Minor_Faults|Major_Faults" > "$RESULTS_FILE"

echo -e "${GREEN}=====================================${NC}"
echo -e "${GREEN}s3dlio v0.9.9 Buffer Pool Benchmark${NC}"
echo -e "${GREEN}=====================================${NC}"
echo "Configuration:"
echo "  Files: $NUM_FILES × $FILE_SIZE_MIB MiB"
echo "  Block size: $BLOCK_SIZE bytes (256 KiB)"
echo "  Passes: $PASSES"
echo "  Results: $RESULTS_FILE"
echo ""

# Verify benchmark binary exists
if [ ! -f "$BENCH_BIN" ]; then
    echo -e "${RED}Error: Benchmark binary not found at $BENCH_BIN${NC}"
    echo "Please run: cargo build --release --bin fs_read_bench"
    exit 1
fi

# Verify sudo access
echo -e "${YELLOW}Checking sudo access for page cache clearing...${NC}"
if ! sudo -n true 2>/dev/null; then
    echo -e "${YELLOW}Note: This script works best with sudo to drop page cache between tests${NC}"
    echo -e "${YELLOW}You can run: sudo -v before running this script, or run the whole script with sudo${NC}"
    echo ""
    read -p "Continue without cache clearing? (y/N) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

# ==============================================================================
# Test 1: Baseline - Whole-file reads (file://)
# ==============================================================================
run_benchmark "1. Whole-file reads (file://)" \
    --dir "$BENCH_DIR" \
    --num-files "$NUM_FILES" \
    --file-size-mib "$FILE_SIZE_MIB" \
    --block-size 0 \
    --page-cache dontneed \
    --passes "$PASSES"

# ==============================================================================
# Test 2: Fixed 256 KiB blocks (file://, vdbench-style)
# ==============================================================================
run_benchmark "2. 256 KiB blocks (file://)" \
    --dir "$BENCH_DIR" \
    --block-size "$BLOCK_SIZE" \
    --page-cache dontneed \
    --passes "$PASSES"

# ==============================================================================
# Test 3: Fixed 256 KiB blocks with Sequential hint (file://)
# ==============================================================================
run_benchmark "3. 256 KiB blocks + Sequential (file://)" \
    --dir "$BENCH_DIR" \
    --block-size "$BLOCK_SIZE" \
    --page-cache sequential \
    --passes "$PASSES"

# ==============================================================================
# Test 4: Fixed 256 KiB blocks with Auto hint (file://)
# ==============================================================================
run_benchmark "4. 256 KiB blocks + Auto (file://)" \
    --dir "$BENCH_DIR" \
    --block-size "$BLOCK_SIZE" \
    --page-cache auto \
    --passes "$PASSES"

# ==============================================================================
# Test 5: O_DIRECT - Whole-file reads (direct://)
# ==============================================================================
run_benchmark "5. Whole-file reads (direct://)" \
    --dir "$BENCH_DIR" \
    --block-size 0 \
    --direct-io \
    --passes "$PASSES"

# ==============================================================================
# Test 6: O_DIRECT - Fixed 256 KiB blocks (direct://)
# ==============================================================================
run_benchmark "6. 256 KiB blocks (direct://)" \
    --dir "$BENCH_DIR" \
    --block-size "$BLOCK_SIZE" \
    --direct-io \
    --passes "$PASSES"

# ==============================================================================
# Test 7: Larger blocks - 1 MiB (file://)
# ==============================================================================
run_benchmark "7. 1 MiB blocks (file://)" \
    --dir "$BENCH_DIR" \
    --block-size 1048576 \
    --page-cache dontneed \
    --passes "$PASSES"

# ==============================================================================
# Test 8: Smaller blocks - 64 KiB (file://)
# ==============================================================================
run_benchmark "8. 64 KiB blocks (file://)" \
    --dir "$BENCH_DIR" \
    --block-size 65536 \
    --page-cache dontneed \
    --passes "$PASSES"

# ==============================================================================
# Summary
# ==============================================================================
echo -e "\n${GREEN}=====================================${NC}"
echo -e "${GREEN}Benchmark Complete!${NC}"
echo -e "${GREEN}=====================================${NC}"
echo ""
echo "Results summary:"
echo ""
column -t -s'|' "$RESULTS_FILE"
echo ""
echo -e "${BLUE}Full results saved to: $RESULTS_FILE${NC}"
echo ""
echo "Key observations to look for:"
echo "  1. Buffer pool improvements should show in reduced minor page faults"
echo "  2. O_DIRECT (direct://) should have near-zero page faults"
echo "  3. 256 KiB blocks should benefit most from buffer pooling"
echo "  4. Throughput should improve for fixed-block reads vs whole-file"
echo ""
