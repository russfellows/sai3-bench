# sai3-bench: Multi-Protocol I/O Benchmarking Suite

[![Version](https://img.shields.io/badge/version-0.8.94-blue.svg)](https://github.com/russfellows/sai3-bench/releases)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/russfellows/sai3-bench)
[![Tests](https://img.shields.io/badge/tests-712%20passing-success.svg)](https://github.com/russfellows/sai3-bench)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-green.svg)](https://www.rust-lang.org/)

**🚀 NEW (v0.8.94)**: **jemalloc global allocator + s3dlio v0.9.92** — Replaced the default glibc allocator with [tikv-jemallocator](https://crates.io/crates/tikv-jemallocator) v0.6 (`#[global_allocator]`), eliminating glibc arena contention and fragmentation. Profiling showed `malloc_consolidate` at ~3% and the allocator frame at ~52% of CPU cycles under load; jemalloc removes both bottlenecks. Measured improvement: **+3.6% throughput at t=32** (32,634 → 33,812 ops/s). Updated s3dlio dependency from v0.9.90 → **v0.9.92** (pinned tag).

**🚀 NEW (v0.8.92)**: **Credential forwarding + HTTP/2 + pre-flight improvements** — Controller now forwards cloud credentials (`AWS_*`, `GOOGLE_APPLICATION_CREDENTIALS`, `AZURE_STORAGE_*`) to agents over gRPC via `--env-file <path>` or from its own environment (disable with `--no-forward-env`). Agents apply credentials before pre-flight, eliminating the manual step of copying secrets to each host. See [docs/CREDENTIAL_FORWARDING.md](docs/CREDENTIAL_FORWARDING.md). s3dlio v0.9.90 adds **HTTP/2 (h2c) support** for S3-protocol `http://` endpoints: auto-probes h2c on first connection and falls back to HTTP/1.1 if refused; `https://` endpoints negotiate via TLS ALPN transparently. Enable via `S3DLIO_H2C=1` or `s3dlio_optimization.h2c: true` in YAML. Pre-flight fixes: per-agent endpoint filtering (fixes 64-error agent-2 regression), bucket-grouped output, actionable `[PERM]`/`[AUTH]`/`[CONF]`/`[NET]` error classification, agent version check table, and two new config validation warnings (redundant multi_endpoint, missing credentials hint). +57 tests (712 total).

**🚀 NEW (v0.8.90)**: **`populate_ledger.tsv` + dgen-data zero-copy fills** — Every prepare phase now writes a lightweight `populate_ledger.tsv` (always-on, independent of the KV cache) recording object counts, bytes, and throughput — usable even at trillion-object scale where listing is infeasible. Data generation rewritten using [`dgen-data`](https://crates.io/crates/dgen-data) v0.2.3: a rolling-pointer pool generates one 1 MB buffer and vends zero-copy `Bytes::slice()` windows per PUT, eliminating per-object allocation. PUT latency now split into **setup** vs. **I/O** histograms for better profiling.

**🚀 NEW (v0.8.89)**: **`enable_metadata_cache` config option** — disables the internal Fjall KV metadata cache for very large or simple workloads (> ~1 B objects/batch). Default `true` (fully backward-compatible). Set `false` to eliminate ~3.4 GB disk usage per 50 M objects and the ~15 s resume scan, at the cost of crash-resume capability. Both standalone and distributed dry-runs print a clear banner showing the current setting. Reference config: [`tests/configs/test_prepare_no_kvcache.yaml`](tests/configs/test_prepare_no_kvcache.yaml).

**🚀 NEW (v0.8.88)**: **KV cache compact encoding + coverage observability** — KV cache entries now use [postcard](https://crates.io/crates/postcard) binary encoding (56% smaller, 2× faster scans). At startup, a one-line cache summary reports object count and total logical storage (`📊 Cache summary: N objects | X.XX GiB`). Progressive WARN messages fire if a coverage scan exceeds 10 s. Preflight now queries the cache per-spec and logs coverage. Safe write-probe cycle validates writable endpoints before any benchmark I/O. Agent port changed to **7167** (was 7761) with automatic port-conflict detection on startup. +17 new tests.

**🚀 NEW (v0.8.86)**: **GCS RAPID storage fully working** (s3dlio v0.9.86) — `BidiWriteObject` PUTs and `BidiReadObject` GETs verified against Hyperdisk ML RAPID buckets. RAPID mode is auto-detected per bucket or forced via `gcs_rapid_mode: true`. Worker drain deadline bug fixed (execute stage now runs its full configured duration). Timer observability logs added.

**🚀 NEW (v0.8.70)**: **GCS RAPID gRPC support** (s3dlio v0.9.70) — per-trial channel count, range-download control, and write-chunk-size control sent over RPC to agents. **Autotune redesign** — all tuning parameters are YAML-only; new `channels_per_thread` parameter scales gRPC subchannels with thread count; `--dry-run` prints the full sweep plan (computed sizes, loop order, total cases, I/O estimate) before executing.

**🚀 NEW (v0.8.63)**: **Multi-endpoint checkpoint race condition fix** - Eliminates fatal workload aborts at 99% completion for shared storage. **s3dlio optimization support** - +76% GET throughput for large objects (≥64MB).

**🚀 NEW (v0.8.62)**: **Streaming prepare + dry-run memory sampling + stage-aligned perf-log timing** for safer large-scale runs.

**🚀 NEW (v0.8.61)**: **Explicit distributed stages + numeric barrier indices** for consistent orchestration across multi-agent runs. Use the new `convert` command to upgrade legacy YAML files.

**🚀 NEW (v0.8.60)**: **KV cache checkpoint restoration** - Complete resume capability! Checkpoints now automatically restored on startup, enabling agents to resume long-running prepare operations after crashes/restarts. Works for both standalone and distributed modes.

**🚀 (v0.8.53)**: **Critical multi-endpoint + directory tree fix** - GET/PUT/STAT/DELETE operations now correctly route to round-robin endpoints, fixing 0-ops workload failures. Enhanced dry-run shows ALL endpoints with full URIs.

**🚀 (v0.8.52)**: **Deferred retry for prepare failures** eliminates "missing object" errors during execution. Failed creates are automatically retried after the main loop with aggressive exponential backoff (10 attempts, up to 30s delay), ensuring completeness without impacting fast path performance. **Thousand separator display** in dry-run (64,032,768 files) and optional YAML input support ("64,032,768"). **Human-readable time units** in YAML: use "5m", "2h", "30s" instead of seconds (300, 7200, 30).

**🚀 NEW (v0.8.51)**: **Critical blocking I/O fixes** for large-scale deployments (>100K files). Configurable `agent_ready_timeout` (default 120s), non-blocking glob operations, and periodic yielding in prepare loops prevent executor starvation. [See docs/CHANGELOG.md](docs/CHANGELOG.md) for details.

**🚀 NEW (v0.8.50)**: **YAML-driven stage orchestration** with 6 stage types, **barrier synchronization** for coordinated multi-host testing, and **comprehensive timeout configuration** (global/stage/barrier hierarchy).

**🚀 NEW (v0.8.23)**: Pre-flight distributed configuration validation prevents common misconfigurations (base_uri with isolated mode) before execution.

**🚀 NEW (v0.8.22)**: Multi-endpoint load balancing with per-agent static endpoint mapping for shared storage systems with multiple endpoints (NFS, S3, or object storage).

A comprehensive storage performance testing tool supporting multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) for multi-protocol support.

## 🚀 What Makes sai3-bench Unique?

1. **Universal Storage Testing**: Unified interface across 5 storage protocols (file://, direct://, s3://, az://, gs://)
2. **Directory Tree Workloads**: Configurable hierarchical structures for realistic shared filesystem testing
3. **Filesystem Operations**: Full support for nested paths and directory operations across all backends
4. **Pre-flight Validation**: Detect configuration errors before execution (filesystem access, distributed config mismatches)
5. **Workload Replay**: Capture production traffic and replay with microsecond fidelity (1→1, 1→N, N→1 remapping)
6. **Op-Log Management**: Sort, validate, and merge operation logs for analysis and replay
7. **Robust Distributed Execution**: Bidirectional streaming with sub-millisecond agent synchronization (v0.8.5+)
8. **Production-Grade Metrics**: HDR histograms with size-bucketed analysis and aggregate summaries
9. **Realistic Data Patterns**: Lognormal size distributions, configurable deduplication and compression
10. **Machine-Readable Output**: TSV export with per-bucket and aggregate rows for automated analysis
11. **Performance Logging**: Time-series perf-log with 31 columns including mean/p50/p90/p99 latencies, CPU metrics, and warmup filtering (v0.8.17+)
12. **Results Analysis Tool**: Excel spreadsheet generation consolidating multiple test results (sai3-analyze, v0.8.17+)
13. **Automatic Credential Distribution**: Controller forwards cloud credentials to agents over gRPC so each host needs no manual secret setup — with an allow-list, local-wins policy, and audit logging (v0.8.92+)

## 🎯 Supported Storage Backends

All operations work identically across protocols - just change the URI scheme:

- **File System** (`file://`) - Local filesystem with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O bypassing page cache (optimized chunked reads)
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage
- **Google Cloud Storage** (`gs://` or `gcs://`) - Google Cloud Storage with native GCS API, including RAPID (Hyperdisk ML) storage (v0.8.86+)

See [Cloud Storage Setup](docs/CLOUD_STORAGE_SETUP.md) for authentication guides.

## 🚀 Quick Start

### One-Time Setup

**1. Install Rust** (if not already installed):
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env
```

**2. Clone and Build sai3-bench**:
```bash
git clone https://github.com/russfellows/sai3-bench.git
cd sai3-bench
cargo build --release
```

The build creates 4 executables in `target/release/`:
- `sai3-bench` - Single-node testing CLI
- `sai3bench-agent` - Distributed agent (runs on each test host)
- `sai3bench-ctl` - Distributed controller (coordinates agents)
- `sai3-analyze` - Results analysis tool (Excel export)

**3. Install Executables** (optional):

Choose one of the following installation methods:

**Option A: User-local install** (recommended, no sudo required):
```bash
cargo install --path .
```
Installs to `~/.cargo/bin/` (already in your PATH from Rust installation).

**Option B: System-wide install**:
```bash
sudo install -m 755 target/release/{sai3-bench,sai3bench-ctl,sai3bench-agent,sai3-analyze} /usr/local/bin/
```
Installs to `/usr/local/bin/` for all users.

**Option C: Run from build directory**:
```bash
# No installation needed - use full path:
./target/release/sai3-bench --version
./target/release/sai3bench-ctl --version
```

### Testing Modes

sai3-bench supports two testing modes: **Single-Node** and **Distributed**.

```
┌───────────────────────────────────────────────────────────────────┐
│                        SINGLE-NODE MODE                           │
├───────────────────────────────────────────────────────────────────┤
│                                                                   │
│  ┌──────────────┐                                                 │
│  │              │        I/O Operations                           │
│  │  sai3-bench  │  ─────────────────────►  Storage System         │
│  │              │                          (S3/NFS/Azure/etc)     │
│  └──────────────┘                                                 │
│                                                                   │
│  • Simple: One command to run workloads                           │
│  • Use for: Single host testing, development, quick validation    │
│  • Command: ./sai3-bench run --config workload.yaml               │
│                                                                   │
└───────────────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────────┐
│                       DISTRIBUTED MODE                           │
├──────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────────┐                                            │
│  │                  │  gRPC: Config, Start/Stop, Stats           │
│  │  sai3bench-ctl   │────────┬──────────┬──────────┐             │
│  │  (Controller)    │        │          │          │             │
│  └──────────────────┘        ▼          ▼          ▼             │
│                                                                  │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐        │
│  │sai3bench-    │    │sai3bench-    │    │sai3bench-    │        │
│  │agent (Host 1)│    │agent (Host 2)│    │agent (Host N)│        │
│  └──────┬───────┘    └──────┬───────┘    └──────┬───────┘        │
│         │ I/O               │ I/O               │ I/O            │
│         ▼                    ▼                    ▼              │
│    ┌────────────────────────────────────────────────────┐        │
│    │         Storage System (NFS/S3/Azure/etc)          │        │
│    │  • Multiple endpoints for load balancing           │        │
│    │  • Unified namespace across all endpoints          │        │
│    └────────────────────────────────────────────────────┘        │
│                                                                  │
│  • Scalable: Generate load from multiple hosts                   │
│  • Use for: Large-scale testing, multi-endpoint storage          │
│  • Command: ./sai3bench-ctl run --config distributed.yaml        │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
```

### Running Your First Workload

**Single-Node Mode** - Test local filesystem:
```bash
# Create a simple config file
cat > my-test.yaml <<EOF
target: "file:///shared/benchmark/"
duration: "60s"
concurrency: 16

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  agents:
    - address: "host1:7167"
      id: "agent-1"
    - address: "host2:7167"
      id: "agent-2"

prepare:
  ensure_objects:
    - base_uri: "data/"
      count: 1000
      min_size: 1048576
      max_size: 1048576
      fill: random

workload:
  - op: get
    path: "data/*"
    weight: 70
  - op: put
    path: "data/"
    size_spec: 1048576
    weight: 30
EOF

# Validate config (dry-run)
./target/release/sai3-bench run --config my-test.yaml --dry-run

# Run the workload
./target/release/sai3-bench run --config my-test.yaml
```

**Distributed Mode** - Multi-host testing:
```bash
# On each test host (Host 1, Host 2, etc.), start an agent:
./target/release/sai3bench-agent --listen 0.0.0.0:7167

# On the controller host, create a distributed config:
cat > distributed-test.yaml <<EOF
target: "file:///shared/benchmark/"
duration: "120s"
concurrency: 32

perf_log:
  enabled: true
  interval: 1s

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  agents:
    - address: "host1:7167"
      id: "agent-1"
    - address: "host2:7167"
      id: "agent-2"

prepare:
  ensure_objects:
    - base_uri: "data/"
      count: 5000
      min_size: 524288
      max_size: 10485760
      fill: random

workload:
  - op: get
    path: "data/*"
    weight: 60
  - op: put
    path: "data/"
    size_spec: 2097152
    weight: 30
  - op: list
    path: "data/"
    weight: 10
EOF

# Run distributed workload (controller coordinates agents)
./target/release/sai3bench-ctl run --config distributed-test.yaml
```

**Common Operations**:
```bash
# Test storage connectivity
./target/release/sai3-bench util health --uri "s3://my-bucket/"

# Distributed autotune with YAML matrix (--dry-run to preview sweep plan)
./target/release/sai3bench-ctl autotune --config examples/distributed-autotune-minimal.yaml --dry-run
./target/release/sai3bench-ctl autotune --config examples/distributed-autotune-minimal.yaml

# Capture workload for replay
./target/release/sai3-bench --op-log trace.tsv.zst run --config my-test.yaml

# Replay captured workload
./target/release/sai3-bench replay --op-log trace.tsv.zst --target "s3://test-bucket/"

# Analyze results (generate Excel report)
./target/release/sai3-analyze --pattern "sai3-*" --output results.xlsx
```

Minimal autotune YAML example: `examples/distributed-autotune-minimal.yaml`

See [Usage Guide](docs/USAGE.md) for detailed examples and [Distributed Testing Guide](docs/DISTRIBUTED_TESTING_GUIDE.md) for multi-host patterns.

## 🌳 Directory Tree Workloads

Test realistic shared filesystem scenarios with configurable directory hierarchies:

```yaml
prepare:
  directory_structure:
    width: 3              # Subdirectories per level
    depth: 2              # Tree depth (2 = 3 + 9 directories)
    files_per_dir: 10     # Files per directory
    distribution: bottom  # "bottom" (leaf only) or "all" (every level)
    dir_mask: "d%d_w%d.dir"  # Directory naming pattern
  
  ensure_objects:
    - base_uri: "file:///tmp/tree-test/"
      count: 0            # Files created by directory_structure
      size_spec: 
        type: uniform
        min: 4096         # 4 KiB
        max: 16384        # 16 KiB
      fill: random        # Cryptographic random data (recommended)
      dedup_factor: 1     # 1 = unique, 2+ = duplicate blocks
      compress_factor: 1  # 1 = incompressible, 2+ = compressible
```

**Fill Pattern Options:**
- **`random`** (default, recommended): Cryptographic random data - realistic, incompressible
- ***⚠️ `zero`: DO NOT USE for benchmarks - triggers dedup/compression, produces unrealistic results***

**Key Features:**
- **Enhanced `--dry-run`**: Shows directory/file counts and total data size before execution
- **Multi-level distributions**: Place files only in leaves (`bottom`) or at all levels (`all`)
- **Cloud storage compatible**: Works seamlessly with S3, Azure Blob, GCS (implicit directories)
- **Distributed coordination**: TreeManifest ensures collision-free file numbering across agents
- **Realistic data**: Random fill default provides compression-resistant patterns

**Example:**
```bash
# Validate configuration with enhanced dry-run
./sai3-bench run --config tree-test.yaml --dry-run
# Output: Total Directories: 12, Total Files: 60, Total Data: 600 KiB

# Run workload on Azure Blob Storage
./sai3-bench run --config tree-test.yaml
```

See [Directory Tree Test Configs](tests/configs/directory-tree/README.md) for examples.

## 📦 Architecture & Binaries

- **`sai3-bench`** - Single-node CLI with subcommands: `run`, `replay`, `util`
- **`sai3bench-agent`** - Distributed gRPC agent for multi-node load generation  
- **`sai3bench-ctl`** - Controller for coordinating distributed agents
- **`sai3-analyze`** - Results consolidation tool (Excel spreadsheet generation) ✨ NEW in v0.8.17

## 📖 Documentation

- **[Usage Guide](docs/USAGE.md)** - Getting started and common workflows
- **[Config Syntax](docs/CONFIG_SYNTAX.md)** - Complete YAML configuration reference
- **[Config Examples](tests/configs/README.md)** - Annotated test configurations
- **[Distributed Testing Guide](docs/DISTRIBUTED_TESTING_GUIDE.md)** - Multi-host load generation
- **[Cloud Storage Setup](docs/CLOUD_STORAGE_SETUP.md)** - S3, Azure, and GCS authentication
- **[s3dlio Performance Tuning](docs/S3DLIO_PERFORMANCE_TUNING.md)** - Range downloads & multipart upload optimization ✨ NEW
- **[Data Generation Guide](docs/DATA_GENERATION.md)** - Deduplication and compression testing
- **[Results Analysis Tool](docs/ANALYZE_TOOL.md)** - Consolidating multiple results into Excel ✨ NEW
- **[Changelog](docs/CHANGELOG.md)** - Complete version history



## 🔬 Workload Replay

Capture production I/O traces with s3dlio and replay them with microsecond-accurate timing.

### Capturing Workloads with s3dlio

The [s3dlio library](https://github.com/russfellows/s3dlio) provides op-log capture for applications using storage APIs. This is the recommended way to capture real production workloads:

**Python Application:**
```python
import s3dlio

# Initialize op-log capture at application startup
s3dlio.init_op_log("/tmp/production_trace.tsv.zst")

# Your application's normal storage operations - all are logged
data = s3dlio.get("s3://bucket/model.bin")
s3dlio.put("s3://bucket/results/output.json", result_bytes)
files = s3dlio.list("s3://bucket/data/")

# Finalize when done (flushes and closes the log)
s3dlio.finalize_op_log()
```

**Rust Application:**
```rust
use s3dlio::{init_op_logger, store_for_uri, LoggedObjectStore, global_logger};

// Initialize op-log capture
init_op_logger("production_trace.tsv.zst")?;

// Wrap your ObjectStore with logging
let store = store_for_uri("s3://bucket/")?;
let logged_store = LoggedObjectStore::new(Arc::from(store), global_logger().unwrap());

// All operations now captured to op-log
let data = logged_store.get("s3://bucket/file.bin").await?;
```

### Replaying Captured Traces

Once you have an op-log from production, replay it with sai3-bench:

```bash
# Replay against test environment with original timing
sai3-bench replay --op-log /tmp/production_trace.tsv.zst --target "az://test-storage/"

# Replay at 5x speed for accelerated load testing
sai3-bench replay --op-log /tmp/production_trace.tsv.zst --speed 5.0
```

> **Note**: sai3-bench can also capture op-logs during benchmark runs with `--op-log`, but this is primarily for analyzing benchmark I/O patterns rather than capturing production workloads.

### Backpressure Handling (v0.8.9+)
When target storage can't sustain the recorded I/O rate:

```yaml
# replay_config.yaml - controls replay behavior
lag_threshold: 5s        # Switch to best-effort when lag exceeds this
recovery_threshold: 1s   # Switch back when lag drops below this
max_flaps_per_minute: 3  # Exit gracefully if oscillating too much
max_concurrent: 1000     # Maximum in-flight operations
drain_timeout: 10s       # Timeout for draining on exit
```

```bash
sai3-bench replay --op-log trace.tsv.zst --config replay_config.yaml --target "s3://bucket/"
```

### URI Remapping
Transform source URIs during replay for migration testing:

```yaml
# remap.yaml - 1:1 bucket rename (simple migration)
rules:
  - match:
      bucket: "prod-bucket"
    map_to:
      bucket: "staging-bucket"
      prefix: "migrated/"
```

```bash
# Apply remapping during replay
sai3-bench replay --op-log trace.tsv.zst --remap remap.yaml --target "s3://staging-bucket/"
```

**Advanced Remapping Strategies:**
- **1→1**: Simple bucket/prefix rename (migration validation)
- **1→N**: Fanout to replicas (`round_robin` or `sticky_key` distribution)
- **N→1**: Consolidate multiple sources to single target
- **N→M**: Regex-based transformations (e.g., `s3://` → `gs://` for cross-cloud)

See [remap_examples.yaml](tests/configs/remap_examples.yaml) for complete examples.

**Use Cases**: Pre-migration validation, performance regression testing, capacity planning, cross-cloud comparison.

## 💾 Storage Efficiency Testing

Test deduplication and compression with controlled data patterns.

**Important**: `dedup_factor` and `compress_factor` are **optional** - if omitted, both default to `1` (no dedup, no compression).

### Example 1: Default Behavior (No Dedup/Compression)
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/unique-media/"
      count: 500
      size_spec: 10485760  # 10 MB fixed size
      fill: random
      # dedup_factor: 1 (default - omitted, all blocks unique)
      # compress_factor: 1 (default - omitted, incompressible)
```

### Example 2: Testing Storage Deduplication (3:1 Ratio)
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/vm-snapshots/"
      count: 100
      size_spec: 52428800  # 50 MB
      fill: random
      dedup_factor: 3      # 3:1 dedup (1/3 blocks unique, 2/3 duplicates)
      compress_factor: 1   # No compression (incompressible data)
```
**Result**: 100 files × 50 MB = 5 GB logical, but only ~1.67 GB unique data (3:1 dedup).

### Example 3: Combined Dedup + Compression (5:1 and 2:1)
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/log-archives/"
      count: 200
      size_spec:
        type: uniform
        min: 5242880       # 5 MB
        max: 52428800      # 50 MB
      fill: random
      dedup_factor: 5      # 5:1 dedup (1/5 blocks unique)
      compress_factor: 2   # 2:1 compression (50% zeros)
```
**Result**: Avg 28.5 MB × 200 files = 5.7 GB logical → ~1.14 GB unique (5:1) → ~570 MB after compression (2:1).

### Dedup/Compression Ratios Explained

| Setting | Value | Meaning | Storage Impact |
|---------|-------|---------|----------------|
| `dedup_factor: 1` | 1:1 (default) | All blocks unique | No dedup savings |
| `dedup_factor: 3` | 3:1 | 1/3 unique, 2/3 duplicate | 67% space savings |
| `dedup_factor: 5` | 5:1 | 1/5 unique, 4/5 duplicate | 80% space savings |
| `compress_factor: 1` | 1:1 (default) | Incompressible | No compression savings |
| `compress_factor: 2` | 2:1 | 50% zeros | 50% compression savings |
| `compress_factor: 4` | 4:1 | 75% zeros | 75% compression savings |

**Fill Pattern Guidelines:**
| Pattern | Speed | Use Case |
|---------|-------|----------|
| `random` | Standard | Production benchmarks, realistic workloads (RECOMMENDED) |
| ***⚠️ `zero`*** | Fastest | ***DO NOT USE - triggers dedup/compression, unrealistic results*** |

**Use Cases**: Validate vendor dedup/compression claims, predict migration space requirements, model hot vs. cold data.

See [Data Generation Guide](docs/DATA_GENERATION.md) for detailed patterns.

## 📐 Realistic Size Distributions

Model real-world object storage patterns with statistical distributions:

```yaml
workload:
  - op: put
    path: "data/"
    weight: 100
    size_spec:
      type: lognormal    # Many small files, few large files
      mean: 1048576      # Mean: 1 MB
      std_dev: 524288    # Std dev: 512 KB
      min: 1024          # Floor: 1 KB
      max: 10485760      # Ceiling: 10 MB
    fill: random         # Cryptographic random (recommended)
```

**Why lognormal?** Research shows object storage naturally follows lognormal distributions (many small configs/thumbnails, few large videos/backups).

**Distribution Types:**
- `lognormal`: Realistic - many small, few large (requires `mean`, `std_dev`)
- `uniform`: Even spread between `min` and `max`
- Fixed size: Just use `size_spec: 1048576` (integer value)

See [Config Syntax](docs/CONFIG_SYNTAX.md) for complete options.

## 🌐 Distributed Testing

Generate large-scale coordinated load across multiple nodes with automated deployment:

### Automated SSH Deployment
```bash
# One-time setup: Configure passwordless SSH
sai3bench-ctl ssh-setup --hosts ubuntu@vm1,ubuntu@vm2,ubuntu@vm3

# Run distributed test: Agents deploy automatically
sai3bench-ctl run --config distributed-workload.yaml
```

### Configuration-Driven Agents
Define all agents in YAML with per-agent customization:
```yaml
distributed:
  agents:
    - address: "vm1.example.com"
      id: "us-west-agent"
      target_override: "s3://us-west-bucket/"
      concurrency_override: 128
      env: { AWS_PROFILE: "benchmark" }
    
    - address: "vm2.example.com"
      id: "us-east-agent"
      target_override: "s3://us-east-bucket/"
  
  ssh:
    enabled: true
    key_path: "~/.ssh/sai3bench_id_rsa"
  
  deployment:
    container_runtime: "docker"  # or "podman"
    image: "sai3bench:latest"
    network_mode: "host"
```

### Flexible Scaling Strategies

**Scale-Out** (Multiple VMs): Maximum network bandwidth, fault tolerance
```yaml
# 8 VMs, 1 container each = 8× network interfaces
agents:
  - { address: "vm1:7167", id: "agent-1" }
  - { address: "vm2:7167", id: "agent-2" }
  # ... vm3-vm8
```

**Scale-Up** (Single VM): Cost optimization, lower latency
```yaml
# 1 large VM, 8 containers on different ports
agents:
  - { address: "big-vm:7167", id: "c1", listen_port: 7167 }
  - { address: "big-vm:7168", id: "c2", listen_port: 7168 }
  # ... c3-c8
```

### Cloud Automation
Pre-built scripts for rapid deployment:
- **GCP**: `scripts/gcp_distributed_test.sh` - Complete VM lifecycle automation
- **AWS/Azure**: `scripts/cloud_test_template.sh` - Customizable templates
- **Local**: `scripts/local_docker_test.sh` - Test distributed mode without cloud

### Key Features
- **Automated lifecycle**: SSH, container deployment, health checks, cleanup
- **Per-agent overrides**: Target storage, concurrency, environment variables, volumes
- **Graceful shutdown**: Ctrl+C handling with automatic container cleanup
- **Result aggregation**: Proper HDR histogram merging for accurate percentiles
- **Container flexibility**: Docker or Podman via YAML (no recompilation)

**Learn More**:
- [Distributed Testing Guide](docs/DISTRIBUTED_TESTING_GUIDE.md) - Complete workflows and patterns
- [SSH Setup Guide](docs/SSH_SETUP_GUIDE.md) - One-command SSH automation
- [Scale-Out vs Scale-Up](docs/SCALE_OUT_VS_SCALE_UP.md) - Performance and cost comparison
- [Cloud Scripts Guide](scripts/README.md) - GCP automation and templates

## ⚙️ Key Features

### I/O Rate Control (v0.7.1)
Throttle operation start rate with realistic arrival patterns:
```yaml
io_rate:
  iops: 1000              # Target operations per second
  distribution: exponential  # Poisson arrivals (realistic)
                             # or "uniform" (fixed intervals)
                             # or "deterministic" (precise timing)
```
- **Inspired by rdf-bench**: Similar to `iorate=` parameter with enhanced distributions
- **Three distribution types**: Exponential (Poisson), Uniform (fixed), Deterministic (precise)
- **Drift compensation**: tokio::time::Interval for uniform distribution accuracy
- **Zero overhead when disabled**: Optional wrapper for maximum performance
- **Per-worker division**: Target IOPS automatically split across concurrent workers

See [I/O Rate Control Guide](docs/IO_RATE_CONTROL_GUIDE.md) for detailed usage and examples.

### TSV Export with Aggregate Rows
Machine-readable output with per-bucket and aggregate summary rows:
- **Per-bucket rows**: Statistics for each size bucket (zero, 1B-8KiB, 8KiB-64KiB, etc.)
- **Aggregate rows**: "ALL" rows combining all size buckets per operation type (GET/PUT/META)
- **Accurate latency merging**: HDR histogram merging for statistically correct percentiles
- **Distributed support**: Per-agent TSVs and consolidated TSV with overall aggregates

### Per-Operation Concurrency
Fine-grained worker pool control:
```yaml
concurrency: 32  # Global default
workload:
  - op: get
    path: "data/*"
    weight: 70
    concurrency: 64  # More GET workers
  - op: put
    path: "uploads/"
    weight: 30
    concurrency: 8   # Fewer PUT workers
```

### Config Validation
Verify YAML before execution:
```bash
sai3-bench run --config my-workload.yaml --dry-run
```

See [Config Syntax](docs/CONFIG_SYNTAX.md) for complete reference.

## 🛠️ Development

### Requirements
- Rust stable toolchain (2024 edition)
- `protoc` compiler for gRPC (distributed mode)
- Storage credentials for cloud backends

### Building & Testing
```bash
# Build
cargo build --release

# Run tests
cargo test

# Streaming replay tests (must run sequentially)
cargo test --test streaming_replay_tests -- --test-threads=1
```

See [Cloud Storage Setup](docs/CLOUD_STORAGE_SETUP.md) for authentication details.

## 🔗 Related Projects

- **[s3dlio](https://github.com/russfellows/s3dlio)** - The underlying multi-protocol storage library powering sai3-bench
- **[polarWarp](https://github.com/russfellows/polarWarp)** - Op-log analysis tool for parsing and visualizing s3dlio operation logs

## 📄 License

GPL-3.0 License - See [LICENSE](LICENSE) for details.
