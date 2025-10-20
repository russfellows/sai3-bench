# sai3-bench: Multi-Protocol I/O Benchmarking Suite

[![Version](https://img.shields.io/badge/version-0.6.10-blue.svg)](https://github.com/russfellows/sai3-bench/releases)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/russfellows/sai3-bench)
[![Tests](https://img.shields.io/badge/tests-35%20passing-success.svg)](https://github.com/russfellows/sai3-bench)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-green.svg)](https://www.rust-lang.org/)

A comprehensive storage performance testing tool supporting multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) for multi-protocol support.

> **Latest (v0.6.9)**: 173x faster direct:// I/O through chunked reads + clean 3-binary distribution. See [CHANGELOG](docs/CHANGELOG.md) for details.

## 🚀 What Makes sai3-bench Unique?

1. **Universal Storage Testing**: Unified interface across 5 storage protocols (file://, direct://, s3://, az://, gs://)
2. **Workload Replay**: Capture production traffic and replay with microsecond fidelity (1→1, 1→N, N→1 remapping)
3. **Distributed Architecture**: gRPC-based agent/controller for coordinated multi-node load generation
4. **Production-Grade Metrics**: HDR histograms with size-bucketed analysis and aggregate summaries
5. **Realistic Data Patterns**: Lognormal size distributions, configurable deduplication and compression
6. **Machine-Readable Output**: TSV export with per-bucket and aggregate rows for automated analysis

## 🎯 Supported Storage Backends

All operations work identically across protocols - just change the URI scheme:

- **File System** (`file://`) - Local filesystem with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O bypassing page cache (optimized chunked reads)
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage (MinIO, Ceph, etc.)
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage
- **Google Cloud Storage** (`gs://` or `gcs://`) - Google Cloud Storage with native GCS API

See [Cloud Storage Setup](docs/CLOUD_STORAGE_SETUP.md) for authentication guides.

## 📦 Architecture & Binaries

- **`sai3-bench`** - Single-node CLI with subcommands: `run`, `replay`, `util`
- **`sai3bench-agent`** - Distributed gRPC agent for multi-node load generation  
- **`sai3bench-ctl`** - Controller for coordinating distributed agents

## 📖 Documentation

- **[Usage Guide](docs/USAGE.md)** - Getting started and common workflows
- **[Config Syntax](docs/CONFIG_SYNTAX.md)** - Complete YAML configuration reference
- **[Config Examples](tests/configs/README.md)** - Annotated test configurations
- **[Distributed Testing Guide](docs/DISTRIBUTED_TESTING_GUIDE.md)** - Multi-host load generation
- **[Cloud Storage Setup](docs/CLOUD_STORAGE_SETUP.md)** - S3, Azure, and GCS authentication
- **[Data Generation Guide](docs/DATA_GENERATION.md)** - Deduplication and compression testing
- **[Changelog](docs/CHANGELOG.md)** - Complete version history

## 🚀 Quick Start

```bash
# Build
cargo build --release

# Test local filesystem
./target/release/sai3-bench util health --uri "file:///tmp/test/"

# Validate config before running
./target/release/sai3-bench run --config my-workload.yaml --dry-run

# Run workload
./target/release/sai3-bench run --config my-workload.yaml

# Capture and replay workload
./target/release/sai3-bench --op-log /tmp/workload.tsv.zst run --config my-workload.yaml
./target/release/sai3-bench replay --op-log /tmp/workload.tsv.zst --target "az://test/"

# Test cloud storage (requires authentication)
export AZURE_STORAGE_ACCOUNT="your-storage-account"
export AZURE_STORAGE_ACCOUNT_KEY="your-account-key"
./target/release/sai3-bench util health --uri "az://your-storage-account/container/"
```

See [Usage Guide](docs/USAGE.md) for detailed examples.

## 🔬 Workload Replay

Record production workloads and replay with microsecond fidelity:

```bash
# Capture production workload
sai3-bench --op-log /tmp/production.tsv.zst run --config production.yaml

# Replay against test environment
sai3-bench replay --op-log /tmp/production.tsv.zst --target "az://test-storage/"

# Replay at higher speed for load testing
sai3-bench replay --op-log /tmp/prod.tsv.zst --speed 5.0

# 1→N fanout remapping
sai3-bench replay --op-log /tmp/workload.tsv.zst --remap fanout.yaml
```

**Use Cases**: Pre-migration validation, performance regression testing, capacity planning, cross-cloud comparison.

See [Usage Guide](docs/USAGE.md) for remapping strategies and examples.

## 💾 Storage Efficiency Testing

Test deduplication and compression with controlled data patterns:

```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/templates/"
      count: 100
      size_distribution: {type: fixed, size: 10485760}  # 10 MB
      fill: random      # RECOMMENDED for realistic testing
      dedup_factor: 20  # 95% duplicate blocks
      compress_factor: 2
    
    - base_uri: "s3://bucket/media/"
      count: 500
      size_distribution: {type: uniform, min: 5242880, max: 52428800}
      fill: random
      dedup_factor: 1   # No deduplication
      compress_factor: 1  # Uncompressible
```

**Use Cases**: Validate vendor dedup/compression claims, predict migration space requirements, model hot vs. cold data.

See [Data Generation Guide](docs/DATA_GENERATION.md) for detailed patterns.

## 📐 Realistic Size Distributions

Model real-world object storage patterns with statistical distributions:

```yaml
workload:
  - op: put
    path: "data/"
    weight: 100
    size_distribution:
      type: lognormal  # Many small files, few large files
      mean: 1048576    # Mean: 1 MB
      std_dev: 524288  # Std dev: 512 KB
      min: 1024        # Floor: 1 KB
      max: 10485760    # Ceiling: 10 MB
    fill: random
```

**Why lognormal?** Research shows object storage naturally follows lognormal distributions (many small configs/thumbnails, few large videos/backups).

Other distributions: `fixed` (exact size), `uniform` (evenly distributed).

See [Config Syntax](docs/CONFIG_SYNTAX.md) for complete options.

## 🌐 Distributed Testing

Coordinate workloads across multiple agent nodes for large-scale load generation:

```bash
# Start agents on each host
sai3bench-agent --listen 0.0.0.0:7761

# Run distributed workload from controller
sai3bench-ctl --insecure \
  --agents host1:7761,host2:7761,host3:7761 \
  run --config workload.yaml
```

**Features**: Coordinated start, automatic storage detection, result aggregation with HDR histogram merging, per-agent and consolidated metrics.

See [Distributed Testing Guide](docs/DISTRIBUTED_TESTING_GUIDE.md) for details.

## ⚙️ Key Features

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

## 📄 License

GPL-3.0 License - See [LICENSE](LICENSE) for details.
