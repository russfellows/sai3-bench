# Handoff Document — sai3-bench v0.8.71

**Date**: 2026-03-16  
**Branch**: `release/v0.8.70`  
**Status**: All code changes complete, `cargo check` clean, **pending git commit**  
**Prepared for**: Next chat agent continuing v0.8.71 development

---

## 0. How to Get Up to Speed

1. Read `docs/GCS_WORKER_HANG_INVESTIGATION_v0.8.71.md` **first** — it contains
   the full evidence trail, root-cause analysis, and detailed explanation of the
   two fixes that were implemented this session. This handoff document does NOT
   duplicate that material.

2. Read `WORKSPACE_STATUS_DEC_2025.md` (workspace root) for overall project
   architecture and cross-project dependency map.

---

## 1. What Was Investigated and Fixed This Session

### 1.1  Fixes inherited from prior session (already confirmed on live cluster)

| Area | File | Description |
|------|------|-------------|
| Barrier auto-enable | `src/bin/controller.rs` | Multi-stage configs with no explicit barrier flags now auto-enable barriers |
| Object storage preflight | `src/preflight/object_storage.rs` | Real `list_stream` bucket accessibility check (was a stub) with 30s timeout |
| Cloud metadata detection | `src/bin/agent.rs` | GCP/EC2/Azure metadata probe now checks HTTP status, not just TCP reachability |
| AI/ML YAML configs | `tests/configs/ai-ml/*.yaml` (7 files) | All converted to multi-stage with barriers, perf_log, force_overwrite |

All four were confirmed working on a live GCS RAPID single-agent run.

### 1.2  Fixes implemented this session

**Problem**: Execute stage GET throughput collapsed to 0 at T+154s and reported
zero IOPS for 321 more seconds. Stage overran the configured 300s deadline,
finishing at 475.82s. Full diagnosis in `docs/GCS_WORKER_HANG_INVESTIGATION_v0.8.71.md`.

| Fix | File | Description |
|-----|------|-------------|
| Per-op timeout | `src/workload.rs`, `src/constants.rs` | 120s `tokio::time::timeout` on `get/put/stat/delete_object_cached`. Constant: `DEFAULT_OP_TIMEOUT_SECS = 120` |
| Bounded drain | `src/workload.rs`, `src/constants.rs` | Replaced unbounded `for h in handles { h.await }` with `timeout_at` + `h.abort()`. Constant: `DEFAULT_WORKER_DRAIN_TIMEOUT_SECS = 150`. Logs how many workers were aborted. |

### 1.3  State of the repository

```
Branch:  release/v0.8.70
cargo check: CLEAN (zero warnings, zero errors)
git status: 20 files modified/added, uncommitted
```

The commit was deferred pending this handoff document. Once documentation is
complete, commit all staged changes:

```bash
cd /home/eval/Documents/Code/sai3-bench
git add -A
git commit -m "v0.8.70: barrier auto-enable, preflight, cloud metadata, op timeouts, bounded drain, AI/ML configs"
git tag v0.8.70
git push origin release/v0.8.70 --tags
```

---

## 2. Open Issues — Ordered by Priority

### 2.1  🔴 CRITICAL: Performance gap vs raw s3dlio capability

This is the most important unresolved problem.

**s3-cli on the same GCS RAPID cluster, same s3dlio library version:**

```
128 threads:   6.36 GiB/s GET    0 errors
256 threads:  10.54 GiB/s GET    0 errors
```

**sai3-bench on the same cluster, same files, concurrency=32:**

```
Peak:          2.19 GiB/s GET    ~5,554 zero-byte returns
Sustained avg: 0.20 GiB/s (dragged down by the stall — see investigation doc)
```

**The gap is ~5x at comparable concurrency, and sai3-bench uses the same s3dlio
library.** This is not acceptable. The hang fix will improve the sustained
average, but it will not close the 5x peak gap.

#### Hypotheses to investigate (in priority order):

**H1 — Shared `GcsObjectStore` vs per-thread stores**

sai3-bench creates ONE `GcsObjectStore` per base URI before workers start, and
all `concurrency` workers share it. The store has `gcs_channel_count=32` gRPC
subchannels. With 32 workers each issuing sequential requests, there is no
headroom for pipelining — every worker holds one subchannel for the duration of
its current GET.

`s3-cli` with 128 threads creates 128 independent stores (or at minimum 128
independent connections), multiplying the effective channel depth.

**To test**: Try setting `gcs_channel_count` to 128 or 256 in the config and/or
creating per-worker `GcsObjectStore` instances. See if peak throughput improves.

**H2 — Object size mismatch**

`s3-cli` benchmarks may use large objects (e.g. 100 MB). The unet3d dataset
objects may be much smaller. GCS RAPID throughput is highly sensitive to object
size for small objects (IOPS-limited).

Check the actual size distribution in `03_execute_results.tsv` (the `size_bucket`
histogram columns). If most ops are in the 1–10 MB range, the throughput ceiling
is lower than for 100 MB objects.

**H3 — Hot-path overhead in the worker loop**

Each sai3-bench worker does (per iteration): deadline check → path selection →
store lookup (Arc clone) → metrics recording (atomic increments) → HDR histogram
update (mutex?). The metrics and histogram updates at 650 IOPS × 32 workers may
be adding latency that `s3-cli` (which just times the call) does not have.

Profile with `cargo flamegraph` or add timing probes around the non-I/O sections
of the worker loop to quantify overhead.

**H4 — Single-object sequential worker model**

Each worker completes one GET before starting the next. With mean latency ≈ 45ms
and concurrency=32, theoretical max throughput is `32 / 0.045s ≈ 711 IOPS`. That
matches the observed 650 IOPS peak — so we are already at the concurrency limit.

To go higher, increasing `concurrency` would help directly. Try
`concurrency: 64` or `concurrency: 128` and measure if throughput scales
proportionally (if it does, H1/H2 is the bottleneck; if it doesn't, H3 is).

**H5 — Prefill/warmup stage not running**

The first test run may not have a `prefill` or `warmup` stage, meaning the data
generation step is skipped and objects are pre-existing. Confirm the test config.
If a prefill ran correctly and objects are the right size, this hypothesis is
ruled out.

#### Suggested next test matrix

Run on the live GCS RAPID cluster once the hang fix is committed:

```yaml
# Test A: baseline with fix
concurrency: 32
gcs_channel_count: 32

# Test B: more concurrency
concurrency: 128
gcs_channel_count: 32

# Test C: more channels
concurrency: 32
gcs_channel_count: 128

# Test D: both scaled up
concurrency: 128
gcs_channel_count: 128
```

Compare peak IOPS, peak GiB/s, and error counts across tests.

---

### 2.2  🟡 MEDIUM: Zero-byte GET returns silently counted as success

From the last run: 5,554 GETs returned `Ok(Bytes::new())`. These are not counted
as errors, so they pass through error-threshold checks unnoticed. They land in
the `bucket_index(0)` = "zero" size histogram bucket.

**Why this matters**: A high zero-byte rate indicates silent data corruption or
partial connection resets. If this rate is high during a future run (after the
hang fix), it deserves its own investigation — it may be a GCS RAPID-specific
quirk with s3dlio's streaming GET implementation.

**Suggested fix**: In `get_object_cached`, after the timeout wrapper, check:
```rust
if bytes.is_empty() {
    return Err(anyhow!("GET returned empty body (0 bytes): {}", uri));
}
```
This turns silent zero-byte returns into counted, retriable errors.

**First step**: Run with the hang fix and observe whether zero-byte count drops
(they may have been caused by the same gRPC stall, not a separate bug).

---

### 2.3  🟡 MEDIUM: gRPC channel-level reconnection not implemented

Both timeout fixes treat a stalled channel as a per-operation problem: timeout,
report error, move on. They do NOT force the `GcsObjectStore` to drop and
recreate its gRPC subchannels.

In a severe stall, up to 32 workers may each wait up to 120s before timing out.
During that window throughput collapses. A better fix would detect N consecutive
timeouts and signal s3dlio to reset the channel.

This requires either:
- An s3dlio API addition (`store.reset_connections()` or similar), or
- sai3-bench recreating the store on repeated timeout (expensive, requires Arc
  swap or per-worker store ownership)

**Tag this as a future s3dlio enhancement**. File an issue in s3dlio referencing
this hang scenario.

---

### 2.4  🟢 LOW: Multi-agent distributed test not yet run with all fixes

The single-agent test confirmed the hang fixes compile and the barrier/preflight
fixes work. A multi-agent run (2+ agents) with the full AI/ML config suite has
not been executed this session.

The configs in `tests/configs/ai-ml/` that use `distributed.agents: 2+` need to
be validated on actual multi-agent infrastructure before v0.8.71 can be called
fully tested.

---

## 3. Test Environment Reference

| Item | Value |
|------|-------|
| Cluster | GCS RAPID |
| Bucket | `gs://sig65-rapid1/unet3d/` |
| Agent | `10.128.0.11:7761` (single agent, GCP VM) |
| s3dlio version | v0.9.65 (GCS gRPC, RAPID mode) |
| Test config | `tests/configs/ai-ml/unet3d_1-host.yaml` |
| Last run output | `/tmp/sai3-20260316-2126-unet3d_1-host_gcs-rapid/` (on agent VM) |
| s3-cli peak | 10.54 GiB/s GET (256 threads, same bucket) |
| sai3-bench peak | 2.19 GiB/s GET (concurrency=32) |

---

## 4. Immediate Next Steps for New Agent

1. **Commit** the pending changes (see §1.3 for commit command).
2. **Run** `tests/configs/ai-ml/unet3d_1-host.yaml` on the GCS RAPID cluster
   with the hang fixes, and confirm:
   - No IOPS cliff (no more 321s of zeros)
   - Stage wall time ≤ 420s (300s + 120s timeout bound)
   - Zero-byte GET count — does it drop?
3. **Investigate** the 5x performance gap (§2.1). Start with H4 (increase
   `concurrency` to 128) as the quickest thing to test — it requires no code
   changes, just a config update.
4. If H4 shows linear scaling, increase `gcs_channel_count` to match s3-cli's
   degree of parallelism and attempt to reach 6+ GiB/s.
5. If H4 shows diminishing returns, instrument the worker hot path to find where
   time is being spent outside of `store.get()`.
