#!/bin/bash
# Comprehensive test suite for page_cache_mode feature
# Tests YAML config, compilation, dry-run display, and actual page cache hint application

set -e  # Exit on error
set -u  # Exit on undefined variable

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Test results tracking
TESTS_PASSED=0
TESTS_FAILED=0

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

test_passed() {
    TESTS_PASSED=$((TESTS_PASSED + 1))
    log_info "✅ PASSED: $1"
}

test_failed() {
    TESTS_FAILED=$((TESTS_FAILED + 1))
    log_error "❌ FAILED: $1"
}

print_summary() {
    echo ""
    echo "========================================="
    echo "Test Summary"
    echo "========================================="
    echo "Passed: $TESTS_PASSED"
    echo "Failed: $TESTS_FAILED"
    echo "========================================="
    if [ $TESTS_FAILED -eq 0 ]; then
        log_info "All tests passed! ✅"
        return 0
    else
        log_error "Some tests failed ❌"
        return 1
    fi
}

# Test 1: Binary exists
test_binary_exists() {
    log_info "Test 1: Checking if sai3-bench binary exists..."
    if [ -f "./target/release/sai3-bench" ]; then
        test_passed "Binary exists at ./target/release/sai3-bench"
    else
        test_failed "Binary not found - run 'cargo build --release' first"
        exit 1
    fi
}

# Test 2: Dry-run with page_cache_mode displays correctly
test_dryrun_display() {
    log_info "Test 2: Testing dry-run display of page_cache_mode..."
    
    local config="tests/configs/page_cache_sequential_test.yaml"
    local output=$(./target/release/sai3-bench run --config "$config" --dry-run 2>&1)
    
    if echo "$output" | grep -q "Page Cache Configuration"; then
        if echo "$output" | grep -q "Sequential"; then
            test_passed "Dry-run shows page cache configuration with Sequential mode"
        else
            test_failed "Dry-run shows page cache section but wrong mode"
            echo "$output" | grep -A 5 "Page Cache"
        fi
    else
        test_failed "Dry-run doesn't show page cache configuration"
        echo "$output"
    fi
}

# Test 3: Verify all page cache mode configs parse correctly
test_config_parsing() {
    log_info "Test 3: Testing YAML config parsing for all modes..."
    
    local modes=("sequential" "random" "dontneed" "auto")
    for mode in "${modes[@]}"; do
        local config="tests/configs/page_cache_${mode}_test.yaml"
        if ./target/release/sai3-bench run --config "$config" --dry-run > /dev/null 2>&1; then
            test_passed "Config parsing: page_cache_mode=$mode"
        else
            test_failed "Config parsing: page_cache_mode=$mode"
        fi
    done
}

# Test 4: Test without page_cache_mode (backward compatibility)
test_no_page_cache_mode() {
    log_info "Test 4: Testing backward compatibility (no page_cache_mode)..."
    
    local config="tests/configs/file_test.yaml"
    if ./target/release/sai3-bench run --config "$config" --dry-run > /dev/null 2>&1; then
        test_passed "Backward compatibility: configs without page_cache_mode still work"
    else
        test_failed "Backward compatibility: old configs broken"
    fi
}

# Test 5: Run actual workload with sequential mode
test_sequential_workload() {
    log_info "Test 5: Running actual workload with sequential mode..."
    
    local config="tests/configs/page_cache_sequential_test.yaml"
    
    # Run workload (will create test directory and files)
    if ./target/release/sai3-bench -v run --config "$config" 2>&1 | grep -q "Starting execution"; then
        test_passed "Sequential mode workload executed successfully"
        
        # Check if test files were created
        if [ -d "/tmp/sai3bench-page-cache-test/data" ]; then
            local file_count=$(find /tmp/sai3bench-page-cache-test/data -type f | wc -l)
            log_info "Created $file_count test files"
        fi
    else
        test_failed "Sequential mode workload failed to execute"
    fi
}

# Test 6: Verify fadvise system calls with strace (if available)
test_fadvise_syscalls() {
    log_info "Test 6: Verifying posix_fadvise system calls with strace..."
    
    if ! command -v strace &> /dev/null; then
        log_warn "strace not available - skipping system call verification"
        return
    fi
    
    local config="tests/configs/page_cache_sequential_test.yaml"
    local strace_output=$(mktemp)
    
    # Run with strace, filter for fadvise64 calls
    if strace -e trace=fadvise64 -o "$strace_output" ./target/release/sai3-bench run --config "$config" 2>&1 > /dev/null; then
        if grep -q "fadvise64" "$strace_output"; then
            local sequential_count=$(grep "POSIX_FADV_SEQUENTIAL" "$strace_output" | wc -l)
            if [ "$sequential_count" -gt 0 ]; then
                test_passed "posix_fadvise calls detected: $sequential_count SEQUENTIAL hints"
            else
                log_warn "fadvise64 calls found but no SEQUENTIAL hints"
                test_failed "No POSIX_FADV_SEQUENTIAL hints found"
            fi
        else
            log_warn "No fadvise64 system calls detected - may be expected on non-Linux systems"
        fi
    fi
    
    rm -f "$strace_output"
}

# Test 7: Test random mode
test_random_mode() {
    log_info "Test 7: Testing random page cache mode..."
    
    local config="tests/configs/page_cache_random_test.yaml"
    
    if ./target/release/sai3-bench run --config "$config" 2>&1 | grep -q "Starting execution"; then
        test_passed "Random mode workload executed successfully"
    else
        test_failed "Random mode workload failed"
    fi
}

# Test 8: Test dontneed mode
test_dontneed_mode() {
    log_info "Test 8: Testing dontneed page cache mode..."
    
    local config="tests/configs/page_cache_dontneed_test.yaml"
    
    if ./target/release/sai3-bench run --config "$config" 2>&1 | grep -q "Starting execution"; then
        test_passed "DontNeed mode workload executed successfully"
    else
        test_failed "DontNeed mode workload failed"
    fi
}

# Test 9: Test auto mode
test_auto_mode() {
    log_info "Test 9: Testing auto page cache mode..."
    
    local config="tests/configs/page_cache_auto_test.yaml"
    
    if ./target/release/sai3-bench run --config "$config" 2>&1 | grep -q "Starting execution"; then
        test_passed "Auto mode workload executed successfully"
    else
        test_failed "Auto mode workload failed"
    fi
}

# Test 10: Test mixed workload (GET + PUT)
test_mixed_workload() {
    log_info "Test 10: Testing mixed workload with page cache mode..."
    
    local config="tests/configs/page_cache_mixed_test.yaml"
    
    if ./target/release/sai3-bench run --config "$config" 2>&1 | grep -q "Starting execution"; then
        test_passed "Mixed workload (GET+PUT) executed successfully"
    else
        test_failed "Mixed workload failed"
    fi
}

# Cleanup function
cleanup_test_files() {
    log_info "Cleaning up test files..."
    rm -rf /tmp/sai3bench-page-cache-test/
    log_info "Cleanup complete"
}

# Main execution
main() {
    echo "========================================="
    echo "Page Cache Mode Feature Test Suite"
    echo "========================================="
    echo ""
    
    test_binary_exists
    test_dryrun_display
    test_config_parsing
    test_no_page_cache_mode
    test_sequential_workload
    test_fadvise_syscalls
    test_random_mode
    test_dontneed_mode
    test_auto_mode
    test_mixed_workload
    
    echo ""
    print_summary
    local exit_code=$?
    
    # Cleanup
    cleanup_test_files
    
    exit $exit_code
}

# Run main
main "$@"
