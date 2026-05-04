# sai3-bench: Multi-Protocol I/O Benchmarking Suite

[![Version](https://img.shields.io/badge/version-0.8.97-blue.svg)](https://github.com/russfellows/sai3-bench/releases)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/russfellows/sai3-bench)
[![Tests](https://img.shields.io/badge/tests-731%20passing-success.svg)](https://github.com/russfellows/sai3-bench)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-green.svg)](https://www.rust-lang.org/)

**Latest: v0.8.97** — Key-prefix sharding for hash-partitioned storage (VAST/Weka), dynamic PUT pool for live-updated GET workloads. See [docs/CHANGELOG.md](docs/CHANGELOG.md) for the full release history.

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

Example configs are in [`tests/configs/`](tests/configs/). See [docs/USAGE.md](docs/USAGE.md) for full annotated examples.

**Single-Node Mode**:
```bash
sai3-bench run --config tests/configs/sai3_put_100k-1k.yaml --dry-run
sai3-bench run --config tests/configs/sai3_put_100k-1k.yaml
```

**Distributed Mode**:
```bash
# Start agents on each test host
./target/release/sai3bench-agent --listen 0.0.0.0:7167

# Run from the controller host
./target/release/sai3bench-ctl run --config tests/configs/distributed-test.yaml
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

Test realistic shared filesystem workloads with configurable directory depth, width, and file distribution. Works across all backends (file, S3, Azure, GCS). Supports `distribution: bottom` (leaf nodes only) or `all` (every level). `--dry-run` shows total counts and data size before execution.

See [tests/configs/directory-tree/README.md](tests/configs/directory-tree/README.md) for annotated example configs, and [docs/USAGE.md](docs/USAGE.md) for a full configuration walkthrough.

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

Capture I/O traces from production applications via s3dlio op-logs, then replay them against any target with original timing, accelerated speed, or remapped URIs. Supports backpressure handling and 1→1, 1→N, N→1, and N→M URI remapping strategies.

See [docs/USAGE.md](docs/USAGE.md) for capture and replay examples including Python/Rust instrumentation, backpressure configuration, and URI remapping patterns.

## 💾 Storage Efficiency Testing

Validate vendor dedup/compression claims with controlled data patterns. `dedup_factor` and `compress_factor` both default to `1` (unique, incompressible) when omitted. Always use `fill: random` — `zero` triggers storage dedup/compression and produces unrealistic results.

See [docs/USAGE.md](docs/USAGE.md) for configuration examples and ratio reference table.

## 📐 Realistic Size Distributions

Model real-world access patterns using `lognormal` (many small, few large), `uniform`, or fixed-size specs for the `size_spec` field. See [docs/CONFIG_SYNTAX.md](docs/CONFIG_SYNTAX.md) for the full reference.

## 🌐 Distributed Testing

Generate large-scale coordinated load across multiple nodes:

- **SSH deployment**: `sai3bench-ctl ssh-setup` handles passwordless SSH and automatic agent deployment
- **Per-agent overrides**: target storage, concurrency, environment variables, and volumes per agent
- **Cloud automation**: pre-built scripts for GCP, AWS, and Azure in `scripts/`
- **Flexible scaling**: scale-out (multiple VMs) or scale-up (multiple containers on one large VM)

See [docs/DISTRIBUTED_TESTING_GUIDE.md](docs/DISTRIBUTED_TESTING_GUIDE.md) for complete workflows and [docs/USAGE.md](docs/USAGE.md) for config examples.

## ⚙️ Key Features

### I/O Rate Control (v0.7.1)
Throttle operation start rate with Poisson (`exponential`), fixed-interval (`uniform`), or precise (`deterministic`) distributions. Per-worker IOPS division, drift compensation, zero overhead when disabled. See [docs/IO_RATE_CONTROL_GUIDE.md](docs/IO_RATE_CONTROL_GUIDE.md).

### TSV Export with Aggregate Rows
Machine-readable output with per-bucket and aggregate summary rows:
- **Per-bucket rows**: Statistics for each size bucket (zero, 1B–8KiB, 8KiB–64KiB, etc.)
- **Aggregate rows**: "ALL" rows combining all size buckets per operation type (GET/PUT/META)
- **Accurate latency merging**: HDR histogram merging for statistically correct percentiles
- **Distributed support**: Per-agent TSVs and consolidated TSV with overall aggregates

### Per-Operation Concurrency
Set a global `concurrency:` with per-op overrides in the workload block. See [docs/CONFIG_SYNTAX.md](docs/CONFIG_SYNTAX.md) and [docs/USAGE.md](docs/USAGE.md) for examples.

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
