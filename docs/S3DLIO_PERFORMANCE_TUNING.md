# s3dlio Performance Tuning Guide

This guide covers s3dlio v0.9.50+ performance optimizations and how to enable them in sai3-bench v0.8.63+.

## 📦 Version Requirements

- **sai3-bench**: v0.8.63+ (check `Cargo.toml`)
- **s3dlio**: v0.9.50+ (dependency in `Cargo.toml`)

To update:
```bash
cd /path/to/sai3-bench
# Edit Cargo.toml to update s3dlio tag to v0.9.50
cargo update -p s3dlio
cargo build --release
```

---

## 🚀 Features Overview

### 1. Range Download Optimization (Opt-In)
**76% faster downloads** for large objects using parallel range requests.

**Performance Benchmark** (16× 148 MB objects, MinIO):
- Without: 429 MB/s (5.52s)
- With 64 MB threshold: **755 MB/s (3.14s) - 76% faster** 🏆

**When to enable**:
- ✅ Objects ≥64 MB, cross-region traffic, high-latency scenarios, GET-heavy workloads

**When to disable**:
- ❌ Small files (< 64 MB), same-region high-bandwidth, PUT-heavy workloads, rate-limited HEAD requests

### 2. Multipart Upload Improvements (Automatic)
Zero-copy chunking for objects > 5 MB. **Always enabled**, no configuration needed.

---

## ⚙️ Configuration (YAML Method - Recommended)

Add `s3dlio_optimization` section to your workload YAML:

```yaml
target: "s3://my-bucket/large-files/"
duration: 300s
concurrency: 16

# Enable s3dlio optimizations
s3dlio_optimization:
  enable_range_downloads: true   # Enable parallel range requests
  range_threshold_mb: 64          # Only objects ≥64 MB (recommended)
  # range_concurrency: 16         # Optional: override auto-calculation
  # chunk_size_mb: 4              # Optional: override auto-calculation

prepare:
  ensure_objects:
    - base_uri: "data/"
      count: 1000
      min_size: 67108864   # 64 MB
      max_size: 157286400  # 150 MB

workload:
  - op: get
    path: "data/*"
    weight: 80
  - op: put
    path: "data/"
    weight: 20
    size_distribution:
      type: uniform
      min: 67108864
      max: 157286400
```

Run normally - optimization settings applied automatically:
```bash
sai3-bench run --config workload.yaml
```

**📁 See**: Complete example at `tests/configs/s3dlio_optimization_example.yaml`

---

## 🎯 Threshold Selection

| Threshold | Use Case | Speedup | Overhead Risk |
|-----------|----------|---------|---------------|
| 16 MB | Aggressive - mixed sizes | +69% | Medium |
| 32 MB | Moderate - larger files | +71% | Low |
| **64 MB** | **Recommended - large objects** | **+76%** | **Minimal** |
| 128 MB | Conservative - huge files only | +71% | Very Low |

**Default**: 64 MB provides optimal balance.

---

## 📋 Configuration Options

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enable_range_downloads` | bool | `false` | Enable parallel range requests |
| `range_threshold_mb` | int | `64` | Minimum object size for parallel ranges |
| `range_concurrency` | int | auto | Number of concurrent range requests (8-32) |
| `chunk_size_mb` | int | auto | Chunk size per range request (1-8 MB) |

**Note**: Leave `range_concurrency` and `chunk_size_mb` unset for auto-tuning (recommended).

---

## 🔧 Tuning Scenarios

**Cross-Region / High Latency**:
```yaml
s3dlio_optimization:
  enable_range_downloads: true
  range_threshold_mb: 16      # More aggressive
  range_concurrency: 32        # Higher parallelism
```

**Same-Region / Low Latency**:
```yaml
s3dlio_optimization:
  enable_range_downloads: true
  range_threshold_mb: 128     # Conservative - avoid coordination overhead
```

**Small Files (< 64 MB average)**:
```yaml
# DON'T include s3dlio_optimization - range optimization adds HEAD overhead
# Multipart upload improvements work automatically
```

---

## 🧪 Verification

**Check logs** for confirmation:
```bash
export RUST_LOG=sai3_bench=info
sai3-bench run --config workload.yaml

# Look for:
# INFO Applied s3dlio optimization configuration from YAML
# INFO Set s3dlio range threshold: 64 MB
```

**Compare performance**:
```bash
# Baseline (without optimization)
sai3-bench run --config workload_no_opt.yaml

# Optimized (with s3dlio_optimization section)
sai3-bench run --config workload_optimized.yaml

# Check throughput improvement in results
```

---

## 🐛 Troubleshooting

**No performance improvement?**
- Verify objects ≥ threshold size
- Check logs for "Applied s3dlio optimization" message
- Ensure using newly built binary (`which sai3-bench`)

**Slower after enabling?**
- Objects may be smaller than threshold
- Increase threshold to 128 MB or disable for your workload

**Permission denied on HEAD requests?**
- Some S3-compatible stores don't allow HEAD operations
- Disable range optimization for that backend

---

## 🌐 Distributed Mode

Configuration works identically for distributed deployments:

```yaml
target: "s3://bucket/data/"
duration: 600s

s3dlio_optimization:
  enable_range_downloads: true
  range_threshold_mb: 64

distributed:
  agents:
    - address: "agent1:7761"
      id: "agent-1"
    - address: "agent2:7761"
      id: "agent-2"

workload:
  - op: get
    path: "data/*"
    weight: 90
```

Agents automatically receive and apply optimization settings from controller.

---

## 📚 Alternative: Environment Variables

If you prefer environment variables over YAML config:

```bash
export S3DLIO_ENABLE_RANGE_OPTIMIZATION=1
export S3DLIO_RANGE_THRESHOLD_MB=64
sai3-bench run --config workload.yaml
```

**For distributed**, set on each agent host or via systemd:
```ini
[Service]
Environment="S3DLIO_ENABLE_RANGE_OPTIMIZATION=1"
Environment="S3DLIO_RANGE_THRESHOLD_MB=64"
ExecStart=/usr/local/bin/sai3bench-agent --listen 0.0.0.0:7761
```

---

## 📝 Quick Reference

| Feature | YAML Config | Benefit |
|---------|-------------|---------|
| **Range downloads** | `s3dlio_optimization.enable_range_downloads: true` | +76% for ≥64 MB |
| **Range threshold** | `s3dlio_optimization.range_threshold_mb: 64` | When to parallelize |
| **Multipart uploads** | *(automatic)* | Zero-copy, always on |

**Recommended for most workloads**:
```yaml
s3dlio_optimization:
  enable_range_downloads: true
  range_threshold_mb: 64
```

---

## 📖 Additional Resources

- **Example Config**: `tests/configs/s3dlio_optimization_example.yaml`
- **s3dlio Docs**: [RANGE_OPTIMIZATION_IMPLEMENTATION.md](https://github.com/russfellows/s3dlio/blob/main/docs/performance/RANGE_OPTIMIZATION_IMPLEMENTATION.md)
- **s3dlio Changelog**: [Changelog.md](https://github.com/russfellows/s3dlio/blob/main/docs/Changelog.md)
