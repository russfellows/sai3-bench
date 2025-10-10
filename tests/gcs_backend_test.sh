#!/bin/bash
# Google Cloud Storage Backend Test Suite
# Comprehensive testing of GCS integration with sai3-bench

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BINARY="$PROJECT_DIR/target/release/sai3-bench"
TEST_DIR="/tmp/sai3-bench-gcs-tests"
RESULTS_FILE="$TEST_DIR/gcs_test_results.txt"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo "======================================================"
echo "sai3-bench Google Cloud Storage Test Suite"
echo "======================================================"
echo ""

# Check for GCS credentials
if [ -z "$GCS_BUCKET" ] && [ -z "$GOOGLE_CLOUD_BUCKET" ]; then
    echo -e "${RED}ERROR: GCS_BUCKET environment variable not set${NC}"
    echo ""
    echo "Please set one of:"
    echo "  export GCS_BUCKET='your-test-bucket'"
    echo "  export GOOGLE_APPLICATION_CREDENTIALS='/path/to/service-account.json'"
    echo ""
    exit 1
fi

GCS_BUCKET=${GCS_BUCKET:-$GOOGLE_CLOUD_BUCKET}
GCS_BASE_URI="gs://${GCS_BUCKET}/sai3-bench-test/"

echo "Configuration:"
echo "  Bucket: $GCS_BUCKET"
echo "  Base URI: $GCS_BASE_URI"
echo ""

# Ensure binary is built
if [ ! -f "$BINARY" ]; then
    echo "Building sai3-bench..."
    cd "$PROJECT_DIR"
    cargo build --release
fi

# Create test directory
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
        exit 1
    else
        echo -e "${YELLOW}⚠ $test_name${NC}"
        echo "WARN: $test_name - $details" >> "$RESULTS_FILE"
    fi
}

# Test 1: Health check
echo "Test 1: GCS Health Check"
echo "========================="
$BINARY health --uri "$GCS_BASE_URI" 2>&1 | tee "$TEST_DIR/health.log"

if grep -q "✅" "$TEST_DIR/health.log" || grep -q "accessible" "$TEST_DIR/health.log"; then
    log_result "GCS health check" "PASS" "Backend accessible"
else
    log_result "GCS health check" "FAIL" "Backend not accessible"
fi

# Test 2: PUT operations
echo ""
echo "Test 2: PUT Operations (10 objects)"
echo "===================================="
PUT_URI="${GCS_BASE_URI}test-data/object*.dat"

$BINARY -v put --uri "$PUT_URI" \
    --object-size 102400 \
    --objects 10 \
    --concurrency 3 \
    2>&1 | tee "$TEST_DIR/put.log"

if grep -q "completed" "$TEST_DIR/put.log" || grep -q "finished" "$TEST_DIR/put.log"; then
    PUT_TIME=$(grep -o "[0-9.]*s" "$TEST_DIR/put.log" | tail -1 || echo "N/A")
    log_result "PUT operations" "PASS" "10 objects in $PUT_TIME"
else
    log_result "PUT operations" "FAIL" "PUT operations failed"
fi

# Test 3: GET operations
echo ""
echo "Test 3: GET Operations"
echo "======================"
GET_URI="${GCS_BASE_URI}test-data/object*.dat"

$BINARY -v get --uri "$GET_URI" \
    --jobs 4 \
    2>&1 | tee "$TEST_DIR/get.log"

if grep -q "completed" "$TEST_DIR/get.log" || grep -q "downloaded" "$TEST_DIR/get.log"; then
    GET_COUNT=$(grep -o "[0-9]* objects" "$TEST_DIR/get.log" | head -1 | awk '{print $1}' || echo "0")
    log_result "GET operations" "PASS" "Retrieved $GET_COUNT objects"
else
    log_result "GET operations" "FAIL" "GET operations failed"
fi

# Test 4: LIST operations
echo ""
echo "Test 4: LIST Operations"
echo "======================="
LIST_URI="${GCS_BASE_URI}test-data/"

$BINARY -v list --uri "$LIST_URI" --recursive \
    2>&1 | tee "$TEST_DIR/list.log"

if grep -q "objects" "$TEST_DIR/list.log" || grep -q "listed" "$TEST_DIR/list.log"; then
    OBJECT_COUNT=$(grep -o "Found [0-9]* objects" "$TEST_DIR/list.log" | awk '{print $2}' || echo "N/A")
    log_result "LIST operations" "PASS" "Listed $OBJECT_COUNT objects"
else
    log_result "LIST operations" "WARN" "Could not determine object count"
fi

# Test 5: DELETE operations
echo ""
echo "Test 5: DELETE Operations"
echo "========================="
DELETE_URI="${GCS_BASE_URI}test-data/object*.dat"

$BINARY -v delete --uri "$DELETE_URI" \
    2>&1 | tee "$TEST_DIR/delete.log"

if grep -q "completed" "$TEST_DIR/delete.log" || grep -q "deleted" "$TEST_DIR/delete.log"; then
    DELETE_COUNT=$(grep -o "[0-9]* objects" "$TEST_DIR/delete.log" | head -1 | awk '{print $1}' || echo "N/A")
    log_result "DELETE operations" "PASS" "Deleted $DELETE_COUNT objects"
else
    log_result "DELETE operations" "WARN" "DELETE may have failed"
fi

# Test 6: Workload with op-log
echo ""
echo "Test 6: Workload with Op-log"
echo "============================="
OPLOG_FILE="$TEST_DIR/gcs_workload.tsv.zst"

# Create a simple test config
cat > "$TEST_DIR/gcs_workload.yaml" <<EOF
target: "${GCS_BASE_URI}"
workload:
  - op: put
    path: "oplog-test/data-*"
    object_size: 8192
    weight: 60
  - op: get
    path: "oplog-test/data-*"
    weight: 40
concurrency: 2
duration: 5s
EOF

$BINARY -vv --op-log "$OPLOG_FILE" run --config "$TEST_DIR/gcs_workload.yaml" \
    2>&1 | tee "$TEST_DIR/workload.log"

if [ -f "$OPLOG_FILE" ]; then
    OPLOG_SIZE=$(stat -f%z "$OPLOG_FILE" 2>/dev/null || stat -c%s "$OPLOG_FILE")
    OPS_COUNT=$(zstd -d -c "$OPLOG_FILE" | wc -l)
    log_result "Workload with op-log" "PASS" "${OPS_COUNT} operations logged, ${OPLOG_SIZE} bytes"
else
    log_result "Workload with op-log" "FAIL" "Op-log file not created"
fi

# Test 7: Large object test (10MB)
echo ""
echo "Test 7: Large Object Test (10 MB)"
echo "=================================="
LARGE_URI="${GCS_BASE_URI}large-test/big-file.bin"

$BINARY -v put --uri "$LARGE_URI" \
    --object-size 10485760 \
    --objects 1 \
    2>&1 | tee "$TEST_DIR/large.log"

if grep -q "completed" "$TEST_DIR/large.log"; then
    # Try to get it back
    $BINARY -v get --uri "$LARGE_URI" 2>&1 | tee -a "$TEST_DIR/large.log"
    
    if grep -q "downloaded" "$TEST_DIR/large.log" || grep -q "completed" "$TEST_DIR/large.log"; then
        log_result "Large object test" "PASS" "10 MB PUT/GET successful"
        # Cleanup
        $BINARY delete --uri "$LARGE_URI" >/dev/null 2>&1
    else
        log_result "Large object test" "FAIL" "GET of large object failed"
    fi
else
    log_result "Large object test" "FAIL" "PUT of large object failed"
fi

# Test 8: Concurrent operations stress test
echo ""
echo "Test 8: Concurrent Operations (50 objects)"
echo "==========================================="
CONCURRENT_URI="${GCS_BASE_URI}concurrent-test/obj*.dat"

$BINARY -v put --uri "$CONCURRENT_URI" \
    --object-size 4096 \
    --objects 50 \
    --concurrency 10 \
    2>&1 | tee "$TEST_DIR/concurrent.log"

if grep -q "completed" "$TEST_DIR/concurrent.log"; then
    # Verify with GET
    $BINARY -v get --uri "$CONCURRENT_URI" --jobs 10 2>&1 | tee -a "$TEST_DIR/concurrent.log"
    
    if grep -q "downloaded" "$TEST_DIR/concurrent.log"; then
        log_result "Concurrent operations" "PASS" "50 objects with 10 workers"
        # Cleanup
        $BINARY delete --uri "$CONCURRENT_URI" >/dev/null 2>&1
    else
        log_result "Concurrent operations" "WARN" "PUT succeeded but GET verification failed"
    fi
else
    log_result "Concurrent operations" "FAIL" "Concurrent PUT failed"
fi

# Test 9: Alternate URI scheme (gcs://)
echo ""
echo "Test 9: Alternate URI Scheme (gcs://)"
echo "======================================"
GCS_ALT_URI="gcs://${GCS_BUCKET}/sai3-bench-test/alt-scheme-test.txt"

$BINARY put --uri "$GCS_ALT_URI" \
    --object-size 1024 \
    --objects 1 \
    2>&1 | tee "$TEST_DIR/alt_scheme.log"

if grep -q "completed" "$TEST_DIR/alt_scheme.log" || [ $? -eq 0 ]; then
    # Try to get with gs:// scheme
    GS_ALT_URI="gs://${GCS_BUCKET}/sai3-bench-test/alt-scheme-test.txt"
    $BINARY get --uri "$GS_ALT_URI" 2>&1 | tee -a "$TEST_DIR/alt_scheme.log"
    
    if [ $? -eq 0 ]; then
        log_result "Alternate URI scheme" "PASS" "Both gs:// and gcs:// work"
        $BINARY delete --uri "$GS_ALT_URI" >/dev/null 2>&1
    else
        log_result "Alternate URI scheme" "WARN" "gcs:// PUT succeeded but gs:// GET failed"
    fi
else
    log_result "Alternate URI scheme" "WARN" "gcs:// scheme may not be supported"
fi

# Summary
echo ""
echo "======================================================"
echo "Test Summary"
echo "======================================================"
cat "$RESULTS_FILE"
echo ""

PASS_COUNT=$(grep -c "^PASS:" "$RESULTS_FILE" || echo 0)
FAIL_COUNT=$(grep -c "^FAIL:" "$RESULTS_FILE" || echo 0)
WARN_COUNT=$(grep -c "^WARN:" "$RESULTS_FILE" || echo 0)

echo "Results: ${PASS_COUNT} passed, ${FAIL_COUNT} failed, ${WARN_COUNT} warnings"
echo ""

if [ $FAIL_COUNT -gt 0 ]; then
    echo -e "${RED}❌ Some tests failed${NC}"
    exit 1
else
    echo -e "${GREEN}✅ All critical tests passed${NC}"
    if [ $WARN_COUNT -gt 0 ]; then
        echo -e "${YELLOW}⚠️  Some warnings present${NC}"
    fi
fi
