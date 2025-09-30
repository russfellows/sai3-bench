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
This project is being extended to support multi-backend storage URIs (`file://`, `direct://`) for dl-driver replay functionality. Currently supports only `s3://` URIs via `parse_s3_uri()` function.

### Critical Dependencies
- **s3dlio** - Multi-backend storage library (currently unpinned, needs pinning to `cd4ee2e`)
- **AWS SDK** - Direct S3 operations for performance optimization
- **tonic/prost** - gRPC framework for distributed execution

## Development Workflows

### Building
```bash
cargo build --release  # Builds all three binaries
```

### Testing Single-Node
```bash
./target/release/s3-bench --config mixed.yaml
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
- Uses `s3dlio::s3_utils::parse_s3_uri()` throughout codebase
- Returns `(bucket, pattern)` tuple for S3 operations
- **EXTENSION NEEDED**: Support `file://` and `direct://` schemes

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
Currently S3-specific but designed for extension:
- `get_object()` and `put_object_async()` from s3dlio
- Direct AWS SDK calls for performance-critical paths

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
- Replay functionality needs `file://` and `direct://` URI support
- Must maintain S3 performance optimizations while adding new backends

### s3dlio Library
- Provides `ObjectStore` trait for unified storage access
- Use `store_for_uri()` pattern for multi-backend operations
- Current version mismatch requires pinning to `cd4ee2e` revision

## Immediate Development Priorities

1. **Pin s3dlio version** in `Cargo.toml` to match dl-driver dependency
2. **Extend URI parsing** from `parse_s3_uri()` to `parse_storage_uri()`
3. **Add backend detection** for `file://`, `direct://`, and `s3://` schemes
4. **Update workload engine** to route operations to appropriate storage backends

When modifying URI handling, ensure backward compatibility for existing S3 workloads and maintain performance characteristics for S3 operations.