# sai3-bench Project Handoff

**Date**: December 9, 2025  
**Purpose**: Complete context for AI agents working on sai3-bench  
**Current State**: Feature branch ready for review/merge

---

## Quick Context (Read This First)

**sai3-bench** is a multi-protocol I/O benchmarking suite supporting S3, Azure Blob, GCS, and filesystem backends via the `s3dlio` library.

**Current Version**: v0.8.11  
**Current Branch**: `feature/azure-gcs-custom-endpoints` (ahead of main by 5 commits)  
**Working Tree**: Clean - all changes committed and pushed  
**Build Status**: Compiles with zero warnings, all tests passing

### What's Pending

The `feature/azure-gcs-custom-endpoints` branch contains v0.8.11 work that is **ready to merge to main**:

| Commit | Description |
|--------|-------------|
| `049f745` | Version bump to v0.8.11 |
| `e603c6d` | docs: add YAML test config guidelines to copilot-instructions |
| `485b672` | style: fix all clippy warnings |
| `935ef25` | feat: Azure/GCS custom endpoint support and test infrastructure |
| `60447d9` | fix: distributed abort handling and agent progress bars |

**To merge this branch**:
```bash
git checkout main
git merge feature/azure-gcs-custom-endpoints
git push origin main
git branch -d feature/azure-gcs-custom-endpoints
git push origin --delete feature/azure-gcs-custom-endpoints
```

---

## Project Overview

### Architecture

```
sai3-bench (Single-node CLI)
    ↓ subcommands: run, replay, util, sort
    ↓
sai3bench-agent (gRPC server on each load-gen host)
    ↓ bidirectional streaming: ExecuteWorkload
    ↓
sai3bench-ctl (Coordinator for distributed execution)
    ↓
s3dlio (Multi-protocol ObjectStore: s3://, az://, gs://, file://, direct://)
```

### Key Binaries

| Binary | Purpose | Entry Point |
|--------|---------|-------------|
| `sai3-bench` | Single-node CLI with subcommands | `src/main.rs` |
| `sai3bench-agent` | gRPC agent (distributed node) | `src/bin/agent.rs` |
| `sai3bench-ctl` | Controller (orchestrates agents) | `src/bin/controller.rs` |

### Dependencies

- **s3dlio v0.9.24** (via git tag) - Multi-protocol storage
- **tonic 0.13** - gRPC with bidirectional streaming
- **hdrhistogram 7.5** - Latency percentile tracking
- **indicatif 0.17** - Progress bars

---

## v0.8.11 Changes (Current Branch)

### 1. Agent Progress Bars
- **Prepare phase**: Progress bar showing object count and rate
- **Workload phase**: Spinner with GET/PUT operation stats
- Proper cleanup on completion, error, disconnect, or abort

### 2. Azure/GCS Custom Endpoint Support
- New test modules: `azure_tests.rs`, `gcs_tests.rs`, `file_tests.rs`, `s3_tests.rs`
- Environment-driven endpoint configuration for private clouds
- RangeEngine enable/disable testing for both backends

### 3. Distributed Abort Handling
- Wrapped all execution phases in `tokio::select!` with abort check
- Stats writer task checks abort channel and exits cleanly
- Fixed SIGINT handler panic (double-await bug)

### 4. Legacy gRPC Removal (~824 lines)
- Removed: `RunPrepare`, `RunWorkload`, `GetPrepareStats`, `GetWorkloadStats`, `Cleanup`, `Abort`
- Single bidirectional stream `ExecuteWorkload` now handles all functionality

### 5. Zero Clippy Warnings
- Fixed needless_range_loop, empty_line_after_doc_comments, collapsible_match
- Fixed bool_assert_comparison in tests
- Added `#[allow(clippy::too_many_arguments)]` where appropriate

---

## Key Files and Modules

### Core Source (`src/`)

| File | Purpose |
|------|---------|
| `main.rs` | CLI with subcommands: run, replay, util, sort |
| `bin/agent.rs` | gRPC agent with state machine (Idle→Ready→Running) |
| `bin/controller.rs` | Coordinator with bidirectional streaming |
| `config.rs` | YAML config parsing (PrepareConfig, WorkloadConfig, CleanupMode) |
| `constants.rs` | **Check here first** - all timeouts, defaults, thresholds |
| `workload.rs` | Workload execution engine with weighted operations |
| `prepare.rs` | Object preparation with parallel/sequential strategies |
| `cleanup.rs` | Cleanup module with `list_existing_objects()` |
| `live_stats.rs` | Thread-safe LiveStatsTracker for real-time metrics |
| `rate_controller.rs` | I/O rate limiting (deterministic, uniform, exponential) |

### Tests (`tests/`)

| File | Coverage |
|------|----------|
| `azure_tests.rs` | Azure Blob custom endpoints (NEW in v0.8.11) |
| `gcs_tests.rs` | GCS custom endpoints (NEW in v0.8.11) |
| `s3_tests.rs` | S3 endpoint testing (NEW in v0.8.11) |
| `file_tests.rs` | Filesystem backend tests (NEW in v0.8.11) |
| `distributed_config_tests.rs` | Distributed mode config parsing |
| `directory_tree_creation_tests.rs` | Tree structure validation |

### Test Configs (`tests/configs/`)

| Directory/File | Purpose |
|----------------|---------|
| `file_test.yaml` | Quick filesystem test |
| `directory-tree/*.yaml` | Tree structure tests (4 configs) |
| `replay_backpressure.yaml` | Replay backpressure config |

---

## Critical Development Guidelines

### 1. Build Commands - NEVER Pipe Output

```bash
# CORRECT:
cargo build --release
cargo test

# WRONG - user needs to see ALL output:
cargo build --release 2>&1 | head -100
```

### 2. Debug with Verbose Flags

```bash
# CORRECT - flags BEFORE subcommand:
./target/release/sai3-bench -vv run --config test.yaml

# WRONG - flags after subcommand won't work:
./target/release/sai3-bench run --config test.yaml -vv
```

### 3. YAML Configs - NEVER Create from Scratch

```bash
# CORRECT: Copy existing config, modify, validate
cp tests/configs/file_test.yaml /tmp/my_test.yaml
# Edit /tmp/my_test.yaml
./target/release/sai3-bench run --config /tmp/my_test.yaml --dry-run

# WRONG: Creating YAML from memory (will have errors)
```

### 4. Starting Test Agents

```bash
# CORRECT - use the script:
./scripts/start_local_agents.sh 2 7761 "-vv"

# Agent logs: /tmp/agent1.log, /tmp/agent2.log
```

### 5. Test Directories

| Directory | Use For |
|-----------|---------|
| `/mnt/test` | Performance testing (real I/O) |
| `/tmp` | Quick validation only (may be tmpfs) |

---

## Common Workflows

### Build and Test

```bash
cd /home/eval/Documents/Code/sai3-bench

# Build
cargo build --release

# Run all tests
cargo test

# Run specific test
cargo test azure_tests

# Clippy check
cargo clippy
```

### Run Single-Node Benchmark

```bash
# Quick filesystem test
./target/release/sai3-bench -v run --config tests/configs/file_test.yaml

# Dry run (validate config)
./target/release/sai3-bench run --config tests/configs/file_test.yaml --dry-run
```

### Run Distributed Benchmark

```bash
# Terminal 1: Start agents
./scripts/start_local_agents.sh 2 7761 "-vv"

# Terminal 2: Run controller
./target/release/sai3bench-ctl run --config tests/configs/distributed_test.yaml

# Cleanup
pkill -9 sai3bench-agent
```

### Merge Feature Branch

```bash
# Merge v0.8.11 to main
git checkout main
git merge feature/azure-gcs-custom-endpoints
git push origin main

# Cleanup
git branch -d feature/azure-gcs-custom-endpoints
git push origin --delete feature/azure-gcs-custom-endpoints
```

---

## Version History (Recent)

| Version | Date | Key Changes |
|---------|------|-------------|
| v0.8.11 | 2025-12-05 | Agent progress bars, Azure/GCS endpoints, abort fix |
| v0.8.10 | 2025-12-02 | Replay backpressure, YAML replay config |
| v0.8.9 | 2025-12-01 | Stage tracking, store efficiency, config fixes |
| v0.8.8 | 2025-11-30 | Logging improvements, agent timeout fix |
| v0.8.7 | 2025-11-29 | Distributed cleanup with dedicated module |

---

## s3dlio Dependency

Currently using **s3dlio v0.9.24** via git tag:

```toml
s3dlio = { git = "https://github.com/russfellows/s3dlio.git", tag = "v0.9.24" }
```

For local development with unreleased s3dlio changes:

```toml
s3dlio = { path = "../s3dlio" }
```

---

## Known Patterns

### ObjectStore Usage

Always use `s3dlio::object_store::store_for_uri()`:

```rust
pub fn create_store_for_uri(uri: &str) -> anyhow::Result<Box<dyn ObjectStore>> {
    store_for_uri(uri).context("Failed to create object store")
}
```

### RangeEngine (Default: DISABLED)

Keep disabled for same-region workloads - parallel chunks are 35% slower due to coordination overhead:

```yaml
range_engine:
  enabled: false  # RECOMMENDED for same-region
```

### Size Distributions

Three types: Fixed, Uniform, Lognormal (recommended for realistic workloads):

```yaml
size_distribution:
  type: lognormal
  mean: 1048576
  std_dev: 524288
  min: 1024
  max: 10485760
```

---

## Next Steps

### Immediate

1. **Merge v0.8.11** - Branch is ready, just needs PR merge
2. **Tag release** - `git tag -a v0.8.11 -m "Release v0.8.11"`

### Future Work

- Live stats streaming improvements
- Additional backend testing (DAOS planned in warpio)
- Performance regression testing automation

---

## Quick Reference

### Commands

```bash
# Build
cargo build --release

# Test
cargo test

# Single-node run
./target/release/sai3-bench -v run --config <config>

# Start agents
./scripts/start_local_agents.sh 2 7761 "-vv"

# Controller run
./target/release/sai3bench-ctl run --config <config>

# Kill agents
pkill -9 sai3bench-agent
```

### Key Files

```
src/constants.rs         - All magic numbers (check first!)
src/config.rs            - YAML parsing
src/bin/agent.rs         - gRPC agent
src/bin/controller.rs    - Coordinator
tests/configs/           - Test configurations
docs/CHANGELOG.md        - Version history
```

---

**Document version**: 1.0  
**Created**: December 9, 2025  
**For**: New AI agent session on sai3-bench project
