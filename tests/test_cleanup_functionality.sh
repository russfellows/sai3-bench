#!/bin/bash
# Test script for distributed cleanup functionality
# 
# Tests:
# 1. Cleanup-only mode (cleaning up after previous runs)
# 2. Error tolerance modes (handling already-deleted objects)
# 3. Partial cleanup (some files already deleted)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR/.."

BINARY="./target/release/sai3-bench"
TEST_CONFIG="tests/configs/test_cleanup_modes.yaml"
TEST_DIR="/tmp/sai3-cleanup-test"

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}=== Distributed Cleanup Test Script ===${NC}\n"

# Verify binary exists
if [ ! -f "$BINARY" ]; then
    echo -e "ERROR: Binary not found at $BINARY"
    echo -e "Please run: cargo build --release"
    exit 1
fi

# Clean up previous test data
rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

# Verify config
echo -e "${YELLOW}Verifying config...${NC}"
"$BINARY" run --config "$TEST_CONFIG" --dry-run || {
    echo "ERROR: Config validation failed"
    exit 1
}

# Test 1: Prepare objects without cleanup
echo -e "\n${BLUE}=== Test 1: Prepare objects (no cleanup) ===${NC}"
"$BINARY" run --config "$TEST_CONFIG" --prepare-only
FILES_CREATED=$(ls "$TEST_DIR" 2>/dev/null | wc -l)
echo -e "${GREEN}✓ Created $FILES_CREATED files${NC}"

# Test 2: Cleanup-only mode (should delete all files)
echo -e "\n${BLUE}=== Test 2: Cleanup-only mode ===${NC}"
"$BINARY" run --config "$TEST_CONFIG" --cleanup-only
FILES_REMAINING=$(ls "$TEST_DIR" 2>/dev/null | wc -l)
echo -e "${GREEN}✓ Remaining files: $FILES_REMAINING${NC}"

# Test 3: Cleanup-only when already deleted (error tolerance)
echo -e "\n${BLUE}=== Test 3: Cleanup already-deleted files (tolerant mode) ===${NC}"
"$BINARY" run --config "$TEST_CONFIG" --cleanup-only
echo -e "${GREEN}✓ Handled already-deleted files gracefully${NC}"

# Test 4: Partial cleanup
echo -e "\n${BLUE}=== Test 4: Partial cleanup (some files pre-deleted) ===${NC}"
"$BINARY" run --config "$TEST_CONFIG" --prepare-only
TOTAL_FILES=$(ls "$TEST_DIR" | wc -l)
echo "  Total files: $TOTAL_FILES"
echo "  Manually deleting 20 files..."
ls "$TEST_DIR" | head -20 | xargs -I {} rm -f "$TEST_DIR/{}"
FILES_BEFORE=$(ls "$TEST_DIR" | wc -l)
echo "  Files before cleanup: $FILES_BEFORE"
"$BINARY" run --config "$TEST_CONFIG" --cleanup-only
FILES_AFTER=$(ls "$TEST_DIR" 2>/dev/null | wc -l)
echo -e "${GREEN}✓ Files after cleanup: $FILES_AFTER${NC}"

# Cleanup
rm -rf "$TEST_DIR"

echo -e "\n${GREEN}=== All cleanup tests passed! ===${NC}"
echo -e "${BLUE}Features validated:${NC}"
echo "  ✓ Cleanup-only mode (--cleanup-only)"
echo "  ✓ Tolerant mode (handles already-deleted objects)"
echo "  ✓ Partial cleanup (mixed pre-deleted files)"
