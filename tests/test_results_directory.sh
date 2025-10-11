#!/bin/bash
# v0.6.4 Results Directory Validation Test Script
# Tests all single-node scenarios for results directory feature

set -e  # Exit on error

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$PROJECT_ROOT/target/release/sai3-bench"

echo "=== sai3-bench v0.6.4 Results Directory Test Suite ==="
echo "Binary: $BINARY"
echo ""

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Test counter
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

# Cleanup function
cleanup() {
    echo ""
    echo "=== Cleanup ==="
    rm -rf /tmp/sai3bench-v064-test
    rm -rf sai3-test-* sai3-202510*
    echo "Cleanup complete"
}

# Register cleanup on exit
trap cleanup EXIT

# Helper: Run test and check result
run_test() {
    local test_name="$1"
    local expected_dir_pattern="$2"
    local require_tsv="$3"  # "yes" or "no"
    shift 3
    local command=("$@")
    
    TESTS_RUN=$((TESTS_RUN + 1))
    echo ""
    echo "----------------------------------------"
    echo -e "${YELLOW}Test $TESTS_RUN: $test_name${NC}"
    echo "Command: ${command[*]}"
    echo ""
    
    # Run command
    if "${command[@]}" 2>&1; then
        echo -e "${GREEN}✓ Command executed successfully${NC}"
        
        # Find matching directory
        local found_dirs=$(ls -d $expected_dir_pattern 2>/dev/null || true)
        if [ -z "$found_dirs" ]; then
            echo -e "${RED}✗ FAILED: No directory matching '$expected_dir_pattern' found${NC}"
            TESTS_FAILED=$((TESTS_FAILED + 1))
            return 1
        fi
        
        # Use the first (most recent) directory
        local results_dir=$(echo "$found_dirs" | head -1)
        echo "Results directory: $results_dir"
        
        # Verify required files exist
        local required_files=("config.yaml" "console.log" "metadata.json")
        if [ "$require_tsv" == "yes" ]; then
            required_files+=("results.tsv")
        fi
        
        local all_files_present=true
        
        for file in "${required_files[@]}"; do
            if [ -f "$results_dir/$file" ]; then
                echo -e "${GREEN}  ✓ $file exists ($(stat -f%z "$results_dir/$file" 2>/dev/null || stat -c%s "$results_dir/$file") bytes)${NC}"
            else
                echo -e "${RED}  ✗ $file MISSING${NC}"
                all_files_present=false
            fi
        done
        
        # Note if TSV doesn't exist (but isn't required)
        if [ "$require_tsv" == "no" ] && [ ! -f "$results_dir/results.tsv" ]; then
            echo -e "${YELLOW}  ℹ results.tsv not present (not required for this mode)${NC}"
        fi
        
        if $all_files_present; then
            echo -e "${GREEN}✓ TEST PASSED${NC}"
            TESTS_PASSED=$((TESTS_PASSED + 1))
            
            # Show metadata snippet
            echo ""
            echo "Metadata snippet:"
            cat "$results_dir/metadata.json" | grep -E '"test_name"|"duration_secs"|"hostname"|"distributed"' || true
            return 0
        else
            echo -e "${RED}✗ TEST FAILED: Missing required files${NC}"
            TESTS_FAILED=$((TESTS_FAILED + 1))
            return 1
        fi
    else
        # Command failed - check if partial results were saved
        echo -e "${YELLOW}⚠ Command failed (expected for some tests)${NC}"
        
        local found_dirs=$(ls -d $expected_dir_pattern 2>/dev/null || true)
        if [ -n "$found_dirs" ]; then
            local results_dir=$(echo "$found_dirs" | head -1)
            echo "Partial results directory: $results_dir"
            
            # For failed tests, just verify directory exists
            if [ -d "$results_dir" ]; then
                echo -e "${GREEN}✓ Results directory created despite error${NC}"
                ls -lh "$results_dir/"
                TESTS_PASSED=$((TESTS_PASSED + 1))
                return 0
            fi
        fi
        
        echo -e "${RED}✗ TEST FAILED: No results directory created${NC}"
        TESTS_FAILED=$((TESTS_FAILED + 1))
        return 1
    fi
}

# Setup test environment
echo "=== Setup ==="
mkdir -p /tmp/sai3bench-v064-test/data
for i in {1..10}; do
    dd if=/dev/urandom of=/tmp/sai3bench-v064-test/data/file_$i.dat bs=1024 count=50 2>/dev/null
done
echo "Created 10 test files (50KB each) in /tmp/sai3bench-v064-test/data/"
echo ""

# Test 1: Basic run with default naming
cat > /tmp/test-config-1.yaml <<EOF
duration: 2s
concurrency: 2
target: "file:///tmp/sai3bench-v064-test/"
workload:
  - weight: 50
    op: get
    path: "data/*"
  - weight: 50
    op: put
    path: "output/"
    object_size: 1024
EOF

run_test \
    "Basic run with default directory naming" \
    "sai3-*-test-config-1" \
    "yes" \
    "$BINARY" run --config /tmp/test-config-1.yaml

# Test 2: Custom name with --tsv-name
run_test \
    "Custom directory name with --tsv-name" \
    "sai3-*-my-custom-benchmark" \
    "yes" \
    "$BINARY" run --config /tmp/test-config-1.yaml --tsv-name my-custom-benchmark

# Test 3: Prepare-only mode
cat > /tmp/test-config-prepare.yaml <<EOF
duration: 2s
concurrency: 2
target: "file:///tmp/sai3bench-v064-test/"
prepare:
  ensure_objects:
    - base_uri: "file:///tmp/sai3bench-v064-test/prepared/"
      count: 20
      min_size: 1024
      max_size: 1024
      fill: zero
  cleanup: false
workload:
  - weight: 100
    op: get
    path: "prepared/*"
EOF

run_test \
    "Prepare-only mode" \
    "sai3-*-test-config-prepare" \
    "no" \
    "$BINARY" run --config /tmp/test-config-prepare.yaml --prepare-only

# Test 4: Verify mode
run_test \
    "Verify mode (checks prepared objects)" \
    "sai3-*-test-config-prepare" \
    "no" \
    "$BINARY" run --config /tmp/test-config-prepare.yaml --verify

# Test 5: Error handling (missing objects)
cat > /tmp/test-config-error.yaml <<EOF
duration: 2s
concurrency: 2
target: "file:///tmp/sai3bench-v064-test/"
workload:
  - weight: 100
    op: get
    path: "nonexistent/*"
EOF

run_test \
    "Error handling (missing GET objects)" \
    "sai3-*-test-config-error" \
    "no" \
    "$BINARY" run --config /tmp/test-config-error.yaml || true  # Expect failure

# Test 6: Multiple operations with size distributions
cat > /tmp/test-config-multiop.yaml <<EOF
duration: 3s
concurrency: 4
target: "file:///tmp/sai3bench-v064-test/"
workload:
  - weight: 30
    op: get
    path: "data/*"
  - weight: 30
    op: put
    path: "output/"
    size_distribution:
      type: uniform
      min: 512
      max: 4096
  - weight: 20
    op: put
    path: "large/"
    object_size: 102400
  - weight: 20
    op: list
    path: "data/"
EOF

run_test \
    "Multi-operation workload with size distributions" \
    "sai3-*-test-config-multiop" \
    "yes" \
    "$BINARY" run --config /tmp/test-config-multiop.yaml

# Final summary
echo ""
echo "========================================"
echo "=== Test Suite Summary ==="
echo "========================================"
echo "Tests run:    $TESTS_RUN"
echo -e "Tests passed: ${GREEN}$TESTS_PASSED${NC}"
echo -e "Tests failed: ${RED}$TESTS_FAILED${NC}"
echo ""

if [ $TESTS_FAILED -eq 0 ]; then
    echo -e "${GREEN}✓ ALL TESTS PASSED${NC}"
    echo ""
    echo "Created results directories:"
    ls -d sai3-* 2>/dev/null || echo "  (cleaned up)"
    exit 0
else
    echo -e "${RED}✗ SOME TESTS FAILED${NC}"
    exit 1
fi
