# Distributed Testing Guide

Complete guide to running distributed I/O benchmarks with sai3-bench across multiple hosts.

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Core Concepts](#core-concepts)
3. [Quick Start](#quick-start)
4. [Configuration Examples](#configuration-examples)
5. [Multi-Endpoint Testing](#multi-endpoint-testing)
6. [Advanced Patterns](#advanced-patterns)
7. [Troubleshooting](#troubleshooting)

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                     CONTROLLER (sai3bench-ctl)                  │
│  • Parses workload config                                       │
│  • Coordinates distributed test execution                       │
│  • Aggregates results from all agents                          │
│  • Generates performance reports                               │
└───────────┬────────────┬────────────┬───────────────────────────┘
            │            │            │
       gRPC │       gRPC │       gRPC │
            │            │            │
┌───────────▼────┐  ┌────▼────────┐  ┌▼─────────────┐
│ AGENT 1        │  │ AGENT 2     │  │ AGENT N      │
│ sai3bench-agent│  │sai3bench-   │  │sai3bench-    │
│                │  │agent        │  │agent         │
│ • Executes I/O │  │             │  │              │
│ • Collects     │  │             │  │              │
│   metrics      │  │             │  │              │
│ • Streams      │  │             │  │              │
│   stats        │  │             │  │              │
└────────────────┘  └─────────────┘  └──────────────┘
       │                   │                 │
       └───────────────────┴─────────────────┘
                          │
                  Storage Backend(s)
           (S3, Azure, GCS, File, Direct I/O)
```

### Components

**Controller (`sai3bench-ctl`)**
- Runs on a single host (can be your laptop or one of the test VMs)
- Orchestrates the entire test workflow
- Communicates with agents via gRPC (bidirectional streaming)
- Aggregates results and generates reports

**Agents (`sai3bench-agent`)**
- Run on each test host (VMs, bare metal, containers)
- Execute actual I/O operations
- Collect per-host performance metrics
- Stream live stats to controller
- Can be started manually, via SSH, or in containers

### Communication Flow

1. **Setup Phase**: Controller connects to all agents via gRPC
2. **Config Distribution**: Controller sends workload config to each agent
3. **Prepare Phase**: Agents create test data (if specified in config)
4. **Coordinated Start**: Controller synchronizes start time across all agents
5. **Execution**: Agents execute I/O operations, stream stats every 100ms
6. **Completion**: Agents send final results to controller
7. **Aggregation**: Controller combines results and writes reports

---

## Core Concepts

### Shared vs Independent Filesystems

**Shared Filesystem (`shared_filesystem: true`)**
- All agents access the SAME data
- Common in distributed storage testing (NAS, parallel filesystems, object stores)
- File creation is distributed (each agent creates subset of files)
- Example: Multiple VMs accessing same S3 bucket

**Independent Filesystem (`shared_filesystem: false`)**
- Each agent creates and accesses its OWN data
- Common in local storage testing (DAS, node-local SSDs)
- Each agent creates full dataset independently
- Example: Testing local SSDs on multiple hosts

### Tree Creation Modes

**Coordinator Mode (`tree_creation_mode: coordinator`)**
- Controller creates directory tree before test starts
- Agents only create files (distributed across agents)
- **Use when**: Testing object stores or shared filesystems where directory structure matters

**Agent Mode (`tree_creation_mode: agent`)**
- Each agent creates its own directory tree
- **Use when**: Testing independent filesystems or when agents need different tree structures

**Controller Mode (`tree_creation_mode: controller`)**
- Controller creates EVERYTHING (tree + files) before agents start
- Agents only perform workload operations
- **Use when**: Testing read-only workloads or specific data layouts

### Path Selection Strategies

**Random (`path_selection: random`)**
- Each agent randomly selects from available files
- Simulates independent clients accessing shared data
- **Use when**: Modeling multi-user workloads

**Distributed (`path_selection: distributed`)**
- Each agent accesses a specific subset of files
- No overlap between agents
- **Use when**: Maximizing aggregate throughput or avoiding contention

### Agent ID Assignment

**Explicit IDs (Recommended)**
```yaml
distributed:
  agents:
    - address: "host1:7761"
      id: "agent-us-east-1a"  # Descriptive ID
    
    - address: "host2:7761"
      id: "agent-us-east-1b"
```

**Auto-generated IDs**
```yaml
distributed:
  agents:
    - address: "host1:7761"  # ID: "agent-0"
    - address: "host2:7761"  # ID: "agent-1"
```

Agent IDs appear in:
- Results directory names (`sai3-*/agents/agent-us-east-1a/`)
- Performance logs
- Console output

---

## Quick Start

### 1. Start Agents

On each test host:

```bash
# Start agent on default port (7761)
sai3bench-agent --listen 0.0.0.0:7761

# Or specify custom port
sai3bench-agent --listen 0.0.0.0:8000

# Run in background
nohup sai3bench-agent --listen 0.0.0.0:7761 > agent.log 2>&1 &
```

**Verify agents are listening:**
```bash
# Check agent log
tail -f agent.log
# Should show: "sai3bench-agent listening (PLAINTEXT) on 0.0.0.0:7761"

# Test from controller
telnet host1 7761  # Should connect
```

### 2. Create Workload Config

Create `distributed-test.yaml`:

```yaml
# Basic distributed test
target: "file:///tmp/benchmark-data/"
duration: 60s
concurrency: 16

# Distributed configuration
distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  
  agents:
    - address: "host1:7761"
      id: "agent-1"
    
    - address: "host2:7761"
      id: "agent-2"
    
    - address: "host3:7761"
      id: "agent-3"

# Prepare test data
prepare:
  tree:
    base_uri: "file:///tmp/benchmark-data/"
    width: 4
    depth: 3
    files_per_dir: 50
    size_spec:
      uniform:
        min: 1MB
        max: 10MB

# Workload operations
workload:
  - op: get
    path: "*/*.dat"
    weight: 70
  
  - op: put
    path: "uploads/new-{}.dat"
    object_size: 5MB
    weight: 20
  
  - op: list
    path: "*"
    weight: 10
```

### 3. Run Distributed Test

```bash
sai3bench-ctl run --config distributed-test.yaml
```

**Expected output:**
```
=== Distributed Workload ===
Config: distributed-test.yaml
Agents: 3
Start delay: 2s
Storage mode: shared (all agents access same data)

Starting workload on 3 agents with live stats...

⏳ Waiting for agents to validate configuration...
✅ agent-1 ready
✅ agent-2 ready
✅ agent-3 ready
✅ All 3 agents ready - starting workload execution

⏳ Starting in 2s...
✅ Starting workload now!

████████████████████████████████████████      60/60      s
✓ All 3 agents completed

=== Distributed Results ===
Total agents: 3

--- Agent: agent-1 ---
  Wall time: 60.02s
  Total ops: 45231 (753.65 ops/s)
  Total bytes: 226155.00 MB (3766.64 MiB/s)
  ...

--- Agent: agent-2 ---
  Wall time: 60.01s
  Total ops: 46012 (766.76 ops/s)
  Total bytes: 230060.00 MB (3831.76 MiB/s)
  ...

--- Agent: agent-3 ---
  Wall time: 60.03s
  Total ops: 44789 (746.03 ops/s)
  Total bytes: 223945.00 MB (3729.53 MiB/s)
  ...

=== Aggregate Totals ===
Total ops: 136032 (2266.44 ops/s)
Total bytes: 680160.00 MB (11327.93 MiB/s)

✅ Distributed workload complete!
Results saved to: ./sai3-20260129-1534-distributed_test
```

---

## Configuration Examples

All examples use verified YAML files from `tests/configs/`.

### Example 1: Basic Local Testing (2 Agents)

**File**: [`tests/configs/local_test_2agents.yaml`](../tests/configs/local_test_2agents.yaml)

Tests file I/O with 2 local agents on different ports:

```yaml
target: "file:///tmp/sai3-test/"
duration: 30s
concurrency: 8

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  
  agents:
    - address: "127.0.0.1:7761"
      id: "test-agent-1"
    
    - address: "127.0.0.1:7762"
      id: "test-agent-2"

prepare:
  tree:
    base_uri: "file:///tmp/sai3-test/"
    width: 4
    depth: 2
    files_per_dir: 20
    size_spec:
      uniform:
        min: 1MB
        max: 5MB

workload:
  - op: get
    path: "*/*.dat"
    weight: 60
  
  - op: put
    path: "new/file-{}.dat"
    object_size: 2MB
    weight: 30
  
  - op: list
    path: "*"
    weight: 10
```

**Run:**
```bash
# Start 2 agents locally
./scripts/start_local_agents.sh 2

# Run test
sai3bench-ctl run --config tests/configs/local_test_2agents.yaml
```

**Use case**: Development testing, CI/CD validation, local performance testing.

### Example 2: Mixed Operations Test

**File**: [`tests/configs/distributed_mixed_test.yaml`](../tests/configs/distributed_mixed_test.yaml)

Tests multiple file sizes with mixed operations:

```yaml
target: "file:///tmp/sai3bench-test/"
duration: 3s
concurrency: 16

prepare:
  ensure_objects:
    # Small files (1KB)
    - base_uri: "file:///tmp/sai3bench-test/mixed/"
      count: 100
      size_spec:
        fixed: 1KB
      fill: random
    
    # Medium files (128KB)
    - base_uri: "file:///tmp/sai3bench-test/mixed/"
      count: 50
      size_spec:
        fixed: 128KB
      fill: random
    
    # Large files (1MB)
    - base_uri: "file:///tmp/sai3bench-test/mixed/"
      count: 30
      size_spec:
        fixed: 1MB
      fill: random

workload:
  - op: get
    path: "mixed/*"
    weight: 40
  
  - op: put
    path: "mixed/new-{}.dat"
    size_spec:
      uniform:
        min: 1KB
        max: 64KB
    weight: 30
  
  - op: list
    path: "mixed/"
    weight: 20
  
  - op: stat
    path: "mixed/*"
    weight: 10
```

**Use case**: Realistic workload simulation with varied object sizes.

---

## Multi-Endpoint Testing

**Critical use case**: Testing scenarios where different clients access different storage endpoints.

### Architecture: Per-Agent Endpoint Assignment

```
┌─────────────┐
│ CONTROLLER  │
└──────┬──────┘
       │
       ├──────────────────────────────────┐
       │                                  │
   Agent-1                            Agent-2
   Endpoints: A & B                   Endpoints: C & D
       │                                  │
       ├─────────┬─────────               ├─────────┬─────────
       │         │                        │         │
   Endpoint-A  Endpoint-B            Endpoint-C  Endpoint-D
   (S3 us-e1)  (S3 us-e1)           (S3 us-w2)  (S3 us-w2)
```

### Example 3: Multi-Endpoint Prepare Phase

**File**: [`tests/configs/multi_endpoint_prepare.yaml`](../tests/configs/multi_endpoint_prepare.yaml)

Creates test data with per-agent endpoint assignments:

```yaml
duration: 0s  # Prepare only, no workload
concurrency: 8

# Global multi-endpoint (fallback)
multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file:///tmp/sai3-multi-ep-test/endpoint-a/"
    - "file:///tmp/sai3-multi-ep-test/endpoint-b/"
    - "file:///tmp/sai3-multi-ep-test/endpoint-c/"
    - "file:///tmp/sai3-multi-ep-test/endpoint-d/"

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  
  agents:
    # Agent 1: Uses ONLY endpoints A & B
    - address: "localhost:7761"
      id: "agent-dc-a"
      concurrency_override: 8
      multi_endpoint:
        endpoints:
          - "file:///tmp/sai3-multi-ep-test/endpoint-a/"
          - "file:///tmp/sai3-multi-ep-test/endpoint-b/"
        strategy: round_robin
    
    # Agent 2: Uses ONLY endpoints C & D
    - address: "localhost:7762"
      id: "agent-dc-b"
      concurrency_override: 8
      multi_endpoint:
        endpoints:
          - "file:///tmp/sai3-multi-ep-test/endpoint-c/"
          - "file:///tmp/sai3-multi-ep-test/endpoint-d/"
        strategy: least_connections
  
  start_delay: 2

prepare:
  ensure_objects:
    - base_uri: "testdata/"
      count: 100
      size_spec:
        fixed: 512KB\n      fill: random     # Use random for realistic testing
```

**Key points:**
- Global `multi_endpoint` defines all 4 endpoints (fallback)
- Agent-1 override: ONLY endpoints A & B (round_robin)
- Agent-2 override: ONLY endpoints C & D (least_connections)
- Each agent creates 50 files (distributed prepare)
- Files are created in agent's assigned endpoints only

**Run:**
```bash
# Phase 1: Prepare (creates data in per-agent endpoints)
sai3bench-ctl run --config tests/configs/multi_endpoint_prepare.yaml

# Phase 2: Manual replication (simulate shared storage)
for src in /tmp/sai3-multi-ep-test/endpoint-{a,b,c,d}; do
  for dst in /tmp/sai3-multi-ep-test/endpoint-{a,b,c,d}; do
    [ "$src" != "$dst" ] && cp -n $src/testdata/*.dat $dst/testdata/ 2>/dev/null || true
  done
done
```

**Verify endpoint isolation:**
```bash
# Check per-agent endpoint stats
cat sai3-*/agents/agent-dc-a/prepare_endpoint_stats.tsv
# Should show: ONLY endpoints A & B (50/50 split)

cat sai3-*/agents/agent-dc-b/prepare_endpoint_stats.tsv
# Should show: ONLY endpoints C & D (50/50 split)
```

### Example 4: Multi-Endpoint Workload Test

**File**: [`tests/configs/multi_endpoint_workload.yaml`](../tests/configs/multi_endpoint_workload.yaml)

Tests I/O operations with per-agent endpoint isolation:

```yaml
duration: 20s
concurrency: 8

# Global multi-endpoint (fallback)
multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file:///tmp/sai3-multi-ep-test/endpoint-a/"
    - "file:///tmp/sai3-multi-ep-test/endpoint-b/"
    - "file:///tmp/sai3-multi-ep-test/endpoint-c/"
    - "file:///tmp/sai3-multi-ep-test/endpoint-d/"

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  
  agents:
    # Agent 1: ONLY endpoints A & B
    - address: "localhost:7761"
      id: "agent-dc-a"
      concurrency_override: 8
      multi_endpoint:
        endpoints:
          - "file:///tmp/sai3-multi-ep-test/endpoint-a/"
          - "file:///tmp/sai3-multi-ep-test/endpoint-b/"
        strategy: round_robin
    
    # Agent 2: ONLY endpoints C & D
    - address: "localhost:7762"
      id: "agent-dc-b"
      concurrency_override: 8
      multi_endpoint:
        endpoints:
          - "file:///tmp/sai3-multi-ep-test/endpoint-c/"
          - "file:///tmp/sai3-multi-ep-test/endpoint-d/"
        strategy: least_connections
  
  start_delay: 2

# Workload operations (Modified: More reads, fewer lists)
workload:
  - op: get
    path: "testdata/*"
    use_multi_endpoint: true
    weight: 75
  
  - op: put
    path: "testdata/newfile-{}.dat"
    object_size: 524288
    use_multi_endpoint: true
    weight: 20
  
  - op: list
    path: "testdata/"
    use_multi_endpoint: true
    weight: 5

# Performance logging
perf_log:
  enabled: true
  path: "perf_log.tsv"
  interval_seconds: 1
```

**Run:**
```bash
# Must complete prepare phase first (Example 3)
# Then run workload test
sai3bench-ctl run --config tests/configs/multi_endpoint_workload.yaml
```

**Verify endpoint isolation:**
```bash
# Check per-agent endpoint stats
cat sai3-*/agents/agent-dc-a/workload_endpoint_stats.tsv
# Should show: ONLY endpoints A & B (~50/50 split)

cat sai3-*/agents/agent-dc-b/workload_endpoint_stats.tsv
# Should show: ONLY endpoints C & D (~50/50 split)
```

**Output includes endpoint statistics:**
```
sai3-20260129-2221-multi_endpoint_workload/
├── results.tsv                    # Aggregate results
├── perf_log.tsv                   # Time-series performance
├── workload_endpoint_stats.tsv    # Aggregated endpoint stats
└── agents/
    ├── agent-dc-a/
    │   ├── results.tsv
    │   ├── perf_log.tsv
    │   └── workload_endpoint_stats.tsv  # ONLY A & B
    └── agent-dc-b/
        ├── results.tsv
        ├── perf_log.tsv
        └── workload_endpoint_stats.tsv  # ONLY C & D
```

### Real-World Multi-Endpoint Use Cases

**1. Multi-Region Cloud Testing**
```yaml
# Agent in US East accesses S3 us-east-1
# Agent in EU West accesses S3 eu-west-1
distributed:
  agents:
    - address: "us-east-vm:7761"
      id: "agent-us-east-1"
      multi_endpoint:
        endpoints:
          - "s3://my-bucket-us-east-1/data/"
        strategy: round_robin
    
    - address: "eu-west-vm:7761"
      id: "agent-eu-west-1"
      multi_endpoint:
        endpoints:
          - "s3://my-bucket-eu-west-1/data/"
        strategy: round_robin
```

**2. Multi-Account Testing**
```yaml
# Different agents use different cloud accounts/credentials
# Agent 1 uses account A endpoints
# Agent 2 uses account B endpoints
```

**3. Load Balancing Across MinIO Instances**
```yaml
# Each agent talks to specific MinIO nodes
distributed:
  agents:
    - address: "client1:7761"
      multi_endpoint:
        endpoints:
          - "s3://minio-node1:9000/bucket/"
          - "s3://minio-node2:9000/bucket/"
        strategy: round_robin
    
    - address: "client2:7761"
      multi_endpoint:
        endpoints:
          - "s3://minio-node3:9000/bucket/"
          - "s3://minio-node4:9000/bucket/"
        strategy: least_connections
```

---

## Advanced Patterns

### Per-Agent Concurrency Override

Different agents can run with different thread counts:

```yaml
distributed:
  agents:
    - address: "powerful-vm:7761"
      id: "agent-high-perf"
      concurrency_override: 64  # 64 threads
    
    - address: "small-vm:7761"
      id: "agent-low-perf"
      concurrency_override: 8   # 8 threads
```

Global concurrency is ignored when override is set.

### Coordinated Start Delay

Synchronize agent start times:

```yaml
distributed:
  start_delay: 5  # All agents start in 5 seconds

  agents:
    - address: "host1:7761"
    - address: "host2:7761"
```

Controller calculates synchronized start time across all agents.

### SSH Auto-Deployment

Controller can SSH to hosts and start agents automatically:

```yaml
distributed:
  ssh:
    enabled: true
    user: ubuntu
    key_path: /home/user/.ssh/sai3bench_id_rsa
  
  agents:
    - address: "host1.example.com"
      id: "agent-1"
    
    - address: "host2.example.com"
      id: "agent-2"
```

Controller will:
1. SSH to each host
2. Start `sai3bench-agent` in background
3. Connect via gRPC
4. Stop agents when test completes

**Setup:**
```bash
# Automated setup
sai3bench-ctl ssh-setup --hosts ubuntu@host1,ubuntu@host2

# Or manual
ssh-keygen -t rsa -f ~/.ssh/sai3bench_id_rsa -N ""
ssh-copy-id -i ~/.ssh/sai3bench_id_rsa.pub ubuntu@host1
```

---

## Troubleshooting

### Agent Connection Failures

**Symptom**: Controller can't connect to agent

**Check 1: Agent is running**
```bash
# On agent host
ps aux | grep sai3bench-agent
```

**Check 2: Port is listening**
```bash
# On agent host
netstat -tlnp | grep 7761
# Or
lsof -i :7761
```

**Check 3: Network connectivity**
```bash
# From controller host
telnet agent-host 7761
# Should connect
```

**Check 4: Firewall rules**
```bash
# Allow port 7761 inbound on agent host
sudo ufw allow 7761/tcp

# AWS Security Group: Add inbound rule
# Type: Custom TCP, Port: 7761, Source: <controller-sg-id>
```

### Agent Ready Timeout

**Symptom**: `⏳ 0/N agents ready, N remaining (40s timeout remaining)`

**Cause**: Agent is running but config validation failed

**Check agent logs:**
```bash
# Agent console output
docker logs sai3-agent

# Look for errors like:
# - "Failed to parse config"
# - "Invalid URI"
# - "Permission denied"
```

**Common fixes:**
- Verify cloud credentials are set (AWS_ACCESS_KEY_ID, etc.)
- Check URI syntax in config
- Ensure agent has write permissions to target directory

### Inconsistent Results Across Agents

**Symptom**: One agent much slower than others

**Check 1: Network latency**
```bash
# From slow agent to storage
ping s3.amazonaws.com
```

**Check 2: CPU/Memory resources**
```bash
# During test
htop
# or
docker stats sai3-agent
```

**Check 3: Per-agent results**
```bash
# Compare per-agent metrics
cat sai3-*/agents/agent-1/results.tsv
cat sai3-*/agents/agent-2/results.tsv
```

### Endpoint Stats Showing Wrong Endpoints

**Symptom**: Agent accessing endpoints it shouldn't

**Verify config:**
```bash
# Dry-run to see what each agent gets
sai3bench-ctl run --config test.yaml --dry-run | grep -A 10 "Agent List"
```

**Check per-agent endpoint stats:**
```bash
# Should show ONLY assigned endpoints
cat sai3-*/agents/agent-1/workload_endpoint_stats.tsv
```

**Fix**: Ensure `multi_endpoint` is set in agent config (not just global)

### Results Directory Not Created

**Symptom**: Controller completes but no results directory

**Check**: Controller output for "Results saved to:" message

**Common causes:**
- Controller doesn't have write permission in current directory
- Test failed during execution (check error messages)

**Verify:**
```bash
ls -ld sai3-*
```

---

## Performance Analysis Tools

### sai3-analyze: Compare Multiple Test Runs

Compare distributed test results in Excel:

```bash
# Generate comparison spreadsheet
sai3-analyze --dirs sai3-run1,sai3-run2,sai3-run3 --output comparison.xlsx
```

**Output includes:**
- Results tabs for each run
- Performance logs (time-series)
- Endpoint statistics (multi-endpoint tests)
- Timestamps formatted as readable dates

**Usage:**
```bash
# Compare two multi-endpoint tests
sai3-analyze \
  --dirs sai3-20260129-2221-multi_endpoint_workload,sai3-20260129-2223-multi_endpoint_workload \
  --output multi_endpoint_comparison.xlsx

# Open in Excel/LibreOffice to visualize trends
```

### Performance Log Analysis

**Time-series performance data:**
```bash
# View aggregate performance over time
cat sai3-*/perf_log.tsv | column -t

# Per-agent performance
cat sai3-*/agents/agent-1/perf_log.tsv | column -t
```

**Columns include** (30 total):
- `timestamp_epoch_ms` - Millisecond timestamp
- `elapsed_s` - Seconds since test start
- `get_iops`, `put_iops`, `meta_iops` - Operations per second
- `get_mbps`, `put_mbps` - Throughput in MiB/s
- `get_p50_us`, `get_p90_us`, `get_p99_us` - Latency percentiles
- `cpu_user_pct`, `cpu_system_pct`, `cpu_iowait_pct` - CPU utilization
- `errors` - Error count

---

## See Also

- [USAGE.md](USAGE.md) - Complete workload configuration syntax
- [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) - Configuration file reference
- [DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md) - SSH and container deployment
- [CLOUD_STORAGE_SETUP.md](CLOUD_STORAGE_SETUP.md) - Cloud storage credentials
- [ANALYZE_TOOL.md](ANALYZE_TOOL.md) - Results analysis and visualization
- [PERF_LOG_FORMAT.md](PERF_LOG_FORMAT.md) - Performance log format specification
