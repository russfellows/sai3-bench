# Container Deployment Guide

This guide covers running sai3-bench distributed tests using pre-started containers across multiple VMs in cloud environments (AWS, Azure, GCP).

## Overview

**Use Case**: You have multiple VMs in the cloud and want to:
1. Run the same container image on each VM
2. Manually start agents in each container
3. Run coordinated distributed tests
4. Test cloud storage (S3, Azure Blob, GCS)

**Advantages**:
- Full control over container lifecycle
- Easy to integrate with existing container orchestration
- Works with any container runtime (Docker, Podman)
- No SSH setup required
- Same container image across all environments

## Prerequisites

- Container image with sai3-bench binaries built and pushed to registry
- Multiple VMs with network connectivity
- Container runtime installed (Docker or Podman)
- Cloud credentials configured

## Quick Start: 3-VM Multi-Cloud Example

### Step 1: Prepare Your Container Image

Your container should include:
- `sai3bench-agent` binary
- `sai3bench-ctl` binary (for one VM to act as controller)
- `sai3-bench` binary (optional, for single-node testing)

**Example Dockerfile** (if building your own):
```dockerfile
FROM ubuntu:22.04

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy sai3-bench binaries
COPY target/release/sai3bench-agent /usr/local/bin/
COPY target/release/sai3bench-ctl /usr/local/bin/
COPY target/release/sai3-bench /usr/local/bin/

# Set working directory
WORKDIR /workspace

# Default command (can be overridden)
CMD ["/bin/bash"]
```

Build and push:
```bash
docker build -t myregistry/sai3-tools:latest .
docker push myregistry/sai3-tools:latest
```

### Step 2: Start Agent Containers on Each VM

**On VM1 (AWS region us-east-1):**
```bash
docker run --net=host -it \
  --name sai3-agent-vm1 \
  -e AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID}" \
  -e AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY}" \
  -e AWS_REGION=us-east-1 \
  -e RUST_LOG=info \
  -v /data/results:/results \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761 --agent-id aws-vm1
```

**On VM2 (Azure region eastus):**
```bash
docker run --net=host -it \
  --name sai3-agent-vm2 \
  -e AZURE_STORAGE_ACCOUNT="${AZURE_STORAGE_ACCOUNT}" \
  -e AZURE_STORAGE_KEY="${AZURE_STORAGE_KEY}" \
  -e RUST_LOG=info \
  -v /data/results:/results \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761 --agent-id azure-vm2
```

**On VM3 (GCP region us-central1):**
```bash
docker run --net=host -it \
  --name sai3-agent-vm3 \
  -e GOOGLE_APPLICATION_CREDENTIALS=/creds/gcp-sa.json \
  -e RUST_LOG=info \
  -v /home/user/.gcp/sa-key.json:/creds/gcp-sa.json:ro \
  -v /data/results:/results \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761 --agent-id gcp-vm3
```

**Agent Output** (you should see):
```
[INFO] sai3bench-agent starting on 0.0.0.0:7761
[INFO] Agent ID: aws-vm1
[INFO] gRPC server listening...
```

### Step 3: Create Workload Config

Create `distributed-multicloud.yaml`:
```yaml
target: "s3://my-test-bucket/data/"
duration: 60s
concurrency: 32

# Distributed agent addresses (no SSH, manual startup)
distributed:
  agents:
    # AWS S3 testing
    - address: "vm1-aws.cloud.com:7761"
      id: "aws-vm1"
    
    # Azure Blob testing
    - address: "vm2-azure.cloud.com:7761"
      id: "azure-vm2"
      target_override: "az://mystorageacct/testcontainer/"
    
    # GCS testing
    - address: "vm3-gcp.cloud.com:7761"
      id: "gcp-vm3"
      target_override: "gs://my-test-bucket/data/"

# Prepare test data (only for S3 in this example)
prepare:
  ensure_objects:
    - base_uri: "s3://my-test-bucket/data/"
      count: 1000
      size_distribution:
        type: lognormal
        mean: 1048576
        std_dev: 524288
        min: 1024
        max: 10485760
      fill: random

# Mixed read/write workload
workload:
  - op: get
    path: "data/*"
    weight: 70
  
  - op: put
    path: "uploads/"
    weight: 25
    size_distribution:
      type: lognormal
      mean: 1048576
      std_dev: 524288
  
  - op: list
    path: "data/"
    weight: 5
```

### Step 4: Run Controller

From any machine (could be one of the VMs or your local workstation):

**Option A: From a container on one of the VMs**
```bash
# On VM1, in a second terminal or detached container
docker run --net=host \
  -v $(pwd)/distributed-multicloud.yaml:/workspace/config.yaml:ro \
  -v /data/results:/results \
  -w /results \
  myregistry/sai3-tools:latest \
  sai3bench-ctl --insecure \
    run --config /workspace/config.yaml
```

**Option B: From your workstation** (if you have sai3-bench installed locally)
```bash
sai3bench-ctl --insecure \
  --agents vm1-aws:7761,vm2-azure:7761,vm3-gcp:7761 \
  run --config distributed-multicloud.yaml
```

### Step 5: Monitor and Results

Controller output:
```
[INFO] Connecting to 3 agents...
[INFO] Agent aws-vm1 ready
[INFO] Agent azure-vm2 ready
[INFO] Agent gcp-vm3 ready
[INFO] Coordinated start in 2 seconds...
[INFO] Workload running... (60s duration)
[INFO] Collecting results...
[INFO] Results saved to: sai3-20251022-1430-distributed-multicloud/
```

Results structure:
```
sai3-20251022-1430-distributed-multicloud/
├── config.yaml              # Your workload config
├── console.log              # Complete execution log
├── metadata.json            # Test metadata
├── results.tsv              # Consolidated aggregate metrics
└── agents/
    ├── aws-vm1/
    │   └── results.tsv
    ├── azure-vm2/
    │   └── results.tsv
    └── gcp-vm3/
        └── results.tsv
```

## Cloud Provider Configuration

### AWS S3

**Environment Variables:**
```bash
export AWS_ACCESS_KEY_ID="AKIA..."
export AWS_SECRET_ACCESS_KEY="..."
export AWS_REGION="us-east-1"  # Optional, defaults to us-east-1
```

**Container Run:**
```bash
docker run --net=host -it \
  -e AWS_ACCESS_KEY_ID \
  -e AWS_SECRET_ACCESS_KEY \
  -e AWS_REGION \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761
```

**URI Format:** `s3://bucket-name/prefix/`

**Testing Multiple Regions:**
```yaml
distributed:
  agents:
    - address: "vm-us-east-1:7761"
      id: "us-east-1"
      env:
        AWS_REGION: "us-east-1"
      target_override: "s3://bucket-us-east-1/data/"
    
    - address: "vm-eu-west-1:7761"
      id: "eu-west-1"
      env:
        AWS_REGION: "eu-west-1"
      target_override: "s3://bucket-eu-west-1/data/"
```

### Azure Blob Storage

**Environment Variables (Account Key):**
```bash
export AZURE_STORAGE_ACCOUNT="mystorageaccount"
export AZURE_STORAGE_KEY="base64encodedkey=="
```

**Environment Variables (SAS Token):**
```bash
export AZURE_STORAGE_ACCOUNT="mystorageaccount"
export AZURE_STORAGE_SAS_TOKEN="sv=2021-06-08&ss=..."
```

**Container Run:**
```bash
docker run --net=host -it \
  -e AZURE_STORAGE_ACCOUNT \
  -e AZURE_STORAGE_KEY \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761
```

**URI Format:** `az://storage-account-name/container-name/prefix/`

### Google Cloud Storage (GCS)

**Service Account Key:**
```bash
# Create service account with Storage Admin role
gcloud iam service-accounts create sai3bench-sa
gcloud projects add-iam-policy-binding PROJECT_ID \
  --member="serviceAccount:sai3bench-sa@PROJECT_ID.iam.gserviceaccount.com" \
  --role="roles/storage.admin"

# Download key
gcloud iam service-accounts keys create ~/gcp-sa-key.json \
  --iam-account=sai3bench-sa@PROJECT_ID.iam.gserviceaccount.com
```

**Container Run:**
```bash
docker run --net=host -it \
  -e GOOGLE_APPLICATION_CREDENTIALS=/creds/gcp-sa.json \
  -v ~/gcp-sa-key.json:/creds/gcp-sa.json:ro \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761
```

**URI Format:** `gs://bucket-name/prefix/`

## Advanced Patterns

### Pattern 1: Same-Region Cross-Provider Comparison

Test S3, Azure, and GCS in the same region (e.g., US East):

```yaml
distributed:
  agents:
    - address: "vm-aws-use1:7761"
      id: "aws-s3"
      target_override: "s3://bench-us-east-1/data/"
    
    - address: "vm-azure-eastus:7761"
      id: "azure-blob"
      target_override: "az://bencheastus/testdata/"
    
    - address: "vm-gcp-us-east1:7761"
      id: "gcs"
      target_override: "gs://bench-us-east1/data/"

workload:
  - op: get
    path: "data/1mb-*"  # Pre-populated 1MB objects
    weight: 100
```

**Analysis**: Compare latency and throughput across providers in equivalent regions.

### Pattern 2: Multi-Region Latency Testing

Test storage performance from different geographic locations:

```yaml
distributed:
  agents:
    # Client in US, accessing US bucket (same region)
    - address: "vm-us-east:7761"
      id: "us-to-us"
      target_override: "s3://bucket-us-east-1/data/"
      env:
        AWS_REGION: "us-east-1"
    
    # Client in US, accessing EU bucket (cross-region)
    - address: "vm-us-east:7762"
      id: "us-to-eu"
      target_override: "s3://bucket-eu-west-1/data/"
      env:
        AWS_REGION: "eu-west-1"
    
    # Client in EU, accessing EU bucket (same region)
    - address: "vm-eu-west:7761"
      id: "eu-to-eu"
      target_override: "s3://bucket-eu-west-1/data/"
      env:
        AWS_REGION: "eu-west-1"
```

### Pattern 3: Scale-Up in Containers

Run multiple agents on one powerful VM using different ports:

```bash
# Agent 1 on port 7761
docker run -d --net=host \
  --name agent-1 \
  -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761 --agent-id vm1-c1

# Agent 2 on port 7762
docker run -d --net=host \
  --name agent-2 \
  -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7762 --agent-id vm1-c2

# Agent 3 on port 7763
docker run -d --net=host \
  --name agent-3 \
  -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7763 --agent-id vm1-c3
```

Config:
```yaml
distributed:
  agents:
    - address: "vm1.cloud.com:7761"
      id: "vm1-c1"
    - address: "vm1.cloud.com:7762"
      id: "vm1-c2"
    - address: "vm1.cloud.com:7763"
      id: "vm1-c3"
```

## Troubleshooting

### Issue: "Failed to connect to agent"

**Symptoms:**
```
[ERROR] Failed to connect to agent vm1:7761: connection refused
```

**Checks:**
1. Is agent container running? `docker ps | grep sai3-agent`
2. Is agent listening? `docker logs <container-id>`
3. Is port accessible? `nc -zv vm1 7761` (from controller host)
4. Firewall rules allow TCP 7761?
5. Using `--net=host` on agent container?

### Issue: "Authentication failed" for cloud storage

**AWS:**
- Verify `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` are set
- Check IAM permissions (need `s3:GetObject`, `s3:PutObject`, `s3:ListBucket`)
- Verify `AWS_REGION` matches bucket region

**Azure:**
- Verify `AZURE_STORAGE_ACCOUNT` is correct storage account name (not full URL)
- Check `AZURE_STORAGE_KEY` is the access key (not SAS token, unless using SAS)
- Ensure container exists and is accessible

**GCS:**
- Verify service account JSON file is mounted correctly
- Check `GOOGLE_APPLICATION_CREDENTIALS` env var points to mounted file
- Ensure service account has `storage.admin` or equivalent role

### Issue: "No objects found" in GET operations

**Solution:**
1. Run prepare step first (in config or manually)
2. Verify URI path matches between prepare and workload
3. Check storage backend connectivity: `sai3-bench util health --uri s3://bucket/`

### Issue: Container crashes or agent exits immediately

**Debug:**
```bash
# Run interactively to see errors
docker run -it --net=host \
  -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  myregistry/sai3-tools:latest \
  /bin/bash

# Inside container, test manually
sai3bench-agent --listen 0.0.0.0:7761 -vv
```

## Best Practices

### Security

1. **Use read-only mounts for credentials:**
   ```bash
   -v ~/.gcp/sa-key.json:/creds/gcp-sa.json:ro
   ```

2. **Use IAM roles when possible** (AWS EC2 instance profiles, Azure managed identities)
   - No need to pass credentials explicitly
   - Automatically rotated

3. **Use secrets management:**
   - AWS Secrets Manager
   - Azure Key Vault
   - GCP Secret Manager

### Performance

1. **Use `--net=host` for lowest latency** (no Docker NAT overhead)

2. **Match VM region to storage region** for best throughput

3. **Consider CPU limits:**
   ```bash
   docker run --cpus="4.0" --net=host ...
   ```

4. **Monitor container resource usage:**
   ```bash
   docker stats
   ```

### Results Management

1. **Mount results directory:**
   ```bash
   -v /data/results:/results -w /results
   ```

2. **Copy results out after test:**
   ```bash
   docker cp <container-id>:/results/sai3-20251022-1430-test/ ./local-results/
   ```

3. **Use object storage for long-term results:**
   ```bash
   # After test completes
   aws s3 sync /data/results/ s3://my-results-bucket/sai3-bench/
   ```

## Comparison: Manual vs SSH-Automated Deployment

| Aspect | Manual Container (This Guide) | SSH-Automated (v0.6.11+) |
|--------|------------------------------|--------------------------|
| **Setup** | Manual `docker run` on each VM | One-time `sai3bench-ctl ssh-setup` |
| **Deployment** | Pre-start containers | Automatic via SSH + Docker |
| **Control** | Full lifecycle control | Automated start/stop |
| **Cleanup** | Manual container stop | Automatic cleanup |
| **SSH Required** | No | Yes |
| **Best For** | Existing container orchestration, long-running agents | Ephemeral testing, CI/CD |
| **Complexity** | Higher initial setup | Lower after SSH setup |

## Example: Complete Multi-Cloud Workflow

```bash
# ===== On AWS VM (us-east-1) =====
docker run -d --net=host --name sai3-agent-aws \
  --restart unless-stopped \
  -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e AWS_REGION=us-east-1 \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761 --agent-id aws-use1

# ===== On Azure VM (eastus) =====
docker run -d --net=host --name sai3-agent-azure \
  --restart unless-stopped \
  -e AZURE_STORAGE_ACCOUNT -e AZURE_STORAGE_KEY \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761 --agent-id azure-eastus

# ===== On GCP VM (us-central1) =====
docker run -d --net=host --name sai3-agent-gcp \
  --restart unless-stopped \
  -e GOOGLE_APPLICATION_CREDENTIALS=/creds/gcp-sa.json \
  -v ~/.gcp/sa-key.json:/creds/gcp-sa.json:ro \
  myregistry/sai3-tools:latest \
  sai3bench-agent --listen 0.0.0.0:7761 --agent-id gcp-usc1

# ===== From Controller (your workstation or one of the VMs) =====
cat > multicloud-comparison.yaml <<EOF
target: "s3://benchmark-bucket/data/"
duration: 300s
concurrency: 64

distributed:
  agents:
    - address: "aws-vm.us-east-1:7761"
      id: "aws-use1"
    - address: "azure-vm.eastus:7761"
      id: "azure-eastus"
      target_override: "az://bencheastus/data/"
    - address: "gcp-vm.us-central1:7761"
      id: "gcp-usc1"
      target_override: "gs://benchmark-bucket/data/"

workload:
  - op: get
    path: "data/*"
    weight: 80
  - op: put
    path: "uploads/"
    weight: 20
    size_distribution:
      type: lognormal
      mean: 1048576
      std_dev: 524288
EOF

sai3bench-ctl --insecure run --config multicloud-comparison.yaml

# View results
cat sai3-*/results.tsv | column -t
```

## Next Steps

- See [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md) for SSH-automated deployment
- See [CLOUD_STORAGE_SETUP.md](CLOUD_STORAGE_SETUP.md) for detailed cloud provider configuration
- See [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for complete configuration reference
- See [SCALE_OUT_VS_SCALE_UP.md](SCALE_OUT_VS_SCALE_UP.md) for scaling strategy comparison
