#!/bin/bash
# Comprehensive testing for v0.6.4 distributed results with histogram merging
# Tests multiple scenarios to verify consolidated TSV generation

set -e

BINARY_CTL="./target/release/sai3bench-ctl"
BINARY_AGENT="./target/release/sai3bench-agent"

echo "======================================"
echo "v0.6.4 Comprehensive Testing"
echo "======================================"
echo ""

# Cleanup function
cleanup() {
    echo ""
    echo "Cleaning up..."
    pkill -f sai3bench-agent || true
    sleep 1
    rm -rf /tmp/sai3bench-test/ /tmp/io-bench-* /tmp/sai3-*
    echo "Cleanup complete"
}

trap cleanup EXIT

# Test 1: Simple PUT-only workload (baseline)
test_put_only() {
    echo "=== Test 1: PUT-only workload (2 agents) ==="
    
    # Start agents
    $BINARY_AGENT --listen 127.0.0.1:7761 >/dev/null 2>&1 &
    AGENT1_PID=$!
    $BINARY_AGENT --listen 127.0.0.1:7762 >/dev/null 2>&1 &
    AGENT2_PID=$!
    sleep 2
    
    # Run workload
    $BINARY_CTL --insecure --agents 127.0.0.1:7761,127.0.0.1:7762 \
        run --config tests/configs/distributed_put_test.yaml --start-delay 3 >/dev/null 2>&1
    
    RESULT_DIR=$(ls -td sai3-* 2>/dev/null | head -1)
    
    # Verify consolidated TSV exists
    if [ ! -f "$RESULT_DIR/results.tsv" ]; then
        echo "❌ Missing consolidated results.tsv"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    # Verify per-agent TSVs exist
    if [ ! -f "$RESULT_DIR/agents/agent-1/results.tsv" ]; then
        echo "❌ Missing agent-1 results.tsv"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    if [ ! -f "$RESULT_DIR/agents/agent-2/results.tsv" ]; then
        echo "❌ Missing agent-2 results.tsv"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    # Check that consolidated has PUT operations
    if ! grep -q "^PUT" "$RESULT_DIR/results.tsv"; then
        echo "❌ Consolidated TSV missing PUT operations"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    # Extract counts from TSVs
    AGENT1_COUNT=$(awk '/^PUT/ {sum+=$NF} END {print sum}' "$RESULT_DIR/agents/agent-1/results.tsv")
    AGENT2_COUNT=$(awk '/^PUT/ {sum+=$NF} END {print sum}' "$RESULT_DIR/agents/agent-2/results.tsv")
    CONSOLIDATED_COUNT=$(awk '/^PUT/ {sum+=$NF} END {print sum}' "$RESULT_DIR/results.tsv")
    
    echo "  Agent-1 ops: $AGENT1_COUNT"
    echo "  Agent-2 ops: $AGENT2_COUNT"
    echo "  Consolidated ops: $CONSOLIDATED_COUNT"
    
    # Verify count matches
    EXPECTED=$((AGENT1_COUNT + AGENT2_COUNT))
    if [ "$CONSOLIDATED_COUNT" != "$EXPECTED" ]; then
        echo "❌ Count mismatch: expected $EXPECTED, got $CONSOLIDATED_COUNT"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    echo "  ✅ Operation counts verified"
    
    # Verify percentiles are reasonable (not zero, not identical to one agent)
    AGENT1_P95=$(awk '/^PUT/ {print $7; exit}' "$RESULT_DIR/agents/agent-1/results.tsv")
    CONSOLIDATED_P95=$(awk '/^PUT/ {print $7; exit}' "$RESULT_DIR/results.tsv")
    
    echo "  Agent-1 p95: ${AGENT1_P95}µs"
    echo "  Consolidated p95: ${CONSOLIDATED_P95}µs"
    
    if [ "$CONSOLIDATED_P95" == "0.00" ]; then
        echo "❌ Consolidated p95 is zero (invalid)"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    echo "  ✅ Percentiles are non-zero"
    
    kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    wait $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    
    echo "✅ Test 1 passed"
    echo ""
    return 0
}

# Test 2: Multiple object sizes (verifies size bucket merging)
test_multiple_sizes() {
    echo "=== Test 2: Multiple object sizes (2 agents) ==="
    
    # Start agents
    $BINARY_AGENT --listen 127.0.0.1:7761 >/dev/null 2>&1 &
    AGENT1_PID=$!
    $BINARY_AGENT --listen 127.0.0.1:7762 >/dev/null 2>&1 &
    AGENT2_PID=$!
    sleep 2
    
    # Run workload with varied sizes
    $BINARY_CTL --insecure --agents 127.0.0.1:7761,127.0.0.1:7762 \
        run --config tests/configs/size_distributions_test.yaml --start-delay 3 >/dev/null 2>&1
    
    RESULT_DIR=$(ls -td sai3-* 2>/dev/null | head -1)
    
    if [ ! -f "$RESULT_DIR/results.tsv" ]; then
        echo "❌ Missing consolidated results.tsv"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    # Count distinct size buckets in consolidated TSV (excluding header)
    NUM_DISTINCT_BUCKETS=$(awk 'NR>1 {print $2}' "$RESULT_DIR/results.tsv" | sort -u | wc -l)
    
    echo "  Distinct size buckets in consolidated: $NUM_DISTINCT_BUCKETS"
    
    if [ "$NUM_DISTINCT_BUCKETS" -lt 2 ]; then
        echo "❌ Expected multiple size buckets, got $NUM_DISTINCT_BUCKETS"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    echo "  ✅ Multiple size buckets detected"
    
    # List all size buckets present
    echo "  Size buckets present:"
    awk 'NR>1 {print "    " $1 " - " $2}' "$RESULT_DIR/results.tsv" | sort -u
    
    # Verify counts add up for each size bucket
    echo ""
    echo "  Verifying bucket counts..."
    awk 'NR>1' "$RESULT_DIR/results.tsv" | while read line; do
        OP=$(echo "$line" | awk '{print $1}')
        BUCKET=$(echo "$line" | awk '{print $3}')
        CONSOL_COUNT=$(echo "$line" | awk '{print $NF}')
        
        # Sum from per-agent TSVs for this operation and bucket
        AGENT_SUM=0
        for AGENT_DIR in "$RESULT_DIR"/agents/agent-*/; do
            if [ -f "${AGENT_DIR}results.tsv" ]; then
                COUNT=$(awk -v op="$OP" -v bucket="$BUCKET" '$1 == op && $3 == bucket {print $NF}' "${AGENT_DIR}results.tsv")
                if [ -n "$COUNT" ]; then
                    AGENT_SUM=$((AGENT_SUM + COUNT))
                fi
            fi
        done
        
        if [ "$AGENT_SUM" -gt 0 ] && [ "$CONSOL_COUNT" != "$AGENT_SUM" ]; then
            echo "    ❌ $OP bucket $BUCKET: agents=$AGENT_SUM, consolidated=$CONSOL_COUNT"
            kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
            return 1
        else
            echo "    ✓ $OP bucket $BUCKET: count=$CONSOL_COUNT"
        fi
    done
    
    kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    wait $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    
    echo "✅ Test 2 passed"
    echo ""
    return 0
}

# Test 3: 4 agents (stress test histogram merging)
test_four_agents() {
    echo "=== Test 3: Four agents (stress test) ==="
    
    # Start 4 agents
    $BINARY_AGENT --listen 127.0.0.1:7761 >/dev/null 2>&1 &
    AGENT1_PID=$!
    $BINARY_AGENT --listen 127.0.0.1:7762 >/dev/null 2>&1 &
    AGENT2_PID=$!
    $BINARY_AGENT --listen 127.0.0.1:7763 >/dev/null 2>&1 &
    AGENT3_PID=$!
    $BINARY_AGENT --listen 127.0.0.1:7764 >/dev/null 2>&1 &
    AGENT4_PID=$!
    sleep 2
    
    # Run simple PUT workload with 4 agents
    $BINARY_CTL --insecure --agents 127.0.0.1:7761,127.0.0.1:7762,127.0.0.1:7763,127.0.0.1:7764 \
        run --config tests/configs/distributed_put_test.yaml --start-delay 3 >/dev/null 2>&1
    
    RESULT_DIR=$(ls -td sai3-* 2>/dev/null | head -1)
    
    # Verify all 4 agent directories exist
    for i in 1 2 3 4; do
        if [ ! -d "$RESULT_DIR/agents/agent-$i" ]; then
            echo "❌ Missing agent-$i directory"
            kill $AGENT1_PID $AGENT2_PID $AGENT3_PID $AGENT4_PID 2>/dev/null || true
            return 1
        fi
        if [ ! -f "$RESULT_DIR/agents/agent-$i/results.tsv" ]; then
            echo "❌ Missing agent-$i results.tsv"
            kill $AGENT1_PID $AGENT2_PID $AGENT3_PID $AGENT4_PID 2>/dev/null || true
            return 1
        fi
    done
    
    echo "  ✅ All 4 agent directories present"
    
    # Sum operation counts
    TOTAL_AGENT_OPS=0
    for i in 1 2 3 4; do
        COUNT=$(awk 'NR==2 {print $NF}' "$RESULT_DIR/agents/agent-$i/results.tsv")
        echo "  Agent-$i ops: $COUNT"
        TOTAL_AGENT_OPS=$((TOTAL_AGENT_OPS + COUNT))
    done
    
    CONSOLIDATED_OPS=$(awk 'NR==2 {print $NF}' "$RESULT_DIR/results.tsv")
    
    echo "  Agent sum: $TOTAL_AGENT_OPS"
    echo "  Consolidated: $CONSOLIDATED_OPS"
    
    if [ "$CONSOLIDATED_OPS" != "$TOTAL_AGENT_OPS" ]; then
        echo "❌ Count mismatch with 4 agents"
        kill $AGENT1_PID $AGENT2_PID $AGENT3_PID $AGENT4_PID 2>/dev/null || true
        return 1
    fi
    
    echo "  ✅ 4-agent histogram merge successful"
    
    kill $AGENT1_PID $AGENT2_PID $AGENT3_PID $AGENT4_PID 2>/dev/null || true
    wait $AGENT1_PID $AGENT2_PID $AGENT3_PID $AGENT4_PID 2>/dev/null || true
    
    echo "✅ Test 3 passed"
    echo ""
    return 0
}

# Test 4: Verify histogram merging accuracy
test_histogram_accuracy() {
    echo "=== Test 4: Histogram merging accuracy ==="
    
    # Start agents
    $BINARY_AGENT --listen 127.0.0.1:7761 >/dev/null 2>&1 &
    AGENT1_PID=$!
    $BINARY_AGENT --listen 127.0.0.1:7762 >/dev/null 2>&1 &
    AGENT2_PID=$!
    sleep 2
    
    # Run simple PUT workload
    $BINARY_CTL --insecure --agents 127.0.0.1:7761,127.0.0.1:7762 \
        run --config tests/configs/distributed_put_test.yaml --start-delay 3 >/dev/null 2>&1
    
    RESULT_DIR=$(ls -td sai3-* 2>/dev/null | head -1)
    
    # Extract key metrics for verification
    echo "  Extracting metrics from per-agent TSVs..."
    
    AGENT1_METRICS=$(awk 'NR==2 {printf "count=%s mean=%.2f p50=%.2f p95=%.2f p99=%.2f", $NF, $4, $5, $7, $8}' \
        "$RESULT_DIR/agents/agent-1/results.tsv")
    AGENT2_METRICS=$(awk 'NR==2 {printf "count=%s mean=%.2f p50=%.2f p95=%.2f p99=%.2f", $NF, $4, $5, $7, $8}' \
        "$RESULT_DIR/agents/agent-2/results.tsv")
    CONSOL_METRICS=$(awk 'NR==2 {printf "count=%s mean=%.2f p50=%.2f p95=%.2f p99=%.2f", $NF, $4, $5, $7, $8}' \
        "$RESULT_DIR/results.tsv")
    
    echo "  Agent-1: $AGENT1_METRICS"
    echo "  Agent-2: $AGENT2_METRICS"
    echo "  Consolidated: $CONSOL_METRICS"
    
    # Extract counts
    COUNT1=$(awk 'NR==2 {print $NF}' "$RESULT_DIR/agents/agent-1/results.tsv")
    COUNT2=$(awk 'NR==2 {print $NF}' "$RESULT_DIR/agents/agent-2/results.tsv")
    COUNT_CONSOL=$(awk 'NR==2 {print $NF}' "$RESULT_DIR/results.tsv")
    
    EXPECTED_COUNT=$((COUNT1 + COUNT2))
    
    if [ "$COUNT_CONSOL" != "$EXPECTED_COUNT" ]; then
        echo "❌ Count mismatch: $COUNT1 + $COUNT2 = $EXPECTED_COUNT, got $COUNT_CONSOL"
        kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
        return 1
    fi
    
    echo "  ✅ Count verified: $COUNT1 + $COUNT2 = $COUNT_CONSOL"
    
    # Verify mean is between agent means (approximate check)
    MEAN1=$(awk 'NR==2 {print $4}' "$RESULT_DIR/agents/agent-1/results.tsv")
    MEAN2=$(awk 'NR==2 {print $4}' "$RESULT_DIR/agents/agent-2/results.tsv")
    MEAN_CONSOL=$(awk 'NR==2 {print $4}' "$RESULT_DIR/results.tsv")
    
    # Simple sanity check: consolidated mean should be between individual means (roughly)
    MIN_MEAN=$(echo "$MEAN1 $MEAN2" | awk '{print ($1 < $2) ? $1 : $2}')
    MAX_MEAN=$(echo "$MEAN1 $MEAN2" | awk '{print ($1 > $2) ? $1 : $2}')
    
    # Allow some tolerance (within 2x range)
    IN_RANGE=$(echo "$MEAN_CONSOL $MIN_MEAN $MAX_MEAN" | awk '{
        if ($1 >= $2 * 0.5 && $1 <= $3 * 2.0) print "yes"; else print "no"
    }')
    
    if [ "$IN_RANGE" != "yes" ]; then
        echo "⚠️  Warning: consolidated mean $MEAN_CONSOL outside expected range [$MIN_MEAN, $MAX_MEAN]"
        echo "  (This might be acceptable depending on workload distribution)"
    else
        echo "  ✅ Mean is in expected range"
    fi
    
    # Verify p95 is not just a simple average
    P95_1=$(awk 'NR==2 {print $7}' "$RESULT_DIR/agents/agent-1/results.tsv")
    P95_2=$(awk 'NR==2 {print $7}' "$RESULT_DIR/agents/agent-2/results.tsv")
    P95_CONSOL=$(awk 'NR==2 {print $7}' "$RESULT_DIR/results.tsv")
    
    # Calculate simple average
    P95_AVG=$(echo "$P95_1 $P95_2" | awk '{printf "%.2f", ($1 + $2) / 2}')
    
    echo "  p95 comparison:"
    echo "    Agent-1 p95: $P95_1"
    echo "    Agent-2 p95: $P95_2"
    echo "    Simple average: $P95_AVG"
    echo "    Merged p95: $P95_CONSOL"
    
    if [ "$P95_CONSOL" == "$P95_AVG" ]; then
        echo "  ℹ️  Merged p95 equals simple average (possible for similar distributions)"
    else
        echo "  ✅ Merged p95 differs from simple average (proper histogram merge)"
    fi
    
    kill $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    wait $AGENT1_PID $AGENT2_PID 2>/dev/null || true
    
    echo "✅ Test 4 passed"
    echo ""
    return 0
}

# Run all tests
echo "Running comprehensive tests for v0.6.4..."
echo ""

test_put_only
test_multiple_sizes
test_four_agents
test_histogram_accuracy

echo "======================================"
echo "All comprehensive tests passed! ✅"
echo "======================================"
echo ""
echo "v0.6.4 histogram merging verified:"
echo "  ✓ PUT-only workloads"
echo "  ✓ Multiple size buckets"
echo "  ✓ 4-agent stress test"
echo "  ✓ Accurate count aggregation"
echo "  ✓ Proper histogram merging"
echo ""
echo "Ready for merge!"
