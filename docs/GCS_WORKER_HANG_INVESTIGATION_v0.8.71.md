# GCS Worker Hang Investigation & Fix — v0.8.71

**Date**: 2026-03-16  
**sai3-bench version**: v0.8.70 (fixes shipped as v0.8.70 commit, labelled 0.8.71 changes)  
**Test config**: `tests/configs/ai-ml/unet3d_1-host.yaml`  
**Target backend**: `gs://sig65-rapid1/unet3d/` (GCS RAPID)  
**Symptom**: Execute stage GET throughput fell from 650 IOPS to 0 at T+154s (146s
before the 300s deadline), then reported 0 IOPS for 321 more seconds. Total wall
time: 475.82s vs 300s configured.

---

## 1. Observed Symptoms

### perf_log.tsv profile for execute stage

```
T=9s    650 IOPS   2219 MiB/s   mean=53ms   ← peak
T=30s   355 IOPS   1199 MiB/s   mean=48ms
T=60s  ~220 IOPS    ~750 MiB/s  mean=46ms   ← declining
T=90s    53 IOPS    154 MiB/s   mean=45ms   ← still declining
T=153s   13 IOPS     33 MiB/s   mean=45ms   ← last non-zero row
T=154s    0 IOPS      0 MiB/s   mean=45ms   ← hard cliff
T=155s–T=475s: 0 IOPS for 321 consecutive seconds
```

Two features of this trace are diagnostic:

1. **Latency stayed constant (45ms) throughout the decline.** If the network or
   storage were degrading you would see latency spike first. Constant latency with
   falling IOPS means workers are completing their current operations normally but
   then not *starting new ones*.  
2. **Gradual linear decline, not a sudden crash.** Starting ~T=20s, throughput fell
   at roughly 4–5 IOPS per second — matching the rate at which 32 workers would
   each commit to one in-flight call that never returns.

### 03_execute_results.tsv summary

```
Total GET ops:     28,531
Total GET bytes:   101.24 GB
Wall time:         475.82s
Mean IOPS:          59.96 ops/s   ← dragged down by 321s of zeros
Mean bandwidth:    202.91 MiB/s
"zero" size bucket: 5,554 ops     ← GET succeeded but returned 0 bytes
Hard error count:      32
```

The 5,554 "zero" bucket ops are GETs that returned `Ok(Bytes::new())` — not
errors, so they bypassed the error threshold. Object size bucketing records them
as zero-byte reads.

---

## 2. Root Cause Analysis

### What was happening inside the workers

`workload::run()` spawns `concurrency` (32) Tokio tasks. Each worker sits in a
tight loop:

```
loop {
    if Instant::now() >= deadline { break; }  ← deadline = T+300s
    ...
    let bytes = store.get(uri).await          ← NO TIMEOUT
        .with_context(...)?;
    ...
}
```

All `store.get()` calls go to the same pre-created `GcsObjectStore` instance,
which uses a fixed pool of gRPC subchannels (32 in this config).

**What a gRPC subchannel hang looks like:**  
GCS RAPID uses HTTP/2 multiplexing over long-lived gRPC connections. Under certain
conditions (load-balancer reset, connection idle timeout, RAPID transport fault)
an active gRPC stream can stop delivering data without signalling an error. The
`store.get()` future awaits bytes from the stream indefinitely — it never returns
an `Err`, it simply never resolves.

**The decline timeline explained:**

- T=0 to T=9s: All 32 workers are active, system at full throughput.
- T≈9s onward: Individual workers start `store.get()` calls that hang. As each
  worker commits to a hung call it stops contributing IOPS. With 32 workers and
  ~5 hanging per second, throughput falls linearly.
- T≈154s: The last non-hung worker starts its final hanging call. All 32 workers
  are now blocked inside `store.get().await`. IOPS → 0.
- T+154s to T+475s: All workers are suspend inside in-flight gRPC reads. Progress
  bar shows 0 IOPS. The stage deadline fired at T+300s but the workers that were
  past the deadline check are already inside `store.get()` — the `if elapsed >=
  deadline { break; }` check only runs at the *top* of the loop.
- T≈475s: GCS server-side connection reset / keepalive timeout fires. All 32
  pending gRPC calls finally return (with an error or empty body). Workers check
  `elapsed >= deadline`, see 475s >> 300s, and break. The `for h in handles {
  h.await }` join then completes immediately.

**Why s3dlio itself was fine:**  
`s3-cli put/get` opens fresh connections per invocation and completes in seconds.
sai3-bench pre-creates one `GcsObjectStore` and reuses it for 32 workers across
300s. A hung channel that wouldn't affect a short-lived CLI invocation freezes
sai3-bench indefinitely because there was no timeout safety net.

### Why the stage ran 475s instead of 300s

The original drain code:

```rust
for h in handles {
    match h.await {   // ← NO TIMEOUT
```

This is an unconditional `.await` on each join handle. After the workers finally
unblocked at T≈475s the loop completed instantly, but none of the 321-second
"dead time" was recoverable. The drain itself was not the *cause* of the
overrun — the per-op hang was — but the lack of a drain timeout means that if
`store.get()` had hung forever (no server-side reset), sai3-bench would have
waited forever too.

### The 5,554 zero-byte reads

GCS sometimes returns an empty body for a request that hit a degraded or
resetting subchannel, rather than an explicit error. The `store.get()` call
resolves with `Ok(Bytes::new())`. This bypasses error counting and lands in the
`bucket_index(0)` = "zero" histogram bucket. These are not the stuck workers
(stuck workers never returned at all); they are early-stage transient failures
that preceded the hang.

---

## 3. Fixes Implemented

### Fix 1 — Per-operation timeout on all cached store calls (`workload.rs`)

Added a 120-second `tokio::time::timeout` wrapper around every storage operation
in the hot path. A timed-out call surfaces as an `Err` which is handled by the
existing `ErrorHandlingConfig` logic (skip / retry / abort).

**Constant added to `constants.rs`:**

```rust
/// Per-operation timeout for object storage calls (GET/PUT/STAT/DELETE).
pub const DEFAULT_OP_TIMEOUT_SECS: u64 = 120;
```

**Pattern applied to `get_object_cached` (same pattern for put/stat/delete):**

```rust
let timeout_secs = crate::constants::DEFAULT_OP_TIMEOUT_SECS;
let bytes = tokio::time::timeout(
    Duration::from_secs(timeout_secs),
    store.get(uri),
)
.await
.map_err(|_| anyhow!("GET timed out after {}s: {}", timeout_secs, uri))?
.with_context(|| format!("Failed to get object from URI: {}", uri))?;
```

Functions patched: `get_object_cached`, `put_object_cached`,
`delete_object_cached`, `stat_object_cached`.

**Effect in the hung-channel scenario:**  
Workers that commit to a stalled `store.get()` will now receive a timeout error
after at most 120s. The error goes through the normal error-handling path:

- With default `error_handling` (skip, max 100 errors): the operation is skipped
  and the worker proceeds to its next iteration, checking the deadline.
- If the channel is completely dead, all 32 workers will time out within 120s of
  each other, hit the deadline check, and break cleanly.
- Worst-case stage overrun is now bounded to `120s` (the per-op timeout) instead
  of unlimited.

### Fix 2 — Bounded worker drain after deadline (`workload.rs`)

Replaced the unbounded `for h in handles { h.await }` with a deadline-based
drain that aborts stuck workers after `DEFAULT_WORKER_DRAIN_TIMEOUT_SECS`:

**Constant added to `constants.rs`:**

```rust
/// Maximum time to wait for worker tasks to finish after the workload
/// deadline fires (the "drain" phase).
pub const DEFAULT_WORKER_DRAIN_TIMEOUT_SECS: u64 = 150;
```

**New drain logic:**

```rust
let drain_budget = Duration::from_secs(crate::constants::DEFAULT_WORKER_DRAIN_TIMEOUT_SECS);
let drain_deadline = tokio::time::Instant::now() + drain_budget;
let mut drained = 0usize;
let total_handles = handles.len();

for mut h in handles {
    match tokio::time::timeout_at(drain_deadline, &mut h).await {
        Ok(join_result) => {
            drained += 1;
            // ... merge stats or record error as before
        }
        Err(_elapsed) => {
            h.abort();
            let stuck = total_handles - drained;
            warn!("⚠️  Worker drain timed out after {}s ({}/{} workers completed); \
                   {} stuck worker(s) aborted.", ...);
            workload_error = Some(anyhow!("Worker drain timeout: ..."));
            break;
        }
    }
}
```

Aborted workers whose in-flight `store.get()` eventually resolves are cleaned up
by Tokio automatically (the JoinHandle was explicitly `abort()`ed).

**Effect:** The stage wall time is now bounded to approximately
`configured_duration + DEFAULT_OP_TIMEOUT_SECS + small_epsilon`. With the current
constants that is `300s + 120s = 420s` worst case, down from unlimited.

---

## 4. Interaction between Fix 1 and Fix 2

Fix 1 (per-op timeout) is the primary defence. It fires while workers are still
running their loop, surfaces a clean error, and allows the deadline check to
take effect at the top of the next iteration.

Fix 2 (bounded drain) is a secondary safety net that protects against future
bugs where Fix 1 might not fire (e.g. a hang inside a code path that was not
patched, or a new backend added without a timeout wrapper).

The two timeouts are intentionally non-overlapping:  
- Per-op timeout (120s) fires first — workers break out, stage ends.  
- Drain budget (150s) is larger than 120s — by the time the drain runs, any
  worker whose per-op timeout fired will have already exited cleanly.

---

## 5. Expected Behaviour After Fix

### With a single stalled gRPC subchannel (partial hang):

| Phase | Before fix | After fix |
|-------|-----------|-----------|
| Worker hits stalled GET | Hung forever | Times out after ≤120s, GET counted as error, worker continues |
| Other workers | Unaffected | Unaffected |
| Stage completion | Runs until all subchannels stall | Completes at deadline + ≤120s overrun |

### With all subchannels stalled (total hang, re-creates the observed incident):

| Metric | Before fix | After fix |
|--------|-----------|-----------|
| IOPS → 0 at | T+154s (when last worker hangs) | T + ~120s after first hang (timeout fires, worker breaks at deadline or retries) |
| Stage wall time | 475s (server-side reset saved it) | ≤ configured_duration + 120s |
| Barrier outcome | Fails ("insufficient agents") | Success (agent exits stage within the configured timeout_secs) |
| drain overrun | 321s → potentially infinite | Bounded to 150s |

---

## 6. What Was NOT Fixed

### Zero-byte GET returns

The 5,554 zero-byte reads are not addressed by these fixes. They are a different
class of problem: the gRPC call returns `Ok(empty_bytes)` rather than hanging.
Possible causes:
- s3dlio returning empty body on subchannel RST during body streaming
- Race between connection reset and response body completion

These operations are silently counted as "zero" size successful GETs. A future
improvement would be to detect `bytes.is_empty()` in `get_object_cached` and
treat it as a retriable error.

### Channel-level reconnection strategy

Both fixes treat a stalled channel as a per-operation problem (timeout, report
error). They do not force the `GcsObjectStore` to drop and recreate the stalled
gRPC subchannel. With the fix, 32 workers may time out one by one over 120s
before the stall clears, producing lower throughput during that window. A more
aggressive fix would detect repeated timeouts on the same subchannel and trigger
s3dlio-level reconnection — that is a future enhancement.

---

## 7. Files Changed

| File | Change |
|------|--------|
| `src/constants.rs` | Added `DEFAULT_OP_TIMEOUT_SECS = 120`, `DEFAULT_WORKER_DRAIN_TIMEOUT_SECS = 150` |
| `src/workload.rs` | 120s `tokio::time::timeout` on `get/put/stat/delete_object_cached`; bounded drain loop with `timeout_at` + `h.abort()` |
