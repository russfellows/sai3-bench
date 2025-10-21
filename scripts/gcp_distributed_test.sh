#!/bin/bash
# GCP Distributed Testing Automation for sai3-bench
# 
# This script automates the complete workflow:
# 1. Create GCE VMs in specified region/zone
# 2. Install Docker on each VM
# 3. Setup SSH keys for passwordless access
# 4. Run distributed benchmark
# 5. Collect results
# 6. Cleanup VMs (optional)
#
# Prerequisites:
# - gcloud CLI installed and configured (gcloud auth login)
# - sai3bench binaries built (cargo build --release)
# - GCP project with Compute Engine API enabled
# - Appropriate IAM permissions (Compute Instance Admin)
#
# Usage:
#   ./gcp_distributed_test.sh --project my-project --zone us-central1-a --vm-count 3 --run-benchmark

set -e  # Exit on error
set -u  # Exit on undefined variable

# ============================================================================
# Configuration - Customize these for your test
# ============================================================================

# GCP Settings
GCP_PROJECT="${GCP_PROJECT:-}"
GCP_ZONE="${GCP_ZONE:-us-central1-a}"
GCP_REGION="${GCP_REGION:-us-central1}"

# VM Settings
VM_PREFIX="sai3bench-test"
VM_COUNT=3
VM_MACHINE_TYPE="n2-standard-4"  # 4 vCPUs, 16 GB RAM
VM_IMAGE_FAMILY="ubuntu-2204-lts"
VM_IMAGE_PROJECT="ubuntu-os-cloud"
VM_DISK_SIZE="50GB"
VM_DISK_TYPE="pd-ssd"  # or pd-standard for slower/cheaper

# Network Settings (optional - use default VPC if not specified)
VM_NETWORK="default"
VM_SUBNET=""

# Docker Image
DOCKER_IMAGE="sai3bench:latest"
DOCKER_IMAGE_TAR="../sai3bench-docker-image.tar"  # Path to save/load image

# Benchmark Settings
WORKLOAD_CONFIG="workload.yaml"
BENCHMARK_DURATION="60s"
GCS_BUCKET="${GCS_BUCKET:-}"  # e.g., gs://my-benchmark-bucket

# Cleanup
CLEANUP_VMS_AFTER_TEST=false

# ============================================================================
# Color output
# ============================================================================

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

log_info() { echo -e "${BLUE}[INFO]${NC} $*"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $*"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_error() { echo -e "${RED}[ERROR]${NC} $*"; }

# ============================================================================
# Parse command line arguments
# ============================================================================

parse_args() {
    while [[ $# -gt 0 ]]; do
        case $1 in
            --project)
                GCP_PROJECT="$2"
                shift 2
                ;;
            --zone)
                GCP_ZONE="$2"
                GCP_REGION="${2%-*}"  # Extract region from zone
                shift 2
                ;;
            --vm-count)
                VM_COUNT="$2"
                shift 2
                ;;
            --machine-type)
                VM_MACHINE_TYPE="$2"
                shift 2
                ;;
            --bucket)
                GCS_BUCKET="$2"
                shift 2
                ;;
            --workload)
                WORKLOAD_CONFIG="$2"
                shift 2
                ;;
            --cleanup)
                CLEANUP_VMS_AFTER_TEST=true
                shift
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
GCP Distributed Testing for sai3-bench

Usage: $0 [OPTIONS]

Required:
  --project PROJECT     GCP project ID

Optional:
  --zone ZONE           GCP zone (default: us-central1-a)
  --vm-count N          Number of VMs to create (default: 3)
  --machine-type TYPE   GCE machine type (default: n2-standard-4)
  --bucket BUCKET       GCS bucket for testing (e.g., gs://my-bucket)
  --workload FILE       Workload config file (default: workload.yaml)
  --cleanup             Delete VMs after test completes
  --help                Show this help message

Examples:
  # Create 3 VMs and run benchmark
  $0 --project my-project --bucket gs://my-bucket

  # Create 5 VMs in specific zone with cleanup
  $0 --project my-project --zone us-west1-b --vm-count 5 --cleanup

  # Use powerful VMs for intensive testing
  $0 --project my-project --machine-type n2-standard-16
EOF
}

# ============================================================================
# Validation
# ============================================================================

validate_requirements() {
    log_info "Validating requirements..."
    
    # Check gcloud CLI
    if ! command -v gcloud &> /dev/null; then
        log_error "gcloud CLI not found. Install from: https://cloud.google.com/sdk/docs/install"
        exit 1
    fi
    
    # Check project
    if [[ -z "$GCP_PROJECT" ]]; then
        log_error "GCP project not specified. Use --project or set GCP_PROJECT env var"
        exit 1
    fi
    
    # Set project
    gcloud config set project "$GCP_PROJECT" --quiet
    
    # Check authentication
    if ! gcloud auth list --filter=status:ACTIVE --format="value(account)" &> /dev/null; then
        log_error "Not authenticated. Run: gcloud auth login"
        exit 1
    fi
    
    # Check sai3bench binaries
    if [[ ! -f "../target/release/sai3bench-ctl" ]]; then
        log_error "sai3bench-ctl not found. Run: cargo build --release"
        exit 1
    fi
    
    log_success "Requirements validated"
}

# ============================================================================
# VM Creation
# ============================================================================

create_vms() {
    log_info "Creating $VM_COUNT VMs in zone $GCP_ZONE..."
    
    local startup_script=$(cat << 'SCRIPT'
#!/bin/bash
# Install Docker
curl -fsSL https://get.docker.com | sh
usermod -aG docker $(whoami)

# Enable and start Docker
systemctl enable docker
systemctl start docker

# Install useful tools
apt-get update -qq
apt-get install -y -qq htop iotop
SCRIPT
)
    
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        
        log_info "Creating VM: $vm_name"
        
        gcloud compute instances create "$vm_name" \
            --zone="$GCP_ZONE" \
            --machine-type="$VM_MACHINE_TYPE" \
            --image-family="$VM_IMAGE_FAMILY" \
            --image-project="$VM_IMAGE_PROJECT" \
            --boot-disk-size="$VM_DISK_SIZE" \
            --boot-disk-type="$VM_DISK_TYPE" \
            --network="$VM_NETWORK" \
            --tags="sai3bench" \
            --metadata=startup-script="$startup_script" \
            --scopes=cloud-platform \
            --quiet || {
            log_warn "VM $vm_name may already exist"
        }
    done
    
    log_success "VM creation initiated"
}

wait_for_vms() {
    log_info "Waiting for VMs to be ready..."
    
    local max_wait=300  # 5 minutes
    local elapsed=0
    
    while [[ $elapsed -lt $max_wait ]]; do
        local ready_count=0
        
        for i in $(seq 1 $VM_COUNT); do
            local vm_name="${VM_PREFIX}-${i}"
            
            if gcloud compute ssh "$vm_name" --zone="$GCP_ZONE" --command="docker --version" &> /dev/null; then
                ((ready_count++))
            fi
        done
        
        if [[ $ready_count -eq $VM_COUNT ]]; then
            log_success "All $VM_COUNT VMs are ready"
            return 0
        fi
        
        log_info "VMs ready: $ready_count/$VM_COUNT (waiting...)"
        sleep 10
        ((elapsed+=10))
    done
    
    log_error "Timeout waiting for VMs to be ready"
    return 1
}

# ============================================================================
# Get VM Information
# ============================================================================

get_vm_addresses() {
    log_info "Retrieving VM IP addresses..."
    
    VM_ADDRESSES=()
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        local ip=$(gcloud compute instances describe "$vm_name" \
            --zone="$GCP_ZONE" \
            --format='get(networkInterfaces[0].accessConfigs[0].natIP)')
        
        VM_ADDRESSES+=("$ip")
        log_info "  $vm_name: $ip"
    done
    
    log_success "Retrieved ${#VM_ADDRESSES[@]} VM addresses"
}

# ============================================================================
# Docker Image Distribution
# ============================================================================

distribute_docker_image() {
    log_info "Distributing Docker image to VMs..."
    
    # Check if image exists locally
    if [[ ! -f "$DOCKER_IMAGE_TAR" ]]; then
        log_info "Docker image tar not found. Building from local source..."
        
        # Build Docker image locally (assumes Dockerfile exists)
        if [[ -f "../Dockerfile" ]]; then
            docker build -t "$DOCKER_IMAGE" ..
            docker save "$DOCKER_IMAGE" -o "$DOCKER_IMAGE_TAR"
            log_success "Docker image built and saved"
        else
            log_warn "No Dockerfile found. Assuming image exists on VMs or will be pulled."
            return 0
        fi
    fi
    
    # Copy image to each VM
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        
        log_info "Copying image to $vm_name..."
        gcloud compute scp "$DOCKER_IMAGE_TAR" "${vm_name}:~/" --zone="$GCP_ZONE" --quiet
        
        log_info "Loading image on $vm_name..."
        gcloud compute ssh "$vm_name" --zone="$GCP_ZONE" --command="sudo docker load -i ~/$(basename $DOCKER_IMAGE_TAR)" --quiet
    done
    
    log_success "Docker image distributed to all VMs"
}

# ============================================================================
# SSH Key Setup
# ============================================================================

setup_ssh_keys() {
    log_info "Setting up SSH keys for passwordless access..."
    
    # Build host list
    local hosts=""
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        if [[ -n "$hosts" ]]; then
            hosts="${hosts},"
        fi
        hosts="${hosts}${vm_name}"
    done
    
    log_info "Running sai3bench-ctl ssh-setup..."
    
    # Note: gcloud compute ssh handles authentication automatically
    # We just need to verify connectivity
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        gcloud compute ssh "$vm_name" --zone="$GCP_ZONE" --command="echo 'SSH OK'" --quiet
    done
    
    log_success "SSH access verified for all VMs"
}

# ============================================================================
# Generate Workload Config
# ============================================================================

generate_workload_config() {
    log_info "Generating workload configuration..."
    
    if [[ -z "$GCS_BUCKET" ]]; then
        log_error "GCS bucket not specified. Use --bucket gs://your-bucket"
        exit 1
    fi
    
    # Build agent list
    local agents_yaml=""
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        agents_yaml="${agents_yaml}    - address: \"${vm_name}\"\n"
        agents_yaml="${agents_yaml}      id: \"agent-${i}\"\n"
        agents_yaml="${agents_yaml}      env:\n"
        agents_yaml="${agents_yaml}        RUST_LOG: \"info\"\n"
    done
    
    cat > "$WORKLOAD_CONFIG" << EOF
# Auto-generated GCP distributed workload configuration
# Generated: $(date)
# VMs: $VM_COUNT
# Zone: $GCP_ZONE
# Bucket: $GCS_BUCKET

target: "${GCS_BUCKET}/sai3bench-test/"
duration: ${BENCHMARK_DURATION}
concurrency: 64

# Distributed configuration
distributed:
  agents:
$(echo -e "$agents_yaml")
  
  ssh:
    enabled: true
    user: "$(whoami)"
    timeout: 15
  
  deployment:
    deploy_type: "docker"
    image: "${DOCKER_IMAGE}"
    network_mode: "host"
    pull_policy: "if_not_present"
  
  start_delay: 3
  path_template: "agent-{id}/"

# Pre-populate test data
prepare:
  ensure_objects:
    - base_uri: "${GCS_BUCKET}/sai3bench-test/data/"
      count: 1000
      min_size: 1024
      max_size: 1048576
      fill: random
  post_prepare_delay: 2

# Mixed workload
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
    
    log_success "Workload config generated: $WORKLOAD_CONFIG"
}

# ============================================================================
# Run Benchmark
# ============================================================================

run_benchmark() {
    log_info "Running distributed benchmark..."
    
    log_info "Workload config:"
    cat "$WORKLOAD_CONFIG"
    
    # Run sai3bench-ctl
    ../target/release/sai3bench-ctl run --config "$WORKLOAD_CONFIG" || {
        log_error "Benchmark failed"
        return 1
    }
    
    log_success "Benchmark completed successfully"
}

# ============================================================================
# Collect Results
# ============================================================================

collect_results() {
    log_info "Collecting results from VMs..."
    
    local results_dir="gcp-distributed-results-$(date +%Y%m%d-%H%M%S)"
    mkdir -p "$results_dir"
    
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        
        log_info "Collecting logs from $vm_name..."
        
        # Get container logs if available
        gcloud compute ssh "$vm_name" --zone="$GCP_ZONE" \
            --command="sudo docker logs sai3bench-agent-agent-${i} 2>&1" \
            > "${results_dir}/${vm_name}.log" || true
    done
    
    log_success "Results collected to: $results_dir"
}

# ============================================================================
# Cleanup
# ============================================================================

cleanup_vms() {
    log_info "Cleaning up VMs..."
    
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        
        log_info "Deleting VM: $vm_name"
        gcloud compute instances delete "$vm_name" \
            --zone="$GCP_ZONE" \
            --quiet || true
    done
    
    log_success "VM cleanup complete"
}

# ============================================================================
# Main workflow
# ============================================================================

main() {
    log_info "=== GCP Distributed Testing for sai3-bench ==="
    log_info "Project: $GCP_PROJECT"
    log_info "Zone: $GCP_ZONE"
    log_info "VMs: $VM_COUNT x $VM_MACHINE_TYPE"
    log_info ""
    
    validate_requirements
    
    # Setup phase
    create_vms
    wait_for_vms
    get_vm_addresses
    
    # Docker distribution (optional if pulling from registry)
    # distribute_docker_image
    
    # SSH setup
    setup_ssh_keys
    
    # Generate config
    generate_workload_config
    
    # Run benchmark
    if run_benchmark; then
        collect_results
    else
        log_error "Benchmark failed. VMs left running for debugging."
        log_info "Connect to VMs with: gcloud compute ssh ${VM_PREFIX}-1 --zone=$GCP_ZONE"
        exit 1
    fi
    
    # Cleanup if requested
    if [[ "$CLEANUP_VMS_AFTER_TEST" == "true" ]]; then
        cleanup_vms
    else
        log_info "VMs left running. Cleanup manually with:"
        log_info "  gcloud compute instances delete ${VM_PREFIX}-{1..$VM_COUNT} --zone=$GCP_ZONE"
    fi
    
    log_success "=== Test Complete ==="
}

# ============================================================================
# Entry point
# ============================================================================

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    parse_args "$@"
    main
fi
