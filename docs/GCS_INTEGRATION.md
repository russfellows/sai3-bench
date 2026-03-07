# GCS / RAPID Integration Guide — sai3-bench

**Status**: Implemented (v0.9.65 integration, pending `cargo check` verification)  
**Date**: 2026-03-06  
**sai3-bench current version**: v0.8.63 (s3dlio now pinned to v0.9.65)  
**Target**: s3dlio v0.9.65 GCS API integration  
**Prerequisite reading**: [s3dlio/docs/GCS-API-Configuration.md](../../s3dlio/docs/GCS-API-Configuration.md)

---

## Overview

sai3-bench drives s3dlio to perform all GCS I/O.  Several GCS tuning parameters
must be communicated to s3dlio before the first `gs://` operation, because the
GCS client is a process-wide singleton.

s3dlio v0.9.65 exposes a direct Rust API for GCS subchannel count, RAPID mode,
and write chunk size configuration.  This document describes:
- What is in s3dlio v0.9.65 (already available)
- What was implemented in sai3-bench to use it
- YAML config examples including chunk size and concurrency scaling
- Smart-default and concurrency-scaling behaviour
- Known limitations

---

## Current State

### What is already in s3dlio (v0.9.65)

All of these are public Rust functions confirmed in `s3dlio/src/lib.rs` and
available to any crate that links against s3dlio:

| Function | Signature | Purpose |
|----------|-----------|---------|
| `s3dlio::set_gcs_channel_count(n)` | `fn(usize)` | Pre-configure gRPC subchannel count |
| `s3dlio::get_gcs_channel_count()` | `fn() -> usize` | Read back count (0 = unset) |
| `s3dlio::set_gcs_rapid_mode(force)` | `fn(Option<bool>)` | Force RAPID on/off or restore auto-detect |
| `s3dlio::get_gcs_rapid_mode()` | `fn() -> Option<bool>` | Read back effective mode |
| `s3dlio::query_gcs_rapid_bucket(uri)` | `async fn(&str) -> bool` | Query whether URI is RAPID (cached) |

Python callers have equivalent functions registered in the `s3dlio` module
(see [Python API](#python-api) below).

### What is NOT yet in sai3-bench (4 changes needed)

sai3-bench is currently pinned to s3dlio **v0.9.50** and has no GCS API calls.
The four changes below are the complete implementation plan, ready for review
before any code is touched.

---

## Implemented Changes

### Change 1 — `Cargo.toml`: bumped s3dlio and AWS SDK versions

```toml
s3dlio      = { git = "...", tag = "v0.9.65" }  # was v0.9.50
s3dlio-oplog = { git = "...", tag = "v0.9.65" }  # was v0.9.50
aws-config  = "=1.8.14"                           # was =1.8.11
aws-sdk-s3  = "=1.124.0"                          # was =1.116.0
```

---

### Change 2 — `src/config.rs`: new GCS fields in `S3dlioOptimizationConfig`

Three new optional fields added after the existing S3 range fields:

```rust
// ── GCS-specific settings ─────────────────────────────────────────────────

/// GCS: gRPC subchannel count override (v0.9.65+)
/// When set to N, concurrency is treated as per-channel and scaled to concurrency×N
/// (capped at CPU count, WARN emitted). Absent = smart default (channels = concurrency).
#[serde(default)]
pub gcs_channel_count: Option<usize>,

/// GCS: RAPID mode override (v0.9.65+)
/// true=force on, false=force off, absent=auto-detect per bucket
#[serde(default)]
pub gcs_rapid_mode: Option<bool>,

/// GCS: gRPC write chunk size in bytes (v0.9.65+)
/// Sets S3DLIO_GRPC_WRITE_CHUNK_SIZE. Max safe: 4128768 (4 MiB − 64 KiB).
/// Default: 2097152 (2 MiB)
#[serde(default)]
pub gcs_write_chunk_size_bytes: Option<u64>,
```

---

### Change 3 — `src/config.rs`: `S3dlioOptimizationConfig::apply()` additions

At the end of `apply()`, after the existing S3 chunk-size block:

- Calls `s3dlio::set_gcs_channel_count(n)` when `gcs_channel_count` is explicitly set
- Calls `s3dlio::set_gcs_rapid_mode(Some(rapid))` when `gcs_rapid_mode` is set
- Sets `S3DLIO_GRPC_WRITE_CHUNK_SIZE` env var when `gcs_write_chunk_size_bytes` is set

---

### Change 4 — `src/config.rs`: new `Config::apply_gcs_defaults()` method

Added to `impl Config`. Called on every config-parsing path after `apply()`.

**Behaviour for `gs://` / `gcs://` targets** (checked via `config.target` and
`config.multi_endpoint`):

| `gcs_channel_count` in YAML | Action |
|-----------------------------|--------|
| **Explicit `N`** (already set in apply()) | Scale `config.concurrency` to `min(concurrency × N, num_cpus)`. Emit `WARN` for both cases: scaled-up, and CPU-capped. Never decrease below original. |
| **Absent** (smart default) | Call `set_gcs_channel_count(concurrency)` — one channel per task. No concurrency change. Emit `INFO`. |

Non-GCS targets: no-op.

---

### Change 5 — `src/main.rs` and `src/bin/agent.rs`

`config.apply_gcs_defaults()` is called immediately after `s3dlio_opt.apply()`
in three locations:
- `src/main.rs` — standalone mode (both prepare and run phases use the same config)
- `src/bin/agent.rs` `run_workload` RPC — deprecated distributed path
- `src/bin/agent.rs` `execute_workload` RPC — primary distributed path

---

### Status Summary

| Item | File | Status |
|------|------|--------|
| Bump s3dlio tag v0.9.50 → v0.9.65 | `Cargo.toml` | ✅ Done |
| Bump aws-config =1.8.11 → =1.8.14 | `Cargo.toml` | ✅ Done |
| Bump aws-sdk-s3 =1.116.0 → =1.124.0 | `Cargo.toml` | ✅ Done |
| `gcs_channel_count` field | `src/config.rs` | ✅ Done |
| `gcs_rapid_mode` field | `src/config.rs` | ✅ Done |
| `gcs_write_chunk_size_bytes` field | `src/config.rs` | ✅ Done |
| `apply()` GCS API calls | `src/config.rs` | ✅ Done |
| `apply_gcs_defaults()` method | `src/config.rs` | ✅ Done |
| Call site — standalone | `src/main.rs` | ✅ Done |
| Call site — agent run_workload | `src/bin/agent.rs` | ✅ Done |
| Call site — agent execute_workload | `src/bin/agent.rs` | ✅ Done |

---

## YAML Configuration

### Minimal GCS benchmark — smart defaults (recommended)

No `s3dlio_optimization` block needed.  sai3-bench auto-sets the gRPC channel
count to match `concurrency`, and RAPID is auto-detected per bucket:

```yaml
protocol: s3  # s3dlio accepts gs:// URIs regardless of this label
target: "gs://my-gcs-bucket/bench/"
num_objects: 1000
object_size_mb: 32
concurrency: 64   # channel count auto-set to 64 for gs:// targets
```

### Force RAPID mode (known Hyperdisk ML bucket)

```yaml
target: "gs://my-rapid-bucket/bench/"
concurrency: 64

s3dlio_optimization:
  gcs_rapid_mode: true         # force RAPID, skips GetStorageLayout RPC
  enable_range_downloads: false  # GCS uses bidi-read, not HTTP ranges
```

### Channel count scaling (high-throughput RAPID write)

When `gcs_channel_count` is set, YAML `concurrency` is treated as
*per-channel* parallelism.  Total Tokio tasks = `concurrency × gcs_channel_count`,
capped at available CPUs.  A `WARN` is emitted if scaled or capped:

```yaml
concurrency: 16   # per-channel tasks

s3dlio_optimization:
  gcs_channel_count: 8   # → effective total = 128 tasks (warn emitted)
  gcs_rapid_mode: true
  enable_range_downloads: false
```

Log output example:
```
WARN GCS: concurrency scaled up 16 → 128 (8 channels × 16 per-channel)
```

If the machine has fewer than 128 CPUs (e.g. 64-CPU VM):
```
WARN GCS: concurrency scaled 16 → 64 (8 channels × 16 per-channel), capped at 64 available CPUs
```

### GCS write chunk size override (RAPID puts)

```yaml
s3dlio_optimization:
  gcs_channel_count: 8
  gcs_rapid_mode: true
  gcs_write_chunk_size_bytes: 4128768  # 4 MiB − 64 KiB (max safe)
  enable_range_downloads: false
```

### High-throughput RAPID read benchmark (performance target ≥ 3.5 GB/s)

```yaml
protocol: s3
target: "gs://my-rapid-bucket/data/"
num_objects: 1000
object_size_mb: 32
concurrency: 64

s3dlio_optimization:
  gcs_rapid_mode: true
  enable_range_downloads: false   # leave false — RAPID uses bidi streaming,
                                  # not range requests
```

### Disable RAPID (standard GCS, force standard path)

```yaml
s3dlio_optimization:
  gcs_rapid_mode: false           # forceOff: standard read/write path
  enable_range_downloads: false
```

---

## Channel Count

**Implemented (see `Config::apply_gcs_defaults()` in `src/config.rs`)**.

sai3-bench sets the gRPC subchannel count in two ways:

**Smart default** (no `gcs_channel_count` in YAML):
- `set_gcs_channel_count(concurrency)` — one subchannel per concurrent task
- No change to `config.concurrency`
- Logged at `INFO`

**Explicit `gcs_channel_count: N`**:
- `set_gcs_channel_count(N)` called in `apply()` (direct s3dlio Rust API)
- YAML `concurrency` treated as per-channel → total = `concurrency × N`
- Total capped at `num_cpus::get()`
- `WARN` emitted for any adjustment

The scaled `config.concurrency` takes effect BEFORE the Tokio semaphore is
created for both prepare and run phases, so both use the correct concurrency.

Until upgrading to this version, you can still set channel count via env var:

```bash
export S3DLIO_GCS_GRPC_CHANNELS=64
./sai3-bench run --config my_gcs_bench.yaml
```

---

## Python API

s3dlio v0.9.65 adds GCS tuning functions to the Python module.  Any Python
workload (DLIO integration, custom scripts, PyTorch DataLoader wrappers) can
call these before issuing `gs://` I/O:

```python
import s3dlio

# Set channel count to match your concurrency BEFORE first gs:// I/O
s3dlio.gcs_set_channel_count(64)

# Optionally force RAPID mode (default is auto-detect per bucket)
s3dlio.gcs_set_rapid_mode(True)   # True / False / None

# Read back current settings
channels = s3dlio.gcs_get_channel_count()   # 0 = not set, will auto-detect
rapid    = s3dlio.gcs_get_rapid_mode()      # True / False / None

# Query a specific bucket (cached for process lifetime)
is_rapid = s3dlio.gcs_query_rapid_bucket("gs://my-bucket/")
print(f"Bucket is RAPID: {is_rapid}")
```

| Python function | Equivalent Rust | Notes |
|----------------|----------------|-------|
| `gcs_set_channel_count(n)` | `s3dlio::set_gcs_channel_count(n)` | Must call before first I/O |
| `gcs_set_rapid_mode(force)` | `s3dlio::set_gcs_rapid_mode(force)` | Must call before first I/O |
| `gcs_get_channel_count()` → `int` | `s3dlio::get_gcs_channel_count()` | 0 = unset |
| `gcs_get_rapid_mode()` → `bool\|None` | `s3dlio::get_gcs_rapid_mode()` | None = auto |
| `gcs_query_rapid_bucket(uri)` → `bool` | `s3dlio::query_gcs_rapid_bucket(uri)` | Blocking; cached |

---

## Environment Variable Overrides

All s3dlio GCS environment variables work alongside YAML config.  Variables
always take precedence over programmatic settings.

| Variable | Recommended value | Notes |
|----------|------------------|-------|
| `S3DLIO_GCS_RAPID` | *(not needed — use YAML)* | `true` / `false` / `auto` |
| `S3DLIO_GCS_GRPC_CHANNELS` | *(not needed — auto)* | Override subchannel count |
| `S3DLIO_GRPC_INITIAL_WINDOW_MIB` | `128` (default) | Increase to `256` for >64 jobs |
| `S3DLIO_GRPC_WRITE_CHUNK_SIZE` | `2097152` (default) | Max safe: `4128768` (4 MiB − 64 KiB) |

---

## Performance Reference

Measured on **c4-standard-8** (8 vCPU) against a RAPID bucket, 32 jobs,
1,000 × 32 MiB objects:

| Operation | Throughput | Notes |
|-----------|-----------|-------|
| Multi-part PUT (RAPID) | **3.83 GB/s** | 32 jobs, 2 MiB write chunks |
| GET (bidi streaming, RAPID) | TBD | Run your own workload to characterise |

Note: RAPID objects must be read back with the bidi-read API.  s3dlio handles
this automatically when RAPID mode is on (forced or auto-detected).

---

## Known Issues and Limitations

### `enable_range_downloads` has no effect for GCS

GCS GET uses the `BidiReadObject` streaming RPC, not HTTP byte-range requests.
Setting `enable_range_downloads: true` in YAML activates the S3 range-read code
path, which does not apply to `gs://` URIs.  Always set it to `false` (or omit
it) for GCS benchmarks to avoid confusion in output logs.

### GCS write chunk size maximum

The maximum safe value for `gcs_write_chunk_size_bytes` is **4 128 768**
(4 MiB − 64 KiB).  This is the gRPC maximum frame size minus the envelope
overhead.  Values larger than this will be silently truncated or rejected by
the gRPC layer.  Recommended default: 2 097 152 (2 MiB).

### `aws-config` / `aws-sdk-s3` version gap (resolved)

The version conflict (`aws-config =1.8.11` vs s3dlio 0.9.65 requiring `=1.8.14`)
was resolved by Change 1 above.

---

## Configuration Priority Summary

For any GCS setting, the effective value is determined from highest to lowest
priority:

```
  env var (S3DLIO_GCS_*)
      ↓
  YAML s3dlio_optimization.gcs_rapid_mode           → set_gcs_rapid_mode()
  YAML s3dlio_optimization.gcs_channel_count        → set_gcs_channel_count()
  YAML s3dlio_optimization.gcs_write_chunk_size_bytes → S3DLIO_GRPC_WRITE_CHUNK_SIZE env var
      ↓
  smart default: set_gcs_channel_count(concurrency) → no scaling
  concurrency scaling: concurrency × N → config.concurrency mutated in-place
      ↓
  s3dlio library defaults (RAPID auto-detect; channel count from hardware)
```

---

## References

- [s3dlio GCS API Configuration](../../s3dlio/docs/GCS-API-Configuration.md) — full constant and API reference
- [s3dlio GCS gRPC Fixes](../../s3dlio/docs/GCS-gRPC_Fixes.md) — root cause analysis of RESOURCE_EXHAUSTED + zero-copy rewrite
- [S3DLIO Performance Tuning](S3DLIO_PERFORMANCE_TUNING.md) — general s3dlio tuning for sai3-bench (S3 focused)
