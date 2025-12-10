# Container Deployment Guide

This guide shows how to run distributed sai3-bench tests using containers across multiple VMs.

## Quick Start: Multi-VM Setup

### Prerequisites

- Multiple VMs with network connectivity between them
- Container runtime (Docker or Podman) on each VM
- sai3-bench container image (or binaries)
- Cloud storage credentials (for S3, Azure, or GCS testing)

### Step 1: Start Agent Containers on Each VM

Start an agent on each VM. The agent will listen for commands from the controller.

**On VM1:**
```bash
docker run --net=host -d \
  --name sai3-agent \
  -e AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID}" \
  -e AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY}" \
  -e AWS_REGION=us-east-1 \
  -e RUST_LOG=info \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7761
```

**On VM2:**
```bash
docker run --net=host -d \
  --name sai3-agent \
  -e AWS_ACCESS_KEY_ID="${AWS_ACCESS_KEY_ID}" \
  -e AWS_SECRET_ACCESS_KEY="${AWS_SECRET_ACCESS_KEY}" \
  -e AWS_REGION=us-west-2 \
  -e RUST_LOG=info \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7761
```

**Check agent status:**
```bash
docker logs sai3-agent
# Should show: "sai3bench-agent listening (PLAINTEXT) on 0.0.0.0:7761"
```

**Note**: The agent ID is configured in the YAML config file, not as a command-line option.

### Step 2: Create Workload Config

Create `distributed-test.yaml` on your controller machine:

```yaml
target: "s3://my-test-bucket/data/"
duration: 60s
concurrency: 32

# Define your agents
distributed:
  agents:
    - address: "vm1.example.com:7761"
      id: "agent-1"
    
    - address: "vm2.example.com:7761"
      id: "agent-2"

# Prepare test data
prepare:
  ensure_objects:
    - base_uri: "s3://my-test-bucket/data/"
      count: 1000
      size_spec:
        uniform:
          min: 1KB
          max: 1MB
      fill: random

# Workload operations
workload:
  - op: get
    path: "data/prepared-*.dat"
    weight: 70
  
  - op: put
    path: "uploads/new-"
    weight: 25
    object_size: 1MB
  
  - op: stat
    path: "data/prepared-*.dat"
    weight: 5
```

### Step 3: Run Controller from Container

Run the controller from any VM (or your local machine):

```bash
docker run --net=host \
  -v $(pwd)/distributed-test.yaml:/workspace/config.yaml:ro \
  -v $(pwd)/results:/results \
  -w /results \
  myregistry/sai3-bench:latest \
  sai3-bench run --config /workspace/config.yaml
```

**That's it!** The controller will:
1. Connect to both agents
2. Run the prepare phase (create 1000 objects)
3. Execute the workload for 60 seconds
4. Collect and save results

### Results

Results are saved to a timestamped directory:

```
sai3-20251104-1430-distributed-test/
├── config.yaml              # Your workload config
├── console_log.txt          # Complete execution log
├── metadata.json            # Test metadata
├── workload_results.tsv     # Consolidated metrics
├── prepare_results.tsv      # Prepare phase metrics
└── agents/
    ├── agent-1/
    │   └── results.tsv      # Per-agent metrics
    └── agent-2/
        └── results.tsv
```

## Cloud Provider Setup

### AWS S3

```bash
docker run --net=host -d \
  -e AWS_ACCESS_KEY_ID="AKIA..." \
  -e AWS_SECRET_ACCESS_KEY="..." \
  -e AWS_REGION=us-east-1 \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7761
```

**URI format**: `s3://bucket-name/prefix/`

### Azure Blob Storage

```bash
docker run --net=host -d \
  -e AZURE_STORAGE_ACCOUNT="mystorageacct" \
  -e AZURE_STORAGE_KEY="..." \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7761
```

**URI format**: `az://container-name/prefix/`

### Google Cloud Storage

```bash
docker run --net=host -d \
  -e GOOGLE_APPLICATION_CREDENTIALS=/creds/gcp-sa.json \
  -v /path/to/sa-key.json:/creds/gcp-sa.json:ro \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7761
```

**URI format**: `gs://bucket-name/prefix/`

## Multi-Cloud Testing

Test different storage backends simultaneously by using `target_override` per agent:

```yaml
distributed:
  agents:
    # AWS S3
    - address: "vm-aws:7761"
      id: "aws-agent"
      target_override: "s3://my-bucket/data/"
    
    # Azure Blob
    - address: "vm-azure:7761"
      id: "azure-agent"
      target_override: "az://mycontainer/data/"
    
    # GCS
    - address: "vm-gcp:7761"
      id: "gcp-agent"
      target_override: "gs://my-bucket/data/"

workload:
  - op: get
    path: "data/prepared-*.dat"
    weight: 100
```

Each agent will test its respective cloud storage backend.

## Building the Container Image

If you need to build your own container image:

**Dockerfile:**
```dockerfile
FROM ubuntu:22.04

RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy sai3-bench binaries
COPY target/release/sai3bench-agent /usr/local/bin/
COPY target/release/sai3-bench /usr/local/bin/

WORKDIR /workspace

CMD ["/bin/bash"]
```

**Build:**
```bash
cargo build --release
docker build -t myregistry/sai3-bench:latest .
docker push myregistry/sai3-bench:latest
```

## Advanced Configuration

### TLS/mTLS

For secure communication, start agents with TLS:

```bash
docker run --net=host -d \
  myregistry/sai3-bench:latest \
  sai3bench-agent --listen 0.0.0.0:7761 --tls
```

Configure controller to use TLS (see [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md) for details).

### Per-Agent Configuration

You can override settings per agent in the YAML:

```yaml
distributed:
  agents:
    - address: "vm1:7761"
      id: "agent-1"
      concurrency_override: 64      # More workers on this agent
      
    - address: "vm2:7761"
      id: "agent-2"
      target_override: "s3://other-bucket/data/"  # Different bucket
```

### Shared vs Isolated Storage

See [DIRECTORY_TREE_GUIDE.md](DIRECTORY_TREE_GUIDE.md) for directory tree testing with multiple agents.

## Troubleshooting

### Agent won't start
```bash
# Check logs
docker logs sai3-agent

# Common issues:
# - Port 7761 already in use
# - Missing cloud credentials
# - Network firewall blocking port
```

### Controller can't connect to agents
```bash
# Test connectivity
telnet vm1.example.com 7761

# Ensure:
# - Agents are running (docker ps)
# - Firewall allows port 7761
# - Correct hostnames/IPs in config
```

### Permission errors (S3/Azure/GCS)
```bash
# Verify credentials are passed correctly
docker exec sai3-agent env | grep AWS
docker exec sai3-agent env | grep AZURE
docker exec sai3-agent env | grep GOOGLE
```

## Summary

**Simple workflow**:
1. Start `sai3bench-agent` containers on each VM (with cloud credentials)
2. Create YAML config listing agent addresses
3. Run controller: `sai3-bench run --config mytest.yaml`
4. View results in timestamped directory

**Key points**:
- Agent ID is configured in YAML, not command-line
- Use `--net=host` for simple networking (or expose port 7761)
- Each agent needs appropriate cloud credentials
- Controller can run from any machine with network access to agents

For more details see:
- [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md) - Full distributed testing guide
- [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) - Complete YAML syntax reference
- [USAGE.md](USAGE.md) - Command-line options and examples
