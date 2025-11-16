# Distributed Testing Guide

This guide shows how to run sai3-bench across multiple hosts for large-scale load generation.

## Quick Start: 4-Host Example

### Prerequisites
- sai3-bench binaries (`sai3bench-agent`, `sai3bench-ctl`) installed on all hosts
- Network connectivity between controller and all agent hosts
- Shared storage backend (S3, Azure, GCS) OR independent local storage for isolated testing

### Step 1: Start Agents on Each Host

On **host1** (192.168.1.101):
```bash
./sai3bench-agent --listen 0.0.0.0:7761
```

On **host2** (192.168.1.102):
```bash
./sai3bench-agent --listen 0.0.0.0:7761
```

On **host3** (192.168.1.103):
```bash
./sai3bench-agent --listen 0.0.0.0:7761
```

On **host4** (192.168.1.104):
```bash
./sai3bench-agent --listen 0.0.0.0:7761
```

**Note**: Agent IDs are optional but recommended for easier identification in results.

### Step 2: Run Controller with Config

From your **controller host**:
```bash
./sai3bench-ctl \
  --agents 192.168.1.101:7761,192.168.1.102:7761,192.168.1.103:7761,192.168.1.104:7761 \
  run --config tests/configs/distributed_mixed_test.yaml
```

### Step 3: Review Results

Results are automatically saved to timestamped directory:
```
sai3-20251011-1430-distributed_mixed_test/
├── config.yaml              # Your workload config
├── console.log              # Complete execution log
├── metadata.json            # Test metadata (4 agents, start/end times)
├── results.tsv              # CONSOLIDATED metrics (merged histograms)
└── agents/
    ├── host1/
    │   ├── metadata.json
    │   ├── results.tsv      # Host1's individual results
    │   └── agent_local_path.txt
    ├── host2/...
    ├── host3/...
    └── host4/...
```

**Key file**: `results.tsv` at the top level contains **mathematically accurate** aggregate metrics.

## Common Patterns

### Pattern 1: Shared Storage Testing (S3/Azure/GCS)

**Config**: Point all agents at same bucket
```yaml
target: "s3://my-bucket/test-data/"
duration: 60s
concurrency: 32

workload:
  - op: get
    path: "objects/*"
    weight: 70
  - op: put
    path: "objects/"
    weight: 30
    size_distribution:
      type: lognormal
      mean: 1048576
      std_dev: 524288
```

**Agent Setup**: No special configuration needed - all agents share the same S3 bucket.

**Use Case**: Test cloud storage performance at scale, simulate production load patterns.

### Pattern 2: Independent Storage Testing (Local/DirectIO)

**Config**: Use agent-specific path prefixes
```yaml
target: "file:///data/test/"
duration: 30s
concurrency: 16

prepare:
  ensure_objects:
    - base_uri: "file:///data/test/objects/"
      count: 1000
      min_size: 4096
      max_size: 4096

workload:
  - op: get
    path: "objects/*"
    weight: 100
```

**Agent Behavior**: Each agent automatically creates subdirectories:
- host1 → `/data/test/host1/objects/`
- host2 → `/data/test/host2/objects/`
- etc.

**Use Case**: Test local storage performance, filesystem limits, or independent workloads.

### Pattern 3: Scaling to Many Hosts

For **10+ agents**, use a file to manage the agent list:

**agents.txt**:
```
192.168.1.101:7761
192.168.1.102:7761
192.168.1.103:7761
192.168.1.104:7761
192.168.1.105:7761
```

**Controller command**:
```bash
AGENTS=$(paste -sd, agents.txt)
./sai3bench-ctl --agents "$AGENTS" run --config my-workload.yaml
```

Or use a loop to generate the list:
```bash
# Generate agent list for hosts 1-20
AGENTS=$(for i in {1..20}; do echo "192.168.1.$((100+i)):7761"; done | paste -sd,)
./sai3bench-ctl --agents "$AGENTS" run --config my-workload.yaml
```

## Example: 5-Host Cloud Storage Test

### Scenario
Test Azure Blob Storage performance with 5 agents generating mixed read/write load.

### Setup Script (run_5host_azure_test.sh)
```bash
#!/bin/bash
set -e

# Configuration
AGENTS="
azure-worker1.example.com:7761,
azure-worker2.example.com:7761,
azure-worker3.example.com:7761,
azure-worker4.example.com:7761,
azure-worker5.example.com:7761
"

CONFIG="configs/azure_production_workload.yaml"
RESULTS_DIR="azure-tests-$(date +%Y%m%d-%H%M)"

echo "=== 5-Host Azure Blob Storage Test ==="
echo "Agents: 5 hosts"
echo "Config: $CONFIG"
echo ""

# Test connectivity
echo "Testing agent connectivity..."
./sai3bench-ctl --agents "$AGENTS" ping

# Run workload
echo ""
echo "Starting distributed workload..."
./sai3bench-ctl --agents "$AGENTS" run --config "$CONFIG"

echo ""
echo "✅ Test complete! Results in: $(ls -td sai3-* | head -1)"
```

### Azure Config (azure_production_workload.yaml)
```yaml
target: "az://mystorageaccount/benchmark-data/"
duration: 300s  # 5 minutes
concurrency: 32

prepare:
  ensure_objects:
    - base_uri: "az://mystorageaccount/benchmark-data/small/"
      count: 10000
      min_size: 4096
      max_size: 65536
      fill: zero

workload:
  - op: get
    path: "small/*"
    weight: 60
  
  - op: put
    path: "small/"
    weight: 30
    size_distribution:
      type: lognormal
      mean: 16384
      std_dev: 8192
      min: 1024
      max: 131072
  
  - op: list
    path: "small/"
    weight: 10
```

**Expected Results**: With 5 agents @ 32 concurrency each = 160 concurrent operations hitting Azure Blob Storage.

## TLS/SSL for Production

For production environments, use TLS:

### Agent (with TLS)
```bash
./sai3bench-agent \
  --listen 0.0.0.0:7761 \
  --tls \
  --tls-cert /etc/sai3bench/server.crt \
  --tls-key /etc/sai3bench/server.key \
  --tls-domain agent1.example.com
```

### Controller (with TLS)
```bash
./sai3bench-ctl \
  --agents agent1.example.com:7761,agent2.example.com:7761 \
  --agent-ca /etc/sai3bench/ca.crt \
  --agent-domain agent1.example.com \
  run --config workload.yaml
```

## Tips & Best Practices

1. **Start Small**: Test with 2 agents first, then scale up
2. **Check Connectivity**: Always run `ping` command before `run`
3. **Monitor Resources**: Watch CPU/network on agent hosts during tests
4. **Use Meaningful IDs**: Set `id` field in config YAML for easier debugging
5. **Save Results**: Results directories are timestamped and self-contained
6. **Review Logs**: Check `console.log` in results directory for any errors
7. **Histogram Accuracy**: The consolidated `results.tsv` uses proper histogram merging, not simple averaging of percentiles

## Scale-Out vs Scale-Up Testing

sai3-bench supports both horizontal (scale-out) and vertical (scale-up) scaling strategies.

### Scale-Out: Multiple VMs, One Container Each

**Pattern**: Deploy agents across multiple smaller VMs for network bandwidth and fault isolation.

```yaml
# 8 VMs, 1 container per VM
distributed:
  agents:
    - { address: "vm1.example.com:7761", id: "agent-1" }
    - { address: "vm2.example.com:7761", id: "agent-2" }
    - { address: "vm3.example.com:7761", id: "agent-3" }
    - { address: "vm4.example.com:7761", id: "agent-4" }
    - { address: "vm5.example.com:7761", id: "agent-5" }
    - { address: "vm6.example.com:7761", id: "agent-6" }
    - { address: "vm7.example.com:7761", id: "agent-7" }
    - { address: "vm8.example.com:7761", id: "agent-8" }
```

**Advantages**:
- 8× network bandwidth (each VM has own network interface)
- Better NUMA locality
- Fault isolation (one VM crash doesn't affect others)
- Geographically distributed testing

**Use Cases**: Multi-region benchmarks, high-throughput network testing

### Scale-Up: One Large VM, Multiple Containers

**Pattern**: Deploy multiple agent containers on a single powerful VM to maximize CPU utilization.

```yaml
# 1 big VM, 8 containers with different ports
distributed:
  agents:
    - { address: "big-vm.example.com:7761", id: "c1", listen_port: 7761 }
    - { address: "big-vm.example.com:7762", id: "c2", listen_port: 7762 }
    - { address: "big-vm.example.com:7763", id: "c3", listen_port: 7763 }
    - { address: "big-vm.example.com:7764", id: "c4", listen_port: 7764 }
    - { address: "big-vm.example.com:7765", id: "c5", listen_port: 7765 }
    - { address: "big-vm.example.com:7766", id: "c6", listen_port: 7766 }
    - { address: "big-vm.example.com:7767", id: "c7", listen_port: 7767 }
    - { address: "big-vm.example.com:7768", id: "c8", listen_port: 7768 }
  
  deployment:
    network_mode: "host"  # All containers share host network on different ports
```

**How It Works**:
- Each container has unique `listen_port` (7761-7768)
- Container names are unique: `sai3bench-agent-c1`, `sai3bench-agent-c2`, etc.
- All containers bind to different ports on same host
- Controller connects to `big-vm:7761`, `big-vm:7762`, etc.

**Advantages**:
- Lower network latency (no inter-VM communication)
- Simpler deployment (one VM to manage)
- Often more cost-effective (1 large VM < 8 small VMs)
- Easier resource monitoring

**Use Cases**: Single-region testing, CPU-bound workloads, cost optimization

### Comparison Example

Test the same workload with both strategies to understand performance characteristics:

**Scale-Out Config** (`scale-out-8vms.yaml`):
```yaml
# 8 VMs: n2-standard-8 (8 vCPU, 32 GB each)
# Total: 64 vCPU, 256 GB RAM, 8 network interfaces
target: "gs://my-bucket/data/"
concurrency: 32  # Per agent = 256 total

distributed:
  agents:
    - { address: "vm1:7761", id: "agent-1" }
    # ... vm2-vm8
```

**Scale-Up Config** (`scale-up-1vm.yaml`):
```yaml
# 1 VM: n2-standard-64 (64 vCPU, 256 GB)
# Total: 64 vCPU, 256 GB RAM, 1 network interface
target: "gs://my-bucket/data/"
concurrency: 32  # Per container = 256 total

distributed:
  agents:
    - { address: "big-vm:7761", id: "c1", listen_port: 7761 }
    - { address: "big-vm:7762", id: "c2", listen_port: 7762 }
    # ... c3-c8 (8 containers total)
```

**Both generate equivalent application-level load** (256 concurrent operations), but differ in:
- Network topology (8 NICs vs 1 NIC)
- Memory architecture (distributed NUMA vs single NUMA)
- Fault domain (8 VMs vs 1 VM)
- Cost profile (typically 1 large VM is 10-20% cheaper)

## Using Podman Instead of Docker

sai3-bench supports both Docker and Podman as container runtimes. Specify via `container_runtime` field:

```yaml
distributed:
  deployment:
    deploy_type: "docker"
    container_runtime: "podman"  # Default: "docker"
    image: "sai3bench:v0.6.11"
    network_mode: "host"
```

**Podman advantages**:
- Rootless containers (better security)
- No daemon required
- Drop-in Docker replacement
- Better for rootless CI/CD

**Setup on VMs**:
```bash
# Ubuntu/Debian
sudo apt-get install -y podman

# RHEL/Fedora/CentOS
sudo dnf install -y podman

# Verify
podman --version
```

All SSH automation, deployment, and cleanup commands automatically use the specified runtime.

## Troubleshooting

**Problem**: Agent connection refused
```
ERROR Failed to connect to agent 192.168.1.101:7761
```
**Solution**: 
- Check agent is running: `ps aux | grep sai3bench-agent`
- Verify firewall allows port 7761
- Test with `telnet 192.168.1.101 7761`

**Problem**: Workload fails on some agents
```
ERROR Agent host2 failed: Failed to get object from URI
```
**Solution**:
- Check agent logs on that specific host
- For local storage, ensure paths exist and have write permissions
- For cloud storage, verify credentials are set on ALL agent hosts

**Problem**: Results don't aggregate correctly
```
WARNING Consolidated count doesn't match sum of agents
```
**Solution**: This should not happen with v0.6.4+. If it does:
- Check that all agents reported results
- Review `metadata.json` to see which agents completed
- File a bug report with logs

## Workload Scaling Guidelines

| Agents | Total Concurrency* | Use Case |
|--------|-------------------|----------|
| 1 | 16-64 | Development, local testing |
| 2-4 | 32-256 | Small-scale validation |
| 5-10 | 80-640 | Medium-scale load testing |
| 10-20 | 160-1280 | Large-scale performance testing |
| 20+ | 320+ | Stress testing, capacity planning |

*Assuming concurrency: 16-64 per agent in config

## Next Steps

- Review [CONFIG.sample.yaml](CONFIG.sample.yaml) for all configuration options
- See [USAGE.md](USAGE.md) for single-node testing workflows
- Check [CHANGELOG.md](CHANGELOG.md) for v0.6.4 distributed features
