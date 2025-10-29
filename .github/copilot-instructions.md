# sai3-bench AI Agent Guide

## Project Overview
sai3-bench is a comprehensive multi-protocol I/O benchmarking suite with unified multi-backend support (`file://`, `direct://`, `s3://`, `az://`, `gs://`) using the `s3dlio` library. It provides both single-node CLI and distributed gRPC execution with HDR histogram metrics and professional progress bars.

**Current Version**: v0.6.11 (October 2025) - SSH-Automated Distributed Testing & Config-Driven Agents

**v0.6.11 Key Features**:
- SSH automation for zero-touch distributed deployment
- Config-driven agent specification (no CLI flags needed)
- Flexible container runtime (Docker/Podman) via YAML
- Per-agent customization (target override, concurrency, env vars)
- GCP automation scripts with full lifecycle management

**v0.6.10 Critical Findings**: Pre-stat and RangeEngine optimizations provide **NO performance benefit** for same-region, high-bandwidth cloud storage scenarios. Pre-stat now gated behind `range_engine.enabled` flag to avoid 250ms overhead. RangeEngine is 35% SLOWER than single-stream downloads when network-bound.

## Architecture: Three Binary Strategy
- **`sai3-bench`** (`src/main.rs`) - Single-node CLI with subcommands: `run`, `replay`, `util`
- **`sai3bench-agent`** (`src/bin/agent.rs`) - gRPC server node for distributed loads
- **`sai3bench-ctl`** (`src/bin/controller.rs`) - Coordinator for multi-agent execution

### Removed Binaries (v0.6.9+)
- **`sai3bench-run`** - Legacy standalone runner, replaced by `sai3-bench run` subcommand (more features)
- **`fs_read_bench`** - Internal development tool for buffer pool testing (not needed for production)

Both removed for clarity: users should use `sai3-bench` with appropriate subcommands (`run`, `replay`, `util`).

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
**Key**: Currently using s3dlio v0.9.10 via local path dependency `../s3dlio`.

### Pre-stat Optimization Gating (v0.6.10)
Pre-stat (batch HEAD requests to populate size cache) is **gated behind RangeEngine flag** due to performance findings:
```rust
// Only runs when RangeEngine is enabled
let should_prestat = (uri.starts_with("s3://") || uri.starts_with("gs://") || 
                      uri.starts_with("az://")) && cfg.range_engine.enabled;

if should_prestat {
    info!("Pre-stating {} objects to populate size cache", objects.len());
    store.pre_stat_and_cache(&objects).await?;
}
```

**Why gated**: Testing showed <1% performance difference for same-region scenarios, but pre-stat adds ~250ms startup overhead. Only enable when RangeEngine provides actual benefit (cross-region, low-bandwidth).

### RangeEngine Performance Considerations (v0.6.10)
**Default**: RangeEngine is **DISABLED** by default in s3dlio v0.9.6+ for optimal performance.

**Key Findings from v0.6.10 testing**:
- Parallel chunk downloads are 35% SLOWER than single-stream for same-region cloud storage
- Coordination overhead > parallelism benefit when network bandwidth is saturated (~2.6 GB/s)
- CPU utilization: 100% baseline vs 80% RangeEngine

**When to enable RangeEngine**:
- Cross-region transfers with high latency
- Low-bandwidth network links (<100 Mbps)
- Scenarios where parallel chunk downloads overcome coordination costs

**Configuration** (disabled by default):
```yaml
range_engine:
  enabled: false  # Keep disabled for same-region workloads (RECOMMENDED)
  min_size_bytes: 16777216  # Only for files >= 16 MiB
  chunk_size_bytes: 8388608  # 8 MiB chunks
  max_workers: 8
```

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
- `OpSpec` enum: `Get { path }`, `Put { path, size_spec }`, `List`, `Stat`, `Delete`
- **v0.5.3+**: `WeightedOp` has optional `concurrency` field for per-operation concurrency
- Example from `tests/configs/file_test.yaml`:
```yaml
target: "file:///tmp/sai3bench-test/"
workload:
  - op: get
    path: "data/*"  # Resolves to file:///tmp/sai3bench-test/data/*
    weight: 70
    concurrency: 64  # Optional per-op override
```

### Size Generator System (`src/size_generator.rs`) - v0.5.3+
**Purpose**: Realistic object size distributions for PUT operations and prepare steps.

Three distribution types:
1. **Fixed**: `SizeSpec::Fixed(u64)` - Backward compatible with old `object_size` field
2. **Uniform**: Evenly distributed sizes between min and max
3. **Lognormal**: Realistic distribution (many small, few large) - recommended

Usage pattern:
```rust
use crate::size_generator::{SizeGenerator, SizeSpec};

// Create generator from config
let (base_uri, size_spec) = cfg.get_put_size_spec(op);
let size_generator = SizeGenerator::new(&size_spec)?;

// Generate object size
let size = size_generator.generate();  // Returns u64
```

YAML syntax:
```yaml
# Old syntax (still supported)
- op: put
  path: "data/"
  object_size: 1048576

# New syntax (v0.5.3+)
- op: put
  path: "data/"
  size_distribution:
    type: lognormal
    mean: 1048576
    std_dev: 524288
    min: 1024
    max: 10485760
```

**Key implementation details**:
- Uses `rand_distr` crate for statistical distributions
- Lognormal uses rejection sampling to respect min/max bounds
- `SizeGenerator::description()` provides human-readable description for logging
- Comprehensive unit tests in module cover all distribution types

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
./target/release/sai3-bench -v run --config tests/configs/file_test.yaml

# S3 backend (requires .env with AWS_*)
./target/release/sai3-bench -vv run --config tests/configs/mixed.yaml

# Azure backend (requires AZURE_STORAGE_ACCOUNT and AZURE_STORAGE_ACCOUNT_KEY)
./target/release/sai3-bench util health --uri "az://storage-account/container/"

# GCS backend (requires GOOGLE_APPLICATION_CREDENTIALS)
./target/release/sai3-bench util health --uri "gs://bucket/"

# Direct I/O backend (no credentials, Linux only)
./target/release/sai3-bench -v run --config tests/configs/direct_io_chunked_test.yaml
```

### Distributed Mode Testing
```bash
# Terminal 1: Start agent
./target/release/sai3bench-agent --listen 127.0.0.1:7761

# Terminal 2: Test connectivity
./target/release/sai3bench-ctl --insecure --agents 127.0.0.1:7761 ping

# Terminal 2: Run distributed workload (v0.6.0+)
./target/release/sai3bench-ctl --insecure --agents 127.0.0.1:7761,127.0.0.1:7762 \
    run --config tests/configs/distributed_mixed_test.yaml --start-delay 2
```

### Progress Bar Examples
```bash
# Timed workload with progress bar
./target/release/sai3-bench run --config tests/configs/file_test.yaml

# Operation-based progress (GET/PUT/DELETE)
./target/release/sai3-bench get --uri file:///tmp/test/data/* --jobs 4
./target/release/sai3-bench put --uri file:///tmp/test/ --object-size 1024 --objects 50 --concurrency 5
```

### Operation Logging (op-log)
```bash
# Create operation log during workload (always zstd compressed)
./target/release/sai3-bench -vv --op-log /tmp/operations.tsv.zst run --config tests/configs/file_test.yaml

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
- **v0.6.10**: Cloud storage (s3://, gs://, az://) uses `get_optimized()` for cache-aware reads
- **v0.6.9**: Direct I/O uses intelligent chunked reads for files >8 MiB (173x faster)

### Backend Detection Logic
`BackendType::from_uri()` in `src/workload.rs` determines storage backend:
```rust
// "s3://" -> S3, "file://" -> File, "direct://" -> DirectIO, 
// "az://" -> Azure, "gs://" -> GCS
// Default fallback is File for unrecognized schemes
```

**Special handling**:
- **direct:// URIs**: Automatic chunked reads for files >8 MiB (4 MiB blocks)
- **Cloud URIs** (s3://, gs://, az://): Use `get_optimized()` method for cache-aware operations

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

## Distributed Workload Execution (v0.6.0+)

### Architecture
- **Controller** (`sai3bench-ctl`): Orchestrates multi-agent workloads via gRPC
- **Agents** (`sai3bench-agent`): Execute workloads independently on their nodes
- **Coordination**: Synchronized start time, per-agent path isolation
- **Results**: HDR histogram merging (v0.6.4) for accurate aggregate metrics

### Key Features
- **Automatic storage mode detection**: Shared (s3://, gs://, az://) vs Local (file://, direct://)
- **Per-agent path isolation**: Each agent operates in `agent-{id}/` subdirectory
- **Coordinated start**: All agents begin workload simultaneously (configurable delay)
- **Result aggregation**: Per-agent + consolidated results with proper histogram merging

### Usage Pattern
```bash
# Start multiple agents
sai3bench-agent --listen 0.0.0.0:7761  # On host 1
sai3bench-agent --listen 0.0.0.0:7761  # On host 2

# Run coordinated workload
sai3bench-ctl --insecure --agents host1:7761,host2:7761 \
    run --config workload.yaml --start-delay 2
```

### Critical Implementation Details
- **HDR Histogram Merging** (v0.6.4): Percentiles cannot be averaged - use proper histogram merging via `hdrhistogram` library for mathematically accurate aggregate metrics
- **Shared storage**: Single prepare phase, all agents read same dataset
- **Local storage**: Each agent prepares independent isolated dataset
- **Path template**: Default `agent-{id}/` customizable via `--path-template`

## Results Directory Structure (v0.6.4+)

### Automatic Results Capture
Every workload execution creates timestamped results directory: `sai3-YYYYMMDD-HHMM-{test_name}/`

### Directory Contents
```
sai3-YYYYMMDD-HHMM-{test_name}/
├── config.yaml          # Complete workload configuration
├── console.log          # Full execution log
├── metadata.json        # Test metadata (distributed: true/false)
├── results.tsv          # Single-node OR consolidated aggregate (merged histograms)
└── agents/              # Only in distributed mode
    ├── agent-1/
    │   ├── config.yaml  # Agent's modified config (with path prefix)
    │   ├── console.log  # Agent's execution log
    │   └── results.tsv  # Per-agent results
    └── agent-2/...
```

### Key Points
- **Consolidated TSV** (distributed): Contains mathematically accurate merged histogram percentiles, NOT simple averages
- **Per-agent TSVs**: Preserved for debugging and per-node analysis
- **Automatic creation**: No manual setup required

## Performance Characteristics
- **File backend**: 25k+ ops/s with 1ms latency validated
- **Azure backend**: 2-3 ops/s with ~700ms latency validated
- **GCS same-region**: ~2.6 GB/s throughput (network-bound)
- **Pre-stat overhead**: ~250ms for 1000 objects
- **Direct I/O**: 173x faster with chunked reads (v0.6.9) - 1.73 GiB/s for large files
- **Concurrency**: Configurable via YAML `concurrency` field
- **Logging**: `-v` operational info, `-vv` detailed tracing

When extending functionality, always maintain ObjectStore abstraction and ensure new features work across all backend types (`file://`, `direct://`, `s3://`, `az://`, `gs://`).