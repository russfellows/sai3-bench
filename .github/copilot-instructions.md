# s3-bench AI Agent Guide

## Project Overview
s3-bench is a Rust-based S3 performance testing tool with unified multi-backend support (`file://`, `direct://`, `s3://`) using the `s3dlio` library. It provides both single-node CLI and distributed gRPC execution with HDR histogram metrics.

## Architecture: Three Binary Strategy
- **`s3-bench`** (`src/main.rs`) - Single-node CLI for immediate testing
- **`s3bench-agent`** (`src/bin/agent.rs`) - gRPC server node for distributed loads
- **`s3bench-ctl`** (`src/bin/controller.rs`) - Coordinator for multi-agent execution

Generated from `proto/s3bench.proto` via `tonic-build` in `build.rs`.

## Critical Dependencies & Patterns

### ObjectStore Abstraction (Migration Complete)
All operations use `s3dlio::object_store::store_for_uri()` - **never** direct AWS SDK calls:
```rust
// In src/workload.rs - always use this pattern:
pub fn create_store_for_uri(uri: &str) -> anyhow::Result<Box<dyn ObjectStore>> {
    store_for_uri(uri).context("Failed to create object store")
}
```
**Key**: s3dlio pinned to `rev = "cd4ee2e"` for API stability.

### Config System (`src/config.rs`)
- YAML parsing with `target` base URI + relative `path` resolution
- `OpSpec` enum: `Get { path }` and `Put { path, object_size }`
- Example from `tests/configs/file_test.yaml`:
```yaml
target: "file:///tmp/s3bench-test/"
workload:
  - op: get
    path: "data/*"  # Resolves to file:///tmp/s3bench-test/data/*
    weight: 70
```

### Metrics Architecture (HDR Histograms)
9 size buckets per operation type in `src/main.rs`:
```rust
const BUCKET_LABELS: [&str; NUM_BUCKETS] = [
    "zero", "1B-8KiB", "8KiB-64KiB", "64KiB-512KiB", /* ... */
];
```
Use `bucket_index(nbytes)` function for consistent bucketing.

## Essential Development Commands

### Build All Binaries
```bash
cargo build --release  # Requires protoc installed for gRPC
```

### Test Multi-Backend Operations
```bash
# File backend (no credentials needed)
./target/release/s3-bench -v run --config tests/configs/file_test.yaml

# S3 backend (requires .env with AWS_*)
./target/release/s3-bench -vv run --config tests/configs/mixed.yaml
```

### Distributed Mode Testing
```bash
# Terminal 1: Start agent
./target/release/s3bench-agent --listen 127.0.0.1:7761

# Terminal 2: Test connectivity
./target/release/s3bench-ctl --insecure --agents 127.0.0.1:7761 ping
```

## Code Conventions & Gotchas

### Workload Execution Pattern (`src/workload.rs`)
- Pre-resolution: GET operations fetch object lists **once** before workload starts
- Weighted selection via `rand_distr::WeightedIndex` from config weights
- Semaphore-controlled concurrency: `Arc<Semaphore::new(cfg.concurrency)>`

### Backend Detection Logic
`BackendType::from_uri()` in `src/workload.rs` determines storage backend:
```rust
// "s3://" -> S3, "file://" -> File, "direct://" -> DirectIO
// Default fallback is File for unrecognized schemes
```

### gRPC Protocol Buffer Build
- `build.rs` generates `src/pb/s3bench.rs` from `proto/s3bench.proto`
- **Requires**: `protoc` compiler installed system-wide
- Output goes to tracked `src/pb/` directory (not target/)

### TLS Configuration (Distributed Mode)
- Agent: `--tls --tls-domain hostname --tls-write-ca cert.pem`
- Controller: `--agent-ca cert.pem --agent-domain hostname`
- Default domain is "localhost" - use `--tls-sans` for additional names

## Integration Context

### s3dlio Library Integration
- **Never import AWS SDK directly** for storage operations
- Use `ObjectStore` trait methods: `get()`, `put()`, `list()`
- URI schemes automatically route to correct backend implementation

### Legacy Code Markers
Look for `TODO: Remove legacy s3_utils imports` - these indicate partial migration areas that should be updated to use ObjectStore when touched.

## Performance Characteristics
- **File backend**: 25k+ ops/s with 1ms latency validated
- **Concurrency**: Configurable via YAML `concurrency` field
- **Logging**: `-v` operational info, `-vv` detailed tracing

When extending functionality, always maintain ObjectStore abstraction and ensure new features work across all backend types (`file://`, `direct://`, `s3://`).