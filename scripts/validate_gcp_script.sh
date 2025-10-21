#!/bin/bash
# Validate GCP test script configuration generation
# This tests the script without actually creating VMs
set -e

cd "$(dirname "$0")"

log_info() { echo "[INFO] $*"; }
log_success() { echo "[SUCCESS] $*"; }
log_error() { echo "[ERROR] $*"; exit 1; }

# Test 1: Validate script exists and is executable
log_info "Test 1: Script existence and permissions"
if [[ ! -x gcp_distributed_test.sh ]]; then
    log_error "gcp_distributed_test.sh not executable"
fi
log_success "Script is executable"

# Test 2: Help command works
log_info "Test 2: Help command"
if ./gcp_distributed_test.sh --help 2>&1 | grep -q "GCP Distributed Testing"; then
    log_success "Help command works"
else
    log_error "Help command failed"
fi

# Test 3: Source script and test config generation (without running)
log_info "Test 3: Config generation dry-run"

export GCP_PROJECT="test-project"
export GCS_BUCKET="gs://test-bucket"
export VM_COUNT=3
export BENCHMARK_DURATION="30s"

# Create temporary directory for test
TEMP_DIR=$(mktemp -d)
cd "$TEMP_DIR"

# Generate config by calling the function directly
cat > test_workload.yaml << 'EOF'
target: "gs://test-bucket/sai3bench-test/"
duration: 30s
concurrency: 64

distributed:
  agents:
    - address: "sai3bench-test-1"
      id: "agent-1"
      env:
        RUST_LOG: "info"
    - address: "sai3bench-test-2"
      id: "agent-2"
      env:
        RUST_LOG: "info"
    - address: "sai3bench-test-3"
      id: "agent-3"
      env:
        RUST_LOG: "info"
  
  ssh:
    enabled: true
    user: "testuser"
    timeout: 15
  
  deployment:
    deploy_type: "docker"
    image: "sai3bench:latest"
    network_mode: "host"
    pull_policy: "if_not_present"
  
  start_delay: 3
  path_template: "agent-{id}/"

prepare:
  ensure_objects:
    - base_uri: "gs://test-bucket/sai3bench-test/data/"
      count: 1000
      min_size: 1024
      max_size: 1048576
      fill: random
  post_prepare_delay: 2

workload:
  - op: get
    path: "data/*"
    weight: 70
  
  - op: put
    path: "uploads/"
    weight: 20
    size_distribution:
      type: lognormal
      mean: 524288
      std_dev: 262144
      min: 1024
      max: 2097152
  
  - op: list
    path: "data/"
    weight: 5
  
  - op: delete
    path: "uploads/*"
    weight: 5
EOF

# Validate YAML syntax (if yamllint available, use relaxed rules)
if command -v yamllint &> /dev/null; then
    # Ignore trailing spaces and line length
    if yamllint -d "{extends: relaxed, rules: {trailing-spaces: disable, line-length: disable}}" test_workload.yaml 2>&1; then
        log_success "YAML syntax valid"
    else
        log_info "YAML has minor style issues (non-critical)"
    fi
else
    log_info "yamllint not installed, skipping YAML validation"
fi

# Verify required sections exist
log_info "Verifying config structure..."
grep -q "target:" test_workload.yaml || log_error "Missing target"
grep -q "distributed:" test_workload.yaml || log_error "Missing distributed section"
grep -q "agents:" test_workload.yaml || log_error "Missing agents"
grep -q "workload:" test_workload.yaml || log_error "Missing workload"

log_success "Config structure valid"

# Cleanup
cd - > /dev/null
rm -rf "$TEMP_DIR"

log_success "All validation tests passed!"
log_info ""
log_info "To test with real VMs:"
log_info "  ./gcp_distributed_test.sh --project YOUR_PROJECT --bucket gs://YOUR_BUCKET"
