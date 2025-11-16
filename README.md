# sai3-bench: Multi-Protocol I/O Benchmarking Suite

[![Version](https://img.shields.io/badge/version-0.7.8-blue.svg)](https://github.com/russfellows/sai3-bench/releases)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/russfellows/sai3-bench)
[![Tests](https://img.shields.io/badge/tests-44%20passing-success.svg)](https://github.com/russfellows/sai3-bench)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-green.svg)](https://www.rust-lang.org/)

A comprehensive storage performance testing tool supporting multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) for multi-protocol support.

## 🌟 Latest Release - v0.7.8 (November 15, 2025)

**🎯 Prepare Phase Metrics Persistence:**

- **Prepare metrics via gRPC**: Complete transmission and aggregation of prepare phase statistics
- **`prepare_results.tsv` files**: Per-agent and consolidated TSV files with PUT operation metrics
- **HDR histogram merging**: Accurate percentile aggregation across multiple agents
- **Bug fixes**: Workload timer reset, bucket label consistency, shared storage path handling

```bash
# Run distributed test and get prepare metrics
sai3bench-ctl --agents host1:7761,host2:7762 run --config workload.yaml

# Results include prepare_results.tsv (consolidated) and per-agent files
ls sai3-*/
  prepare_results.tsv       # Merged histograms from all agents
  results.tsv               # Workload results
  agents/agent-1/prepare_results.tsv
  agents/agent-2/prepare_results.tsv
```

See [CHANGELOG](docs/CHANGELOG.md#078) for complete details.

**Previous Release - v0.7.7 (November 15, 2025)** - CLI improvements with `--tls` flag and `--dry-run` support

## 🚀 What Makes sai3-bench Unique?

1. **Universal Storage Testing**: Unified interface across 5 storage protocols (file://, direct://, s3://, az://, gs://)
2. **Directory Tree Workloads**: Configurable hierarchical structures for realistic shared filesystem testing (v0.7.0)
3. **Filesystem Operations**: Full support for nested paths and directory operations across all backends (v0.7.0)
4. **Workload Replay**: Capture production traffic and replay with microsecond fidelity (1→1, 1→N, N→1 remapping)
5. **Op-Log Management**: Sort, validate, and merge operation logs for analysis and replay (v0.7.4)
6. **Distributed Architecture**: gRPC-based agent/controller for coordinated multi-node load generation
7. **Production-Grade Metrics**: HDR histograms with size-bucketed analysis and aggregate summaries
8. **Realistic Data Patterns**: Lognormal size distributions, configurable deduplication and compression
9. **Machine-Readable Output**: TSV export with per-bucket and aggregate rows for automated analysis

## 🎯 Supported Storage Backends

All operations work identically across protocols - just change the URI scheme:

- **File System** (`file://`) - Local filesystem with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O bypassing page cache (optimized chunked reads)
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage (MinIO, Ceph, etc.)
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage
- **Google Cloud Storage** (`gs://` or `gcs://`) - Google Cloud Storage with native GCS API

See [Cloud Storage Setup](docs/CLOUD_STORAGE_SETUP.md) for authentication guides.

## 🌳 Directory Tree Workloads (v0.7.0)

Test realistic shared filesystem scenarios with configurable directory hierarchies:

```yaml
directory_tree:
  width: 3              # Subdirectories per level
  depth: 2              # Tree depth (2 = 3 + 9 directories)
  files_per_dir: 10     # Files per directory
  distribution: bottom  # "bottom" (leaf only) or "all" (every level)
  
  size:
    type: uniform       # or "lognormal", "fixed"
    min_size_kb: 4
    max_size_kb: 16
  
  fill: random          # "random" (default), "zero", or "sequential"
  dedup_factor: 1       # Compression/dedup testing
  compress_factor: 1
```

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

## 📄 License

GPL-3.0 License - See [LICENSE](LICENSE) for details.
