#!/bin/bash
# Cloud-Agnostic Distributed Testing Template for sai3-bench
#
# This is a generic template that can be adapted for any cloud provider.
# Customize the CLOUD_* functions for your specific environment.
#
# Usage:
#   1. Copy this template to your environment-specific script
#   2. Implement the cloud_* functions for your provider
#   3. Run the script with appropriate arguments

set -e

# ============================================================================
# Configuration
# ============================================================================

# Test Settings
VM_COUNT=3
VM_PREFIX="sai3bench-test"
WORKLOAD_CONFIG="workload.yaml"
STORAGE_URI=""  # e.g., s3://bucket, gs://bucket, az://container
CLEANUP_AFTER_TEST=false

# Docker Settings
DOCKER_IMAGE="sai3bench:latest"

# Benchmark Settings
BENCHMARK_DURATION="60s"
BENCHMARK_CONCURRENCY=64

# ============================================================================
# Color output
# ============================================================================

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
# Cloud Provider Interface - CUSTOMIZE THESE FOR YOUR CLOUD
# ============================================================================

# Create VMs in your cloud environment
cloud_create_vms() {
    log_error "cloud_create_vms() not implemented"
    log_info "Implement this function to create $VM_COUNT VMs"
    log_info "Each VM should:"
    log_info "  - Be named ${VM_PREFIX}-\$i (i=1..$VM_COUNT)"
    log_info "  - Have Docker installed"
    log_info "  - Be accessible via SSH"
    exit 1
}

# Wait for VMs to be ready
cloud_wait_for_vms() {
    log_info "Waiting for VMs to be ready..."
    
    local max_wait=300
    local elapsed=0
    
    while [[ $elapsed -lt $max_wait ]]; do
        if cloud_check_vms_ready; then
            log_success "All VMs ready"
            return 0
        fi
        sleep 10
        ((elapsed+=10))
    done
    
    log_error "Timeout waiting for VMs"
    return 1
}

# Check if all VMs are ready (return 0 if ready, 1 if not)
cloud_check_vms_ready() {
    log_error "cloud_check_vms_ready() not implemented"
    return 1
}

# Get VM hostnames/IPs for SSH access
cloud_get_vm_addresses() {
    log_error "cloud_get_vm_addresses() not implemented"
    log_info "Implement this function to populate VM_ADDRESSES array"
    log_info "Example: VM_ADDRESSES=(\"vm1.example.com\" \"vm2.example.com\")"
    exit 1
}

# Execute SSH command on a VM
cloud_ssh_exec() {
    local vm_name="$1"
    local command="$2"
    
    log_error "cloud_ssh_exec() not implemented"
    log_info "Implement SSH execution for: $vm_name"
    log_info "Command: $command"
    exit 1
}

# Copy file to VM
cloud_scp_to_vm() {
    local local_file="$1"
    local vm_name="$2"
    local remote_path="$3"
    
    log_error "cloud_scp_to_vm() not implemented"
    log_info "Implement file copy to: $vm_name:$remote_path"
    exit 1
}

# Delete VMs
cloud_delete_vms() {
    log_info "Deleting VMs..."
    
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        log_info "Deleting $vm_name"
        # Implement VM deletion here
    done
}

# ============================================================================
# Example Implementations for Common Clouds
# ============================================================================

# Example: AWS EC2 implementation
example_aws_create_vms() {
    local ami="ami-0c55b159cbfafe1f0"  # Ubuntu 22.04 in us-east-1
    local instance_type="t3.medium"
    local key_name="my-keypair"
    
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        aws ec2 run-instances \
            --image-id "$ami" \
            --instance-type "$instance_type" \
            --key-name "$key_name" \
            --user-data file://install_docker.sh \
            --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$vm_name}]"
    done
}

example_aws_ssh_exec() {
    local vm_name="$1"
    local command="$2"
    
    ssh -i ~/.ssh/my-keypair.pem "ubuntu@$vm_name" "$command"
}

# Example: Azure implementation
example_azure_create_vms() {
    local resource_group="sai3bench-rg"
    local location="eastus"
    local vm_size="Standard_D2s_v3"
    
    az group create --name "$resource_group" --location "$location"
    
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        az vm create \
            --resource-group "$resource_group" \
            --name "$vm_name" \
            --image "UbuntuLTS" \
            --size "$vm_size" \
            --admin-username "azureuser" \
            --generate-ssh-keys \
            --custom-data install_docker.sh
    done
}

# Example: GCP implementation (see gcp_distributed_test.sh for complete version)
example_gcp_create_vms() {
    local project="my-project"
    local zone="us-central1-a"
    
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        gcloud compute instances create "$vm_name" \
            --project="$project" \
            --zone="$zone" \
            --machine-type="n2-standard-4" \
            --image-family="ubuntu-2204-lts" \
            --image-project="ubuntu-os-cloud" \
            --metadata-from-file=startup-script=install_docker.sh
    done
}

# ============================================================================
# Docker Setup
# ============================================================================

setup_docker_on_vms() {
    log_info "Setting up Docker on VMs..."
    
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        
        log_info "Installing Docker on $vm_name..."
        cloud_ssh_exec "$vm_name" "curl -fsSL https://get.docker.com | sudo sh"
        cloud_ssh_exec "$vm_name" "sudo usermod -aG docker \$(whoami)"
    done
    
    log_success "Docker installed on all VMs"
}

# ============================================================================
# Workload Configuration
# ============================================================================

generate_workload_config() {
    log_info "Generating workload configuration..."
    
    if [[ -z "$STORAGE_URI" ]]; then
        log_error "Storage URI not specified (use --storage-uri)"
        exit 1
    fi
    
    # Build agent list
    local agents_yaml=""
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        agents_yaml="${agents_yaml}    - address: \"${vm_name}\"\n"
        agents_yaml="${agents_yaml}      id: \"agent-${i}\"\n"
    done
    
    cat > "$WORKLOAD_CONFIG" << EOF
# Auto-generated distributed workload
target: "${STORAGE_URI}/sai3bench-test/"
duration: ${BENCHMARK_DURATION}
concurrency: ${BENCHMARK_CONCURRENCY}

distributed:
  agents:
$(echo -e "$agents_yaml")
  
  ssh:
    enabled: true
    timeout: 15
  
  deployment:
    deploy_type: "docker"
    image: "${DOCKER_IMAGE}"
    network_mode: "host"
  
  start_delay: 3

# Prepare test data
prepare:
  ensure_objects:
    - base_uri: "${STORAGE_URI}/sai3bench-test/data/"
      count: 1000
      min_size: 1024
      max_size: 1048576

# Workload operations
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
  
  - op: list
    path: "data/"
    weight: 5
  
  - op: delete
    path: "uploads/*"
    weight: 5
EOF
    
    log_success "Config generated: $WORKLOAD_CONFIG"
}

# ============================================================================
# Benchmark Execution
# ============================================================================

run_benchmark() {
    log_info "Running distributed benchmark..."
    
    ../target/release/sai3bench-ctl run --config "$WORKLOAD_CONFIG" || {
        log_error "Benchmark failed"
        return 1
    }
    
    log_success "Benchmark completed"
}

# ============================================================================
# Main Workflow
# ============================================================================

main() {
    log_info "=== Cloud Distributed Testing for sai3-bench ==="
    log_info "VMs: $VM_COUNT"
    log_info "Storage: $STORAGE_URI"
    
    # 1. Create VMs
    cloud_create_vms
    
    # 2. Wait for VMs to be ready
    cloud_wait_for_vms
    
    # 3. Get VM addresses
    cloud_get_vm_addresses
    
    # 4. Setup Docker (if not done by cloud-init)
    # setup_docker_on_vms
    
    # 5. Generate workload config
    generate_workload_config
    
    # 6. Run benchmark
    if run_benchmark; then
        log_success "Test completed successfully"
    else
        log_error "Test failed"
        exit 1
    fi
    
    # 7. Cleanup
    if [[ "$CLEANUP_AFTER_TEST" == "true" ]]; then
        cloud_delete_vms
    else
        log_info "VMs left running for manual inspection"
    fi
}

# ============================================================================
# Argument Parsing
# ============================================================================

parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            --vm-count)
                VM_COUNT="$2"
                shift 2
                ;;
            --storage-uri)
                STORAGE_URI="$2"
                shift 2
                ;;
            --cleanup)
                CLEANUP_AFTER_TEST=true
                shift
                ;;
            --help)
                show_help
                exit 0
                ;;
            *)
                log_error "Unknown option: $1"
                exit 1
                ;;
        esac
    done
}

show_help() {
    cat << EOF
Cloud-Agnostic Distributed Testing Template

Usage: $0 [OPTIONS]

Options:
  --vm-count N         Number of VMs (default: 3)
  --storage-uri URI    Storage URI (e.g., s3://bucket, gs://bucket)
  --cleanup            Delete VMs after test
  --help               Show this help

Note: This is a TEMPLATE. You must implement the cloud_* functions
for your specific cloud provider before running.

See examples in this file:
  - example_aws_*
  - example_azure_*
  - example_gcp_*

Or use the complete GCP implementation:
  - scripts/gcp_distributed_test.sh
EOF
}

# ============================================================================
# Entry Point
# ============================================================================

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    parse_args "$@"
    main
fi
