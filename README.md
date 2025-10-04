# io-bench: Multi-Protocol I/O Benchmarking Suite

[![Version](https://img.shields.io/badge/version-0.5.3-blue.svg)](https://github.com/russfellows/s3-bench/releases)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/russfellows/s3-bench)
[![Tests](https://img.shields.io/badge/tests-18%20passing-success.svg)](https://github.com/russfellows/s3-bench)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-green.svg)](https://www.rust-lang.org/)

A comprehensive storage performance testing tool that supports multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) for robust multi-protocol support.

## 🚀 What Makes io-bench Different?

1. **Multi-Protocol Support**: Unlike tools that focus on a single protocol, io-bench supports 5 storage backends
2. **Unified Interface**: Consistent CLI and configuration across all storage types  
3. **Advanced Metrics**: Microsecond-precision HDR histogram performance measurements with size-bucketed analysis
4. **Machine-Readable Results**: TSV export for automated analysis and performance tracking
5. **Distributed Execution**: gRPC-based agent/controller architecture for scale testing
6. **Workload Replay**: Timing-faithful replay with advanced remapping (1→1, 1→N, N→1, regex)
7. **Warp Compatibility**: Near-identical testing capabilities to MinIO Warp/warp-replay

## 🎯 Supported Storage Backends

- **File System** (`file://`) - Local filesystem testing with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O for maximum throughput
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage (MinIO, etc.)
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage with full authentication support
- **Google Cloud Storage** (`gs://` or `gcs://`) - Google Cloud Storage with native GCS API support

## 📦 Architecture & Binaries

- **`io-bench`** - Single-node CLI for immediate testing across all backends
- **`iobench-agent`** - Distributed gRPC agent for multi-node load generation  
- **`iobench-ctl`** - Controller for coordinating distributed agents
- **`iobench-run`** - Dedicated workload runner (legacy, being integrated)

## 📖 Documentation
- **[Usage Guide](docs/USAGE.md)** - Getting started with io-bench
- **[Warp Parity Plan](docs/WARP_PARITY_PLAN.md)** - Warp/warp-replay compatibility roadmap
- **[Changelog](docs/CHANGELOG.md)** - Complete version history and release notes
- **[Azure Setup Guide](docs/AZURE_SETUP.md)** - Azure Blob Storage configuration

## 🎊 Latest Release (v0.5.3) - Realistic Size Distributions & Advanced Configurability

### 📐 Object Size Distributions (Surpasses Warp!)
**Problem**: Warp's "random" distribution is unrealistic. Real-world object storage shows lognormal patterns (many small files, few large ones).

**Solution**: Three distribution types for PUT operations and prepare steps:

1. **Fixed Size** (backward compatible):
   ```yaml
   - op: put
     path: "data/"
     object_size: 1048576  # Exactly 1 MB
   ```

2. **Uniform Distribution** (evenly distributed):
   ```yaml
   - op: put
     path: "data/"
     size_distribution:
       type: uniform
       min: 1024        # 1 KB
       max: 10485760    # 10 MB
   ```

3. **Lognormal Distribution** (realistic - recommended):
   ```yaml
   - op: put
     path: "data/"
     size_distribution:
       type: lognormal
       mean: 1048576      # Mean size: 1 MB
       std_dev: 524288    # Std deviation: 512 KB
       min: 1024          # Floor
       max: 10485760      # Ceiling (10 MB)
   ```

**Why lognormal?** Research shows object storage workloads naturally follow lognormal distributions - users create many small files (configs, thumbnails, metadata) and few large files (videos, backups). This is far more realistic than warp's simple random distribution.

### ⚙️ Per-Operation Concurrency
Fine-grained control over worker pools per operation type:

```yaml
concurrency: 32  # Global default

workload:
  - op: get
    path: "data/*"
    weight: 70
    concurrency: 64  # Override: More GET workers
  
  - op: put
    path: "data/"
    object_size: 1048576
    weight: 30
    concurrency: 8   # Override: Fewer PUT workers
```

**Use case**: Simulate real-world scenarios where reads far outnumber writes, or model slow backend write performance.

### 🎯 Prepare Profiles
Documented patterns for realistic test data preparation:

```yaml
prepare:
  ensure_objects:
    # Small objects (thumbnails, metadata) - lognormal
    - base_uri: "s3://bucket/small/"
      count: 10000
      size_distribution:
        type: lognormal
        mean: 4096
        std_dev: 2048
        min: 1024
        max: 65536
      fill: random
    
    # Medium objects (documents, images) - lognormal
    - base_uri: "s3://bucket/medium/"
      count: 1000
      size_distribution:
        type: lognormal
        mean: 1048576
        std_dev: 524288
        min: 65536
        max: 10485760
      fill: zero
    
    # Large objects (videos, backups) - uniform
    - base_uri: "s3://bucket/large/"
      count: 100
      size_distribution:
        type: uniform
        min: 10485760
        max: 104857600
      fill: zero
```

### 🔄 Advanced Remapping Examples
**N↔N (Many-to-Many) Remapping** with regex:

```yaml
# Map 3 source buckets → 2 destination buckets
remap:
  # Source bucket 1 & 2 → Destination bucket A
  - pattern: "s3://source-1/(.+)"
    replacement: "s3://dest-a/$1"
  - pattern: "s3://source-2/(.+)"
    replacement: "s3://dest-a/$1"
  
  # Source bucket 3 → Destination bucket B
  - pattern: "s3://source-3/(.+)"
    replacement: "s3://dest-b/$1"
```

### 🏆 Competitive Advantage vs Warp

| Feature | Warp | io-bench v0.5.3 |
|---------|------|-----------------|
| **Size distributions** | Random only | **Uniform + Lognormal** (realistic) |
| **Concurrency control** | Global only | **Per-operation** override |
| **Prepare profiles** | Basic | **Documented patterns** with realistic distributions |
| **Backend support** | S3 only | **5 backends** (S3, Azure, GCS, File, Direct I/O) |
| **Remapping** | 1:1 only | **1:1, 1→N, N→1, N↔N** (regex) |
| **Output format** | Text analysis | **TSV** (13 columns, machine-readable) |
| **Memory usage** | High (replay) | **Constant** (streaming replay ~1.5 MB) |

## 🌟 Previous Releases

### v0.5.2 - Machine-Readable Results & Enhanced Metrics
- **TSV export**: 13-column format for automated analysis
- **Enhanced metrics**: Mean + median, size-bucketed histograms
- **Performance validated**: 19.6k ops/s on file backend
- **Default concurrency**: Increased to 32 workers

### v0.5.0 - Advanced Replay Remapping
- **1→N Fanout**: Distribute operations across multiple targets (round_robin, random, sticky_key)
- **N→1 Consolidation**: Merge multiple sources to single target
- **Regex Remapping**: Pattern-based URI transformation for complex migrations
- **Streaming replay**: Constant ~1.5 MB memory via s3dlio-oplog integration

### v0.4.3 - Prepare/Pre-population
- **Prepare step**: Ensure objects exist before testing (Warp parity)
- **Cleanup support**: Optional deletion of prepared objects after tests
- **CLI flags**: `--prepare-only` and `--no-cleanup` for flexible workflows

### v0.4.2 - Google Cloud Storage Support
- **GCS backend**: Full integration with `gs://` and `gcs://` URI schemes
- **Application Default Credentials**: Seamless gcloud CLI authentication
- **Performance validated**: 9-11 MB/s for large objects, ~400-600ms latency

### v0.4.1 - Streaming Replay
- **Constant memory**: Stream replay with ~1.5 MB footprint (vs. full file in memory)
- **Background decompression**: Efficient handling of zstd-compressed op-logs

### v0.4.0 - Timing-Faithful Replay
- **Microsecond precision**: Replay with ~10µs accuracy
- **Backend retargeting**: Simple 1:1 URI remapping
- **Speed control**: Adjustable replay speed (e.g., 2x, 0.5x)

### v0.3.x - Enhanced UX
- **Interactive progress bars**: Professional real-time visualization
- **Time-based progress**: Smooth animated tracking with ETA
- **Smart messages**: Dynamic context showing concurrency, sizes, rates

### 🚀 Quick Start

```bash
# Install and build
cargo build --release

# Test local filesystem
./target/release/io-bench health --uri "file:///tmp/test/"
./target/release/io-bench put --uri "file:///tmp/test/data*.txt" --object-size 1024 --objects 100

# Capture workload with op-log
./target/release/io-bench --op-log /tmp/workload.tsv.zst \
  run --config my-workload.yaml

# Run workload with TSV export for machine-readable results
./target/release/io-bench run --config my-workload.yaml \
  --results-tsv /tmp/benchmark-results

# Replay workload to different backend
./target/release/io-bench replay --op-log /tmp/workload.tsv.zst \
  --target "s3://mybucket/prefix/" --speed 2.0

# Advanced: Replay with 1→N fanout remapping
./target/release/io-bench replay --op-log /tmp/workload.tsv.zst \
  --remap fanout-config.yaml

# Test Azure Blob Storage (requires setup)
export AZURE_STORAGE_ACCOUNT="your-storage-account"
export AZURE_STORAGE_ACCOUNT_KEY="your-account-key"
./target/release/io-bench health --uri "az://your-storage-account/container/"

# Test Google Cloud Storage (requires gcloud auth)
gcloud auth application-default login
./target/release/io-bench health --uri "gs://my-bucket/prefix/"
```

### 📊 Performance Characteristics

- **File Backend**: 25k+ ops/s, sub-millisecond latencies, 19.6k ops/s with full metrics
- **Direct I/O Backend**: 10+ MB/s throughput, ~100ms latencies  
- **S3 Backend**: Network-dependent, validated with real buckets
- **Azure Blob Storage**: 2-3 ops/s, ~700ms latencies (network dependent)
- **GCS Backend**: 9-11 MB/s for large objects, ~400-600ms for small operations
- **Cross-Backend Workloads**: Mixed protocol operations in single configuration

### 🔧 Requirements

- **Rust**: Stable toolchain (2024 edition)
- **Protobuf**: `protoc` compiler for gRPC (distributed mode)
- **Storage Credentials**: Backend-specific authentication (AWS, Azure, GCS)

See the [Usage Guide](docs/USAGE.md) for detailed setup instructions.

### 🎬 Workload Replay Examples

```bash
# Capture a workload to op-log
io-bench -v --op-log /tmp/production.tsv.zst run --config prod-workload.yaml

# Replay with exact timing
io-bench replay --op-log /tmp/production.tsv.zst

# Replay to different backend (migration testing)
io-bench replay --op-log /tmp/s3-workload.tsv.zst \
  --target "az://newstorage/container/"

# Replay at 10x speed for quick testing
io-bench replay --op-log /tmp/workload.tsv.zst --speed 10.0

# Advanced: 1→N fanout with round-robin strategy
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
io-bench replay --op-log /tmp/workload.tsv.zst --remap fanout.yaml

# Replay with error tolerance
io-bench replay --op-log /tmp/workload.tsv.zst --continue-on-error
```

### 📈 TSV Export for Analysis

```bash
# Run workload with machine-readable results export
io-bench run --config workload.yaml --results-tsv /tmp/results

# Generated file: /tmp/results-results.tsv with 13 columns:
# operation, size_bucket, bucket_idx, mean_us, p50_us, p90_us, p95_us, 
# p99_us, max_us, avg_bytes, ops_per_sec, throughput_mibps, count

# Parse with any TSV tool (awk, pandas, polars, etc.)
awk -F'\t' 'NR>1 {print $1, $2, $4, $5, $12}' /tmp/results-results.tsv
# Shows: operation, size_bucket, mean_us, p50_us, throughput_mibps
```

**Key Features:**
- **Microsecond Precision**: ~10µs timing accuracy using absolute timeline scheduling
- **Verbose Logging**: `-v` for operational info, `-vv` for detailed debug tracing
- **Pattern Matching**: Glob patterns (`*`) and directory listings supported across backends
- **Concurrent Execution**: Semaphore-controlled concurrency with configurable worker counts

### Technical Notes

**v0.4.0 Release** - Added timing-faithful workload replay with microsecond-precision scheduling. All operations use ObjectStore trait abstraction via s3dlio library for unified multi-backend support (file://, direct://, s3://, az://). Production-ready with comprehensive testing and logging.

**Migration History** - Successfully migrated from direct AWS SDK calls to ObjectStore trait. Multi-backend URIs fully operational with comprehensive logging and performance validation.


