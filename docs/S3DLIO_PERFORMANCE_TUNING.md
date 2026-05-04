# s3dlio Performance Tuning Guide

This guide covers s3dlio v0.9.50+ performance optimizations and how to enable them in sai3-bench v0.8.63+.

## 📦 Version Requirements

- **sai3-bench**: v0.8.63+ (check `Cargo.toml`)
- **s3dlio**: v0.9.50+ (dependency in `Cargo.toml`)
- **HTTP/2 (h2c) support**: sai3-bench v0.8.92+ / s3dlio v0.9.90+

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

### 3. HTTP/2 and h2c Support (s3dlio v0.9.90+)

HTTP/2 for S3-protocol endpoints — reduces per-request overhead via multiplexing.

**Mode selection via `S3DLIO_H2C`** (or `s3dlio_optimization.h2c` in YAML):

- **Unset / Auto** (default) — probes h2c on first `http://` connection; falls back silently to
  HTTP/1.1 if the server refuses the HTTP/2 preface.  `https://` endpoints negotiate via TLS
  ALPN automatically with no probe overhead.
- **`S3DLIO_H2C=1`** (`h2c: true`) — force HTTP/2 prior-knowledge on `http://`; `https://`
  still uses ALPN.
- **`S3DLIO_H2C=0`** (`h2c: false`) — always HTTP/1.1, skips the probe entirely.

The resolved mode is logged at `INFO` on startup so operators can confirm the active setting
without a packet capture.

**When to use h2c**:

- ✅ Plain-HTTP (`http://`) S3-compatible stores that advertise HTTP/2 (MinIO v21+, VAST, etc.)
- ✅ High-concurrency workloads where per-connection TCP overhead is a bottleneck

**When NOT to use**:

- ❌ `https://` endpoints (ALPN already handles it; setting `h2c: true` has no extra effect)
- ❌ Stores that do not support HTTP/2 (auto mode falls back gracefully; force mode may error)

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

**HTTP/2 (h2c) fields** (v0.9.90+, all optional):

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `h2c` | bool | absent | Force h2c (`true`), force HTTP/1.1 (`false`), or auto-probe (absent) |
| `h2_adaptive_window` | bool | `true` | BDP estimator auto-tunes flow-control window |
| `h2_stream_window_mb` | int | 4 | Per-stream receive window in MiB (static mode only) |
| `h2_conn_window_mb` | int | 4× stream | Connection-level receive window in MiB (static mode only) |

**Note**: `h2_stream_window_mb` and `h2_conn_window_mb` are only used when `h2_adaptive_window: false`.

**HTTP/2 YAML example**:

```yaml
s3dlio_optimization:
  enable_range_downloads: true
  range_threshold_mb: 64
  h2c: true                     # force HTTP/2 on http:// endpoints
  # h2_adaptive_window: false   # uncomment for fixed window (benchmarks)
  # h2_stream_window_mb: 32
  # h2_conn_window_mb: 128
```

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
    - address: "agent1:7167"
      id: "agent-1"
    - address: "agent2:7167"
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
# Range downloads
export S3DLIO_ENABLE_RANGE_OPTIMIZATION=1
export S3DLIO_RANGE_THRESHOLD_MB=64

# HTTP/2 h2c (v0.9.90+)
export S3DLIO_H2C=1           # force h2c on http:// endpoints
# export S3DLIO_H2C=0         # force HTTP/1.1
# unset S3DLIO_H2C            # auto-probe (default)

# Thread counts (v0.8.92)
export S3DLIO_RT_THREADS=384  # s3dlio internal runtime threads
export TOKIO_WORKER_THREADS=384  # sai3bench-agent Tokio threads

sai3-bench run --config workload.yaml
```

**For distributed**, set on each agent host or via systemd:

```ini
[Service]
Environment="S3DLIO_ENABLE_RANGE_OPTIMIZATION=1"
Environment="S3DLIO_RANGE_THRESHOLD_MB=64"
Environment="S3DLIO_H2C=1"
ExecStart=/usr/local/bin/sai3bench-agent --listen 0.0.0.0:7167
```

Or use `--env-file` on the controller to forward credentials and env vars automatically (see [CREDENTIAL_FORWARDING.md](CREDENTIAL_FORWARDING.md)).

---

## 🧵 Thread Count Tuning (v0.8.92)

High-concurrency S3 workloads — especially small-object PUT/GET at 50k+ ops/s — require more
threads than the defaults allow.  Two independent thread pools must be tuned together.

### Background: Two Thread Pools

| Pool | Default | Purpose |
|------|---------|---------|
| **s3dlio global RT** | `min(num_cpus × 2, 32)` | Executes every S3 API call (put, get, list…) |
| **sai3bench-agent Tokio** | `num_cpus` | Agent gRPC server + workload coordination |

For `concurrency: 384` the s3dlio RT default of 32 threads is the **primary bottleneck** —
Little's Law requires `threads ≥ concurrency × avg_latency_s`:

| Avg latency | Threads for 67k ops/s |
|-------------|-----------------------|
| 2 ms        | 135                   |
| 3 ms        | 202                   |
| 5 ms        | 338                   |
| 5.7 ms      | 384                   |
| 10 ms       | 675                   |

### YAML Configuration (Recommended)

Add under `s3dlio_optimization` in your workload YAML:

```yaml
s3dlio_optimization:
  # ... existing fields ...

  # ── Thread counts (v0.8.92) ──────────────────────────────────────────────
  # Override the s3dlio global RT cap (default: min(num_cpus*2, 32)).
  # Set to at least your concurrency value for small-object workloads.
  s3dlio_rt_threads: 384

  # Document the desired agent Tokio thread count.
  # NOTE: This field is informational — start the agent with --worker-threads N.
  tokio_worker_threads: 384

  # Set false to disable automatic derivation from 'concurrency' if you prefer
  # to always set the count explicitly.  Default: true.
  auto_threads: true
```

**Auto-derivation** (when `auto_threads: true`, the default): if `s3dlio_rt_threads` is not
set and `S3DLIO_RT_THREADS` is not in the environment, the agent automatically sets
`S3DLIO_RT_THREADS = max(concurrency × 1.5, 32)` before the first s3dlio call.

### Agent CLI Flag

Because the sai3bench-agent Tokio runtime starts **before** the YAML config arrives from the
controller, `tokio_worker_threads` in YAML cannot be applied automatically.  Use the CLI flag
when starting the agent:

```bash
# Start agent with 384 Tokio worker threads
sai3bench-agent --listen 0.0.0.0:7167 --worker-threads 384
```

Or set `TOKIO_WORKER_THREADS` in the agent's environment (lower precedence than `--worker-threads`):

```bash
TOKIO_WORKER_THREADS=384 sai3bench-agent --listen 0.0.0.0:7167
```

### Complete Example: Trillion-Object PUT Workload

```yaml
# workload.yaml — 384-concurrency small-object PUT on 16 VAST S3 endpoints
concurrency: 384
operation: put
object_size: 1KiB

s3dlio_optimization:
  s3dlio_rt_threads: 384   # matches concurrency; controller sends to all agents
  tokio_worker_threads: 384  # reminder to start agents with --worker-threads 384
  h2c: true                # enable h2c for http:// VAST endpoints (verify VAST supports it)

# ... endpoint and bucket config ...
```

Start each of the 4 agents with:

```bash
sai3bench-agent --listen 0.0.0.0:7167 --worker-threads 384
```

The agent logs the active thread counts at startup:

```text
INFO Tokio runtime started with 384 worker threads (set via --worker-threads CLI flag)
INFO Auto-set s3dlio runtime threads: 576 (concurrency=384)
```

---

## 📝 Quick Reference

| Feature | YAML Config | Env Var | Benefit |
|---------|-------------|---------|---------|
| **Range downloads** | `s3dlio_optimization.enable_range_downloads: true` | `S3DLIO_ENABLE_RANGE_OPTIMIZATION=1` | +76% for ≥64 MB |
| **Range threshold** | `s3dlio_optimization.range_threshold_mb: 64` | `S3DLIO_RANGE_THRESHOLD_MB=64` | When to parallelize |
| **Multipart uploads** | *(automatic)* | — | Zero-copy, always on |
| **HTTP/2 h2c** | `s3dlio_optimization.h2c: true` | `S3DLIO_H2C=1` | Lower per-request overhead |
| **HTTP/2 auto-probe** | *(omit `h2c` field)* | *(unset `S3DLIO_H2C`)* | Try h2c, fall back |
| **Force HTTP/1.1** | `s3dlio_optimization.h2c: false` | `S3DLIO_H2C=0` | Reproducible baselines |
| **s3dlio RT threads** | `s3dlio_optimization.s3dlio_rt_threads: 384` | `S3DLIO_RT_THREADS=384` | Unlock 50k+ ops/s |
| **Agent Tokio threads** | `s3dlio_optimization.tokio_worker_threads: 384` ¹ | `TOKIO_WORKER_THREADS=384` | Agent concurrency |
| **Auto thread derive** | `s3dlio_optimization.auto_threads: true` *(default)* | — | Auto-set from concurrency |

**Recommended for most workloads**:

```yaml
s3dlio_optimization:
  enable_range_downloads: true
  range_threshold_mb: 64
```

**Recommended for high-concurrency small-object S3 workloads** (`concurrency: ≥64`):

```yaml
s3dlio_optimization:
  enable_range_downloads: true
  range_threshold_mb: 64
  s3dlio_rt_threads: 384    # ≥ concurrency
  tokio_worker_threads: 384  # ¹ start agent with --worker-threads 384
```

¹ `tokio_worker_threads` in YAML is informational — use `sai3bench-agent --worker-threads N` or `TOKIO_WORKER_THREADS=N`.

---

## 📖 Additional Resources

- **Example Config**: `tests/configs/s3dlio_optimization_example.yaml`
- **s3dlio Docs**: [RANGE_OPTIMIZATION_IMPLEMENTATION.md](https://github.com/russfellows/s3dlio/blob/main/docs/performance/RANGE_OPTIMIZATION_IMPLEMENTATION.md)
- **s3dlio Changelog**: [Changelog.md](https://github.com/russfellows/s3dlio/blob/main/docs/Changelog.md)
