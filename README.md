# sai3-bench: Multi-Protocol I/O Benchmarking Suite

[![Version](https://img.shields.io/badge/version-0.8.22-blue.svg)](https://github.com/russfellows/sai3-bench/releases)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/russfellows/sai3-bench)
[![Tests](https://img.shields.io/badge/tests-277%20passing-success.svg)](https://github.com/russfellows/sai3-bench)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-green.svg)](https://www.rust-lang.org/)

**🚀 NEW (v0.8.22)**: Multi-endpoint load balancing with per-agent static endpoint mapping for shared storage systems with multiple endpoints (NFS, S3, or object storage).

**🚀 NEW (v0.8.21)**: Zero-copy API migration with s3dlio v0.9.36 and enhanced data generation via `fill_controlled_data()` (86-163 GB/s, 20-50x faster).

A comprehensive storage performance testing tool supporting multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) for multi-protocol support.

## 🚀 What Makes sai3-bench Unique?

1. **Universal Storage Testing**: Unified interface across 5 storage protocols (file://, direct://, s3://, az://, gs://)
2. **Directory Tree Workloads**: Configurable hierarchical structures for realistic shared filesystem testing
3. **Filesystem Operations**: Full support for nested paths and directory operations across all backends
4. **Workload Replay**: Capture production traffic and replay with microsecond fidelity (1→1, 1→N, N→1 remapping)
5. **Op-Log Management**: Sort, validate, and merge operation logs for analysis and replay
6. **Robust Distributed Execution**: Bidirectional streaming with sub-millisecond agent synchronization (v0.8.5+)
7. **Production-Grade Metrics**: HDR histograms with size-bucketed analysis and aggregate summaries
8. **Realistic Data Patterns**: Lognormal size distributions, configurable deduplication and compression
9. **Machine-Readable Output**: TSV export with per-bucket and aggregate rows for automated analysis
10. **Performance Logging**: Time-series perf-log with 31 columns including mean/p50/p90/p99 latencies, CPU metrics, and warmup filtering (v0.8.17+)
11. **Results Analysis Tool**: Excel spreadsheet generation consolidating multiple test results (sai3-analyze, v0.8.17+)

## 🎯 Supported Storage Backends

All operations work identically across protocols - just change the URI scheme:

- **File System** (`file://`) - Local filesystem with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O bypassing page cache (optimized chunked reads)
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage
- **Google Cloud Storage** (`gs://` or `gcs://`) - Google Cloud Storage with native GCS API

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
target: "file:///tmp/benchmark/"
duration: "30s"
concurrency: 8

prepare:
  ensure_objects:
    - base_uri: "file:///tmp/benchmark/data/"
      count: 100
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
./target/release/sai3bench-agent --listen 0.0.0.0:7761

# On the controller host, create a distributed config:
cat > distributed-test.yaml <<EOF
target: "file:///shared/benchmark/"
duration: "60s"
concurrency: 16

distributed:
  agents:
    - address: "host1:7761"
      id: "agent-1"
    - address: "host2:7761"
      id: "agent-2"

prepare:
  ensure_objects:
    - base_uri: "file:///shared/benchmark/data/"
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

# Run distributed workload (controller coordinates agents)
./target/release/sai3bench-ctl run --config distributed-test.yaml
```

**Common Operations**:
```bash
# Test storage connectivity
./target/release/sai3-bench util health --uri "s3://my-bucket/"

# Capture workload for replay
./target/release/sai3-bench --op-log trace.tsv.zst run --config my-test.yaml

# Replay captured workload
./target/release/sai3-bench replay --op-log trace.tsv.zst --target "s3://test-bucket/"

# Analyze results (generate Excel report)
./target/release/sai3-analyze --pattern "sai3-*" --output results.xlsx
```

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
      fill: random        # "random" (recommended) or "prand" (faster)
      dedup_factor: 1     # 1 = unique, 2+ = duplicate blocks
      compress_factor: 1  # 1 = incompressible, 2+ = compressible
```

**Fill Pattern Options:**
- **`random`** (default, recommended): Cryptographic random data - realistic, incompressible
- **`prand`**: Pseudo-random using XorShift - faster generation, still unique per file
- **`zero`**: All zeros - AVOID for benchmarks (triggers dedup/compression, unrealistic)

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

Test deduplication and compression with controlled data patterns:

```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/templates/"
      count: 100
      size_spec: 10485760  # 10 MB fixed size
      fill: random         # Recommended: realistic, incompressible
      dedup_factor: 20     # 95% duplicate blocks (1/20 unique)
      compress_factor: 2   # 2:1 compressible
    
    - base_uri: "s3://bucket/media/"
      count: 500
      size_spec:
        type: uniform
        min: 5242880       # 5 MB
        max: 52428800      # 50 MB
      fill: prand          # Faster pseudo-random, still unique per file
      dedup_factor: 1      # All unique blocks
      compress_factor: 1   # Incompressible
```

**Fill Pattern Guidelines:**
| Pattern | Speed | Use Case |
|---------|-------|----------|
| `random` | Slower | Production benchmarks, realistic workloads |
| `prand` | Faster | Large-scale testing, still unique data per file |
| `zero` | Fastest | ⚠️ AVOID - triggers dedup/compression, unrealistic results |

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
    fill: random         # "random" or "prand"
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
  - { address: "vm1:7761", id: "agent-1" }
  - { address: "vm2:7761", id: "agent-2" }
  # ... vm3-vm8
```

**Scale-Up** (Single VM): Cost optimization, lower latency
```yaml
# 1 large VM, 8 containers on different ports
agents:
  - { address: "big-vm:7761", id: "c1", listen_port: 7761 }
  - { address: "big-vm:7762", id: "c2", listen_port: 7762 }
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
