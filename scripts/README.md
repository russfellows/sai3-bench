# Cloud Testing Scripts for sai3-bench

This directory contains automated testing scripts for deploying and running distributed sai3-bench tests in various cloud environments.

## Available Scripts

### 1. `local_docker_test.sh` - Local Multi-Container Testing
**Recommended starting point** - Test distributed mode locally before deploying to cloud.

**Features**:
- Simulates distributed testing using Docker containers on a single machine
- No cloud credentials or VMs required
- Optional MinIO for local S3 testing
- Perfect for development and testing workflows

**Prerequisites**:
- Docker installed and running
- sai3bench binaries built (`cargo build --release`)

**Basic Usage**:
```bash
# Test with 3 agents using local filesystem
./local_docker_test.sh

# Test with 5 agents using MinIO (local S3-compatible storage)
./local_docker_test.sh --agent-count 5 --storage-type minio

# Cleanup containers
./local_docker_test.sh --cleanup-only
```

**When to Use**:
- ✅ Testing distributed configuration before cloud deployment
- ✅ Development and debugging of distributed features
- ✅ CI/CD pipeline testing
- ✅ Learning the distributed workflow
- ❌ Actual performance benchmarking (use real cloud VMs)

### 2. `gcp_distributed_test.sh` - Google Cloud Platform
Complete automation for GCP with Compute Engine VMs.

**Features**:
- Automated VM creation with Docker pre-installed
- SSH key setup via gcloud CLI
- Container image distribution (Docker or Podman)
- Workload configuration generation
- Results collection
- Optional VM cleanup

**Prerequisites**:
- `gcloud` CLI installed and configured
- GCP project with Compute Engine API enabled
- sai3bench binaries built (`cargo build --release`)

**Basic Usage**:
```bash
# Create 3 VMs and run benchmark
./gcp_distributed_test.sh \
  --project my-gcp-project \
  --bucket gs://my-benchmark-bucket

# Specify zone and VM count
./gcp_distributed_test.sh \
  --project my-gcp-project \
  --zone us-west1-b \
  --vm-count 5 \
  --bucket gs://my-bucket \
  --cleanup

# Use powerful VMs for intensive testing
./gcp_distributed_test.sh \
  --project my-gcp-project \
  --machine-type n2-standard-16 \
  --bucket gs://my-bucket
```

**Options**:
- `--project PROJECT` - GCP project ID (required)
- `--zone ZONE` - GCP zone (default: us-central1-a)
- `--vm-count N` - Number of VMs (default: 3)
- `--machine-type TYPE` - VM machine type (default: n2-standard-4)
- `--bucket BUCKET` - GCS bucket for testing (e.g., gs://my-bucket)
- `--workload FILE` - Custom workload config (optional)
- `--cleanup` - Delete VMs after test completes
- `--help` - Show help message

**Example Workflow**:
```bash
# 1. Set environment variables (optional)
export GCP_PROJECT="my-project"
export GCS_BUCKET="gs://my-benchmark-bucket"

# 2. Run test with 5 VMs in us-west1
./gcp_distributed_test.sh \
  --vm-count 5 \
  --zone us-west1-a \
  --cleanup

# 3. Results will be saved to: gcp-distributed-results-YYYYMMDD-HHMMSS/
```

### 3. `cloud_test_template.sh` - Cloud-Agnostic Template
```bash
# Create 3 VMs and run benchmark
./gcp_distributed_test.sh \
  --project my-gcp-project \
  --bucket gs://my-benchmark-bucket

# Specify zone and VM count
./gcp_distributed_test.sh \
  --project my-gcp-project \
  --zone us-west1-b \
  --vm-count 5 \
  --bucket gs://my-bucket \
  --cleanup

# Use powerful VMs for intensive testing
./gcp_distributed_test.sh \
  --project my-gcp-project \
  --machine-type n2-standard-16 \
  --bucket gs://my-bucket
```

**Options**:
- `--project PROJECT` - GCP project ID (required)
- `--zone ZONE` - GCP zone (default: us-central1-a)
- `--vm-count N` - Number of VMs (default: 3)
- `--machine-type TYPE` - VM machine type (default: n2-standard-4)
- `--bucket BUCKET` - GCS bucket for testing (e.g., gs://my-bucket)
- `--workload FILE` - Custom workload config (optional)
- `--cleanup` - Delete VMs after test completes
- `--help` - Show help message

**Example Workflow**:
```bash
# 1. Set environment variables (optional)
export GCP_PROJECT="my-project"
export GCS_BUCKET="gs://my-benchmark-bucket"

# 2. Run test with 5 VMs in us-west1
./gcp_distributed_test.sh \
  --vm-count 5 \
  --zone us-west1-a \
  --cleanup

# 3. Results will be saved to: gcp-distributed-results-YYYYMMDD-HHMMSS/
```

### 2. `cloud_test_template.sh` - Cloud-Agnostic Template
Generic template that can be adapted for any cloud provider (AWS, Azure, etc.).

**Use Cases**:
- Starting point for AWS, Azure, or other cloud implementations
- Custom on-premises or private cloud environments
- Learning the workflow structure

**How to Use**:
1. Copy the template to your environment-specific script:
   ```bash
   cp cloud_test_template.sh aws_distributed_test.sh
   ```

2. Implement the `cloud_*` functions for your provider:
   - `cloud_create_vms()` - Create VMs
   - `cloud_wait_for_vms()` - Wait for readiness
   - `cloud_get_vm_addresses()` - Get hostnames/IPs
   - `cloud_ssh_exec()` - Execute SSH command
   - `cloud_scp_to_vm()` - Copy files to VM
   - `cloud_delete_vms()` - Cleanup VMs

3. See example implementations in the template:
   - `example_aws_*` functions
   - `example_azure_*` functions
   - `example_gcp_*` functions

**Basic Template Structure**:
```bash
# Implement cloud-specific functions
cloud_create_vms() {
    # Use your cloud's CLI or API
    aws ec2 run-instances ...
}

# Rest of the workflow is cloud-agnostic
main() {
    cloud_create_vms
    cloud_wait_for_vms
    generate_workload_config
    run_benchmark
    cloud_delete_vms
}
```

### 4. `validate_gcp_script.sh` - Configuration Validation
Test script to validate the GCP script without deploying actual VMs.

**Features**:
- Tests script executability
- Validates help command
- Tests configuration generation
- Checks YAML syntax

**Usage**:
```bash
./validate_gcp_script.sh
```

**When to Use**:
- ✅ After modifying cloud test scripts
- ✅ Before committing changes
- ✅ CI/CD pipeline validation
- ✅ Verifying script functionality without cloud costs

## Common Workflow

All scripts follow this general pattern:

1. **VM Creation** - Create VMs with Docker pre-installed
2. **Readiness Check** - Wait for VMs to be SSH-accessible
3. **Docker Setup** - Ensure Docker is running (usually via cloud-init)
4. **SSH Verification** - Test passwordless SSH access
5. **Config Generation** - Create distributed workload YAML
6. **Benchmark Execution** - Run `sai3bench-ctl run`
7. **Results Collection** - Gather logs and metrics
8. **Cleanup** - Optionally delete VMs

## Creating Your Own Cloud Script

### Option 1: Use GCP Script as Reference
The GCP script is the most complete implementation. You can:
- Copy and modify for AWS/Azure
- Replace `gcloud` commands with `aws` or `az` commands
- Adapt VM creation and SSH connection methods

### Option 2: Start from Template
1. Copy `cloud_test_template.sh`
2. Implement the `cloud_*` interface functions
3. Test incrementally (create VMs, SSH, run benchmark, cleanup)

### Example: AWS Implementation
```bash
#!/bin/bash
# aws_distributed_test.sh

source cloud_test_template.sh

cloud_create_vms() {
    local ami="ami-0c55b159cbfafe1f0"  # Ubuntu 22.04
    local instance_type="t3.medium"
    
    for i in $(seq 1 $VM_COUNT); do
        local vm_name="${VM_PREFIX}-${i}"
        aws ec2 run-instances \
            --image-id "$ami" \
            --instance-type "$instance_type" \
            --key-name "my-keypair" \
            --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$vm_name}]"
    done
}

cloud_ssh_exec() {
    local vm_name="$1"
    local command="$2"
    ssh -i ~/.ssh/my-keypair.pem "ubuntu@$vm_name" "$command"
}

# ... implement other cloud_* functions ...

main "$@"
```

## Storage Backend Configuration

Each cloud provider has native storage options:

### Google Cloud Platform (GCP)
```bash
--bucket gs://my-bucket
```
**Setup**:
- Create GCS bucket in same region as VMs
- Ensure VMs have Storage Object Viewer/Admin role
- Uses Application Default Credentials automatically

### Amazon Web Services (AWS)
```bash
--storage-uri s3://my-bucket
```
**Setup**:
- Create S3 bucket
- Attach IAM role to EC2 instances with S3 access
- Or set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY env vars

### Microsoft Azure
```bash
--storage-uri az://my-storage-account/container
```
**Setup**:
- Create Azure Storage account and container
- Set AZURE_STORAGE_ACCOUNT and AZURE_STORAGE_ACCOUNT_KEY env vars
- Or use Azure Managed Identity on VMs

### Cross-Cloud Testing
You can test cross-region/cross-cloud scenarios:
```bash
# GCP VMs testing AWS S3
./gcp_distributed_test.sh \
  --project my-gcp-project \
  --bucket s3://my-aws-bucket  # Add AWS credentials to VMs

# AWS VMs testing GCS
./aws_distributed_test.sh \
  --storage-uri gs://my-gcs-bucket  # Add GCP credentials to VMs
```

## Troubleshooting

### VMs don't become ready
- Check cloud provider quotas (VM count, CPUs, disk)
- Verify network/firewall rules allow SSH (port 22)
- Increase timeout in `cloud_wait_for_vms()`

### SSH connection failures
- Verify SSH key permissions (`chmod 600 ~/.ssh/key.pem`)
- Check VM security groups/firewall rules
- Ensure VM has public IP or bastion access
- Try manual SSH: `ssh -v user@vm-hostname`

### Docker not available
- Check startup script execution: `cloud logs` or `/var/log/cloud-init-output.log`
- Verify Docker installation: `docker --version`
- Check Docker service: `systemctl status docker`

### Benchmark fails to start
- Check agent connectivity: `sai3bench-ctl ping --agents host1,host2`
- Verify Docker image exists: `docker images | grep sai3bench`
- Check firewall rules for gRPC port (7761)
- Review agent logs: `docker logs sai3bench-agent-*`

### High costs from orphaned VMs
- Always use `--cleanup` flag for automated tests
- Set cloud budget alerts
- Use tagging/labels to identify test VMs
- Create cleanup scripts:
  ```bash
  # GCP: Delete all sai3bench VMs
  gcloud compute instances list --filter="name~sai3bench-test" \
    --format="value(name,zone)" | \
    xargs -n2 gcloud compute instances delete --quiet
  ```

## Best Practices

1. **Use Cleanup Flag**: Always use `--cleanup` unless debugging
2. **Tag Resources**: Use consistent naming/tagging for easy cleanup
3. **Regional Colocation**: Create VMs and storage in same region
4. **Start Small**: Test with 2-3 VMs before scaling to 10+
5. **Monitor Costs**: Set cloud budget alerts before large tests
6. **Security**: Use cloud-managed secrets for credentials
7. **Results Backup**: Copy results to cloud storage before cleanup

## Future Enhancements

Planned additions to this directory:

- [ ] `aws_distributed_test.sh` - Full AWS EC2 implementation
- [ ] `azure_distributed_test.sh` - Full Azure VM implementation
- [ ] `terraform/` - Infrastructure-as-code for repeatable deployments
- [ ] Multi-region testing scripts
- [ ] Cost estimation tools
- [ ] Performance comparison scripts (benchmark different VM types)

## Contributing

When adding new cloud scripts:
1. Follow the template structure
2. Add comprehensive help text (`--help`)
3. Include example usage in this README
4. Test with small VM count first
5. Document cloud-specific prerequisites

## Scale-Out vs Scale-Up Testing

sai3-bench supports both scaling strategies:

### Scale-Out (Horizontal): Multiple VMs
Deploy agents across multiple VMs for maximum network bandwidth and fault isolation.

```bash
# 8 VMs with 1 container each
./gcp_distributed_test.sh --vm-count 8 --machine-type n2-standard-8
```

**Use case**: High network throughput, multi-region testing, fault tolerance

### Scale-Up (Vertical): Multiple Containers on One VM
Deploy multiple agent containers on a single large VM by using different ports.

```yaml
# Example config: 8 containers on 1 VM
distributed:
  agents:
    - { address: "big-vm:7761", id: "c1", listen_port: 7761 }
    - { address: "big-vm:7762", id: "c2", listen_port: 7762 }
    - { address: "big-vm:7763", id: "c3", listen_port: 7763 }
    # ... up to c8 on port 7768
```

**Use case**: Lower network latency, simpler deployment, cost optimization

See [examples/distributed-scale-out.yaml](../examples/distributed-scale-out.yaml) and [examples/distributed-scale-up.yaml](../examples/distributed-scale-up.yaml) for complete configurations.

## Container Runtime Flexibility

All scripts support both Docker and Podman via YAML configuration:

```yaml
distributed:
  deployment:
    container_runtime: "docker"  # or "podman"
    image: "sai3bench:v0.6.11"
```

No recompilation needed - the container runtime command is configurable at runtime!

## Related Documentation

- [Distributed Testing Guide](../docs/DISTRIBUTED_TESTING_GUIDE.md) - Scale-out/scale-up patterns
- [SSH Setup Guide](../docs/SSH_SETUP_GUIDE.md)
- [Configuration Syntax](../docs/CONFIG_SYNTAX.md)
- [Cloud Storage Setup](../docs/CLOUD_STORAGE_SETUP.md)
