# s3-bench AI Agent Guide

## Project Overview
s3-bench is a Rust-based S3 performance testing tool with both single-node CLI and distributed gRPC execution modes. It leverages the `s3dlio` library for multi-backend storage operations and includes comprehensive metrics collection with HDR histograms.

## Project Directory
/home/eval/Documennts/Code/s3-bench

## Architecture Components

### Three Binary Modes
- **`s3-bench`** (`src/main.rs`) - Single-node CLI for immediate testing
- **`s3bench-agent`** (`src/bin/agent.rs`) - gRPC server for distributed execution
- **`s3bench-ctl`** (`src/bin/controller.rs`) - Controller to orchestrate multiple agents

### Core Modules
- **`src/workload.rs`** - Workload execution engine with weighted operation selection
- **`src/config.rs`** - YAML configuration parsing for workload definitions
- **`proto/s3bench.proto`** - gRPC service definitions for distributed coordination

## Key Integration Context
**ObjectStore Migration Complete** - This project has successfully completed migration to multi-backend storage URIs (`file://`, `direct://`, `s3://`) using s3dlio's ObjectStore trait. All workload operations now support multi-backend execution.

### Critical Dependencies
- **s3dlio v0.8.7** - Multi-backend storage library (pinned to rev `cd4ee2e`)
- **ObjectStore trait** - Unified storage interface for all backends
- **tracing** - Comprehensive logging with `-v/-vv` CLI options
- **tonic/prost** - gRPC framework for distributed execution

## Development Workflows

### Building
```bash
cargo build --release  # Builds all three binaries
```

### Testing Multi-Backend Support
```bash
# File backend test
./target/release/s3-bench -v run --config tests/configs/file_test.yaml

# S3 backend test (requires .env with credentials)
./target/release/s3-bench -v run --config tests/configs/mixed.yaml

# Debug level logging
./target/release/s3-bench -vv run --config tests/configs/debug_test.yaml
```

### Testing Distributed Mode
```bash
# Terminal 1: Start agent
./target/release/s3bench-agent --listen 127.0.0.1:7761

# Terminal 2: Run controller
./target/release/s3bench-ctl --insecure --agents 127.0.0.1:7761 ping
```

### Configuration Format
Workloads are defined in YAML with weighted operations:
```yaml
duration: 30s
concurrency: 32
workload:
  - weight: 70
    op: get
    uri: "s3://bucket/prefix/*"
  - weight: 30
    op: put
    bucket: bucket-name
    prefix: "bench/"
    object_size: 1048576
```

## Project-Specific Patterns

### URI Parsing Convention
- Uses `s3dlio::object_store::store_for_uri()` throughout codebase
- Returns ObjectStore instance for any supported URI scheme
- **IMPLEMENTED**: Support for `file://`, `direct://`, and `s3://` schemes

### Metrics Collection
- Uses HDR histograms with 9 size buckets (zero, 1B-8KiB, 8KiB-64KiB, etc.)
- Separate histograms for GET/PUT operations
- Per-operation aggregates (`OpAgg`) and size bins (`SizeBins`)

### Async Concurrency Pattern
```rust
let sem = Arc::new(Semaphore::new(cfg.concurrency));
// Workers acquire semaphore permits for controlled concurrency
```

### Storage Backend Abstraction
Full ObjectStore trait implementation:
- `get_object_multi_backend()` and `put_object_multi_backend()` using ObjectStore
- Automatic backend detection via URI scheme
- `prefetch_uris_multi_backend()` using ObjectStore::list()

## Common Gotchas

### gRPC Build Requirements
- Requires `protoc` (Protocol Buffers compiler) installed
- `tonic-build` runs at compile time via `build.rs`

### TLS Configuration
- Agent supports self-signed certificates with `--tls`
- Controller must use `--agent-ca` to trust agent certificates
- Default domain is "localhost" - use `--tls-sans` for custom domains

### Workload Pre-resolution
GET operations pre-fetch object lists once before workload execution to avoid listing overhead during performance tests.

## Integration Points

### dl-driver Integration
- s3-bench included as GitHub dependency in dl-driver's Cargo.toml
- **COMPLETE**: Replay functionality supports `file://` and `direct://` URI schemes
- Maintains S3 performance optimizations while supporting new backends

### s3dlio Library
- Provides `ObjectStore` trait for unified storage access
- Uses `store_for_uri()` pattern for multi-backend operations
- Pinned to `cd4ee2e` revision for stable API

## Current Status

**Stage 2 Migration Complete** - All workload operations now use ObjectStore trait with full multi-backend support. Key achievements:

1. **Multi-backend Operations**: `file://`, `direct://`, and `s3://` fully supported
2. **Clean Migration**: Removed deprecated AWS SDK functions and imports
3. **Comprehensive Logging**: Added tracing with `-v/-vv` CLI options
4. **Performance Validated**: 25k+ ops/s on file backend with 1ms latency
5. **Zero Warnings**: Clean compilation after proper code analysis

## Future Development Priorities

1. **S3 Credential Integration** - Add dotenvy support for .env file credentials
2. **Distributed Mode Validation** - Test agent/controller with ObjectStore changes
3. **Enhanced Backend Features** - Leverage backend-specific optimizations

When extending functionality, maintain the ObjectStore abstraction and ensure all backends receive equal treatment in performance and feature support.