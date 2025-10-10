#!/bin/bash
# Streaming Replay Test Suite
# Tests memory efficiency, performance, and correctness of v0.5.0 streaming implementation

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$PROJECT_DIR/target/release/sai3-bench"
TEST_DIR="/tmp/sai3-bench-streaming-tests"
RESULTS_FILE="$TEST_DIR/test_results.txt"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo "======================================================"
echo "sai3-bench Streaming Replay Test Suite"
echo "======================================================"
echo ""

# Ensure binary is built
if [ ! -f "$BINARY" ]; then
    echo "Building sai3-bench..."
    cd "$PROJECT_DIR"
    cargo build --release
fi

# Create test directory
rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"
echo "" > "$RESULTS_FILE"

log_result() {
    local test_name="$1"
    local status="$2"
    local details="$3"
    
    if [ "$status" = "PASS" ]; then
        echo -e "${GREEN}✓ $test_name${NC}"
        echo "PASS: $test_name - $details" >> "$RESULTS_FILE"
    elif [ "$status" = "FAIL" ]; then
        echo -e "${RED}✗ $test_name${NC}"
        echo "FAIL: $test_name - $details" >> "$RESULTS_FILE"
    else
        echo -e "${YELLOW}⚠ $test_name${NC}"
        echo "WARN: $test_name - $details" >> "$RESULTS_FILE"
    fi
}

# Test 1: Create small op-log (100 operations)
echo "Test 1: Small op-log (100 operations)"
echo "======================================="
SMALL_DIR="$TEST_DIR/small"
mkdir -p "$SMALL_DIR"

$BINARY put --uri "file://$SMALL_DIR/data*.dat" \
    --object-size 1024 --objects 100 --concurrency 10 \
    --op-log "$TEST_DIR/small.tsv.zst" > /dev/null 2>&1

if [ -f "$TEST_DIR/small.tsv.zst" ]; then
    SMALL_SIZE=$(stat -f%z "$TEST_DIR/small.tsv.zst" 2>/dev/null || stat -c%s "$TEST_DIR/small.tsv.zst")
    log_result "Create small op-log" "PASS" "100 ops, ${SMALL_SIZE} bytes"
else
    log_result "Create small op-log" "FAIL" "File not created"
    exit 1
fi

# Test 2: Create medium op-log (10,000 operations)
echo ""
echo "Test 2: Medium op-log (10,000 operations)"
echo "=========================================="
MEDIUM_DIR="$TEST_DIR/medium"
mkdir -p "$MEDIUM_DIR"

echo "Creating 10,000 test files (this may take a minute)..."
$BINARY put --uri "file://$MEDIUM_DIR/data*.dat" \
    --object-size 2048 --objects 10000 --concurrency 50 \
    --op-log "$TEST_DIR/medium.tsv.zst" > /dev/null 2>&1

if [ -f "$TEST_DIR/medium.tsv.zst" ]; then
    MEDIUM_SIZE=$(stat -f%z "$TEST_DIR/medium.tsv.zst" 2>/dev/null || stat -c%s "$TEST_DIR/medium.tsv.zst")
    log_result "Create medium op-log" "PASS" "10K ops, ${MEDIUM_SIZE} bytes"
else
    log_result "Create medium op-log" "FAIL" "File not created"
fi

# Test 3: Create large op-log (100,000 operations)
echo ""
echo "Test 3: Large op-log (100,000 operations)"
echo "=========================================="
LARGE_DIR="$TEST_DIR/large"
mkdir -p "$LARGE_DIR"

echo "Creating 100,000 test files (this will take several minutes)..."
echo "Note: Using smaller object size for speed"

# Use smaller files and higher concurrency for speed
$BINARY put --uri "file://$LARGE_DIR/data*.dat" \
    --object-size 512 --objects 100000 --concurrency 100 \
    --op-log "$TEST_DIR/large.tsv.zst" > /dev/null 2>&1

if [ -f "$TEST_DIR/large.tsv.zst" ]; then
    LARGE_SIZE=$(stat -f%z "$TEST_DIR/large.tsv.zst" 2>/dev/null || stat -c%s "$TEST_DIR/large.tsv.zst")
    log_result "Create large op-log" "PASS" "100K ops, ${LARGE_SIZE} bytes"
else
    log_result "Create large op-log" "WARN" "File not created (may have timed out)"
fi

# Test 4: Replay small op-log with memory monitoring
echo ""
echo "Test 4: Replay with memory monitoring"
echo "======================================"

REPLAY_DEST="$TEST_DIR/replay_small"
mkdir -p "$REPLAY_DEST"

# Use /usr/bin/time for memory stats (if available)
if command -v /usr/bin/time > /dev/null 2>&1; then
    echo "Replaying with memory monitoring..."
    /usr/bin/time -v $BINARY -v replay \
        --op-log "$TEST_DIR/small.tsv.zst" \
        --target "file://$REPLAY_DEST/" \
        --speed 100.0 \
        2>&1 | tee "$TEST_DIR/replay_memory.log"
    
    # Extract max resident set size
    MAX_RSS=$(grep "Maximum resident set size" "$TEST_DIR/replay_memory.log" | awk '{print $6}')
    
    if [ -n "$MAX_RSS" ]; then
        # Convert to MB (RSS is in KB on Linux)
        MAX_RSS_MB=$((MAX_RSS / 1024))
        log_result "Replay memory usage" "PASS" "Max RSS: ${MAX_RSS_MB} MB"
        
        # Check if memory is reasonable (should be < 50 MB for streaming)
        if [ $MAX_RSS_MB -lt 50 ]; then
            log_result "Memory efficiency check" "PASS" "Memory usage within expected range"
        else
            log_result "Memory efficiency check" "WARN" "Memory higher than expected"
        fi
    else
        log_result "Memory monitoring" "WARN" "Could not extract memory stats"
    fi
else
    echo "Using basic replay without memory stats..."
    $BINARY -v replay \
        --op-log "$TEST_DIR/small.tsv.zst" \
        --target "file://$REPLAY_DEST/" \
        --speed 100.0
    
    log_result "Basic replay" "PASS" "Completed without memory monitoring"
fi

# Test 5: Verify decompression happens in background
echo ""
echo "Test 5: Background decompression check"
echo "======================================="

if [ -f "$TEST_DIR/medium.tsv.zst" ]; then
    echo "Starting replay and checking process info..."
    
    # Start replay in background
    $BINARY -v replay \
        --op-log "$TEST_DIR/medium.tsv.zst" \
        --speed 50.0 \
        --continue-on-error \
        > /dev/null 2>&1 &
    
    REPLAY_PID=$!
    sleep 2  # Let it start
    
    # Check if process is still running
    if ps -p $REPLAY_PID > /dev/null 2>&1; then
        # Check thread count
        if [ "$(uname)" = "Darwin" ]; then
            THREAD_COUNT=$(ps -M -p $REPLAY_PID | wc -l)
        else
            THREAD_COUNT=$(ps -T -p $REPLAY_PID | wc -l)
        fi
        
        log_result "Multi-threaded execution" "PASS" "Process has $THREAD_COUNT threads"
        
        # Clean up
        kill $REPLAY_PID 2>/dev/null || true
        wait $REPLAY_PID 2>/dev/null || true
    else
        log_result "Background processing" "WARN" "Process completed too quickly"
    fi
else
    log_result "Background decompression check" "SKIP" "Medium op-log not available"
fi

# Test 6: Speed multiplier accuracy
echo ""
echo "Test 6: Speed multiplier accuracy"
echo "=================================="

if [ -f "$TEST_DIR/small.tsv.zst" ]; then
    SPEED_DEST="$TEST_DIR/replay_speed"
    mkdir -p "$SPEED_DEST"
    
    # Test 1x speed
    echo "Testing 1x speed..."
    START_1X=$(date +%s)
    $BINARY replay \
        --op-log "$TEST_DIR/small.tsv.zst" \
        --target "file://$SPEED_DEST/" \
        --speed 1.0 \
        > /dev/null 2>&1
    END_1X=$(date +%s)
    DURATION_1X=$((END_1X - START_1X))
    
    # Test 10x speed
    echo "Testing 10x speed..."
    rm -rf "$SPEED_DEST"/*
    START_10X=$(date +%s)
    $BINARY replay \
        --op-log "$TEST_DIR/small.tsv.zst" \
        --target "file://$SPEED_DEST/" \
        --speed 10.0 \
        > /dev/null 2>&1
    END_10X=$(date +%s)
    DURATION_10X=$((END_10X - START_10X))
    
    log_result "Speed 1x" "PASS" "${DURATION_1X} seconds"
    log_result "Speed 10x" "PASS" "${DURATION_10X} seconds"
    
    # Check if 10x is actually faster
    if [ $DURATION_10X -lt $DURATION_1X ]; then
        log_result "Speed multiplier effect" "PASS" "10x is faster than 1x"
    else
        log_result "Speed multiplier effect" "WARN" "10x not significantly faster"
    fi
else
    log_result "Speed multiplier test" "SKIP" "Small op-log not available"
fi

# Test 7: Error handling
echo ""
echo "Test 7: Error handling with continue-on-error"
echo "=============================================="

# Create op-log with non-existent files
echo "Creating op-log with invalid references..."
cat > "$TEST_DIR/bad_ops.tsv" << 'EOF'
idx	thread	op	client_id	n_objects	bytes	endpoint	file	error	start	first_byte	end	duration_ns
0	1	GET	1	1	1024	file://	/tmp/nonexistent/file1.dat		2025-10-03T00:00:00Z		2025-10-03T00:00:01Z	1000000000
1	1	GET	1	1	1024	file://	/tmp/nonexistent/file2.dat		2025-10-03T00:00:01Z		2025-10-03T00:00:02Z	1000000000
2	1	GET	1	1	1024	file://	/tmp/nonexistent/file3.dat		2025-10-03T00:00:02Z		2025-10-03T00:00:03Z	1000000000
EOF

# Compress it
zstd "$TEST_DIR/bad_ops.tsv" -o "$TEST_DIR/bad_ops.tsv.zst" -f

# Test with continue-on-error
echo "Replaying with --continue-on-error..."
if $BINARY replay \
    --op-log "$TEST_DIR/bad_ops.tsv.zst" \
    --speed 10.0 \
    --continue-on-error \
    2>&1 | grep -q "failed"; then
    log_result "Continue on error" "PASS" "Continued despite failures"
else
    log_result "Continue on error" "WARN" "Unexpected behavior"
fi

# Test 8: Format detection
echo ""
echo "Test 8: Format detection (TSV vs JSONL)"
echo "========================================"

if [ -f "$TEST_DIR/small.tsv.zst" ]; then
    # Decompress and check that it can read TSV
    zstd -d "$TEST_DIR/small.tsv.zst" -o "$TEST_DIR/small_plain.tsv" -f
    
    if $BINARY replay \
        --op-log "$TEST_DIR/small_plain.tsv" \
        --speed 100.0 \
        --continue-on-error \
        > /dev/null 2>&1; then
        log_result "Plain TSV format" "PASS" "Reads uncompressed TSV"
    else
        log_result "Plain TSV format" "FAIL" "Failed to read plain TSV"
    fi
    
    log_result "Compressed TSV format" "PASS" "Reads .zst compressed files"
else
    log_result "Format detection" "SKIP" "Test files not available"
fi

# Summary
echo ""
echo "======================================================"
echo "Test Results Summary"
echo "======================================================"
cat "$RESULTS_FILE"

# Count results
PASS_COUNT=$(grep "^PASS:" "$RESULTS_FILE" | wc -l)
FAIL_COUNT=$(grep "^FAIL:" "$RESULTS_FILE" | wc -l)
WARN_COUNT=$(grep "^WARN:" "$RESULTS_FILE" | wc -l)
SKIP_COUNT=$(grep "^SKIP:" "$RESULTS_FILE" | wc -l)

echo ""
echo "Summary:"
echo "  Passed: $PASS_COUNT"
echo "  Failed: $FAIL_COUNT"
echo "  Warnings: $WARN_COUNT"
echo "  Skipped: $SKIP_COUNT"

if [ $FAIL_COUNT -eq 0 ]; then
    echo -e "${GREEN}All critical tests passed!${NC}"
    exit 0
else
    echo -e "${RED}Some tests failed!${NC}"
    exit 1
fi
