# S3 Multi-Endpoint Testing Guide

## Overview

This guide explains how to adapt your NFS multi-mount testing to S3 multi-endpoint testing using sai3-bench's multi-endpoint capabilities with s3dlio.

## Architecture Comparison

### NFS Multi-Mount (Your Current Setup)
```
4 agents × 4 NFS mounts = 16 mount points
┌─────────┐   ┌──────────────┐
│ Agent 1 │──▶│ /mnt/filesys1│  Each mount = separate filesystem
│         │──▶│ /mnt/filesys2│  Copy of data on each filesystem
│         │──▶│ /mnt/filesys3│  OR shared namespace (depends on NFS config)
│         │──▶│ /mnt/filesys4│
└─────────┘   └──────────────┘
```

### S3 Multi-Endpoint (Recommended Approach)
```
4 agents × 2 S3 endpoints = 8 endpoints
┌─────────┐   ┌──────────────┐   ┌─────────────────┐
│ Agent 1 │──▶│ 192.168.1.1  │──▶│                 │
│         │   │ (port 9000)  │   │  Single Bucket  │
│         │   └──────────────┘   │   "benchmark"   │
│         │                      │                 │
│         │   ┌──────────────┐   │  Same logical   │
│         │──▶│ 192.168.1.2  │──▶│  namespace for  │
│         │   │ (port 9000)  │   │  all endpoints  │
└─────────┘   └──────────────┘   └─────────────────┘
```

**Key Difference**: S3 endpoints are **gateway IPs to the same storage pool**, not separate storage locations.

---

## Configuration Strategy

### ✅ Recommended: Single Shared Bucket

**File**: `distributed_4node_8endpoint_s3_test.yaml`

**Structure**:
```yaml
target: s3://benchmark/

distributed:
  agents:
    - id: "agent-1"
      multi_endpoint:
        endpoints:
          - "s3://192.168.1.1:9000/benchmark/"  # Gateway IP 1
          - "s3://192.168.1.2:9000/benchmark/"  # Gateway IP 2
        strategy: round_robin
    
    - id: "agent-2"
      multi_endpoint:
        endpoints:
          - "s3://192.168.1.3:9000/benchmark/"  # Gateway IP 3
          - "s3://192.168.1.4:9000/benchmark/"  # Gateway IP 4
        strategy: round_robin
    # ... agents 3-4 with IPs 5-8
```

**Benefits**:
- ✅ Single namespace (all agents see same objects)
- ✅ Simpler prepare phase (no data replication needed)
- ✅ Tests actual object storage load distribution
- ✅ Works with any S3-compatible storage (MinIO, VAST, etc.)
- ✅ Leverages s3dlio's MultiEndpointStore (automatic failover, stats)

---

### ❌ Alternative: 8 Separate Buckets (Not Recommended)

You **could** use 8 separate buckets:
```yaml
endpoints:
  - "s3://192.168.1.1:9000/bucket-1/"
  - "s3://192.168.1.2:9000/bucket-2/"
  # ... 8 total
```

**Why not recommended**:
- ❌ Need to create/prepare 8 separate buckets
- ❌ 8× data replication (64M objects × 8 = 512M objects!)
- ❌ Doesn't match typical object storage architecture
- ❌ More complex to manage and verify
- ❌ Wastes storage capacity

**When you might use it**:
- Testing bucket-level isolation
- Benchmarking per-bucket quotas/limits
- Different data sets per endpoint (not your use case)

---

## Key Configuration Changes: NFS → S3

| Aspect | NFS Config | S3 Config |
|--------|-----------|-----------|
| **Endpoint Syntax** | `file:///mnt/filesysN/` | `s3://IP:PORT/bucket/` |
| **Bucket/Path** | Different filesystem paths | Same bucket, different IPs |
| **shared_filesystem** | `true` (NFS is shared) | `false` (S3 is not POSIX) |
| **tree_creation_mode** | `coordinator` | `coordinator` |
| **Credentials** | Not needed (filesystem) | `AWS_ACCESS_KEY_ID` + `_SECRET` |
| **Endpoint Count** | 16 (4 per agent) | 8 (2 per agent) |

---

## Setup Instructions

### 1. Configure S3 Credentials

**Option A: Environment Variables**
```bash
export AWS_ACCESS_KEY_ID="your-access-key"
export AWS_SECRET_ACCESS_KEY="your-secret-key"
```

**Option B: AWS Credentials File**
```bash
# Create ~/.aws/credentials
[default]
aws_access_key_id = your-access-key
aws_secret_access_key = your-secret-key
```

### 2. Update Endpoint IPs

Edit `distributed_4node_8endpoint_s3_test.yaml`:
```yaml
# Replace 192.168.1.1-8 with your actual storage IPs
endpoints:
  - "s3://192.168.1.1:9000/benchmark/"  # Change these IPs
  - "s3://192.168.1.2:9000/benchmark/"  # to match your
  - "s3://192.168.1.3:9000/benchmark/"  # storage system
  # ...
```

### 3. Create Bucket (if needed)

```bash
# Create bucket on one endpoint (it will be accessible from all)
aws s3 mb s3://benchmark --endpoint-url http://192.168.1.1:9000

# OR let sai3-bench create it during prepare phase
```

### 4. Start Agents

```bash
# On each agent host (172.21.4.10-13)
./sai3bench-agent --listen 0.0.0.0:7761

# Ensure S3 credentials are available on each agent host
```

### 5. Run Test

```bash
# On controller host
./sai3-bench run --config tests/configs/distributed_4node_8endpoint_s3_test.yaml
```

---

## Performance Optimization

### Enable s3dlio v0.9.50+ Range Downloads

For 64M objects averaging 8 MB, **enable range optimization** for 76% faster downloads:

```yaml
s3dlio_optimization:
  enable_range_downloads: true
  range_threshold_mb: 64  # Benefit for objects ≥64 MB
```

**Benchmark** (16× 148 MB objects):
- Without: 429 MB/s
- With: **755 MB/s (76% faster)** ✨

### Load Balancing Strategy

**round_robin** (default):
- Predictable, even distribution
- Good for similar endpoints

**least_connections**:
- Adaptive routing to least-busy endpoint
- Better if endpoints have different performance characteristics

```yaml
multi_endpoint:
  strategy: least_connections  # Try this if endpoints vary in performance
```

---

## Verification

### Check Endpoint Distribution

```bash
# During test, check agent logs for multi-endpoint activity
tail -f agent-1.log | grep -i "endpoint\|multi"

# Look for:
# "Using MultiEndpointStore with 2 endpoints"
# "Selected endpoint 0: s3://192.168.1.1:9000/benchmark/"
```

### Verify Object Distribution

```bash
# After prepare, check object count
aws s3 ls s3://benchmark/data/ --recursive --endpoint-url http://192.168.1.1:9000 | wc -l

# Should show ~64M objects across all endpoints (same namespace)
```

### Monitor Per-Endpoint Stats

```bash
# sai3-bench will log per-endpoint stats if using s3dlio v0.9.14+
# Check perf_log.tsv for breakdown by endpoint
```

---

## Troubleshooting

### Issue: "Connection refused" or "Host not reachable"

**Problem**: Endpoint IPs are incorrect or S3 service not running  
**Solution**: 
```bash
# Test connectivity to each endpoint
for ip in 192.168.1.{1..8}; do
  curl -v http://$ip:9000/minio/health/live 2>&1 | grep "200 OK"
done
```

### Issue: "Access Denied" errors

**Problem**: S3 credentials not configured or incorrect  
**Solution**:
```bash
# Verify credentials work
aws s3 ls s3://benchmark/ --endpoint-url http://192.168.1.1:9000

# Ensure AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY are set on all agent hosts
```

### Issue: Prepare phase hangs at "Creating directory tree"

**Problem**: LIST operations timing out due to large prefix count  
**Solution**: Already configured in YAML:
```yaml
grpc_keepalive_interval: 60  # Already set
barrier_sync:
  prepare:
    agent_barrier_timeout: 1500  # 25 minutes - already generous
```

### Issue: Objects only visible on one endpoint

**Problem**: Using separate buckets or storage not properly replicated  
**Solution**: 
- Ensure all endpoints use **same bucket name**: `s3://benchmark/`
- Verify object storage replication is configured
- Check that all endpoint IPs point to same storage cluster

---

## Expected Results

### Prepare Phase
- **Tree creation**: ~90 seconds to generate 331,776 key prefixes
- **Object creation**: Depends on write throughput, concurrency
  - 64M objects × 8 MB = ~500 TB
  - Example: 1 GB/s aggregate = ~140 hours (distribute across 4 agents = ~35 hours)
- **Endpoint distribution**: Each agent writes via 2 endpoints, round-robin

### Validation
- All agents see same 64M objects
- Objects accessible from all 8 endpoints
- No 404 errors (proper replication)

---

## Comparison: When to Use Each Approach

| Use Case | NFS Multi-Mount | S3 Multi-Endpoint |
|----------|-----------------|-------------------|
| **Filesystem semantics** | ✅ POSIX required | ❌ Object storage |
| **Shared namespace** | ⚠️ Depends on config | ✅ Always shared |
| **Geographic distribution** | ❌ Local only | ✅ Cross-region capable |
| **Scalability** | ⚠️ Limited by NFS | ✅ Massive scale |
| **Replication** | ⚠️ Manual or NFS-dependent | ✅ Automatic |
| **Latency tolerance** | ❌ Low latency needed | ✅ High latency OK |

---

## Next Steps

1. **Copy and customize**: Start with `distributed_4node_8endpoint_s3_test.yaml`
2. **Update IPs**: Replace `192.168.1.1-8` with your storage endpoint IPs
3. **Test single agent first**: Comment out agents 2-4, run with just agent-1
4. **Scale up**: Add agents incrementally to verify behavior
5. **Enable optimization**: Uncomment `s3dlio_optimization` section for 76% speedup

---

## Additional Resources

- **s3dlio Multi-Endpoint Guide**: `/home/eval/Documents/Code/s3dlio/docs/supplemental/MULTI_ENDPOINT_GUIDE.md`
- **s3dlio Performance Tuning**: `docs/S3DLIO_PERFORMANCE_TUNING.md`
- **sai3-bench Config Syntax**: `docs/CONFIG_SYNTAX.md`
- **Example Configs**: `tests/configs/MULTI_ENDPOINT_README.md`
