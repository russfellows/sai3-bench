# sai3-bench: Multi-Protocol I/O Benchmarking Suite

[![Version](https://img.shields.io/badge/version-0.5.7-blue.svg)](https://github.com/russfellows/sai3-bench/releases)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/russfellows/sai3-bench)
[![Tests](https://img.shields.io/badge/tests-35%20passing-success.svg)](https://github.com/russfellows/sai3-bench)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-green.svg)](https://www.rust-lang.org/)

A storage performance testing tool that supports multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) for multi-protocol support.

> **Latest (v0.5.7)**: Critical fix for DELETE pool corruption in mixed workloads + automatic TSV export with smart naming. See [CHANGELOG](docs/CHANGELOG.md) and [v0.5.7 Release Summary](docs/V0.5.7_RELEASE_SUMMARY.md) for details.

## 🚀 What Makes sai3-bench Unique?

1. **Universal Storage Testing**: Unified interface across 5 storage protocols (file://, direct://, s3://, az://, gs://)
2. **Configurable Data Patterns**: Deduplication and compression testing with configurable data characteristics
3. **Workload Replay**: Timing-faithful replay with flexible remapping capabilities (1→1, 1→N, N→1, regex)
4. **Statistical Size Distributions**: Lognormal, uniform, and fixed distributions for realistic object size modeling
5. **Production-Grade Metrics**: Microsecond-precision HDR histograms with size-bucketed analysis
6. **Machine-Readable Output**: TSV export for automated analysis and CI/CD integration
7. **Distributed Architecture**: gRPC-based agent/controller system for large-scale load generation
8. **Storage Efficiency Testing**: Built-in support for testing deduplication engines and compression algorithms

## 🎯 Supported Storage Backends

- **File System** (`file://`) - Local filesystem testing with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O for maximum throughput
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage (MinIO, etc.)
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage with full authentication support
- **Google Cloud Storage** (`gs://` or `gcs://`) - Google Cloud Storage with native GCS API support

## 📦 Architecture & Binaries

- **`sai3-bench`** - Single-node CLI for immediate testing across all backends
- **`sai3bench-agent`** - Distributed gRPC agent for multi-node load generation  
- **`sai3bench-ctl`** - Controller for coordinating distributed agents
- **`sai3bench-run`** - Dedicated workload runner (legacy, being integrated)

## 📖 Documentation
- **[Usage Guide](docs/USAGE.md)** - Getting started with sai3-bench
- **[Warp Parity Status](docs/WARP_PARITY_STATUS.md)** - Warp/warp-replay compatibility status
- **[Changelog](docs/CHANGELOG.md)** - Complete version history and release notes
- **[Azure Setup Guide](docs/AZURE_SETUP.md)** - Azure Blob Storage configuration

## 🏆 sai3-bench Capabilities Overview

| Capability | Implementation | Use Cases |
|------------|----------------|----------|
| **Storage Backends** | 5 protocols via unified API | Cross-cloud migration, protocol comparison |
| **Size Distributions** | Lognormal, uniform, fixed | Realistic workload modeling |
| **Data Characteristics** | Configurable dedup/compression | Storage efficiency testing |
| **Workload Replay** | Microsecond-precision timing | Production load analysis |
| **Advanced Remapping** | 1:1, 1→N, N→1, N↔N patterns | Complex migration scenarios |
| **Concurrency Control** | Global + per-operation | Fine-grained performance tuning |
| **Output Format** | 13-column TSV export | Automated analysis, CI/CD |
| **Memory Efficiency** | Constant ~1.5MB (streaming) | Large-scale workload replay |
| **Distributed Testing** | gRPC agent/controller | Multi-node load generation |

## 🔬 Workload Replay Capabilities

Most benchmarking tools generate synthetic workloads that may not represent real-world usage patterns. sai3-bench's workload replay capability addresses this by allowing you to record and replay actual production workloads:

**The Problem**: Most tools create artificial load patterns that don't match production behavior:
- Fixed operation ratios that never vary
- Regular timing patterns unlike bursty real workloads  
- Simple object access patterns vs. complex real-world sequences

**sai3-bench's Solution**: Record actual production workloads and replay them with microsecond fidelity:

```bash
# Step 1: Capture your real production workload (transparent logging)
sai3-bench --op-log /tmp/production.tsv.zst run --config production.yaml

# Step 2: Analyze the captured workload patterns
zstd -d /tmp/production.tsv.zst -c | head -10
# Shows: operation_type, object_path, timing, sizes, access_patterns

# Step 3: Replay against test environment with exact timing
sai3-bench replay --op-log /tmp/production.tsv.zst --target "az://test-storage/"

# Step 4: Migration testing - replay S3 workload against other backends
sai3-bench replay --op-log /tmp/s3-prod.tsv.zst --target "gs://migration-test/"

# Step 5: Load testing - replay at higher speeds
sai3-bench replay --op-log /tmp/prod.tsv.zst --speed 5.0  # 5x faster
```

**Real-World Applications**:
- **Pre-Migration Validation**: Test new storage backend with exact production load
- **Performance Regression Testing**: Detect changes using historical workload patterns
- **Capacity Planning**: Model peak load behavior with recorded traffic spikes
- **Cross-Cloud Comparison**: Run identical workloads across AWS, Azure, GCS
- **Cost Analysis**: Measure actual vs. synthetic workload costs

## 💾 Storage Efficiency Testing

Many benchmarking tools generate random data, which provides limited insight into storage system efficiency. sai3-bench includes controlled data generation for more realistic storage testing:

**Why This Matters**: Modern storage systems use deduplication and compression:
- **Enterprise Storage**: NetApp, EMC, Dell systems claim 2-10x space savings
- **Cloud Storage**: AWS, Azure, GCS offer transparent compression
- **Filesystems**: ZFS, Btrfs, NTFS provide built-in compression
- **Backup Systems**: Veeam, CommVault depend on dedup effectiveness

**Testing Strategy**:
```yaml
# Simulate typical enterprise data mix
prepare:
  # Highly dedupable data (VM templates, OS images)
  - path: "templates/"
    num_objects: 100
    size_distribution: {type: fixed, size: 10485760}  # 10MB each
    dedup_factor: 20     # 95% duplicate blocks
    compress_factor: 2   # 2:1 compression ratio
  
  # Document storage (moderate compression)
  - path: "documents/"
    num_objects: 5000
    size_distribution: {type: lognormal, mean: 524288, std_dev: 262144}
    dedup_factor: 3      # 67% unique blocks
    compress_factor: 4   # 4:1 compression ratio
  
  # Media files (minimal compression)
  - path: "media/"
    num_objects: 500
    size_distribution: {type: uniform, min: 5242880, max: 52428800}
    dedup_factor: 1      # 100% unique (no dedup)
    compress_factor: 1   # Uncompressible (encrypted/compressed)
```

**Validation Scenarios**:
- **Storage Vendor Claims**: Verify "up to 10:1 dedup ratio" promises
- **Migration Planning**: Predict space requirements after dedup/compression
- **Tier Optimization**: Model hot vs. cold data characteristics
- **Cost Modeling**: Calculate true storage costs with efficiency features

A storage performance testing tool that supports multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) for multi-protocol support.

## 🚀 What Makes sai3-bench Unique?

1. **Universal Storage Testing**: Unified interface across 5 storage protocols (file://, direct://, s3://, az://, gs://)
2. **Configurable Data Patterns**: Deduplication and compression testing with configurable data characteristics
3. **Workload Replay**: Timing-faithful replay with flexible remapping capabilities (1→1, 1→N, N→1, regex)
4. **Statistical Size Distributions**: Lognormal, uniform, and fixed distributions for realistic object size modeling
5. **Production-Grade Metrics**: Microsecond-precision HDR histograms with size-bucketed analysis
6. **Machine-Readable Output**: TSV export for automated analysis and CI/CD integration
7. **Distributed Architecture**: gRPC-based agent/controller system for large-scale load generation
8. **Storage Efficiency Testing**: Built-in support for testing deduplication engines and compression algorithms

## 🎯 Supported Storage Backends

- **File System** (`file://`) - Local filesystem testing with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O for maximum throughput
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage (MinIO, etc.)
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage with full authentication support
- **Google Cloud Storage** (`gs://` or `gcs://`) - Google Cloud Storage with native GCS API support

## 📦 Architecture & Binaries

- **`sai3-bench`** - Single-node CLI for immediate testing across all backends
- **`sai3bench-agent`** - Distributed gRPC agent for multi-node load generation  
- **`sai3bench-ctl`** - Controller for coordinating distributed agents
- **`sai3bench-run`** - Dedicated workload runner (legacy, being integrated)

## 📖 Documentation
- **[Usage Guide](docs/USAGE.md)** - Getting started with sai3-bench
- **[Warp Parity Status](docs/WARP_PARITY_STATUS.md)** - Warp/warp-replay compatibility status
- **[Changelog](docs/CHANGELOG.md)** - Complete version history and release notes
- **[Azure Setup Guide](docs/AZURE_SETUP.md)** - Azure Blob Storage configuration

## 🎊 Latest Release (v0.5.4) - Storage Efficiency Testing & Advanced Data Patterns

### 🧪 Deduplication & Compression Testing
Test storage system efficiency with controlled data patterns - useful for evaluating deduplication and compression capabilities.

```yaml
prepare:
  - path: "dedupe-test/"
    num_objects: 1000
    size_distribution:
      type: lognormal
      mean: 1048576
      std_dev: 524288
    dedup_factor: 5       # 20% unique blocks (80% duplicate)
    compress_factor: 3    # 67% zeros (3:1 compression ratio)

workload:
  - op: put
    path: "mixed-data/"
    weight: 100
    size_distribution:
      type: uniform
      min: 1024
      max: 10485760
    dedup_factor: 1       # 100% unique (no deduplication)
    compress_factor: 1    # Random data (uncompressible)
```

**Parameters Explained**:
- `dedup_factor`: Controls block-level duplication patterns
  - `1` = All unique blocks (no deduplication opportunity)
  - `2` = 50% unique blocks (50% dedup ratio)
  - `5` = 20% unique blocks (80% dedup ratio)
  - `10` = 10% unique blocks (90% dedup ratio)
  - Higher values = more duplication opportunities

- `compress_factor`: Controls data compressibility
  - `1` = Random data (uncompressible, realistic for encrypted/media files)
  - `2` = 50% zeros (2:1 compression ratio, typical for logs)
  - `3` = 67% zeros (3:1 compression ratio, common for documents)
  - `5` = 80% zeros (5:1 compression ratio, sparse databases)
  - Higher values = more compressible content

**Use Cases**:
- **Deduplication Engine Testing**: Validate NetApp, EMC, Dell, and other enterprise dedup systems
- **Compression Algorithm Validation**: Test ZFS, Btrfs, NTFS, and cloud storage compression
- **Storage Efficiency Analysis**: Measure real-world space savings with realistic data patterns
- **Capacity Planning**: Model storage requirements with various data characteristics
- **Cloud Cost Optimization**: Test compression effectiveness before large migrations
- **Backup System Validation**: Verify dedup ratios match vendor claims

### 📐 Realistic Object Size Distributions
**Research-Based Modeling**: Object storage workloads naturally follow statistical distributions.

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

**Why lognormal?** Research shows object storage workloads naturally follow lognormal distributions - users create many small files (configs, thumbnails, metadata) and few large files (videos, backups). This is far more realistic than simple random distributions.

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

| Feature | Warp | sai3-bench v0.5.3 |
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
./target/release/sai3-bench health --uri "file:///tmp/test/"
./target/release/sai3-bench put --uri "file:///tmp/test/data*.txt" --object-size 1024 --objects 100

# Capture workload with op-log
./target/release/sai3-bench --op-log /tmp/workload.tsv.zst \
  run --config my-workload.yaml

# Run workload with TSV export for machine-readable results
./target/release/sai3-bench run --config my-workload.yaml \
  --results-tsv /tmp/benchmark-results

# Replay workload to different backend
./target/release/sai3-bench replay --op-log /tmp/workload.tsv.zst \
  --target "s3://mybucket/prefix/" --speed 2.0

# Advanced: Replay with 1→N fanout remapping
./target/release/sai3-bench replay --op-log /tmp/workload.tsv.zst \
  --remap fanout-config.yaml

# Test Azure Blob Storage (requires setup)
export AZURE_STORAGE_ACCOUNT="your-storage-account"
export AZURE_STORAGE_ACCOUNT_KEY="your-account-key"
./target/release/sai3-bench health --uri "az://your-storage-account/container/"

# Test Google Cloud Storage (requires gcloud auth)
gcloud auth application-default login
./target/release/sai3-bench health --uri "gs://my-bucket/prefix/"
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
sai3-bench -v --op-log /tmp/production.tsv.zst run --config prod-workload.yaml

# Replay with exact timing
sai3-bench replay --op-log /tmp/production.tsv.zst

# Replay to different backend (migration testing)
sai3-bench replay --op-log /tmp/s3-workload.tsv.zst \
  --target "az://newstorage/container/"

# Replay at 10x speed for quick testing
sai3-bench replay --op-log /tmp/workload.tsv.zst --speed 10.0

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
sai3-bench replay --op-log /tmp/workload.tsv.zst --remap fanout.yaml

# Replay with error tolerance
sai3-bench replay --op-log /tmp/workload.tsv.zst --continue-on-error
```

### 📈 TSV Export for Analysis

```bash
# Run workload with machine-readable results export
sai3-bench run --config workload.yaml --results-tsv /tmp/results

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

## 🛠️ Development

### Running Tests

Most tests can be run in parallel:
```bash
cargo test
```

However, `streaming_replay_tests` must run sequentially due to shared state:
```bash
cargo test --test streaming_replay_tests -- --test-threads=1
```

Or run all tests properly:
```bash
# Unit tests
cargo test --lib

# Integration tests (run individually)
cargo test --test utils
cargo test --test gcs_tests  
cargo test --test grpc_integration
cargo test --test streaming_replay_tests -- --test-threads=1
```

### Building

```bash
# Development build
cargo build

# Release build (optimized)
cargo build --release
```

## 📄 License

GPL-3.0 License - See [LICENSE](LICENSE) for details.

