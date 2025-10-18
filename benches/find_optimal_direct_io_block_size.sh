#!/bin/bash
# find_optimal_direct_io_block_size.sh
# Test various block sizes with direct:// to find optimal performance

set -e

BENCH_BIN="./target/release/fs_read_bench"
TEST_DIR="/tmp/direct_io_block_test"
NUM_FILES=32
FILE_SIZE_MIB=8
PASSES=2

# Block sizes to test (in bytes)
BLOCK_SIZES=(
    4096        # 4 KiB - minimum for O_DIRECT alignment
    8192        # 8 KiB
    16384       # 16 KiB
    32768       # 32 KiB
    65536       # 64 KiB
    131072      # 128 KiB
    262144      # 256 KiB
    524288      # 512 KiB
    1048576     # 1 MiB
    2097152     # 2 MiB
    4194304     # 4 MiB
    8388608     # 8 MiB (whole file)
)

echo "========================================="
echo "Direct I/O Block Size Optimization Test"
echo "========================================="
echo "Test files: $NUM_FILES × $FILE_SIZE_MiB MiB"
echo "Passes: $PASSES"
echo ""

# Prepare test data
rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

echo "Generating test files..."
$BENCH_BIN --dir "$TEST_DIR" --num-files "$NUM_FILES" --file-size-mib "$FILE_SIZE_MIB" \
    --block-size 262144 --passes 1 > /dev/null 2>&1

echo ""
echo "Testing block sizes..."
echo ""
printf "%-12s %-15s %-15s %-15s\n" "Block Size" "Throughput" "Minor Faults" "Major Faults"
printf "%-12s %-15s %-15s %-15s\n" "----------" "----------" "------------" "------------"

RESULTS_FILE="/tmp/direct_io_block_results_$(date +%Y%m%d_%H%M%S).csv"
echo "block_size_bytes,block_size_label,throughput_gibs,minor_faults,major_faults,latency_p50_ms" > "$RESULTS_FILE"

for block_size in "${BLOCK_SIZES[@]}"; do
    # Format block size for display
    if [ $block_size -lt 1024 ]; then
        size_label="${block_size}B"
    elif [ $block_size -lt 1048576 ]; then
        size_label="$((block_size / 1024))K"
    else
        size_label="$((block_size / 1048576))M"
    fi
    
    # Run test
    output=$($BENCH_BIN --dir "$TEST_DIR" --block-size "$block_size" --direct-io --passes "$PASSES" 2>&1)
    
    # Extract metrics
    throughput=$(echo "$output" | grep "total:" | grep -oP '\d+\.\d+(?= GiB/s)' || echo "0.00")
    minor_faults=$(echo "$output" | grep "page faults" | sed 's/.*minor=\([0-9]*\).*/\1/' || echo "0")
    major_faults=$(echo "$output" | grep "page faults" | sed 's/.*major=\([0-9]*\).*/\1/' || echo "0")
    latency_p50=$(echo "$output" | grep "latency per file:" | sed 's/.*p50=\([0-9.]*\)s.*/\1/' || echo "0")
    latency_p50_ms=$(echo "$latency_p50 * 1000" | bc)
    
    printf "%-12s %-15s %-15s %-15s\n" "$size_label" "$throughput GiB/s" "$minor_faults" "$major_faults"
    
    # Save to CSV
    echo "$block_size,$size_label,$throughput,$minor_faults,$major_faults,$latency_p50_ms" >> "$RESULTS_FILE"
done

echo ""
echo "Results saved to: $RESULTS_FILE"
echo ""

# Find optimal block size
echo "Analysis:"
echo "========="
optimal_line=$(tail -n +2 "$RESULTS_FILE" | sort -t',' -k3 -rn | head -1)
optimal_size=$(echo "$optimal_line" | cut -d',' -f2)
optimal_throughput=$(echo "$optimal_line" | cut -d',' -f3)

echo "✅ Optimal block size: $optimal_size ($optimal_throughput GiB/s)"
echo ""
echo "Recommendation for sai3-bench config:"
echo "  For direct:// URIs, use block sizes between 64 KiB and 2 MiB"
echo "  Best performance typically at 256 KiB - 1 MiB range"
echo ""
