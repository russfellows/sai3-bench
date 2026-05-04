# Hot Path Performance & Reliability Analysis — v0.8.71

**Date**: 2026-03-16  
**sai3-bench version**: v0.8.70 (commit 1cba58a)  
**Context**: Post GCS Worker Hang fix, pre-performance optimization  
**Goal**: Close 5x throughput gap vs raw s3dlio (s3-cli), improve reliability

---

## 1. Hot Path Trace — Worker Loop in `workload.rs`

Each of the `concurrency` worker tasks (Tokio spawn) executes this per iteration:

```
loop {
    if Instant::now() >= deadline { break; }
    idx = chooser.sample(&mut rng())           // weighted random op selection
    rate_controller.wait().await               // rate limiting (if configured)
    _p = op_sems[idx].acquire().await          // ← SEMAPHORE (P0)
    match op {
        Get { .. } => {
            full_uri = resolve_uri(...)        // path selection
            cfg_for_get = cfg.clone()          // ← DEEP CLONE (P1)
            store_cache_get = store_cache.clone()  // Arc clone (cheap)
            result = execute_with_error_handling("GET", &error_tracker, || async {
                bytes = get_object_cached(uri, ...)  // 120s timeout wrapper
            })
            match result {
                Ok(Some((bytes, duration))) => {
                    ws.hist_get.record(bucket, duration)   // ← MUTEX (P4)
                    ws.get_ops += 1                        // local counter
                    tracker.record_get(bytes.len(), dur)   // ← SHARED MUTEX (P2)
                    ops_counter.fetch_add(1, Relaxed)      // ← SHARED ATOMIC (P3)
                    bytes_counter.fetch_add(len, Relaxed)  // ← SHARED ATOMIC (P3)
                }
            }
        }
        // PUT, STAT, DELETE, LIST follow same pattern
    }
}
```

---

## 2. Issues Found — Ordered by Performance Impact

### P0 (CRITICAL): Semaphore on every operation — `workload.rs` ~line 2054

```rust
let _p = op_sems[idx].acquire().await.unwrap();
```

Every single I/O operation calls `Semaphore::acquire().await`, even though **all
permits are always available** in the common case:

- `op_semaphores` is created at line 1779 with `Semaphore::new(concurrency)`
  permits per operation type.
- There are exactly `concurrency` worker tasks.
- Each worker does one op at a time (sequential loop).
- So the semaphore can **never** block — it is pure overhead.

**Cost per-op**: async poll machinery + atomic CAS contention across 32+ workers.
At 650 IOPS × 32 workers = ~20,800 semaphore acquire/release cycles/sec. This is
the single biggest unnecessary overhead in the hot path.

The semaphore only adds value when `wo.concurrency < cfg.concurrency` (per-op
limit lower than global). For the common case (no per-op override), it should be
bypassed entirely.

**Why not crossbeam channels?** Crossbeam channels are blocking (not async-aware),
so they'd stall the Tokio runtime. But we don't need a replacement at all — the
spawned worker count IS the concurrency limit. For the rare per-op limiting case,
a simple `AtomicUsize` counter check suffices.

**Fix**: Remove semaphore entirely. If any `wo.concurrency` is set, use an
`Option<Arc<Semaphore>>` that is `None` for ops where limit == global concurrency.

---

### P1 (HIGH): Config clone on every operation — `workload.rs` ~lines 2108, 2224

```rust
let cfg_for_get = cfg.clone();  // Full deep clone of Config struct!
```

`Config` is a heavyweight struct containing `Vec<WeightedOp>`, multiple
`String`/`Option<String>`, nested `DistributedConfig`, `ErrorHandlingConfig`,
`MultiEndpointConfig`, etc. This is cloned **on every single GET/PUT/STAT/DELETE**
to satisfy the `execute_with_error_handling` closure's `Fn() -> Fut` signature
(needs ownership for retries).

**Fix**: Wrap in `Arc<Config>` at worker spawn time. Arc clone = one atomic
increment vs hundreds of bytes of heap allocation + copy.

---

### P2 (HIGH): Shared Mutex on LiveStatsTracker histogram — `live_stats.rs` line 157

```rust
let _ = self.get_hist.lock().record(us);
```

`live_stats_tracker.record_get()` acquires a **shared `Mutex<Histogram>`** across
ALL workers on every successful GET. This is a contention bottleneck.

In standalone mode (`sai3-bench run`) this is `None` (skipped). But in distributed
agent mode — which is exactly the GCS RAPID test scenario — it is `Some` and
every operation hits this lock.

**Fix**: Either use lock-free histogram (e.g., per-worker histograms merged at
snapshot time), or downgrade to atomic-only tracking (drop per-op histogram from
the live tracker, keep it only in `WorkerStats`).

---

### P3 (MEDIUM): Shared atomic counters — `workload.rs` ~lines 2134–2135

```rust
ops_counter.fetch_add(1, Ordering::Relaxed);
bytes_counter.fetch_add(bytes.len() as u64, Ordering::Relaxed);
```

Two shared `AtomicU64` values (`live_ops`, `live_bytes`) hit by all workers on
every operation. Cache-line bouncing across cores. Less impactful than the
semaphore but adds up at high IOPS.

**Fix**: Use per-worker local counters and periodically flush to shared atomics
(e.g., every 64 ops), or accept the minor cost since Relaxed ordering is cheap.

---

### P4 (LOW): Unnecessary Mutex in per-worker OpHists — `metrics.rs` line 40

```rust
pub buckets: Arc<Vec<Mutex<Histogram<u64>>>>,
```

Each worker has its **own** `WorkerStats`, so the Mutex inside `OpHists.record()`
is never contended. But it still has the overhead of lock/unlock (uncontended CAS
on every operation).

**Note**: This is a design artifact — shared `OpHists` needs Mutex for the merge
path, but per-worker instances don't. Minor impact.

---

### P5 (RELIABILITY): Zero-byte GET returns silently succeed — `workload.rs` line 803

```rust
Ok(bytes)  // Returns empty Bytes as success — no size check!
```

`get_object_cached` returns `Ok(Bytes::new())` when GCS returns an empty body on
a stalled/reset subchannel. This bypasses error tracking entirely. **5,554 such
operations** were observed in the last GCS RAPID test run.

These are counted as successful GETs in the "zero" size histogram bucket, masking
what are actually failed reads.

**Fix**: Add `if bytes.is_empty() { return Err(...) }` check in
`get_object_cached`, turning zero-byte returns into counted, retriable errors.

---

### P6 (RELIABILITY): LIST operation missing timeout — `workload.rs` ~line 885

```rust
let keys = store.list(uri, true).await  // NO timeout wrapper!
```

`list_objects_cached` doesn't have the 120s `tokio::time::timeout` that was added
to get/put/stat/delete in the v0.8.70 hang fix. If a LIST operation hangs on a
stalled gRPC channel, the same infinite-hang problem recurs.

**Fix**: Add the same `tokio::time::timeout` pattern used in the other operations.

---

## 3. `gcs_channel_count` Config Mapping

Confirmed mapping chain:

```
YAML: s3dlio_optimization.gcs_channel_count: N
  → config.rs: S3dlioOptimizationConfig.apply() → s3dlio::set_gcs_channel_count(N)
  → s3dlio: sets gRPC subchannel count for GcsObjectStore

Smart default (no explicit setting):
  → config.rs: apply_gcs_defaults() → s3dlio::set_gcs_channel_count(concurrency)
  → One gRPC subchannel per worker task
```

With concurrency=32 + default, you get 32 gRPC subchannels (1:1).  
s3-cli with 128 threads → 128 channels → 4x gRPC parallelism multiplier.

---

## 4. Implementation Plan

| Priority | Issue | Impact | Effort | Status |
|----------|-------|--------|--------|--------|
| **P0** | Remove semaphore from hot path | Eliminates async+atomic overhead on every op | Small | TODO |
| **P1** | `Arc<Config>` instead of clone | Eliminates deep clone on every op | Small | TODO |
| **P2** | LiveStatsTracker histogram contention | Eliminates shared Mutex in agent mode | Medium | TODO |
| **P3** | Per-worker atomic counters | Eliminates cache-line bouncing | Small | TODO |
| **P4** | OpHists per-worker Mutex removal | Minor optimization | Low | DEFERRED |
| **P5** | Zero-byte GET detection | Reliability fix | Small | TODO |
| **P6** | LIST timeout wrapper | Reliability fix | Tiny | TODO |

P4 is deferred — the impact is minimal (uncontended mutex) and fixing it requires
a more invasive refactor of the metrics module.

---

## 5. Expected Impact

### Performance

- P0 + P1 alone should reduce per-operation overhead by 50–80%.
- At 650 IOPS baseline: may unlock 800–1000+ IOPS at concurrency=32.
- Combined with increasing concurrency to 128 (config change, no code):
  theoretical ceiling moves from ~711 IOPS to ~2,844 IOPS.
- Combined with increasing `gcs_channel_count` to 128: matches s3-cli parallelism.

### Reliability

- P5 prevents 5,554+ silent zero-byte failures from being counted as success.
- P6 closes the last timeout gap (LIST operations).
