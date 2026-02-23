# Generation Pipeline Design Guide (High-IOPS Prepare + Workload)

**Status**: Design (planning only)  
**Audience**: Implementation handoff  
**Goal**: Decouple name/path/size generation from I/O to prevent generator bottlenecks in ultra-high-IOPS workloads (e.g., 4 KiB at 1M ops/s).  
**Scope**: Prepare + workload phases.

---

## 1) Problem Statement

Current prepare/workload generation happens on the scheduling thread, directly interleaved with task creation and I/O. For tiny objects, per-item CPU overhead (size generation, string formatting, manifest lookup, endpoint selection) can dominate and throttle I/O throughput. We need:

- Bounded memory (no full precompute)
- Deterministic generation (same config = same paths/sizes)
- High throughput for small-object workloads
- Minimal contention and predictable back-pressure

---

## 2) Design Goals

**Performance**
- Sustain generation throughput >= target I/O throughput (up to 1M ops/s)

**Memory**
- Bounded queue sizes with back-pressure

**Determinism**
- Stable per-object mapping (size, path, endpoint, agent partition) independent of runtime timing

**Extensibility**
- Pluggable queue backends to compare for performance

**Observability**
- Ability to measure queue depth, generator rate, consumer rate, stall time

---

## 3) Architecture Overview

**Pipeline**: generator threads build batches of `PreparedItem`, push to bounded queue; I/O workers consume batches and execute `store.put()` or workload operations.

```
Config → GeneratorPlans → Generator Threads → [bounded queue] → I/O Workers → store
```

Key idea: **decouple generation from I/O** while preserving deterministic ordering and bounded memory.

---

## 4) Core Data Structures

### 4.1 PreparedItem
Represents a single object operation unit.

```rust
struct PreparedItem {
    uri: String,
    size: u64,
    plan_id: usize,   // which ensure_objects / workload entry
    index: u64,       // deterministic index for repeatability
    endpoint_idx: usize,
    fill: FillPattern,
    dedup: usize,
    compress: usize,
    stage: StageType, // Prepare or Workload
}
```

### 4.2 GeneratorPlan
Immutable plan derived from config.

```rust
struct GeneratorPlan {
    plan_id: usize,
    count: u64,
    size_spec: SizeSpec,
    seed: u64,
    prefix: String,
    endpoints: Vec<String>,
    existing_indices: HashSet<u64>,
    force_overwrite: bool,
    skip_verification: bool,
    tree_manifest: Option<TreeManifest>,
    shared_storage: bool,
    agent_id: usize,
    num_agents: usize,
    stage: StageType,
}
```

### 4.3 PlanState
Mutable iterator state per plan, used by generator.

```rust
struct PlanState {
    plan: GeneratorPlan,
    next_idx: u64,
    size_generator: SizeGenerator,
}
```

### 4.4 Batch
Prepared items are passed in batches to amortize overhead.

```rust
struct Batch {
    items: Vec<PreparedItem>, // sized to BATCH_SIZE (1k–10k)
}
```

---

## 5) Generator Logic (Deterministic)

### 5.1 Interface

```rust
trait Generator {
    fn next_batch(&mut self, out: &mut Vec<PreparedItem>) -> bool;
    fn is_exhausted(&self) -> bool;
}
```

### 5.2 PrepareGenerator (round-robin interleave)

Pseudo-logic:
- Keep `PlanState` for each ensure_objects entry.
- Round-robin across plans to maintain size mixing.
- Skip indices using `existing_indices`, agent partitioning, and verification rules.
- Path resolution:
  - If `tree_manifest` exists → `manifest.get_file_path(idx)` (O(1))
  - Else → `prefix + idx` formatted path
- Endpoint selection: `idx % endpoints.len()`

### 5.3 WorkloadGenerator
- PUT: generate sizes/paths similar to prepare.
- GET/STAT/DELETE: derive paths from manifest/listing precomputed indices.
- LIST: generate list patterns instead of per-object items (not fed into pipeline).

---

## 6) Queue / Buffer Strategy (Priority #1)

Benchmark and select best bounded queue. Candidates:

- `crossbeam::queue::ArrayQueue` (lock-free MPMC)
- `crossbeam_channel::bounded`
- `ringbuf` crate (SPSC ring buffer)
- `flume::bounded` (fast + async)

**Preference**:
- **SPSC ring buffer** when a single generator feeds a single consumer.
- **MPMC** when multiple generator threads feed a shared I/O pool.

---

## 7) Scheduling Strategy (Priority #2)

Options:
1. **Spawn per item** (slow for tiny objects).
2. **Spawn per batch** (preferred).
3. **Fixed async loop with blocking queue** (best when bounded).

**Plan**:  
Consume a `Batch`, loop items within task, apply semaphore for I/O concurrency.

---

## 8) Generation Strategy (Priority #3)

- `SizeGenerator::new_with_seed(&spec, seed)` for deterministic sizes.
- `uri` creation uses `String::with_capacity()` for efficiency.
- Endpoint choice: `idx % endpoints.len()`.
- Use `TreeManifest::get_file_path()` for O(1) mapping.

---

## 9) Micro-Benchmarks

### A) Queue/Buffer Throughput (First)
Goal: compare queue backends.

- `ArrayQueue` vs `crossbeam_channel::bounded` vs `ringbuf` vs `flume`.
- Metrics: ops/s, latency, CPU.

### B) Scheduling Overhead (Second)
Compare spawn-per-item vs spawn-per-batch vs fixed loop.

### C) Generation Throughput (Third)
- Path only
- Size only
- Size + path + manifest lookup

### D) End-to-End (Fourth)
- 4 KiB, 1 MiB, 16 MiB
- Compare generator on/off

---

## 10) Rust Features / Crates

**Crates**
- `crossbeam` (queues, channels)
- `ringbuf` (SPSC ring)
- `flume` (async channels)
- `criterion` (micro-bench)
- `tokio` (async runtime, task scheduling)
- `bytes` (buffer generation)

**Language Features**
- `Arc`, `AtomicU64` for queue stats
- `Pin<Box<...>>` if async generators are used
- `std::time::Instant` for latency and throughput
- `#[cfg(feature = "gen-pipeline")]` gating

---

## 11) Metrics / Observability

Track:
- Generator throughput (items/s)
- Queue depth / fullness
- Consumer throughput
- Stall time (generator waiting for space, consumer waiting for items)

---

## 12) Acceptance Criteria

- Generator overhead does not cap throughput for 4 KiB workloads.
- Memory usage remains bounded by queue size.
- Deterministic outputs vs existing prepare/workload.
- Benchmarks show queue backend chosen is best for target regime.

---

## 13) Rollout Plan (Implementation Later)

1. Add generator module behind feature flag.
2. Add queue benchmarks.
3. Add scheduling benchmarks.
4. Integrate into prepare + workload.
5. Validate determinism + performance.
6. Remove legacy code path once stable.
