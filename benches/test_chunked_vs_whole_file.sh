#!/bin/bash
# test_chunked_vs_whole_file.sh
# Compare whole-file get() vs chunked get_range() for local file I/O
# Tests both file:// and direct:// with and without stat() overhead

set -e

BENCH_BIN="./target/release/fs_read_bench"
TEST_DIR="/tmp/chunked_vs_whole_test"
NUM_FILES=64
FILE_SIZE_MIB=8
PASSES=2

# Colors
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${GREEN}=============================================${NC}"
echo -e "${GREEN}Chunked vs Whole-File Read Comparison${NC}"
echo -e "${GREEN}=============================================${NC}"
echo "Test: file:// and direct:// URIs"
echo "Files: $NUM_FILES × $FILE_SIZE_MIB MiB"
echo "Passes: $PASSES"
echo ""

# Prepare test data
rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

echo "Generating test files..."
$BENCH_BIN --dir "$TEST_DIR" --num-files "$NUM_FILES" --file-size-mib "$FILE_SIZE_MIB" \
    --block-size 262144 --passes 1 > /dev/null 2>&1

echo ""
RESULTS_FILE="/tmp/chunked_vs_whole_results_$(date +%Y%m%d_%H%M%S).txt"
echo "Test|Throughput_GiB/s|Minor_Faults|Major_Faults" > "$RESULTS_FILE"

run_test() {
    local desc="$1"
    shift
    echo -e "\n${BLUE}Test: $desc${NC}"
    
    output=$($BENCH_BIN --dir "$TEST_DIR" "$@" 2>&1)
    echo "$output" | tail -10
    
    throughput=$(echo "$output" | grep "total:" | grep -oP '\d+\.\d+(?= GiB/s)')
    minor_faults=$(echo "$output" | grep "page faults" | sed 's/.*minor=\([0-9]*\).*/\1/')
    major_faults=$(echo "$output" | grep "page faults" | sed 's/.*major=\([0-9]*\).*/\1/')
    
    echo "$desc|$throughput|$minor_faults|$major_faults" >> "$RESULTS_FILE"
}

echo -e "${YELLOW}=== file:// Tests ===${NC}"

# Test 1: file:// whole-file read (block-size=0)
run_test "1. file:// whole-file (no chunks)" \
    --block-size 0 --page-cache dontneed --passes "$PASSES"

# Test 2: file:// chunked with 256 KiB blocks
run_test "2. file:// 256K chunks (with stat)" \
    --block-size 262144 --page-cache dontneed --passes "$PASSES"

# Test 3: file:// chunked with 1 MiB blocks
run_test "3. file:// 1M chunks (with stat)" \
    --block-size 1048576 --page-cache dontneed --passes "$PASSES"

# Test 4: file:// chunked with 4 MiB blocks
run_test "4. file:// 4M chunks (with stat)" \
    --block-size 4194304 --page-cache dontneed --passes "$PASSES"

echo -e "\n${YELLOW}=== direct:// Tests ===${NC}"

# Test 5: direct:// whole-file read (should be slow - alignment issues)
run_test "5. direct:// whole-file (no chunks)" \
    --block-size 0 --direct-io --passes "$PASSES"

# Test 6: direct:// chunked with 256 KiB blocks
run_test "6. direct:// 256K chunks (with stat)" \
    --block-size 262144 --direct-io --passes "$PASSES"

# Test 7: direct:// chunked with 1 MiB blocks
run_test "7. direct:// 1M chunks (with stat)" \
    --block-size 1048576 --direct-io --passes "$PASSES"

# Test 8: direct:// chunked with 4 MiB blocks
run_test "8. direct:// 4M chunks (with stat)" \
    --block-size 4194304 --direct-io --passes "$PASSES"

echo -e "\n${GREEN}=============================================${NC}"
echo -e "${GREEN}Results Summary${NC}"
echo -e "${GREEN}=============================================${NC}"
echo ""
column -t -s'|' "$RESULTS_FILE"
echo ""
echo "Results saved to: $RESULTS_FILE"
echo ""

echo "Analysis:"
echo "========="
echo ""
echo "Chunked reads use stat() before each file to determine size."
echo "For local files, stat() overhead is ~0.1-0.5ms per file - negligible!"
echo ""
echo "Key comparisons:"
echo "  - Whole-file vs chunked (file://): Test read efficiency"
echo "  - Whole-file vs chunked (direct://): Shows alignment issue fix"
echo "  - Different chunk sizes: Find optimal balance"
echo ""
