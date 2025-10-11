# sai3-bench: Multi-Protocol I/O Benchmarking Suite

[![Version](https://img.shields.io/badge/version-0.6.4-blue.svg)](https://github.com/russfellows/sai3-bench/releases)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/russfellows/sai3-bench)
[![Tests](https://img.shields.io/badge/tests-35%20passing-success.svg)](https://github.com/russfellows/sai3-bench)
[![License](https://img.shields.io/badge/license-GPL--3.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.90%2B-green.svg)](https://www.rust-lang.org/)

A storage performance testing tool that supports multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) (v0.9.6) for multi-protocol support.

> **Latest (v0.6.4)**: Enhanced output with automatic results directories and HDR histogram merging for distributed workloads. All test results are now captured in timestamped directories with mathematically accurate consolidated metrics. See [CHANGELOG](docs/CHANGELOG.md) for details.

> **Previous (v0.6.3)**: Upgraded to s3dlio v0.9.6 which **disables RangeEngine by default** for optimal performance on typical workloads. This resolves a 20-25% performance regression caused by HEAD request overhead. RangeEngine can still be explicitly enabled for large-file workloads (≥64 MiB).

## 🚀 What Makes sai3-bench Unique?

1. **Universal Storage Testing**: Unified interface across 5 storage protocols (file://, direct://, s3://, az://, gs://)
2. **RangeEngine Performance**: Automatic 30-50% throughput boost for large files (≥4MB) via concurrent byte-range requests
3. **Configurable Data Patterns**: Deduplication and compression testing with configurable data characteristics
4. **Workload Replay**: Timing-faithful replay with flexible remapping capabilities (1→1, 1→N, N→1, regex)
5. **Statistical Size Distributions**: Lognormal, uniform, and fixed distributions for realistic object size modeling
6. **Production-Grade Metrics**: Microsecond-precision HDR histograms with size-bucketed analysis
7. **Machine-Readable Output**: TSV export (sorted by bucket_idx) for automated analysis and CI/CD integration
8. **Distributed Architecture**: gRPC-based agent/controller system for large-scale load generation
9. **Storage Efficiency Testing**: Built-in support for testing deduplication engines and compression algorithms

## 🎯 Supported Storage Backends

- **File System** (`file://`) - Local filesystem testing with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O for maximum throughput
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage (MinIO, etc.) with optional RangeEngine
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage with optional RangeEngine
- **Google Cloud Storage** (`gs://` or `gcs://`) - Google Cloud Storage with optional RangeEngine

**RangeEngine Configuration** (v0.6.3): RangeEngine is **disabled by default** in s3dlio v0.9.6 for optimal performance on typical workloads. For large-file workloads (≥64 MiB), explicitly enable it in your config:
```yaml
range_engine:
  enabled: true
  min_split_size: 16777216  # 16 MiB threshold
  chunk_size: 67108864      # 64 MiB chunks
  max_concurrent_ranges: 16
```
**When to enable**: Only for workloads with large files where network bandwidth is NOT the bottleneck (10+ Gbps networks). See [CHANGELOG](docs/CHANGELOG.md#063) for performance analysis.

## 📦 Architecture & Binaries

- **`sai3-bench`** - Single-node CLI for immediate testing across all backends
- **`sai3bench-agent`** - Distributed gRPC agent for multi-node load generation (now supports all backends!)
- **`sai3bench-ctl`** - Controller for coordinating distributed agents
- **`sai3bench-run`** - Dedicated workload runner (legacy, being integrated)

## 📖 Documentation
- **[Usage Guide](docs/USAGE.md)** - Getting started with sai3-bench
- **[Warp Parity Status](docs/WARP_PARITY_STATUS.md)** - Warp/warp-replay compatibility status
- **[Changelog](docs/CHANGELOG.md)** - Complete version history and release notes
- **[Azure Setup Guide](docs/AZURE_SETUP.md)** - Azure Blob Storage configuration
- **[s3dlio v0.9.4 Migration Guide](docs/S3DLIO_V0.9.4_MIGRATION.md)** - RangeEngine details and upgrade guide
- **[Test Results](docs/S3DLIO_V0.9.4_TEST_RESULTS.md)** - Comprehensive backend performance benchmarks
- **[Config Examples](tests/configs/README.md)** - Complete guide to test configurations

## 🏆 sai3-bench Capabilities Overview

| Capability | Implementation | Use Cases |
|------------|----------------|----------|
| **Storage Backends** | 5 protocols via unified API | Cross-cloud migration, protocol comparison |
| **RangeEngine** | Automatic for files ≥4MB | 30-50% faster downloads on Azure/GCS/S3 |
| **Size Distributions** | Lognormal, uniform, fixed | Realistic workload modeling |
| **Data Characteristics** | Configurable dedup/compression | Storage efficiency testing |
| **Workload Replay** | Microsecond-precision timing | Production load analysis |
| **Advanced Remapping** | 1:1, 1→N, N→1, N↔N patterns | Complex migration scenarios |
| **Concurrency Control** | Global + per-operation | Fine-grained performance tuning |
| **Output Format** | 13-column TSV (bucket-sorted) | Automated analysis, CI/CD |
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
- **[Distributed Design](docs/V0.6.0_DISTRIBUTED_DESIGN.md)** - v0.6.0 distributed workload architecture

## 🎊 Latest Release (v0.6.0) - Distributed Multi-Host Workload Execution

### 🌐 Distributed Benchmarking
Run coordinated workloads across multiple agent nodes with automatic shared/local storage detection.

```bash
# Start agents on multiple hosts
sai3bench-agent --listen 0.0.0.0:7761  # On host 1
sai3bench-agent --listen 0.0.0.0:7761  # On host 2
sai3bench-agent --listen 0.0.0.0:7761  # On host 3

# Run distributed workload from controller
sai3bench-ctl --insecure --agents host1:7761,host2:7761,host3:7761 \
    run --config production-workload.yaml --start-delay 2

# Output shows per-agent and aggregate results
=== Distributed Results ===
Total agents: 3

--- Agent: agent-1 ---
  Wall time: 10.02s
  Total ops: 102156 (10195.21 ops/s)
  Total bytes: 85.23 MB (8.51 MiB/s)
  GET: 71509 ops, 59.66 MB, mean: 225µs, p95: 315µs
  PUT: 30647 ops, 25.57 MB, mean: 109µs, p95: 155µs

--- Agent: agent-2 ---
  Wall time: 10.01s
  Total ops: 101834 (10173.13 ops/s)
  Total bytes: 84.98 MB (8.49 MiB/s)
  GET: 71324 ops, 59.48 MB, mean: 228µs, p95: 318µs
  PUT: 30510 ops, 25.50 MB, mean: 111µs, p95: 157µs

--- Agent: agent-3 ---
  Wall time: 10.03s
  Total ops: 102089 (10181.45 ops/s)
  Total bytes: 85.18 MB (8.49 MiB/s)
  GET: 71463 ops, 59.62 MB, mean: 226µs, p95: 316µs
  PUT: 30626 ops, 25.56 MB, mean: 110µs, p95: 156µs

--- Aggregate ---
  Total ops: 306079
  Total bytes: 255.39 MB
  Combined throughput: 30549.79 ops/s
```

**Key Features**:
- **Coordinated Start**: All agents begin workload simultaneously with nanosecond-precision synchronization
- **Per-Agent Path Isolation**: Each agent operates in isolated subdirectory (e.g., `agent-1/`, `agent-2/`)
- **Smart Storage Detection**: Automatic handling based on URI scheme:
  - **Shared storage** (S3/GCS/Azure): All agents use same prepared dataset
  - **Local storage** (file://): Each agent prepares own isolated dataset
- **Result Aggregation**: Per-agent statistics plus combined throughput metrics
- **Flexible Configuration**: Override agent IDs, path templates, and storage mode

**Controller Flags**:
```bash
--config <file>           # YAML workload configuration
--path-template <template> # Agent path prefix (default: "agent-{id}/")
--agent-ids <list>        # Custom agent identifiers
--start-delay <seconds>   # Coordinated start delay (default: 2)
--shared-prepare          # Override auto-detected storage mode
```

**Use Cases**:
- **Large-Scale Load Testing**: Generate 100k+ ops/s across multiple nodes
- **Distributed System Validation**: Test storage backend under multi-client load
- **Cloud Migration Testing**: Parallel workload execution from multiple regions
- **Performance Baselines**: Measure aggregate throughput across infrastructure

### 🧪 Storage Efficiency Testing (v0.5.4)
Deduplication & compression testing with controlled data patterns.

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

| Feature | Warp | sai3-bench v0.6.0 |
|---------|------|-----------------|
| **Distributed execution** | No | **gRPC-based multi-host** coordination |
| **Storage detection** | N/A | **Auto-detect shared vs local** |
| **Size distributions** | Random only | **Uniform + Lognormal** (realistic) |
| **Concurrency control** | Global only | **Per-operation** override |
| **Prepare profiles** | Basic | **Documented patterns** with realistic distributions |
| **Backend support** | S3 only | **5 backends** (S3, Azure, GCS, File, Direct I/O) |
| **Remapping** | 1:1 only | **1:1, 1→N, N→1, N↔N** (regex) |
| **Output format** | Text analysis | **TSV** (13 columns, machine-readable) |
| **Memory usage** | High (replay) | **Constant** (streaming replay ~1.5 MB) |

## 🌟 Previous Releases

### v0.6.2 - s3dlio v0.9.5 Upgrade & Naming Cleanup
- **s3dlio v0.9.5**: Minor performance improvements and bug fixes
- **Comprehensive cleanup**: All io-bench references updated to sai3-bench
- **Test fixes**: Streaming replay tests now properly documented (require --test-threads=1)
- **RangeEngine**: Uses s3dlio defaults (custom config planned for future release)
- **Documentation**: Updated branding, test configs, and usage examples

### v0.5.9 - Output Clarity & Branding Consistency
- **Enhanced metrics display**: Added mean latency to Results output
- **Cleaner console output**: Removed duplicate histogram display
- **Branding updates**: Consistent "sai3-bench" terminology throughout
- **Documentation**: Updated CLI help examples and code comments

### v0.5.4 - Storage Efficiency Testing
- **Deduplication testing**: Configurable block-level duplication patterns
- **Compression testing**: Adjustable data compressibility (zero-fill ratios)
- **Size distributions**: Lognormal, uniform, and fixed distributions
- **Per-operation concurrency**: Fine-grained worker pool control

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

