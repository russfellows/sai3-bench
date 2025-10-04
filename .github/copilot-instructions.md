# io-bench AI Agent Guide

## Project Overview
io-bench is a comprehensive multi-protocol I/O benchmarking suite with unified multi-backend support (`file://`, `direct://`, `s3://`, `az://`, `gs://`) using the `s3dlio` library. It provides both single-node CLI and distributed gRPC execution with HDR histogram metrics and professional progress bars.

**Current Version**: v0.5.0-dev (October 2025) - Warp Parity Phase 2: Advanced Replay Remapping

## Architecture: Four Binary Strategy
- **`io-bench`** (`src/main.rs`) - Single-node CLI for immediate testing with interactive progress bars
- **`iobench-agent`** (`src/bin/agent.rs`) - gRPC server node for distributed loads
- **`iobench-ctl`** (`src/bin/controller.rs`) - Coordinator for multi-agent execution
- **`iobench-run`** (`src/bin/run.rs`) - Legacy workload runner (being integrated)

Generated from `proto/iobench.proto` via `tonic-build` in `build.rs`.

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

### Progress Bars (v0.3.1)
Professional progress visualization using `indicatif = "0.17"`:
```rust
// Time-based progress for workloads
let pb = ProgressBar::new(duration.as_secs());
pb.set_style(ProgressStyle::with_template(
    "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len}s ({eta_precise}) {msg}"
)?);

// Operation-based progress for GET/PUT/DELETE
let pb = ProgressBar::new(objects.len() as u64);
pb.set_message(format!("downloading with {} workers", jobs));
// ... async operations with pb.inc(1) ...
pb.finish_with_message(format!("downloaded {:.2} MB", total_mb));
```

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

### Code Search Tools
**Primary tool**: `rg` (ripgrep) - Fast, project-aware recursive search
```bash
# Search for function definitions across all Rust files
rg "fn create_store" --type rust

# Find all uses of a specific import
rg "use s3dlio::" --type rust

# Search with context lines (show 3 lines before/after match)
rg "ObjectStore" -C 3

# Case-insensitive search
rg -i "error" --type rust

# Search specific files/directories
rg "workload" src/workload.rs
rg "gRPC" proto/
```

**When to use rg vs VS Code tools**:
- Use `rg` for: Fast pattern searches, checking imports/usage counts, finding string literals
- Use `grep_search` tool for: Regex patterns with VS Code context integration
- Use `semantic_search` tool for: Conceptual searches when exact string is unknown

### Test Multi-Backend Operations
```bash
# File backend (no credentials needed)
./target/release/io-bench -v run --config tests/configs/file_test.yaml

# S3 backend (requires .env with AWS_*)
./target/release/io-bench -vv run --config tests/configs/mixed.yaml

# Azure backend (requires AZURE_STORAGE_ACCOUNT and AZURE_STORAGE_ACCOUNT_KEY)
./target/release/io-bench health --uri "az://storage-account/container/"
```

### Distributed Mode Testing
```bash
# Terminal 1: Start agent
./target/release/iobench-agent --listen 127.0.0.1:7761

# Terminal 2: Test connectivity
./target/release/iobench-ctl --insecure --agents 127.0.0.1:7761 ping
```

### Progress Bar Examples
```bash
# Timed workload with progress bar
./target/release/io-bench run --config tests/configs/file_test.yaml

# Operation-based progress (GET/PUT/DELETE)
./target/release/io-bench get --uri file:///tmp/test/data/* --jobs 4
./target/release/io-bench put --uri file:///tmp/test/ --object-size 1024 --objects 50 --concurrency 5
```

### Operation Logging (op-log)
```bash
# Create operation log during workload (always zstd compressed)
./target/release/io-bench -vv --op-log /tmp/operations.tsv.zst run --config tests/configs/file_test.yaml

# Decompress to view
zstd -d /tmp/operations.tsv.zst -o /tmp/operations.tsv
head -20 /tmp/operations.tsv

# Op-log format: TSV with columns
# idx, thread, op, client_id, n_objects, bytes, endpoint, file, error, start, first_byte, end, duration_ns
```

## Code Conventions & Gotchas

### Workload Execution Pattern (`src/workload.rs`)
- Pre-resolution: GET operations fetch object lists **once** before workload starts
- Weighted selection via `rand_distr::WeightedIndex` from config weights
- Semaphore-controlled concurrency: `Arc<Semaphore::new(cfg.concurrency)>`

### Backend Detection Logic
`BackendType::from_uri()` in `src/workload.rs` determines storage backend:
```rust
// "s3://" -> S3, "file://" -> File, "direct://" -> DirectIO, "az://" -> Azure
// Default fallback is File for unrecognized schemes
```

### gRPC Protocol Buffer Build
- `build.rs` generates `src/pb/iobench.rs` from `proto/iobench.proto`
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
- **Azure backend**: 2-3 ops/s with ~700ms latency validated
- **Concurrency**: Configurable via YAML `concurrency` field
- **Logging**: `-v` operational info, `-vv` detailed tracing

When extending functionality, always maintain ObjectStore abstraction and ensure new features work across all backend types (`file://`, `direct://`, `s3://`, `az://`).