#!/bin/bash
# Local Multi-Container Test for sai3-bench Distributed Mode
# 
# This script simulates distributed testing using Docker containers on a single machine.
# Useful for testing the distributed workflow before deploying to cloud VMs.
#
# Prerequisites:
# - Docker installed and running
# - sai3bench binaries built (cargo build --release)
#
# Usage:
#   ./local_docker_test.sh [--agent-count N] [--storage-type TYPE]

set -e

# ============================================================================
# Configuration
# ============================================================================

AGENT_COUNT=3
STORAGE_TYPE="file"  # file, minio (local S3), or path to cloud storage
DOCKER_IMAGE="sai3bench:latest"
CONTAINER_PREFIX="sai3bench-local-test"
WORKLOAD_CONFIG="local_test_workload.yaml"
NETWORK_NAME="sai3bench-test-net"

# MinIO settings (for local S3 testing)
MINIO_PORT=9000
MINIO_CONSOLE_PORT=9001
MINIO_ROOT_USER="minioadmin"
MINIO_ROOT_PASSWORD="minioadmin"
MINIO_BUCKET="test-bucket"

# Color output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info() { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $*"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*"; }

# ============================================================================
# Parse Arguments
# ============================================================================

parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            --agent-count)
                AGENT_COUNT="$2"
                shift 2
                ;;
            --storage-type)
                STORAGE_TYPE="$2"
                shift 2
                ;;
            --cleanup-only)
                cleanup_all
                exit 0
                ;;
            --help)
                show_help
                exit 0
                ;;
            *)
                log_error "Unknown option: $1"
                show_help
                exit 1
                ;;
        esac
    done
}

show_help() {
    cat << EOF
Local Docker-based Distributed Testing for sai3-bench

Usage: $0 [OPTIONS]

Options:
  --agent-count N       Number of agent containers (default: 3)
  --storage-type TYPE   Storage backend: file, minio, or URI (default: file)
  --cleanup-only        Clean up containers and exit
  --help                Show this help message

Examples:
  # Test with 3 agents using local filesystem
  $0

  # Test with 5 agents using MinIO (local S3)
  $0 --agent-count 5 --storage-type minio

  # Cleanup after testing
  $0 --cleanup-only

Storage Types:
  file      - Local filesystem (file://)
  minio     - Local S3-compatible storage
  <uri>     - Cloud storage (s3://, gs://, az://)
EOF
}

# ============================================================================
# Docker Network Setup
# ============================================================================

setup_network() {
    log_info "Setting up Docker network: $NETWORK_NAME"
    
    if docker network inspect "$NETWORK_NAME" &> /dev/null; then
        log_info "Network already exists"
    else
        docker network create "$NETWORK_NAME"
        log_success "Network created"
    fi
}

# ============================================================================
# MinIO Setup (Optional)
# ============================================================================

setup_minio() {
    log_info "Starting MinIO server..."
    
    docker run -d \
        --name "${CONTAINER_PREFIX}-minio" \
        --network "$NETWORK_NAME" \
        -p "${MINIO_PORT}:9000" \
        -p "${MINIO_CONSOLE_PORT}:9001" \
        -e "MINIO_ROOT_USER=${MINIO_ROOT_USER}" \
        -e "MINIO_ROOT_PASSWORD=${MINIO_ROOT_PASSWORD}" \
        minio/minio server /data --console-address ":9001" || {
        log_warn "MinIO container may already be running"
    }
    
    sleep 5
    
    # Create bucket
    log_info "Creating MinIO bucket: $MINIO_BUCKET"
    docker run --rm \
        --network "$NETWORK_NAME" \
        -e "MC_HOST_myminio=http://${MINIO_ROOT_USER}:${MINIO_ROOT_PASSWORD}@${CONTAINER_PREFIX}-minio:9000" \
        minio/mc mb "myminio/${MINIO_BUCKET}" || true
    
    log_success "MinIO ready at http://localhost:${MINIO_CONSOLE_PORT}"
    log_info "Login: ${MINIO_ROOT_USER} / ${MINIO_ROOT_PASSWORD}"
}

# ============================================================================
# Build Docker Image
# ============================================================================

build_image() {
    log_info "Checking for Docker image: $DOCKER_IMAGE"
    
    if docker image inspect "$DOCKER_IMAGE" &> /dev/null; then
        log_info "Image already exists"
        return 0
    fi
    
    if [[ ! -f "../Dockerfile" ]]; then
        log_warn "No Dockerfile found. You may need to build the image manually."
        log_info "Example Dockerfile:"
        cat << 'EOF'
FROM rust:1.75 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --bin sai3bench-agent

FROM ubuntu:22.04
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/sai3bench-agent /usr/local/bin/
CMD ["sai3bench-agent", "--listen", "0.0.0.0:7761"]
EOF
        return 1
    fi
    
    log_info "Building Docker image..."
    docker build -t "$DOCKER_IMAGE" ..
    log_success "Image built"
}

# ============================================================================
# Start Agent Containers
# ============================================================================

start_agents() {
    log_info "Starting $AGENT_COUNT agent containers..."
    
    for i in $(seq 1 $AGENT_COUNT); do
        local container_name="${CONTAINER_PREFIX}-agent-${i}"
        local port=$((7760 + i))
        
        log_info "Starting $container_name on port $port"
        
        docker run -d \
            --name "$container_name" \
            --network "$NETWORK_NAME" \
            -p "${port}:7761" \
            -e "RUST_LOG=info" \
            "$DOCKER_IMAGE" \
            sai3bench-agent --listen "0.0.0.0:7761" || {
            log_warn "Container $container_name may already be running"
        }
    done
    
    sleep 3
    log_success "All agents started"
}

# ============================================================================
# Generate Workload Config
# ============================================================================

generate_config() {
    log_info "Generating workload configuration..."
    
    # Determine storage URI
    local storage_uri
    case "$STORAGE_TYPE" in
        file)
            storage_uri="file:///tmp/sai3bench-test/"
            ;;
        minio)
            storage_uri="s3://${MINIO_BUCKET}/"
            ;;
        *)
            storage_uri="$STORAGE_TYPE"
            ;;
    esac
    
    # Build agent list
    local agents_yaml=""
    for i in $(seq 1 $AGENT_COUNT); do
        local port=$((7760 + i))
        agents_yaml="${agents_yaml}    - address: \"localhost:${port}\"\n"
        agents_yaml="${agents_yaml}      id: \"agent-${i}\"\n"
    done
    
    cat > "$WORKLOAD_CONFIG" << EOF
# Auto-generated local Docker test configuration
target: "${storage_uri}sai3bench-test/"
duration: 30s
concurrency: 16

# Distributed configuration
distributed:
  agents:
$(echo -e "$agents_yaml")
  
  ssh:
    enabled: false  # Using direct TCP connections
  
  start_delay: 2
  path_template: "agent-{id}/"

# Prepare test data
prepare:
  ensure_objects:
    - base_uri: "${storage_uri}sai3bench-test/data/"
      count: 100
      min_size: 1024
      max_size: 102400

# Workload
workload:
  - op: get
    path: "data/*"
    weight: 70
  
  - op: put
    path: "uploads/"
    weight: 20
    size_distribution:
      type: fixed
      size: 10240
  
  - op: list
    path: "data/"
    weight: 5
  
  - op: delete
    path: "uploads/*"
    weight: 5
EOF
    
    # Add MinIO credentials if needed
    if [[ "$STORAGE_TYPE" == "minio" ]]; then
        cat >> "$WORKLOAD_CONFIG" << EOF

# MinIO credentials (local S3)
# Set these environment variables before running:
# export AWS_ACCESS_KEY_ID=${MINIO_ROOT_USER}
# export AWS_SECRET_ACCESS_KEY=${MINIO_ROOT_PASSWORD}
# export AWS_ENDPOINT=http://localhost:${MINIO_PORT}
EOF
    fi
    
    log_success "Config generated: $WORKLOAD_CONFIG"
}

# ============================================================================
# Run Test
# ============================================================================

run_test() {
    log_info "Running distributed test..."
    
    # Set MinIO credentials if needed
    if [[ "$STORAGE_TYPE" == "minio" ]]; then
        export AWS_ACCESS_KEY_ID="$MINIO_ROOT_USER"
        export AWS_SECRET_ACCESS_KEY="$MINIO_ROOT_PASSWORD"
        export AWS_ENDPOINT="http://localhost:${MINIO_PORT}"
    fi
    
    ../target/release/sai3bench-ctl run \
        --config "$WORKLOAD_CONFIG" \
        --insecure || {
        log_error "Test failed"
        return 1
    }
    
    log_success "Test completed successfully"
}

# ============================================================================
# Cleanup
# ============================================================================

cleanup_all() {
    log_info "Cleaning up containers and network..."
    
    # Stop and remove agent containers
    for i in $(seq 1 $AGENT_COUNT); do
        local container_name="${CONTAINER_PREFIX}-agent-${i}"
        docker stop "$container_name" 2>/dev/null || true
        docker rm "$container_name" 2>/dev/null || true
    done
    
    # Stop and remove MinIO
    docker stop "${CONTAINER_PREFIX}-minio" 2>/dev/null || true
    docker rm "${CONTAINER_PREFIX}-minio" 2>/dev/null || true
    
    # Remove network
    docker network rm "$NETWORK_NAME" 2>/dev/null || true
    
    # Remove config file
    rm -f "$WORKLOAD_CONFIG"
    
    log_success "Cleanup complete"
}

trap cleanup_all EXIT

# ============================================================================
# Main
# ============================================================================

main() {
    log_info "=== Local Docker Distributed Test ==="
    log_info "Agents: $AGENT_COUNT"
    log_info "Storage: $STORAGE_TYPE"
    log_info ""
    
    # Setup
    setup_network
    
    if [[ "$STORAGE_TYPE" == "minio" ]]; then
        setup_minio
    fi
    
    # Build image (if needed)
    # build_image
    
    # Start agents
    start_agents
    
    # Generate config
    generate_config
    
    # Run test
    run_test
    
    log_success "=== Test Complete ==="
    log_info "Results saved to sai3-* directory"
}

# ============================================================================
# Entry Point
# ============================================================================

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    parse_args "$@"
    main
fi
