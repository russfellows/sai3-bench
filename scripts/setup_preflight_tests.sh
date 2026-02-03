#!/bin/bash
# Setup script for pre-flight validation failure tests
# Creates directories with various permission issues

set -e

echo "========================================="
echo "Setting up pre-flight validation tests"
echo "========================================="
echo ""

# Test 1: Read-only directory (user can read but not write)
echo "1. Creating read-only test directory..."
sudo mkdir -p /tmp/sai3-readonly-test
sudo chown $USER:$USER /tmp/sai3-readonly-test
sudo chmod 555 /tmp/sai3-readonly-test
echo "   ✅ /tmp/sai3-readonly-test (read-only, owned by $USER)"

# Test 2: Root-owned directory (writable but owned by root)
echo "2. Creating root-owned test directory..."
sudo mkdir -p /tmp/sai3-root-test
sudo chown root:root /tmp/sai3-root-test
sudo chmod 777 /tmp/sai3-root-test
echo "   ✅ /tmp/sai3-root-test (world-writable, owned by root)"

# Test 3: Non-existent directory in non-writable parent
echo "3. Creating non-writable parent directory..."
sudo mkdir -p /tmp/sai3-locked
sudo chown root:root /tmp/sai3-locked
sudo chmod 555 /tmp/sai3-locked
echo "   ✅ /tmp/sai3-locked (read-only parent, owned by root)"

echo ""
echo "========================================="
echo "Test directories created successfully!"
echo "========================================="
echo ""
echo "Now you can run the pre-flight validation tests:"
echo "  1. Non-existent (should FAIL - cannot create in locked parent):"
echo "     ./target/release/sai3-bench run --config tests/configs/preflight_test_nonexistent_dir.yaml"
echo ""
echo "  2. Read-only (should FAIL - cannot write):"
echo "     ./target/release/sai3-bench run --config tests/configs/preflight_test_readonly_dir.yaml"
echo ""
echo "  3. Root-owned (should WARN - ownership mismatch):"
echo "     ./target/release/sai3-bench run --config tests/configs/preflight_test_root_owned_dir.yaml"
echo ""
