# Test Configuration Files

This directory contains YAML configuration files for testing sai3-bench with various backends and scenarios.

## Quick Reference

### Basic Backend Tests

| File | Backend | Purpose | Notes |
|------|---------|---------|-------|
| `file_test.yaml` | file:// | Basic file backend operations | No credentials needed |
| `direct_test.yaml` | direct:// | DirectIO backend | Requires O_DIRECT support |
| `azure_test.yaml` | az:// | Azure Blob Storage | Requires Azure credentials |
| `gcs_test.yaml` | gs:// | Google Cloud Storage | Requires GCP auth |
| `mixed.yaml` | Multiple | Multi-backend workload | Tests multiple storage types |

### RangeEngine Performance Tests

**RangeEngine** is a feature in s3dlio v0.9.3+ that provides 30-50% throughput improvements for large files (>= 4MB) on network backends by using concurrent byte-range requests.

| File | Backend | Purpose | Test Results |
|------|---------|---------|--------------|
| `range_engine_test.yaml` | file:// | Demonstrates RangeEngine activation | 1,906 ops/s, 19GB workload |
| `azure_rangeengine_test.yaml` | az:// | Full Azure RangeEngine test | 8.46 ops/s (documented) |
| `azure_rangeengine_simple.yaml` | az:// | Minimal Azure example | Same as above, easier to customize |
| `azure_rangeengine_disabled_test.yaml` | az:// | Baseline without RangeEngine | For A/B comparison |
| `gcs_rangeengine_test.yaml` | gs:// | GCS RangeEngine test | 45.26 ops/s (5.3x faster than Azure) |

**Key RangeEngine Findings (v0.6.1 testing)**:
- **GCS**: 45.26 ops/s, mean latency 173ms - Excellent performance
- **Azure**: 8.46 ops/s, mean latency 912ms - Working as expected
- **File**: Limited benefit (already fast local I/O)
- **Range activation**: Confirmed for files >= 4MB on Azure & GCS
- **Multi-range**: Confirmed 2 ranges for 128MB files

### Size Distribution Tests

| File | Purpose | Features |
|------|---------|----------|
| `size_distributions_test.yaml` | Tests SizeGenerator | Lognormal, uniform, fixed distributions |
| `multisize_test.yaml` | Multiple object sizes | Various size buckets |

### Feature-Specific Tests

| File | Purpose |
|------|---------|
| `per_op_concurrency_test.yaml` | Per-operation concurrency overrides |
| `remap_examples.yaml` | URI remapping examples |
| `remap_fanout_test.yaml` | Advanced remapping scenarios |
| `delete_test.yaml` | DELETE operation testing |
| `dedupe_compress_test.yaml` | Deduplication & compression |
| `safe_three_op_test.yaml` | GET/PUT/STAT operations |

### Distributed Testing

| File | Purpose |
|------|---------|
| `warp_parity_mixed.yaml` | Warp compatibility test |
| `warp_cleanup_test.yaml` | Cleanup operations |

### Smoke Tests

| File | Purpose | Duration |
|------|---------|----------|
| `get_smoke.yaml` | Quick GET test | 5s |
| `put_smoke.yaml` | Quick PUT test | 5s |

## Common Usage Patterns

### Testing RangeEngine on Cloud Backends

```bash
# Azure (requires AZURE_STORAGE_ACCOUNT, AZURE_STORAGE_ACCOUNT_KEY)
sai3-bench run --config tests/configs/azure_rangeengine_simple.yaml

# GCS (requires gcloud auth)
gcloud auth application-default login
sai3-bench run --config tests/configs/gcs_rangeengine_test.yaml

# See RangeEngine activation logs
sai3-bench -vv get --uri "gs://bucket/large-file" --jobs 4
```

### File Backend (No Credentials)

```bash
# Basic workload
sai3-bench run --config tests/configs/file_test.yaml

# RangeEngine demonstration
sai3-bench run --config tests/configs/range_engine_test.yaml

# Multiple sizes
sai3-bench run --config tests/configs/multisize_test.yaml
```

### Distributed Workload

```bash
# Terminal 1: Start agent
sai3bench-agent --listen 127.0.0.1:7761 -v

# Terminal 2: Run via controller
sai3bench-ctl --insecure --agents 127.0.0.1:7761 \
  run --config tests/configs/file_test.yaml
```

## Configuration File Structure

Basic structure of a test config:

```yaml
# Backend and workload settings
target: "file:///tmp/test/"
duration: "10s"
concurrency: 4

# Optional: RangeEngine settings (uses defaults if omitted)
range_engine:
  enabled: true
  chunk_size: 67108864      # 64 MB
  max_concurrent_ranges: 32
  min_split_size: 4194304   # 4 MB
  range_timeout_secs: 30

# Workload definition
workload:
  - op: get
    path: "data/*"
    weight: 70
  - op: put
    path: "data/"
    size: 1048576  # 1 MB
    weight: 30

# Optional: Prepare objects before test
prepare:
  cleanup: true
  ensure_objects:
    - base_uri: "file:///tmp/test/data/"
      count: 100
      size: 1048576
      fill: random
```

## Environment Variables

### Azure Blob Storage
```bash
export AZURE_STORAGE_ACCOUNT="your-account"
export AZURE_STORAGE_ACCOUNT_KEY="your-key"
# Or use connection string:
export AZURE_STORAGE_CONNECTION_STRING="DefaultEndpointsProtocol=https;..."
```

### Google Cloud Storage
```bash
# Use application default credentials
gcloud auth application-default login

# Or set service account key
export GOOGLE_APPLICATION_CREDENTIALS="/path/to/key.json"
```

### AWS S3
```bash
export AWS_ACCESS_KEY_ID="your-key-id"
export AWS_SECRET_ACCESS_KEY="your-secret"
export AWS_DEFAULT_REGION="us-east-1"
```

## Creating Custom Configs

1. **Start with an example**: Copy a similar test config
2. **Update URIs**: Change to your bucket/path
3. **Adjust workload**: Modify operations and weights
4. **Set duration**: Test duration or operation count
5. **Configure prepare**: Set up test objects if needed

Example minimal config:

```yaml
target: "file:///tmp/mytest/"
duration: "30s"
concurrency: 4

workload:
  - op: get
    path: "*"
    weight: 100

prepare:
  ensure_objects:
    - base_uri: "file:///tmp/mytest/"
      count: 50
      size: 1048576
      fill: random
```

## Troubleshooting

### "No URIs found for pattern"
- Ensure objects exist at the path
- Check glob pattern syntax (use `*` for matching)
- Verify backend connectivity with `sai3-bench health --uri <uri>`

### "Failed to parse config"
- YAML indentation must be consistent (use spaces, not tabs)
- Check for `size: <number>` not `size_distribution: {type: fixed, ...}`
- Validate with: `sai3-bench run --config <file> --help`

### RangeEngine not activating
- Files must be >= 4 MB (default threshold)
- Use `-vv` flag to see RangeEngine logs
- Check s3dlio version: must be v0.9.3+

## Version History

- **v0.6.1**: Added RangeEngine test configs (Azure, GCS)
- **v0.6.0**: Added distributed workload examples
- **v0.5.3**: Added size distribution tests
- **v0.5.0**: Initial test config collection

## See Also

- [S3DLIO_V0.9.4_MIGRATION.md](../../docs/S3DLIO_V0.9.4_MIGRATION.md) - Migration guide
- [S3DLIO_V0.9.4_TEST_RESULTS.md](../../docs/S3DLIO_V0.9.4_TEST_RESULTS.md) - Test results
- [CONFIG.sample.yaml](../../docs/CONFIG.sample.yaml) - Detailed configuration reference
