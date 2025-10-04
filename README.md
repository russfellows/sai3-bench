# io-bench: Multi-Protocol I/O Benchmarking Suite

A comprehensive storage performance testing tool that supports multiple backends through a unified interface. Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) for robust multi-protocol support.

## 🚀 What Makes io-bench Different?

1. **Multi-Protocol Support**: Unlike tools that focus on a single protocol, io-bench supports 4 storage backends
2. **Unified Interface**: Consistent CLI and configuration across all storage types  
3. **Advanced Metrics**: Microsecond-precision HDR histogram performance measurements
4. **Distributed Execution**: gRPC-based agent/controller architecture for scale testing
5. **Workload Replay**: Support for captured workload replay and analysis
6. **Real-World Patterns**: Glob patterns, concurrent operations, and mixed workloads

## 🎯 Supported Storage Backends

- **File System** (`file://`) - Local filesystem testing with standard POSIX operations
- **Direct I/O** (`direct://`) - High-performance direct I/O for maximum throughput
- **Amazon S3** (`s3://`) - S3 and S3-compatible storage (MinIO, etc.)
- **Azure Blob** (`az://`) - Microsoft Azure Blob Storage with full authentication support

## 📦 Architecture & Binaries

- **`io-bench`** - Single-node CLI for immediate testing across all backends
- **`iobench-agent`** - Distributed gRPC agent for multi-node load generation  
- **`iobench-ctl`** - Controller for coordinating distributed agents
- **`iobench-run`** - Dedicated workload runner (legacy, being integrated)

## 📖 Documentation
- **[Usage Guide](docs/USAGE.md)** - Getting started with io-bench
- **[Release Notes v0.4.0](docs/RELEASE_NOTES_v0.4.0.md)** - Replay feature details and examples
- **[Future Work](docs/REPLAY_FUTURE_WORK.md)** - Planned enhancements for v0.4.1+
- **[Changelog](docs/CHANGELOG.md)** - Version history and release notes
- **[Azure Setup Guide](docs/AZURE_SETUP.md)** - Azure Blob Storage configuration
- **[Integration Context](docs/INTEGRATION_CONTEXT.md)** - Technical integration details

## 🎊 Latest Release (v0.4.1) - Streaming Op-log Replay

### 🌊 Streaming Replay with Memory Efficiency
- **Constant Memory Usage**: Stream replay with ~1.5 MB memory footprint (vs. full file in memory)
- **s3dlio-oplog Integration**: Dedicated streaming reader with background decompression
- **Non-Logging Replay**: Prevents circular logging during replay operations
- **Comprehensive Tests**: 6 integration tests covering streaming, remapping, error handling, concurrency
- **s3dlio v0.8.19+**: Updated to latest s3dlio with bug fixes and GCS backend support

### 🎬 Timing-Faithful Replay (v0.4.0)
- **Op-Log Replay**: Replay captured workloads with microsecond-precision timing (~10µs accuracy)
- **Absolute Timeline Scheduling**: Prevents timing drift for faithful workload reproduction
- **Backend Retargeting**: Simple 1:1 URI remapping to replay on different storage backends
- **Speed Control**: Adjustable replay speed (e.g., 2x faster, 0.5x slower)
- **Universal Support**: Works across all 5 operation types (GET, PUT, DELETE, LIST, STAT)
- **Smart Compression**: Auto-detects zstd compression for space-efficient op-logs

### ✨ Enhanced Capabilities (v0.3.x)
- **Interactive Progress Bars**: Professional real-time progress visualization for all operations
- **Time-based Progress**: Smooth animated progress tracking for timed workloads with ETA
- **Operation Progress**: Visual completion tracking for GET, PUT, DELETE commands  
- **Smart Messages**: Dynamic progress context showing concurrency, data sizes, and rates

### 🎯 Core Features (v0.3.0)

- **Complete Multi-Backend Support**: Unified interface across file://, direct://, s3://, and az:// protocols
- **Microsecond Precision Metrics**: HDR histogram performance measurements with µs accuracy
- **Four Operation Categories**: GET, PUT, LIST, STAT, DELETE with dedicated latency tracking
- **Advanced Glob Patterns**: Cross-backend wildcard support for flexible object selection
- **Azure Blob Storage**: Full Azure authentication and proper URI format support
- **Enhanced Configuration**: Target-based YAML configs with environment variable support
- **Distributed Architecture**: gRPC agent/controller for multi-node load generation
- **Real-Time Performance**: Validated 25k+ ops/s file operations, network-dependent cloud performance

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

# Replay workload to different backend
./target/release/io-bench replay --op-log /tmp/workload.tsv.zst \
  --target "s3://mybucket/prefix/" --speed 2.0

# Test Azure Blob Storage (requires setup)
export AZURE_STORAGE_ACCOUNT="your-storage-account"
export AZURE_STORAGE_ACCOUNT_KEY="your-account-key"
./target/release/io-bench health --uri "az://your-storage-account/container/"

# Run distributed workload
./target/release/iobench-agent --listen 127.0.0.1:7761 &
./target/release/iobench-ctl --insecure --agents 127.0.0.1:7761 ping
```

### 📊 Performance Characteristics

- **File Backend**: 25k+ ops/s, sub-millisecond latencies
- **Direct I/O Backend**: 10+ MB/s throughput, ~100ms latencies  
- **Azure Blob Storage**: 2-3 ops/s, ~700ms latencies (network dependent)
- **Cross-Backend Workloads**: Mixed protocol operations in single configuration

### 🔧 Requirements

- **Rust**: Stable toolchain (2024 edition)
- **Protobuf**: `protoc` compiler for gRPC (distributed mode)
- **Storage Credentials**: Backend-specific authentication (AWS, Azure)

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

# Replay with error tolerance
io-bench replay --op-log /tmp/workload.tsv.zst --continue-on-error
```

**Key Features:**
- **Microsecond Precision**: ~10µs timing accuracy using absolute timeline scheduling
- **Verbose Logging**: `-v` for operational info, `-vv` for detailed debug tracing
- **Pattern Matching**: Glob patterns (`*`) and directory listings supported across backends
- **Concurrent Execution**: Semaphore-controlled concurrency with configurable worker counts

### Technical Notes

**v0.4.0 Release** - Added timing-faithful workload replay with microsecond-precision scheduling. All operations use ObjectStore trait abstraction via s3dlio library for unified multi-backend support (file://, direct://, s3://, az://). Production-ready with comprehensive testing and logging.

**Migration History** - Successfully migrated from direct AWS SDK calls to ObjectStore trait. Multi-backend URIs fully operational with comprehensive logging and performance validation.


