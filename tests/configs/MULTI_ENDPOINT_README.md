# Multi-Endpoint Configuration Examples

These configuration files demonstrate the multi-endpoint load balancing feature added in v0.8.22 and improved in v0.8.96.

> **Full guide**: [docs/S3_MULTI_ENDPOINT_GUIDE.md](../../docs/S3_MULTI_ENDPOINT_GUIDE.md) — architecture comparison, distributed setup, troubleshooting, and expected results.

## How to Use Multiple Endpoints

There are **two ways** to target multiple S3/object-storage endpoints in sai3-bench:

---

### Method 1: YAML `multi_endpoint:` block (recommended)

Add a `multi_endpoint:` block to your config file. All operations — PUT, GET, LIST, STAT, and DELETE — are automatically load-balanced across the listed endpoints. No per-operation flags are needed.

```yaml
# Distribute all I/O across 4 storage nodes using round-robin
multi_endpoint:
  strategy: round_robin      # or: least_connections
  endpoints:
    - "s3://10.9.0.17:80/my-bucket/data/"
    - "s3://10.9.0.18:80/my-bucket/data/"
    - "s3://10.9.0.19:80/my-bucket/data/"
    - "s3://10.9.0.20:80/my-bucket/data/"

concurrency: 128
duration: 60s

workload:
  - op: put
    object_size: 1048576    # 1 MiB
    weight: 100
```

**Key points**:
- The `path:` field under each workload op is **optional** when `multi_endpoint:` is present — the path embedded in each endpoint URI is used.
- A single-element `endpoints:` list works identically to a plain `target:` URI (no load balancing overhead).
- `strategy: round_robin` cycles through endpoints sequentially (predictable, even distribution).
- `strategy: least_connections` routes to the endpoint with the fewest active requests (best for heterogeneous latency).
- Maximum 32 endpoints per config (set by `s3dlio::constants::MAX_ENDPOINTS`).

**Required environment variables** (credentials, not endpoints):
```bash
export AWS_ACCESS_KEY_ID=your_key
export AWS_SECRET_ACCESS_KEY=your_secret
export AWS_REGION=us-east-1
# Do NOT set AWS_ENDPOINT_URL — the host is encoded in the endpoint URIs above
```

---

### Method 2: `S3_ENDPOINT_URIS` environment variable (v0.8.96+)

Set `S3_ENDPOINT_URIS` to a comma-separated list of fully-qualified URIs. This enables multi-endpoint load balancing at **runtime without modifying any YAML file**.

```bash
export S3_ENDPOINT_URIS="s3://10.9.0.17:80/my-bucket/data/,s3://10.9.0.18:80/my-bucket/data/,s3://10.9.0.19:80/my-bucket/data/"
export AWS_ACCESS_KEY_ID=your_key
export AWS_SECRET_ACCESS_KEY=your_secret
export AWS_REGION=us-east-1

# Run any existing single-endpoint config — it will automatically use all 3 endpoints
sai3-bench run --config tests/configs/sai3_put_100k-1k.yaml
```

**Priority rules** (highest wins):
1. **YAML `multi_endpoint:` block** — always takes precedence over env vars
2. **`S3_ENDPOINT_URIS`** — used when no YAML block present; round-robin strategy
3. **`AWS_ENDPOINT_URL`** — single-endpoint fallback (existing behavior)
4. **AWS SDK default** — `s3.amazonaws.com`

At startup, sai3-bench prints a clear banner showing which endpoint source is active:
```
╔═══════════════════════════════════════════════════════════════════════╗
║  ✅  NO multi_endpoint YAML — using S3_ENDPOINT_URIS fallback        ║
╠═══════════════════════════════════════════════════════════════════════╣
║  Endpoints (3):                                                       ║
║    [0] s3://10.9.0.17:80/my-bucket/data/                             ║
║    [1] s3://10.9.0.18:80/my-bucket/data/                             ║
║    [2] s3://10.9.0.19:80/my-bucket/data/                             ║
╚═══════════════════════════════════════════════════════════════════════╝
```

**Whitespace is trimmed** — spaces around commas are ignored:
```bash
export S3_ENDPOINT_URIS="s3://node1:80/bucket/ , s3://node2:80/bucket/ , s3://node3:80/bucket/"
```

---

## Overview

Multi-endpoint configuration enables distributing I/O operations across multiple storage endpoints (IPs, mount points) for improved bandwidth utilization and performance.

**Use Cases**:
- Multi-NIC storage systems (VAST, Weka, MinIO clusters)
- NFS with multiple mount points to same filesystem
- Distributed object storage with multiple IP endpoints
- Maximizing bandwidth across bonded NICs

## Configuration Files

### 0. `multi_endpoint_s3_put_4endpoint.yaml` - S3 PUT across 4 nodes (new in v0.8.96)

**Scenario**: Pure PUT benchmark across 4 S3-compatible endpoints (one bucket, round-robin).

**Run**:
```bash
# Set credentials
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
export AWS_REGION=us-east-1

# Edit the endpoint IPs in the YAML to match your cluster, then:
sai3-bench --dry-run run --config tests/configs/multi_endpoint_s3_put_4endpoint.yaml
sai3-bench -v run --config tests/configs/multi_endpoint_s3_put_4endpoint.yaml
```

---

### 1. `multi_endpoint_global.yaml` - Global Shared Endpoint Pool

**Scenario**: All agents round-robin across the same set of 8 S3 endpoints.

**Architecture**:
- 8 storage IPs (192.168.1.10 through .17)
- All agents can access any endpoint
- Simple round-robin load balancing

**Use When**:
- Quick testing with uniform endpoint access
- Agents don't have specific network path constraints
- Simple setup without per-agent optimization

**Run**:
```bash
# Single-node test (uses first endpoint)
./sai3-bench run --config tests/configs/multi_endpoint_global.yaml

# Distributed test (all agents share same 8 endpoints)
./sai3bench-ctl --agents host1:7167,host2:7167,host3:7167,host4:7167 \
  run --config tests/configs/multi_endpoint_global.yaml
```

---

### 2. `multi_endpoint_per_agent.yaml` - Static Per-Agent Mapping ⭐ RECOMMENDED

**Scenario**: 4 test hosts, 8 storage IPs (2 IPs per host) for perfect load distribution.

**Architecture**:
- 4 physical storage nodes with 2 NICs each = 8 total IPs
- 4 test hosts, each with 2 bonded NICs
- Each test host targets 2 specific storage IPs
- **Result**: Every storage IP used by exactly one test host (no overlap)

**Endpoint Mapping**:
| Test Host | Storage IPs | Storage Nodes Hit |
|-----------|-------------|-------------------|
| testhost1 | 192.168.1.10, 192.168.1.12 | Node 1 NIC A, Node 2 NIC A |
| testhost2 | 192.168.1.11, 192.168.1.13 | Node 1 NIC B, Node 2 NIC B |
| testhost3 | 192.168.1.14, 192.168.1.16 | Node 3 NIC A, Node 4 NIC A |
| testhost4 | 192.168.1.15, 192.168.1.17 | Node 3 NIC B, Node 4 NIC B |

**Why This Pattern**:
- Maximum bandwidth utilization (no endpoint contention)
- Each storage node receives traffic from 2 test hosts
- Each test host uses both bonded NICs (2 endpoints)
- Optimal for VAST, Weka, or any multi-NIC storage system

**Run**:
```bash
# Must run with distributed controller (requires 4 agents)
./sai3bench-ctl --agents testhost1:7167,testhost2:7167,testhost3:7167,testhost4:7167 \
  run --config tests/configs/multi_endpoint_per_agent.yaml
```

**Expected Performance**:
- Each test host: ~2x bandwidth (2 bonded NICs × 2 endpoints)
- Total system: 4 test hosts × 2 endpoints = 8 storage IPs fully saturated

---

### 3. `multi_endpoint_nfs.yaml` - NFS Multi-Mount

**Scenario**: Load balancing across 8 NFS mount points with identical namespaces.

**Architecture**:
- 8 NFS mount points: `/mnt/nfs1/` through `/mnt/nfs8/`
- All mount points to same VAST/Weka VIP pool or export
- Same files visible from all mounts

**Important**: This requires identical namespace across all mounts:
```bash
# File written to one mount is visible on all others
echo "test" > /mnt/nfs1/benchmark/data/file.txt
cat /mnt/nfs2/benchmark/data/file.txt  # Same file
cat /mnt/nfs8/benchmark/data/file.txt  # Same file
```

**Use When**:
- Testing NFS performance across multiple client mount points
- VAST or Weka system with multiple VIPs
- Maximizing NFS client bandwidth

**Run**:
```bash
# Single-node test (requires 8 NFS mounts on test host)
./sai3-bench run --config tests/configs/multi_endpoint_nfs.yaml
```

**Prerequisites**:
```bash
# Mount 8 NFS endpoints (example for multi-NIC storage)
mkdir -p /mnt/nfs{1..8}/benchmark
mount -t nfs 192.168.1.10:/export /mnt/nfs1
mount -t nfs 192.168.1.11:/export /mnt/nfs2
# ... etc for all 8 mounts
```

---

## Configuration Schema

### Global Multi-Endpoint

All agents share same endpoint pool:

```yaml
multi_endpoint:
  strategy: round_robin  # or least_connections
  endpoints:
    - s3://192.168.1.10:9000/bucket/
    - s3://192.168.1.11:9000/bucket/
    - ...

workload:
  - op: get
    path: "data/*"
    weight: 100
```

### Per-Agent Static Mapping

Each agent gets specific endpoints:

```yaml
distributed:
  agents:
    - address: "host1:7167"
      id: agent1
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - s3://192.168.1.10:9000/bucket/
          - s3://192.168.1.11:9000/bucket/
    
    - address: "host2:7167"
      id: agent2
      multi_endpoint:
        endpoints:
          - s3://192.168.1.12:9000/bucket/
          - s3://192.168.1.13:9000/bucket/

workload:
  - op: get
    path: "data/*"
    weight: 100
```

## Load Balancing Strategies

### Round Robin (Default)

- Cycles through endpoints sequentially
- Simple, predictable, no overhead
- Best for uniform endpoints

```yaml
multi_endpoint:
  strategy: round_robin
  endpoints: [...]
```

### Least Connections

- Routes to endpoint with fewest active requests
- Adapts to load dynamically
- Slight overhead (atomic counters)
- Best for heterogeneous endpoints

```yaml
multi_endpoint:
  strategy: least_connections
  endpoints: [...]
```

## Requirements

1. **Identical Namespace**: All endpoints must present same files/directories
   - File written via endpoint 1 must be readable via endpoint 2
   - Automatic for VAST, Weka, MinIO clusters

2. **s3dlio v0.9.14+**: Multi-endpoint support requires s3dlio 0.9.14 or later

3. **Supported Backends**: 
   - ✅ S3 (MinIO, Ceph, AWS)
   - ✅ Azure Blob
   - ✅ Google Cloud Storage
   - ✅ file:// (NFS, Lustre, VAST, Weka)
   - ✅ direct:// (Direct I/O)

## Performance Tips

1. **Static Mapping > Round Robin**: For distributed tests, assign specific endpoints to specific agents to avoid contention

2. **Endpoint Count = Agent Count × NICs**: Match endpoint count to total NIC count across all test hosts

3. **Verify Namespace**: Test that files are visible across all endpoints before running benchmark

4. **Monitor Per-Endpoint Stats**: Use `--perf-log` to track which endpoints are bottlenecks

## Troubleshooting

### "Objects not found" errors

**Cause**: Endpoints don't present identical namespace.

**Solution**: Verify all endpoints access same data:
```bash
# Write via endpoint 1
aws s3 cp test.txt s3://192.168.1.10:9000/bucket/test.txt

# Read via endpoint 2
aws s3 cp s3://192.168.1.11:9000/bucket/test.txt -
```

### Uneven load distribution

**Cause**: Some endpoints slower than others.

**Solution**: Use `least_connections` strategy instead of `round_robin`:
```yaml
multi_endpoint:
  strategy: least_connections
  endpoints: [...]
```

### Per-agent endpoints not applied

**Cause**: Global `target` field overrides per-agent `multi_endpoint`.

**Solution**: Remove global `target`, use only `multi_endpoint`:
```yaml
# ❌ WRONG - global target takes precedence
target: "s3://192.168.1.10:9000/bucket/"
distributed:
  agents:
    - multi_endpoint: ...  # IGNORED!

# ✅ CORRECT - no global target
distributed:
  agents:
    - multi_endpoint: ...  # Applied
```

## See Also

- [S3_MULTI_ENDPOINT_GUIDE.md](../../docs/S3_MULTI_ENDPOINT_GUIDE.md) - Full architecture guide, distributed setup, troubleshooting
- [CONFIG_SYNTAX.md](../../docs/CONFIG_SYNTAX.md) - Full configuration reference
- [DISTRIBUTED_TESTING_GUIDE.md](../../docs/DISTRIBUTED_TESTING_GUIDE.md) - Distributed architecture

## Questions?

See [S3_MULTI_ENDPOINT_GUIDE.md](../../docs/S3_MULTI_ENDPOINT_GUIDE.md) for detailed architecture discussion and troubleshooting.
