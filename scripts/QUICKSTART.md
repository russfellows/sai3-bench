# Cloud Testing Quick Start Guide

This guide shows the recommended progression for testing sai3-bench distributed mode, from local development to cloud deployment.

## Stage 1: Local Docker Testing (No Cloud Costs)

Start here to validate your setup without spending money:

```bash
# Terminal 1: Build binaries
cd /path/to/sai3-bench
cargo build --release

# Terminal 2: Run local multi-container test
cd scripts
./local_docker_test.sh

# Verify distributed workflow works
# Expected output: Agents start, workload runs, results collected
```

**What this validates**:
- ✅ Distributed configuration is valid
- ✅ Agent connectivity works
- ✅ Workload execution completes
- ✅ Results aggregation functions
- ❌ Real network performance (not measured locally)
- ❌ Cloud storage integration (uses file://)

**Next steps**: If local test passes, proceed to cloud testing.

## Stage 2: GCP Cloud Testing (Simplest Cloud Option)

Google Cloud Platform has the simplest setup with `gcloud` CLI:

### Prerequisites
```bash
# Install gcloud CLI (if not already installed)
# See: https://cloud.google.com/sdk/docs/install

# Authenticate
gcloud auth login

# Set your project
export GCP_PROJECT="your-project-id"

# Create GCS bucket (one-time setup)
gsutil mb -l us-central1 gs://your-benchmark-bucket
```

### Run Test
```bash
cd scripts

# Small test: 2 VMs, 30 second duration
./gcp_distributed_test.sh \
  --project "$GCP_PROJECT" \
  --bucket gs://your-benchmark-bucket \
  --vm-count 2 \
  --cleanup

# Full test: 5 VMs, custom workload
./gcp_distributed_test.sh \
  --project "$GCP_PROJECT" \
  --zone us-west1-b \
  --vm-count 5 \
  --machine-type n2-standard-8 \
  --bucket gs://your-benchmark-bucket \
  --cleanup
```

**What this validates**:
- ✅ Real network performance
- ✅ Cloud storage (GCS) integration
- ✅ Multi-VM coordination
- ✅ Production-like environment

**Cost estimate**:
- VMs: ~$0.20/hour for n2-standard-4 (3 VMs = $0.60/hour)
- Storage: ~$0.02/GB/month for GCS Standard
- Example: 5 VMs × 1 hour × n2-standard-4 ≈ $1.00

**Cleanup**: The `--cleanup` flag automatically deletes VMs after testing.

## Stage 3: AWS Testing (If Using AWS Infrastructure)

### Prerequisites
```bash
# Install AWS CLI
# See: https://aws.amazon.com/cli/

# Configure credentials
aws configure

# Create S3 bucket (one-time setup)
aws s3 mb s3://your-benchmark-bucket --region us-east-1
```

### Customize Template
```bash
cd scripts
cp cloud_test_template.sh aws_distributed_test.sh

# Edit aws_distributed_test.sh and implement:
# - cloud_create_vms() using aws ec2 run-instances
# - cloud_ssh_exec() using SSH with your keypair
# - cloud_get_vm_addresses() using aws ec2 describe-instances
# See example implementations in the template
```

### Run Test
```bash
./aws_distributed_test.sh \
  --vm-count 3 \
  --storage-uri s3://your-benchmark-bucket \
  --cleanup
```

## Stage 4: Azure Testing (If Using Azure Infrastructure)

### Prerequisites
```bash
# Install Azure CLI
# See: https://learn.microsoft.com/en-us/cli/azure/install-azure-cli

# Login
az login

# Create resource group and storage (one-time setup)
az group create --name sai3bench-rg --location eastus
az storage account create \
  --name yourbenchmarkstorage \
  --resource-group sai3bench-rg \
  --location eastus

az storage container create \
  --name benchmark-data \
  --account-name yourbenchmarkstorage
```

### Customize Template
```bash
cd scripts
cp cloud_test_template.sh azure_distributed_test.sh

# Edit azure_distributed_test.sh and implement:
# - cloud_create_vms() using az vm create
# - cloud_ssh_exec() using az vm run-command
# - cloud_get_vm_addresses() using az vm list-ip-addresses
# See example implementations in the template
```

### Run Test
```bash
# Set Azure credentials
export AZURE_STORAGE_ACCOUNT="yourbenchmarkstorage"
export AZURE_STORAGE_ACCOUNT_KEY="your-key"

./azure_distributed_test.sh \
  --vm-count 3 \
  --storage-uri az://yourbenchmarkstorage/benchmark-data \
  --cleanup
```

## Troubleshooting Common Issues

### "gcloud command not found"
```bash
# Install Google Cloud SDK
curl https://sdk.cloud.google.com | bash
exec -l $SHELL  # Restart shell
gcloud init
```

### "Permission denied" when creating VMs
```bash
# Ensure you have the correct IAM role
gcloud projects add-iam-policy-binding $GCP_PROJECT \
  --member=user:your-email@example.com \
  --role=roles/compute.instanceAdmin.v1
```

### "SSH connection refused"
- VMs may still be starting up - wait 30 seconds and retry
- Check firewall rules allow SSH (port 22)
- Verify `gcloud compute ssh` works manually:
  ```bash
  gcloud compute ssh sai3bench-test-1 --zone=us-central1-a
  ```

### "Docker not found" on VM
- Check startup script in cloud provider's VM console
- View logs: `gcloud compute ssh VM_NAME --command="sudo journalctl -u google-startup-scripts"`
- Manual install:
  ```bash
  gcloud compute ssh VM_NAME
  curl -fsSL https://get.docker.com | sudo sh
  ```

### High cloud costs
- **Always use `--cleanup` flag** for automated tests
- Set budget alerts in cloud console:
  - GCP: Billing → Budgets & alerts
  - AWS: Billing → Budgets
  - Azure: Cost Management → Budgets
- Use smaller VM types for testing (e.g., `e2-medium` instead of `n2-standard-16`)

### Benchmark fails but VMs are running
```bash
# Don't panic - VMs are left running for debugging
# Connect to a VM:
gcloud compute ssh sai3bench-test-1 --zone=us-central1-a

# Check agent logs:
sudo docker logs sai3bench-agent-agent-1

# Manual cleanup when done:
gcloud compute instances delete sai3bench-test-{1..3} --zone=us-central1-a
```

## Performance Tuning Tips

### VM Sizing
- **Light workload** (testing): `n2-standard-4` (4 vCPU, 16 GB RAM)
- **Medium workload**: `n2-standard-8` (8 vCPU, 32 GB RAM)  
- **Heavy workload**: `n2-standard-16` or `n2-highcpu-32`

### Storage Placement
- **Best**: VMs and storage in same region (lowest latency)
- **Good**: VMs and storage in same continent
- **Poor**: Cross-continent testing (high latency, but useful for specific tests)

### Network Optimization
- Use premium tier networking for consistent performance (GCP)
- Enable accelerated networking (Azure)
- Use placement groups for low latency between VMs (AWS)

### Disk Performance
- GCP: Use `pd-ssd` instead of `pd-standard` for boot disks
- AWS: Use `gp3` or `io2` instead of `gp2`
- Azure: Use Premium SSD instead of Standard HDD

## Cost Optimization

### Development Testing
```bash
# Use minimal resources
./gcp_distributed_test.sh \
  --vm-count 2 \
  --machine-type e2-medium \
  --cleanup
```

**Estimated cost**: $0.20 for a 1-hour test

### Production Benchmarking
```bash
# Use appropriate resources for realistic workload
./gcp_distributed_test.sh \
  --vm-count 10 \
  --machine-type n2-standard-16 \
  --zone us-central1-a \
  --cleanup
```

**Estimated cost**: $10-15 for a 1-hour test

### Tips
1. **Always cleanup**: Use `--cleanup` flag
2. **Use spot/preemptible VMs** for non-critical tests (60-90% cheaper)
3. **Test locally first**: Validate configuration with `local_docker_test.sh`
4. **Batch tests**: Run multiple workloads in single VM deployment
5. **Schedule off-peak**: Run during cloud off-peak hours if possible

## Next Steps

After successful cloud testing:
1. Review results in `sai3-*/` directory
2. Adjust workload configuration for your needs
3. Scale to production VM count
4. Automate with CI/CD pipeline
5. Consider infrastructure-as-code (Terraform) for repeatable deployments

## Getting Help

- **Documentation**: See `docs/DISTRIBUTED_TESTING_GUIDE.md`
- **SSH Setup**: See `docs/SSH_SETUP_GUIDE.md`
- **Config Syntax**: See `docs/CONFIG_SYNTAX.md`
- **Issues**: Check `scripts/README.md` troubleshooting section
