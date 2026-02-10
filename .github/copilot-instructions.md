# sai3-bench AI Agent Guide

## Critical Testing Philosophy

**⚠️ CRITICAL PRINCIPLE**: Tests MUST verify intended behavior, NOT implementation details.

- ✅ **CORRECT**: Write tests that check what the code SHOULD do (specification/requirements)
- ❌ **WRONG**: Write tests that check what the code currently DOES (implementation bugs)

**When tests fail**:
1. **First**: Assume the test is correct (it reflects requirements)
2. **Second**: Fix the code to match the intended behavior
3. **NEVER**: Change the test to match buggy implementation

**Example**:
```rust
// CORRECT - Test verifies intended behavior:
// "AllOrNothing barrier should FAIL if ANY agent fails"
#[test]
fn test_all_or_nothing_with_failure() {
    // Setup: 2 agents, 1 ready, 1 failed
    assert_eq!(status, BarrierStatus::Failed);  // Intended: any failure = abort
}

// WRONG - Test verifies buggy implementation:
// "AllOrNothing returns Ready if all ALIVE agents ready"
#[test]
fn test_all_or_nothing_with_failure() {
    // Setup: 2 agents, 1 ready, 1 failed
    assert_eq!(status, BarrierStatus::Ready);   // Bug: ignores failed agents!
}
```

Tests that verify buggy behavior are worse than no tests - they legitimize bugs and prevent fixes.

## Compiler Warnings Are Critical Diagnostics

**⚠️ NEVER IGNORE RUST COMPILER WARNINGS**: The Rust compiler is your debugging partner, not a nuisance.

**Cardinal Rules**:
1. **NEVER** suppress warnings with `#[allow(dead_code)]`, `#[allow(unused_variables)]`, or underscore prefixes (`_unused`)
2. **ALWAYS** investigate the root cause - warnings indicate LOGIC ERRORS, not style issues
3. **FIX THE LOGIC**, don't silence the messenger

**Common Warning Categories and Real Bugs They Reveal**:

- **`unused variable 'x'`** → Variable is checked but result ignored (incomplete implementation)
- **`field 'y' is never read`** → Data stored but never used (design flaw or missing feature)
- **`function 'foo' is never used`** → Implemented but no caller exists (missing auto-recovery trigger)
- **`value assigned is never read`** → Logic error (assignments before overwrites in tests)
- **`variant 'Bar' is never constructed`** → Incomplete test coverage or dead enum variant

**Real Example from v0.8.60** (Fixed February 2026):
```rust
// WARNING: unused variable `bm` (barrier_manager)
if let Some(ref mut bm) = barrier_manager {
    // Comment: "barrier will be released by stats processing loop"
    // NO ACTUAL WAIT CALL - dead code!
}
// ROOT CAUSE: Race condition - validation barrier timeout bug
// FIX: Remove dead extraction, implement actual wait or remove block entirely
```

**8 Warnings Revealed 8 Critical Bugs**:
1. Incomplete cache integration (TODO not finished)
2. Unused diagnostic field (should log cache location)
3. Dead validation barrier wait (race condition causing timeouts)
4. Missing auto-recovery (reset_barrier_state implemented but never called)
5-7. Test logic errors (assignments immediately overwritten)
8. Test coverage gap (Idle variant never constructed)

**Action Protocol**:
1. See warning → **STOP** and investigate immediately
2. Read the code context around the warning
3. Understand the INTENDED behavior vs ACTUAL behavior
4. Fix the logic bug, not the warning
5. Verify fix with tests
6. Learn from the mistake

Compiler warnings are free bug reports. Ignoring them is technical debt accumulation.

## YAML Configuration Files - NEVER Create From Scratch

**⚠️ CRITICAL RULE**: You WILL ALWAYS get YAML syntax wrong if you create configs from scratch.

**Cardinal Rules**:
1. **ALWAYS** find an existing YAML config file that is similar to what you need
2. **COPY** that existing config file (never modify the original)
3. **MODIFY** only the absolute minimum necessary fields
4. **VALIDATE** with `--dry-run` BEFORE attempting to run
5. **NEVER** assume you know the correct YAML syntax - YOU ARE ALWAYS WRONG

**Correct Workflow**:
```bash
# 1. Find existing similar config
ls tests/configs/
ls tests/configs/directory-tree/
find . -name "*.yaml" -type f | head -20

# 2. Copy existing config
cp tests/configs/directory-tree/tree_test_basic.yaml /tmp/my_test.yaml

# 3. Edit ONLY what's necessary (use replace_string_in_file)
# Modify specific fields, keep everything else unchanged

# 4. ALWAYS validate before running
./target/release/sai3-bench run --config /tmp/my_test.yaml --dry-run

# 5. Only run after dry-run passes
./target/release/sai3-bench run --config /tmp/my_test.yaml
```

**Why This Rule Exists**:
- YAML syntax is context-dependent (indentation, required vs optional fields)
- Config schema evolves (fields added/removed/renamed across versions)
- Some fields are mutually exclusive or have complex dependencies
- Missing required fields cause cryptic parsing errors
- Wrong field types cause runtime failures instead of config errors

**Examples of Past Mistakes**:
- Missing `workload` field in stages (required even when empty)
- Wrong indentation (YAML interprets as different structure)
- Using old field names that were renamed in newer versions
- Missing required fields like `distributed.agents` or `stages[].barrier`
- Incorrect nesting of `prepare` vs `workload` sections

**When You Need a Config**:
1. Search existing configs: `find . -name "*.yaml" | xargs grep -l "similar_keyword"`
2. Read the found config completely
3. Copy it to /tmp/ with descriptive name
4. Make minimal targeted edits
5. Run --dry-run to validate
6. Only proceed if dry-run passes

**DO NOT**:
- ❌ Create YAML from memory or examples in documentation
- ❌ Guess at field names or structure
- ❌ Skip --dry-run validation
- ❌ Modify example configs in-place (always copy to /tmp/)

This is not a suggestion - this is a MANDATORY workflow. Every time you create YAML from scratch, you WILL waste time debugging syntax errors.

## Config Converter: Legacy to Explicit Stages (v0.8.61+)

**PURPOSE**: Automatically convert old implicit-stage YAML configs to new explicit-stage format.

**Background**: Prior to v0.8.61, sai3-bench configs had implicit stages (preflight → prepare → workload → cleanup) that executed automatically in that order. Starting in v0.8.61, ALL configs require explicit `distributed.stages` sections that define the execution order, type, and completion criteria for each stage.

**What Gets Converted**:
- Old configs: Top-level `duration`, `concurrency`, `workload` with optional `prepare`
- New configs: `distributed.stages` array with explicit stage definitions
- Works for BOTH standalone (single-node) and distributed (multi-agent) configs

**Conversion Rules**:
1. **Preflight stage** (always created): Type=validation, order=1, timeout=300s
2. **Prepare stage** (if prepare section exists): Type=prepare, order=2, completion=tasks_done
3. **Execute stage** (always created): Type=execute, order=next, duration from top-level
4. **Cleanup stage** (if prepare.cleanup=true): Type=cleanup, order=last, completion=tasks_done

**Usage Examples**:
```bash
# Convert single file with dry-run (recommended first step)
./target/release/sai3-bench convert --config tests/configs/mixed.yaml --dry-run

# Convert single file (creates .yaml.bak backup)
./target/release/sai3-bench convert --config tests/configs/mixed.yaml

# Convert multiple files at once
./target/release/sai3-bench convert --files tests/configs/*.yaml --dry-run

# Convert without validation (faster but risky)
./target/release/sai3-bench convert --config mixed.yaml --no-validate

# Convert and validate converted config works
./target/release/sai3-bench convert --config mixed.yaml
./target/release/sai3-bench run --config mixed.yaml --dry-run
```

**Conversion Output**:
- Original file: Backed up to `<filename>.yaml.bak`
- Converted file: Replaces original with explicit stages
- New distributed section created if needed (for standalone configs)
- Empty agents list for standalone configs

**Example Conversion**:
```yaml
# OLD FORMAT (implicit stages):
duration: 30s
concurrency: 32
target: "s3://bucket/test/"
workload:
  - op: get
    path: "data/*"
    weight: 100

# NEW FORMAT (explicit stages):
duration: 30s
concurrency: 32
target: "s3://bucket/test/"
workload:
  - op: get
    path: "data/*"
    weight: 100
distributed:
  agents: []  # Empty for standalone configs
  stages:
    - name: preflight
      order: 1
      type: validation
      completion: validation_passed
      timeout_secs: 300
    - name: execute
      order: 2
      type: execute
      completion: duration
      duration: 30s
```

**Validation**:
- Converter uses `validation::display_config_summary()` to validate converted configs
- Use `--no-validate` to skip validation (faster but risky)
- Always test converted configs with `--dry-run` before running workloads

**Skipped Files**:
- Configs that already have `distributed.stages` (detected automatically)
- Configs without `workload` section (invalid configs)
- Non-YAML files

**IMPORTANT**: The converter creates minimal distributed configs for standalone mode with empty agents list. This is correct - stages are required even for single-node execution.

## Critical Build and Debug Instructions

**IMPORTANT**: When running `cargo build` or `cargo test`, do NOT pipe output to `head` or `tail`. The user needs to see the ENTIRE output, including all warnings and errors. Run build commands without filtering:
```bash
# CORRECT:
cargo build --release
cargo test

# WRONG:
cargo build --release 2>&1 | head -100
cargo build --release 2>&1 | tail -50
```

**DEBUGGING RULE**: When troubleshooting issues or investigating unexpected behavior, **ALWAYS** use the verbose flags **BEFORE** other CLI options:
```bash
# CORRECT - verbose flags MUST come BEFORE subcommands:
./target/release/sai3-bench -v run --config <config>     # Basic verbosity
./target/release/sai3-bench -vv run --config <config>    # More verbose (RECOMMENDED)
./target/release/sai3-bench -vvv run --config <config>   # Maximum verbosity

# WRONG - verbose flags after subcommand won't work:
./target/release/sai3-bench run --config <config> -vv    # ERROR: unexpected argument

# NEVER use RUST_LOG - use the built-in -v/-vv/-vvv flags instead
```

The `-v`, `-vv`, and `-vvv` flags provide essential diagnostic output showing:
- Object store initialization and backend detection
- Directory creation operations
- File operations and paths
- Error details and stack traces
- Timing information

**Use `-vv` as the default for all debugging sessions. Remember: flags BEFORE subcommands!**

## Project Overview
sai3-bench is a comprehensive multi-protocol I/O benchmarking suite with unified multi-backend support (`file://`, `direct://`, `s3://`, `az://`, `gs://`) using the `s3dlio` library. It provides both single-node CLI and distributed gRPC execution with HDR histogram metrics and professional progress bars.

**Current Version**: v0.8.16 (December 2025) - Per-Agent Perf Logs and Warmup Reset

**v0.8.16 Key Features**:
- Per-agent perf_log.tsv files in agents/{agent-id}/ subdirectories
- Synchronized perf_log timing (all logs write at same interval tick)
- Warmup timer reset when workload phase starts (not from prepare)
- PerfLogConfig.path field now optional
- Renamed console.log to console_log.txt

**v0.8.15 Key Features**:
- Performance logging (perf-log) module with 28-column TSV format
- Extended LiveStatsSnapshot percentiles (p50, p90, p95, p99)
- Warmup period configuration and is_warmup flagging

**v0.8.14 Features**:
- Distributed listing stage with progress updates
- Robust error handling for LIST operations with ListingErrorTracker

## Module Architecture (v0.8.16)

### Core Source Modules (`src/`)
- **`main.rs`** - Single-node CLI with subcommands: `run`, `replay`, `util`, `sort`, `convert`
- **`bin/agent.rs`** - gRPC agent with bidirectional streaming (state machine: Idle→Ready→Running)
- **`bin/controller.rs`** - Coordinator with bidirectional streaming (orchestrates multi-agent workloads)
- **`config.rs`** - YAML config parsing with PrepareConfig, WorkloadConfig, CleanupMode, StageConfig
- **`config_converter.rs`** - Legacy YAML to explicit stages converter (v0.8.61+)
- **`constants.rs`** - Centralized constants (timeouts, defaults, thresholds) - **check here first**
- **`workload.rs`** - Workload execution engine with weighted operations
- **`prepare.rs`** - Object preparation with parallel/sequential strategies
- **`cleanup.rs`** - Dedicated cleanup module with `list_existing_objects()` and `cleanup_prepared_objects()`
- **`live_stats.rs`** - Thread-safe LiveStatsTracker for real-time metrics streaming
- **`rate_controller.rs`** - I/O rate limiting (deterministic, uniform, exponential)
- **`validation.rs`** - Config validation with detailed error messages
- **`size_generator.rs`** - Size distributions (Fixed, Uniform, Lognormal)
- **`directory_tree.rs`** - Directory tree generation (width/depth model)
- **`multiprocess.rs`** - Local multi-process scaling
- **`cpu_monitor.rs`** - CPU utilization monitoring during workloads

### Key Constants (`src/constants.rs`)
All magic numbers are centralized here - **always check this file first** when tuning:
```rust
// Error handling
DEFAULT_MAX_ERRORS: u64 = 100
DEFAULT_ERROR_RATE_THRESHOLD: f64 = 5.0
DEFAULT_BACKOFF_DURATION: Duration = 5s

// Workload defaults  
DEFAULT_DURATION_SECS: u64 = 60
DEFAULT_CONCURRENCY: usize = 16

// Direct I/O optimization
DIRECT_IO_CHUNK_SIZE: usize = 4 MiB  // 1.73 GiB/s vs 0.01 GiB/s
CHUNKED_READ_THRESHOLD: u64 = 8 MiB

// gRPC timeouts (for distributed mode)
AGENT_READY_TIMEOUT: Duration = 40s
COORDINATED_START_TIMEOUT: Duration = 30s
STATS_INTERVAL: Duration = 1s
```

## Testing Requirements (CRITICAL - v0.8.0+)

### Starting Local Test Agents

**CRITICAL**: Use the `start_local_agents.sh` script to start agents for testing:

```bash
# Start 2 agents with verbose logging (RECOMMENDED for debugging):
./scripts/start_local_agents.sh 2 7761 "-vv"

# Parameters:
# 1. NUM_AGENTS (default: 2)
# 2. BASE_PORT (default: 7761) 
# 3. VERBOSE flag (default: "-v", use "-vv" for debugging)
# 4. LOG_DIR (default: "/tmp")
```

**Agent logs**: Written to `/tmp/agent1.log`, `/tmp/agent2.log`, etc.

**DO NOT** start agents manually with individual commands - ALWAYS use the script!

### YAML Test Configurations - NEVER Create From Scratch

**CRITICAL**: NEVER create YAML test configs from scratch. You WILL guess parameters incorrectly.

**Correct approach**:
1. Find an existing test config that is close to what you need
2. Make a COPY of that config (never modify the original)
3. Modify ONLY what is strictly necessary
4. ALWAYS validate with `--dry-run` before running

```bash
# Example: Copy existing config, modify, validate
cp tests/configs/directory-tree/tree_test_basic.yaml /tmp/my_test.yaml
# Edit /tmp/my_test.yaml as needed
./target/release/sai3-bench run --config /tmp/my_test.yaml --dry-run

# Find available test configs:
ls tests/configs/
ls tests/configs/directory-tree/
```

### Test Directories and Storage Configuration

**Primary Test Directory**: `/mnt/test` (dedicated device and mount point)
- Use for realistic testing with actual I/O characteristics
- Avoids memory caching effects from `/tmp` (may be tmpfs)
- Provides consistent performance baseline
- Recommended for all performance testing and benchmarking

**Secondary Test Directory**: `/tmp` (temporary filesystem)
- Use for quick, small tests only
- May be memory-backed (tmpfs), giving unrealistic performance
- Useful for rapid iteration and unit testing
- NOT suitable for performance validation

**Example Usage**:
```bash
# Realistic performance test
./target/release/sai3-bench run --config tests/configs/directory-tree/tree_test_lognormal.yaml
# (Uses target: "file:///mnt/test/...")

# Quick validation test
./target/release/sai3-bench run --config tests/configs/file_test.yaml
# (Uses target: "file:///tmp/...")
```

### Directory Tree Test Configurations (v0.7.0)

**Location**: `tests/configs/directory-tree/` - Comprehensive test suite with 4 configs

**Available Tests**:
1. **tree_test_basic.yaml**: Basic functionality (3×2 tree, 45 files, uniform 1-4KB)
2. **tree_test_fixed_size.yaml**: Fixed sizes (2×2 tree, 12 files, fixed 8KB)
3. **tree_test_lognormal.yaml**: Realistic workload (4×3 tree, 840 files, lognormal 1KB-10MB)
4. **tree_test_bottom.yaml**: Deep tree validation (3×4 tree, 405 files, strict bottom-only)

**Key Features**:
- Distribution strategies: `bottom` (leaf-only) or `all` (every level)
- Size distributions: Fixed, Uniform, Lognormal
- Global file indexing with unique file names
- Comprehensive documentation in `tests/configs/directory-tree/README.md`

**Usage**:
```bash
# Run prepare-only to create tree structure
./target/release/sai3-bench run --config tests/configs/directory-tree/tree_test_lognormal.yaml --prepare-only

# Run full workload with tree-based operations
./target/release/sai3-bench -v run --config tests/configs/directory-tree/tree_test_basic.yaml

# Verify tree structure
find /mnt/test/sai3bench-tree-lognormal -type d | wc -l  # Count directories
find /mnt/test/sai3bench-tree-lognormal -type f | wc -l  # Count files
```

### NO COMMITS WITHOUT COMPREHENSIVE TESTS
**Enforcement**: Any code commit MUST include:
1. **New tests** for all new functionality (config fields, enums, functions)
2. **All tests passing** (`cargo test --lib` and affected integration tests)
3. **Test count verification** in commit message (e.g., "Added 21 new tests")
4. **Updated tests** for any modified existing tests

### Test Coverage Standards
- **New config fields**: Minimum 3 tests (parse, serialize/deserialize, validation)
- **New enums**: Test all variants + equality + clone + debug format + invalid values
- **New functions**: Unit tests with edge cases + error conditions
- **Integration tests**: End-to-end scenarios for user-facing features

### Example: TreeCreationMode and PathSelectionStrategy (v0.7.0)
Added 21 new tests in `tests/distributed_config_tests.rs`:
- 3 tests for TreeCreationMode variants (isolated, coordinator, concurrent)
- 4 tests for PathSelectionStrategy variants (random, partitioned, exclusive, weighted)
- 4 tests for partition_overlap field (default, zero, one, custom values)
- 2 tests for shared_filesystem field (true, false)
- 3 tests for enum behavior (equality, clone, debug format)
- 2 tests for invalid enum values
- 1 comprehensive integration test
- 1 serialize/deserialize round-trip test
- Updated 9 existing tests to include new required fields

**Current test count**: 283 Rust tests across multiple test files

## Architecture: Three Binary Strategy
- **`sai3-bench`** (`src/main.rs`) - Single-node CLI with subcommands: `run`, `replay`, `util`
- **`sai3bench-agent`** (`src/bin/agent.rs`) - gRPC server node for distributed loads
- **`sai3bench-ctl`** (`src/bin/controller.rs`) - Coordinator for multi-agent execution

### Agent CLI Usage (CRITICAL - ALWAYS USE THESE EXACT FLAGS)

**Starting agents** (for distributed testing):
```bash
# CORRECT - agent uses --listen, NOT --port or --id
./target/release/sai3bench-agent --listen 0.0.0.0:7761 -vv
./target/release/sai3bench-agent --listen 0.0.0.0:7762 -vv

# WRONG - these flags DO NOT EXIST
./target/release/sai3bench-agent --port 7761 --id agent-1    # ERROR!
```

**Agent options**:
- `--listen <ADDR:PORT>` - Listen address (default: 0.0.0.0:7761)
- `-v`, `-vv`, `-vvv` - Verbosity levels (use -vv for debugging)
- `--tls` - Enable TLS with ephemeral self-signed certificate
- `--tls-domain <DOMAIN>` - Subject DNS name for cert (default: localhost)
- `--tls-write-ca <PATH>` - Write generated cert/key for controller trust
- `--tls-sans <SANS>` - Comma-separated Subject Alternative Names

**Agent IDs** come from the **config YAML**, not CLI flags:
```yaml
distributed:
  agents:
    - address: "127.0.0.1:7761"
      id: "agent-1"              # Agent ID defined HERE
    - address: "127.0.0.1:7762"
      id: "agent-2"              # Agent ID defined HERE
```

**Controller usage**:
```bash
# Config-driven (recommended - agents auto-discovered from YAML)
./target/release/sai3bench-ctl run --config test.yaml

# Explicit agents (overrides config)
./target/release/sai3bench-ctl --agents 127.0.0.1:7761,127.0.0.1:7762 run --config test.yaml
```

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
**Key**: Currently using s3dlio v0.9.22 via git tag dependency.

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
├── console_log.txt      # Full execution log
├── metadata.json        # Test metadata (distributed: true/false)
├── results.tsv          # Single-node OR consolidated aggregate (merged histograms)
└── agents/              # Only in distributed mode
    ├── agent-1/
    │   ├── config.yaml  # Agent's modified config (with path prefix)
    │   ├── console_log.txt  # Agent's execution log
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