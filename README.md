# sai3-bench: Multi-Protocol I/O Benchmarking Suite

[![Version](https://img.shields.io/badge/version-0.6.7-blue.svg)](https://github.com/russfellows/sai3-bench/releases)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/russfellows/sai3-bench)
[![Tests](https://img.shields.io/badge/tests-35%20passing-success.svg)](https://github.com/russfellows/sai3-bench)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-green.svg)](https://www.rust-lang.org/)

A storage performance testing tool that supports multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) (v0.9.6) for multi-protocol support.

> **Latest (v0.6.7)**: Added aggregate summary rows to TSV export. Each operation type (GET/PUT/META) now includes an "ALL" row combining statistics across all size buckets with proper HDR histogram merging for accurate latency percentiles. See [CHANGELOG](docs/CHANGELOG.md) for details.

## 🚀 What Makes sai3-bench Unique?

1. **Universal Storage Testing**: Unified interface across 5 storage protocols (file://, direct://, s3://, az://, gs://)
2. **Configurable Data Patterns**: Deduplication and compression testing with configurable data characteristics
3. **Workload Replay**: Timing-faithful replay with flexible remapping capabilities (1→1, 1→N, N→1, regex)
4. **Statistical Size Distributions**: Lognormal, uniform, and fixed distributions for realistic object size modeling
5. **Production-Grade Metrics**: Microsecond-precision HDR histograms with size-bucketed analysis
6. **Machine-Readable Output**: TSV export for automated analysis and CI/CD integration
7. **Distributed Architecture**: gRPC-based agent/controller system for large-scale load generation
8. **Config Validation**: Parse and verify YAML configs before execution with `--dry-run`

## 🎯 Supported Storage Backends

- **File System** (`file://`) - Local filesystem testing with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O for maximum throughput
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage (MinIO, etc.)
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage with full authentication support
- **Google Cloud Storage** (`gs://` or `gcs://`) - Google Cloud Storage with native GCS API support

All backends support RangeEngine for large file downloads (disabled by default for optimal performance). Enable in config for files ≥16 MiB on high-bandwidth networks.

## 📦 Architecture & Binaries

- **`sai3-bench`** - Single-node CLI for immediate testing across all backends
- **`sai3bench-agent`** - Distributed gRPC agent for multi-node load generation  
- **`sai3bench-ctl`** - Controller for coordinating distributed agents

## 📖 Documentation
- **[Usage Guide](docs/USAGE.md)** - Getting started with sai3-bench
- **[Distributed Testing Guide](docs/DISTRIBUTED_TESTING_GUIDE.md)** - Multi-host load generation and testing
- **[Cloud Storage Setup](docs/CLOUD_STORAGE_SETUP.md)** - S3, Azure, and GCS authentication guides
- **[Data Generation Guide](docs/DATA_GENERATION.md)** - Fill patterns, deduplication, and compression testing
- **[Config Syntax](docs/CONFIG_SYNTAX.md)** - YAML configuration reference
- **[Config Examples](tests/configs/README.md)** - Complete guide to test configurations
- **[Changelog](docs/CHANGELOG.md)** - Complete version history and release notes

## 🚀 Quick Start

```bash
# Install and build
cargo build --release

# Test local filesystem
./target/release/sai3-bench util health --uri "file:///tmp/test/"
./target/release/sai3-bench util put --uri "file:///tmp/test/data*.txt" --object-size 1024 --objects 100

# Validate config file before running
./target/release/sai3-bench run --config my-workload.yaml --dry-run

# Run workload with automatic results directory
./target/release/sai3-bench run --config my-workload.yaml

# Capture workload with operation logging
./target/release/sai3-bench --op-log /tmp/workload.tsv.zst \
  run --config my-workload.yaml

# Replay workload to different backend
./target/release/sai3-bench replay --op-log /tmp/workload.tsv.zst \
  --target "az://test-storage/container/"

# Replay at higher speed for load testing
./target/release/sai3-bench replay --op-log /tmp/workload.tsv.zst --speed 5.0

# Test cloud storage (requires authentication)
export AZURE_STORAGE_ACCOUNT="your-storage-account"
export AZURE_STORAGE_ACCOUNT_KEY="your-account-key"
./target/release/sai3-bench util health --uri "az://your-storage-account/container/"

gcloud auth application-default login
./target/release/sai3-bench util health --uri "gs://my-bucket/prefix/"
```

### 📊 Performance Characteristics

- **File Backend**: 25k+ ops/s, sub-millisecond latencies
- **Direct I/O Backend**: 10+ MB/s throughput, ~100ms latencies  
- **S3 Backend**: Network-dependent, validated with real buckets
- **Azure Blob Storage**: 2-3 ops/s, ~700ms latencies (network dependent)
- **GCS Backend**: 9-11 MB/s for large objects, ~400-600ms for small operations

## 🔬 Workload Replay

Record actual production workloads and replay them with microsecond fidelity for realistic testing:

```bash
# Step 1: Capture production workload (transparent logging)
sai3-bench --op-log /tmp/production.tsv.zst run --config production.yaml

# Step 2: Replay against test environment with exact timing
sai3-bench replay --op-log /tmp/production.tsv.zst --target "az://test-storage/"

# Step 3: Migration testing - replay S3 workload against GCS
sai3-bench replay --op-log /tmp/s3-prod.tsv.zst --target "gs://migration-test/"

# Step 4: Load testing - replay at higher speeds
sai3-bench replay --op-log /tmp/prod.tsv.zst --speed 5.0

# Advanced: 1→N fanout remapping with strategy
cat > fanout.yaml <<EOF
rules:
  - match: {bucket: "source"}
    map_to_many:
      targets:
        - {bucket: "dest1", prefix: ""}
        - {bucket: "dest2", prefix: ""}
        - {bucket: "dest3", prefix: ""}
      strategy: "round_robin"
EOF
sai3-bench replay --op-log /tmp/workload.tsv.zst --remap fanout.yaml
```

**Real-World Applications**:
- **Pre-Migration Validation**: Test new storage backend with exact production load
- **Performance Regression Testing**: Detect changes using historical workload patterns
- **Capacity Planning**: Model peak load behavior with recorded traffic spikes
- **Cross-Cloud Comparison**: Run identical workloads across AWS, Azure, GCS
- **Cost Analysis**: Measure actual vs. synthetic workload costs

## 💾 Storage Efficiency Testing

Test deduplication and compression with controlled data generation:

```yaml
prepare:
  ensure_objects:
    # Highly dedupable data (VM templates, OS images)
    - base_uri: "s3://bucket/templates/"
      count: 100
      size_distribution:
        type: fixed
        size: 10485760  # 10 MB
      fill: random      # ⚠️ RECOMMENDED for realistic testing
      dedup_factor: 20  # 95% duplicate blocks
      compress_factor: 2  # 2:1 compression ratio
    
    # Document storage (moderate compression)
    - base_uri: "s3://bucket/documents/"
      count: 5000
      size_distribution:
        type: lognormal
        mean: 524288
        std_dev: 262144
      fill: random
      dedup_factor: 3   # 67% unique blocks
      compress_factor: 4  # 4:1 compression ratio
    
    # Media files (minimal compression)
    - base_uri: "s3://bucket/media/"
      count: 500
      size_distribution:
        type: uniform
        min: 5242880
        max: 52428800
      fill: random
      dedup_factor: 1   # 100% unique (no dedup)
      compress_factor: 1  # Uncompressible
```

**Parameters Explained**:
- `fill: random` - **RECOMMENDED** - Realistic data for storage testing (use `fill: zero` only for specific zero-data testing)
- `dedup_factor`: 1 = all unique, 5 = 80% dedup, 10 = 90% dedup
- `compress_factor`: 1 = uncompressible, 3 = 3:1 ratio, 5 = 5:1 ratio

**Use Cases**:
- Validate storage vendor dedup/compression claims
- Predict space requirements for migrations
- Model hot vs. cold data characteristics
- Calculate true storage costs with efficiency features

## 📐 Realistic Size Distributions

**Lognormal Distribution** (recommended for realistic modeling):
```yaml
workload:
  - op: put
    path: "data/"
    weight: 100
    size_distribution:
      type: lognormal
      mean: 1048576      # Mean: 1 MB
      std_dev: 524288    # Std dev: 512 KB
      min: 1024          # Floor: 1 KB
      max: 10485760      # Ceiling: 10 MB
    fill: random
```

**Why lognormal?** Research shows object storage workloads naturally follow lognormal distributions - many small files (configs, thumbnails) and few large files (videos, backups). Far more realistic than uniform random distributions.

Other supported distributions:
- **Fixed**: Exact size for each object
- **Uniform**: Evenly distributed between min and max

## 🌐 Distributed Testing

Run coordinated workloads across multiple agent nodes:

```bash
# Start agents on multiple hosts
sai3bench-agent --listen 0.0.0.0:7761  # On each host

# Run distributed workload from controller
sai3bench-ctl --insecure \
  --agents host1:7761,host2:7761,host3:7761 \
  run --config workload.yaml --start-delay 2
```

**Key Features**:
- **Coordinated Start**: All agents begin simultaneously
- **Smart Storage Detection**: Automatic shared vs. local storage handling
- **Result Aggregation**: Per-agent and combined metrics with HDR histogram merging
- **Automatic Results Collection**: Consolidated results directory with per-agent subdirectories

See [Distributed Testing Guide](docs/DISTRIBUTED_TESTING_GUIDE.md) for detailed examples.

## ⚙️ Advanced Features

### Per-Operation Concurrency
Fine-grained control over worker pools:
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
    size_distribution: {type: fixed, size: 1048576}
    concurrency: 8   # Fewer PUT workers
```

### Config Validation
Verify YAML syntax and configuration before execution:
```bash
sai3-bench run --config my-workload.yaml --dry-run
```

Displays:
- Test configuration (duration, concurrency, backend)
- Prepare phase details (if configured)
- Workload operations with percentages
- Any configuration errors with clear messages

### Operation Logging
Record all operations for replay or analysis:
```bash
# Always zstd compressed
sai3-bench --op-log /tmp/operations.tsv.zst run --config workload.yaml

# Decompress to view
zstd -d /tmp/operations.tsv.zst -o /tmp/operations.tsv
```

## 🛠️ Development

### Requirements
- **Rust**: Stable toolchain (2024 edition)
- **Protobuf**: `protoc` compiler for gRPC (distributed mode)
- **Storage Credentials**: Backend-specific authentication (AWS, Azure, GCS)

See [Cloud Storage Setup](docs/CLOUD_STORAGE_SETUP.md) for authentication details.

### Building
```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release
```

### Running Tests
```bash
# Most tests can run in parallel
cargo test

# Streaming replay tests must run sequentially
cargo test --test streaming_replay_tests -- --test-threads=1
```

## 📄 License

GPL-3.0 License - See [LICENSE](LICENSE) for details.
