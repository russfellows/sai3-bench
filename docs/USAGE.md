# sai3-bench — Usage Guide

sai3-bench is a multi-protocol I/O benchmarking suite with optional distributed execution via gRPC. It ships three binaries:

- **`sai3-bench`** — single-node CLI (health/list/stat/get/put/delete/run/replay) using `s3dlio`
- **`sai3bench-agent`** — per-host gRPC agent that runs I/O operations on that host
- **`sai3bench-ctl`** — controller that coordinates one or more agents

**Supported Backends**: `file://`, `direct://`, `s3://`, `az://`, `gs://`

## Removed Binaries (v0.6.9+)

- **`sai3bench-run`** — Removed (use `sai3-bench run` instead - same functionality, more features)
- **`fs_read_bench`** — Removed (internal development tool, not needed for production)

## Performance Logging (perf_log)

**v0.8.19+**: Performance logging is **always enabled** with **precise 1-second intervals** (±1ms accuracy). No configuration needed!

The perf_log feature captures time-series performance metrics with exact 1-second timing during test execution, providing detailed visibility into performance trends.

**Timing Precision** (v0.8.19):
- Perf_log updates: Exactly 1000ms intervals using dedicated timer with `MissedTickBehavior::Burst`
- Display updates: 500ms intervals for responsive feedback (independent of perf_log timing)
- All perf_log files (aggregate + per-agent) use synchronized timestamps

**Format**: TSV (tab-separated values) with 31 columns (v0.8.17+)

**Columns**:
- Metadata: agent_id, timestamp_epoch_ms, elapsed_s, stage, is_warmup
- GET metrics: ops, bytes, iops, mbps, mean_us, p50_us, p90_us, p99_us
- PUT metrics: ops, bytes, iops, mbps, mean_us, p50_us, p90_us, p99_us  
- META metrics: ops, iops, mean_us, p50_us, p90_us, p99_us
- CPU metrics: user_pct, system_pct, iowait_pct
- Error tracking: errors

**Output files** (automatically created):
- Standalone mode: `results/perf_log.tsv`
- Distributed mode:
  - Per-agent: `results/agents/{agent-id}/perf_log.tsv` (accurate percentiles)
  - Aggregate: `results/perf_log.tsv` (aggregated across agents)

**⚠️ Important - Aggregate Percentile Limitation:**

In distributed mode, the aggregate perf_log.tsv percentiles are computed using weighted averaging, which is a mathematical approximation. For statistically accurate percentile analysis:
- Use **per-agent perf_log files** (`agents/{agent-id}/perf_log.tsv`) - Computed from local HDR histograms
- Use **final workload_results.tsv** - Uses correct HDR histogram merging across agents

The aggregate perf_log is suitable for monitoring during execution, but final analysis should use the above sources. See [docs/CHANGELOG.md](CHANGELOG.md) v0.8.17 for technical details.

---

## Persistent Metadata Cache (v0.8.60+)

**v0.8.60** introduces a persistent KV-based metadata cache using **fjall v3** (LSM-tree storage) to track object state across prepare, workload, and cleanup phases. This provides dramatic performance improvements and enables resume capability for large-scale tests.

### Key Capabilities

1. **Resume Interrupted Prepare**: If prepare is interrupted (CTRL-C, crash, network failure), restart from the last checkpoint instead of starting over
2. **Instant Path Lookups**: O(1) distributed hash lookups replace O(n) directory tree iteration (10,000x improvement)
3. **Pre-Workload Validation**: Verify ALL planned objects exist before starting benchmark (prevents mid-test failures)
4. **Cleanup Verification**: Delete exactly what was created, detect drift (externally deleted files)
5. **Progress Tracking**: Real-time progress during massive file creation (64M files)
6. **Avoid Expensive LIST**: Cached metadata eliminates slow LIST operations (400x speedup)

### Performance Impact

**Real-world benchmarks** (4 endpoints, 64M files):
- **Tree generation**: 45 seconds → 0.5 seconds (90x speedup on subsequent runs)
- **LIST operations**: 53 minutes → 8 seconds (400x faster with cached metadata)
- **Path lookups**: O(n) iteration → O(1) hash lookups (10,000x improvement)

### Architecture

**Per-Endpoint Caches** (distributed ownership):
- Each endpoint stores metadata ONLY for objects it owns
- Cache location: `{endpoint_uri}/sai3-kv-cache/`
- Example: `file:///mnt/nvme1/sai3-kv-cache/` stores metadata for files on nvme1
- File ownership: Round-robin by file index (`file_idx % num_endpoints == endpoint_idx`)

**Coordinator Cache** (shared global state):
- Stores tree manifests, endpoint registry, global configuration
- Location: `{results_dir}/.sai3-coordinator-cache/`
- Shared across all agents for distributed coordination

### State Tracking

The cache tracks **BOTH** desired state (plan) and current state (actual):

| State | Description | Use Case |
|-------|-------------|----------|
| `Planned` | Object should exist but not created yet | Resume prepare from checkpoint |
| `Creating` | Creation in progress | Detect stale locks, timeout handling |
| `Created` | Successfully created and verified | Pre-workload validation passes |
| `Failed` | Creation failed | Skip during workload, report errors |
| `Deleted` | Was created, now missing | Drift detection, cleanup verification |

**Example workflow**:
1. **Prepare phase**: Plans 64M objects → `Planned` state, creates them → `Created` state
2. **Interrupt**: CTRL-C during prepare at 32M objects
3. **Restart**: Cache knows 32M are `Created`, 32M are `Planned` → continues from 32M
4. **Workload phase**: Validates all 64M objects are `Created` before starting
5. **Cleanup phase**: Deletes only objects in `Created` state, detects drift

### Usage Pattern

**Automatic operation** - No configuration required:
```bash
# Cache automatically created during prepare
./sai3-bench run --config workload.yaml

# Distributed mode - each agent uses its own endpoint cache
./sai3bench-ctl run --config distributed.yaml
```

**Cache locations** (automatically created):
```
# Standalone mode
file:///mnt/test/sai3-kv-cache/               # Endpoint cache
/tmp/sai3-results-20260209/.sai3-coordinator-cache/  # Coordinator cache

# Distributed mode (4 endpoints)
file:///mnt/nvme1/sai3-kv-cache/              # Agent 1, endpoint 0
file:///mnt/nvme2/sai3-kv-cache/              # Agent 1, endpoint 1
file:///mnt/nvme3/sai3-kv-cache/              # Agent 2, endpoint 0
file:///mnt/nvme4/sai3-kv-cache/              # Agent 2, endpoint 1
/tmp/results/.sai3-coordinator-cache/         # Shared coordinator cache
```

### Integration with Metadata Pre-fetching

The metadata cache **eliminates stat() overhead** by providing instant size lookups:

**Without cache** (v0.8.59):
```
GET operation → stat() to fetch size → read file → process
              ↑ Network round-trip for every file!
```

**With cache** (v0.8.60+):
```
Prepare phase → Stores size in cache → Persist to disk
Workload phase → O(1) cache lookup → read file → process
                ↑ ZERO stat() calls, ZERO network round-trips!
```

**Result**: For prepared workloads, achieves **100% cache hit rate** with ZERO stat() overhead.

### When Cache is Used

**Automatically enabled**:
- ✅ All `prepare` stages with `ensure_objects` (creates and tracks objects)
- ✅ Workload phases with `glob_prepared_objects` (uses cached metadata for reads)
- ✅ Directory tree generation (`directory_tree` config) - stores tree manifest
- ✅ Cleanup phases - deletes tracked objects, verifies deletion

**Cache bypassed** (not an error):
- ℹ️ Utility commands (`util ls`, `util health`) - read-only operations
- ℹ️ Replay mode - uses operation log, not live objects
- ℹ️ Manual cleanup (`cleanup_mode: delete_all_test_data`) - scans storage directly

### Debugging Cache Issues

```bash
# Verbose logging shows cache operations
./sai3-bench -vv run --config workload.yaml

# Look for log lines:
#   "Opening metadata cache at file:///mnt/test/sai3-kv-cache"
#   "Planned 10000 objects for endpoint 0"
#   "Marked 10000 objects as Created"
#   "Cache hit rate: 100% (10000/10000)"
```

**Common issues**:
- **"Cache not found"**: Normal for first run - cache created during prepare
- **"Low cache hit rate"**: Objects may not be from current prepare phase (use `cleanup` first)
- **"State mismatch"**: Concurrent modifications or interrupted prepare (restart from checkpoint)

### Cache Persistence and Cleanup

**Cache lifecycle**:
- Created during first `prepare` stage
- Persisted to disk after batch updates (every 1000 objects)
- Reused across runs until cleanup
- Deleted when cleanup runs (configurable via `cleanup_mode`)

**Manual cache cleanup** (if needed):
```bash
# Remove endpoint cache
rm -rf /mnt/test/sai3-kv-cache/

# Remove coordinator cache
rm -rf /tmp/results/.sai3-coordinator-cache/

# Full cleanup (storage + cache)
./sai3-bench run --config workload.yaml --cleanup-only
```

---

## Controller Autotune (v0.8.70+)

`sai3bench-ctl autotune` sweeps a parameter matrix across distributed agents and prints ranked results showing which combination gives the best throughput (or lowest latency).

All tuning parameters live in the YAML config file — there are **no CLI flags** for individual parameters. The only flags are `--config <file>` and `--dry-run`.

### Quick Start

```bash
# Preview the sweep plan without running any trials
./target/release/sai3bench-ctl autotune \
  --config examples/distributed-autotune-minimal.yaml --dry-run

# Run the full sweep
./target/release/sai3bench-ctl autotune \
  --config examples/distributed-autotune-minimal.yaml
```

### Autotune YAML Reference

All parameters go under an `autotune:` key in the config file alongside the normal `distributed:` block (agent list, stages, etc.).

```yaml
autotune:
  # ── Target ─────────────────────────────────────────────────────
  uri: "gs://my-bucket/bench/autotune/obj*.dat"  # glob; must match real objects

  # ── Size sweep ─────────────────────────────────────────────────
  # Option A: log-spaced range (lo-hi, N steps)
  size_range: "32MiB-64MiB"   # inclusive range
  size_steps: 3                # number of log-spaced points (3 → 32, ~45, 64 MiB)

  # Option B: explicit list (mutually exclusive with size_range/size_steps)
  # sizes: "32MiB,64MiB,128MiB"

  # ── Thread sweep ───────────────────────────────────────────────
  threads: "16,32"             # comma-separated thread counts to try

  # ── GCS gRPC channel sweep (choose ONE of the two options) ─────
  # Option A: channels_per_thread — scales with thread count (recommended for GCS RAPID)
  channels_per_thread: "1,2"   # effective = threads × multiplier; valid range 1-8; default 1
  # Option B: channels — absolute counts (mutually exclusive with channels_per_thread)
  # channels: "16,32"

  # ── Range-download tuning ──────────────────────────────────────
  range_enabled: "false,true"  # sweep range-download on/off
  range_thresholds_mb: "32"    # comma list of threshold values in MB

  # ── GCS write-chunk tuning ─────────────────────────────────────
  gcs_write_chunk_sizes: "2097152"   # bytes; e.g. "2097152,4194304"

  # ── GCS RAPID mode ─────────────────────────────────────────────
  # Controls s3dlio's gRPC transport selection for GCS.
  #   auto   — use RAPID if available, fall back to standard gRPC (default)
  #   on     — force RAPID gRPC (fails if not supported)
  #   off    — force standard gRPC even if RAPID is available
  gcs_rapid_mode: auto

  # ── Trial settings ─────────────────────────────────────────────
  objects: 64            # objects issued per trial run
  trials: 1              # repetitions per configuration (increase for stability)
  optimize_for: throughput  # "throughput" or "latency"
  ops: both              # "get", "put", or "both"
```

### Sweep Dimensions and Log-Spacing

When `size_range` + `size_steps` is used, sizes are computed with **log-spacing**:

```
size[i] = lo × (hi / lo) ^ (i / (steps - 1))   for i = 0 … steps-1
```

Each value is rounded to the nearest 1 KiB boundary. Example with `size_range: "32MiB-64MiB"` and `size_steps: 3`:
- Step 0: 32.0 MiB
- Step 1: 45.3 MiB  ← geometric mean
- Step 2: 64.0 MiB

### `channels_per_thread` vs `channels`

For GCS workloads the number of gRPC subchannels significantly affects throughput. Two ways to express the channel sweep:

| Parameter | Meaning | Example |
|---|---|---|
| `channels_per_thread: "1,2"` | Multiplier; effective = threads × value | 16 threads → tries 16 and 32 channels |
| `channels: "16,32"` | Absolute count regardless of thread count | Always uses 16 or 32 channels |

`channels_per_thread` is preferred for GCS RAPID workloads because the optimal subchannel count tends to scale with parallelism.

> **Note**: `channels` and `channels_per_thread` are mutually exclusive. Specifying both is an error. `channels_per_thread: 0` is rejected.

### Dry-Run: Planning Before Running

Always use `--dry-run` first to verify the sweep before committing to a long run:

```bash
./target/release/sai3bench-ctl autotune \
  --config examples/distributed-autotune-minimal.yaml --dry-run
```

Example output:
```
┌─ Autotune Configuration ──────────────────────────────────────────┐
│ URI:          gs://my-bucket/bench/autotune/obj*.dat
│ Size Range:   32MiB-64MiB (log-spaced, 3 steps) → 3 size(s): ["32.0 MiB", "45.3 MiB", "64.0 MiB"]
│ Threads:      16,32 → 2 value(s): [16, 32]
│ Channels/Thr: 1,2 → 2 value(s)  [effective = threads × cpt]
│               e.g. 16 threads × [1, 2] = [16, 32] channels
│ Range Enabled: false,true → 2 value(s)
│ Range Thresh: 32 MB → 1 value(s)
│ GCS Chunks:   2097152 bytes → 1 value(s)
│ GCS RAPID:    auto
│ Objects:      64
│ Trials:       1
│
│ ── Sweep summary ─────────────────────────────────────────────────
│   Loop order  : sizes(3) × threads(2) × channels(2) × range_en(2) × thresh(1) × gcs_chunk(1)
│   Total cases : 24
│   Trials/case : 1
│   Total runs  : 24 case(s) × 1 trial(s) = 24 runs
│   Object I/Os : 24 runs × 2 op(s) × 64 objects = 3,072 I/Os
└───────────────────────────────────────────────────────────────────┘
```

There is **no hard limit** on trial count. The dry-run sweep summary is the planning tool — it warns if the total run count exceeds 200. Narrow your parameter ranges if the run count is too large.

### Reference Example

See `examples/distributed-autotune-minimal.yaml` for a fully commented example including the `distributed:` block with agent addresses.

### Dry-Run Validation Checks

Use these commands to validate YAML parsing and distributed preflight before execution:

```bash
# Single-node validation
./target/release/sai3-bench run --config tests/configs/test_fill_random.yaml --dry-run

# Distributed validation through controller
./target/release/sai3bench-ctl \
  run --config tests/configs/custom_stage_test.yaml --dry-run
```

---

## Configuration Syntax

For detailed YAML configuration syntax, see:
- **[Configuration Syntax Reference](CONFIG_SYNTAX.md)** - Complete syntax guide
- **[Example Configurations](../examples/)** - Ready-to-use example configs

**Quick syntax reminder**:
```yaml
# Use glob patterns with wildcards (*)
- op: get
  path: "data/prepared-*.dat"  # ✅ Correct

# NOT brace expansions
- op: get
  path: "data/obj_{00000..19999}"  # ❌ ERROR
```

This doc focuses on the distributed controller/agent mode, including plaintext and TLS (self‑signed) operation.

---

# Prerequisites

- **Rust toolchain** (stable, 2024 edition)  
- **Protobuf compiler** (`protoc`) — required by `tonic-build`  
  - Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`
- **Storage Backend Credentials** on each agent host:
  - **AWS S3**: The agent uses the AWS SDK default chain. Ensure one of:
    - `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` (+ optional `AWS_SESSION_TOKEN`)
    - or `~/.aws/credentials` with a default or selected profile
    - or `./.env` file containing ACCESS_KEY_ID, SECRET_ACCESS_KEY and other required params
  - **Azure Blob Storage**: Requires environment variables:
    - `AZURE_STORAGE_ACCOUNT="your-storage-account-name"`
    - `AZURE_STORAGE_ACCOUNT_KEY="your-account-key"`
  - **Google Cloud Storage**: Application Default Credentials via gcloud CLI:
    - `gcloud auth application-default login`
  - **File/Direct I/O**: No credentials required, uses local filesystem
- **Open firewall** for the agent port (default: `7167`)

## Custom Endpoints (Local Emulators & Proxies)

All three cloud backends support custom endpoints for local emulators, on-prem storage, or multi-protocol proxies. Set the appropriate environment variable before running sai3-bench:

| Backend | Environment Variable(s) | Example |
|---------|------------------------|---------|
| **S3** | `AWS_ENDPOINT_URL` | `http://localhost:9000` (MinIO) |
| **Azure Blob** | `AZURE_STORAGE_ENDPOINT` or `AZURE_BLOB_ENDPOINT_URL` | `http://localhost:10000` (Azurite) |
| **GCS** | `GCS_ENDPOINT_URL` or `STORAGE_EMULATOR_HOST` | `http://localhost:4443` (fake-gcs-server) |

### Usage Examples

```bash
# S3 with MinIO or other S3-compatible storage
export AWS_ENDPOINT_URL=http://localhost:9000
export AWS_ACCESS_KEY_ID=minioadmin
export AWS_SECRET_ACCESS_KEY=minioadmin
sai3-bench util ls s3://mybucket/

# Azure Blob with Azurite emulator
export AZURE_STORAGE_ENDPOINT=http://127.0.0.1:10000
export AZURE_STORAGE_ACCOUNT=devstoreaccount1
export AZURE_STORAGE_ACCOUNT_KEY="Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="
sai3-bench util ls az://testcontainer/

# GCS with fake-gcs-server
export GCS_ENDPOINT_URL=http://localhost:4443
sai3-bench util ls gs://testbucket/

# Alternative GCS emulator syntax (Google's convention)
export STORAGE_EMULATOR_HOST=localhost:4443  # http:// added automatically
sai3-bench util ls gs://testbucket/
```

### Multi-Protocol Proxy

When using a multi-protocol proxy that exposes S3, Azure, and GCS on different ports:

```bash
# Configure all backends to use the proxy
export AWS_ENDPOINT_URL=http://proxy.local:9000
export AZURE_STORAGE_ENDPOINT=http://proxy.local:9001  
export GCS_ENDPOINT_URL=http://proxy.local:9002

# Now all URI schemes work through the proxy
sai3-bench util ls s3://bucket/
sai3-bench util ls az://container/
sai3-bench util ls gs://bucket/
```

### Path-Style Addressing

When `AWS_ENDPOINT_URL` is set, S3 requests automatically use path-style addressing (`endpoint/bucket/key`) instead of virtual-hosted style (`bucket.endpoint/key`). This is required for most S3-compatible storage systems.

Build all binaries:

```bash
cargo build --release
```

Binaries will be in target/release/

# Agent & Controller CLI Summary
```
sai3bench-agent
USAGE:
  sai3bench-agent [--listen <addr>] [--tls] [--tls-domain <name>]
                [--tls-sans <csv>] [--tls-write-ca <dir>] [--op-log <path>]

FLAGS/OPTIONS:
  --listen <addr>       Listen address (default: 0.0.0.0:7167)
  --tls                 Enable TLS with an ephemeral self-signed cert
  --tls-domain <name>   Subject CN / default SAN if --tls-sans not set (default: "localhost")
  --tls-sans <csv>      Comma-separated SANs (DNS names and/or IPs) for the cert (e.g. "hostA,10.1.2.3,127.0.0.1")
  --tls-write-ca <dir>  If set, writes PEM files (agent_cert.pem, agent_key.pem) into this directory
  --op-log <path>       Optional s3dlio operation log path (e.g., /data/oplogs/trace.tsv.zst)
                        Agent appends agent_id to filename to prevent collisions
                        Can be overridden per-workload via config YAML op_log_path field
                        Supports s3dlio oplog environment variable: S3DLIO_OPLOG_BUF (buffer size)
```

```
sai3bench-ctl
USAGE:
  sai3bench-ctl [--agents <csv>] [--tls] [--agent-ca <path>] [--agent-domain <name>] <SUBCOMMAND> ...

GLOBAL FLAGS/OPTIONS:
  --agents <csv>        Comma-separated list of agent addresses (host:port)
                        Optional: can also specify in config YAML under distributed.agents
                        If both specified, config YAML takes precedence
  --tls                 Enable TLS for secure connections (requires --agent-ca)
                        Default is plaintext HTTP/2 (no TLS)
  --agent-ca <path>     Path to agent's certificate PEM (required when --tls enabled)
  --agent-domain <name> Override SNI / DNS name when validating TLS

SUBCOMMANDS:
  ping                              Ping agents and print versions
  get   --uri <s3://bucket/prefix>  Run GET workload via agents
         [--jobs <N>]

  put   --bucket <bucket> --prefix <prefix>
        [--object-size <bytes>] [--objects <count>] [--concurrency <N>]
```

**Note:** When agents are started with --tls, the controller must also
use --tls --agent-ca <path> to trust the agent's self-signed certificate.
By default (no --tls flag), both controller and agents use plaintext HTTP.

## Specifying Agents (v0.7.12+)

You have three flexible options for specifying agent addresses:

### Option 1: YAML Config Only (Recommended)
```yaml
distributed:
  agents:
    - address: "node1.example.com:7167"
      id: "agent-1"
    - address: "node2.example.com:7167"
      id: "agent-2"
```

```bash
# No --agents flag needed
./sai3bench-ctl run --config workload.yaml
```

### Option 2: CLI Only (Quick Testing)
```bash
# Specify on command line
./sai3bench-ctl --agents node1:7167,node2:7167 run --config workload.yaml
```

### Option 3: Both (Config Takes Precedence)
```bash
# Config YAML agents override CLI agents
./sai3bench-ctl --agents localhost:7167,localhost:7168 run --config workload.yaml
# Uses agents from workload.yaml, not CLI
```

**Best Practice**: Define agents in your YAML config for reproducibility and documentation.
Use CLI `--agents` for quick ad-hoc testing.

**Note**: If agents are defined in your config YAML, the `--agents` CLI flag is optional.
The controller will use agents from the config file when `--agents` is not specified.

If the agent cert doesn't include the default DNS
name the controller uses, add --agent-domain.

## Distributed Configuration Validation (v0.8.23+)

The controller performs pre-flight validation of distributed configurations to catch common misconfigurations before execution:

### Common Error: base_uri in Isolated Mode

**The Problem**: When using `tree_creation_mode: isolated` with `shared_filesystem: false` and `use_multi_endpoint: true`, specifying `base_uri` causes listing failures because agents try to list from storage they're not configured to access.

**❌ WRONG** (causes h2 protocol errors):
```yaml
distributed:
  shared_filesystem: false      # Per-agent storage
  tree_creation_mode: isolated  # Each agent creates separate tree
  agents:
    - address: "node1:7167"
      multi_endpoint:
        endpoints: ["file:///mnt/filesys1/"]
    - address: "node2:7167"
      multi_endpoint:
        endpoints: ["file:///mnt/filesys5/"]

prepare:
  ensure_objects:
    - base_uri: "file:///mnt/filesys1/"  # ❌ BUG - only node1 can access this!
      use_multi_endpoint: true
```

**✅ CORRECT** (each agent uses its own endpoints):
```yaml
prepare:
  ensure_objects:
    - # No base_uri - each agent uses first multi_endpoint for listing
      use_multi_endpoint: true
      count: 1000
```

### Shared Filesystem Configuration

When `shared_filesystem: true`, `base_uri` is allowed because all agents access the same underlying storage:

```yaml
distributed:
  shared_filesystem: true       # All agents access same data
  tree_creation_mode: concurrent
  agents:
    - address: "node1:7167"
      multi_endpoint:
        endpoints: ["file:///mnt/shared/"]  # Different mount, same storage
    - address: "node2:7167"
      multi_endpoint:
        endpoints: ["file:///mnt/shared/"]

prepare:
  ensure_objects:
    - base_uri: "file:///mnt/shared/test/"  # ✅ OK - all agents can access
      use_multi_endpoint: true
```

# Advanced Features

## Multi-Endpoint Load Balancing (v0.8.22+)

Distribute I/O operations across multiple storage endpoints to maximize bandwidth utilization. Perfect for:
- **Multi-NIC storage systems** (VAST, Weka, MinIO clusters)
- **Regional redundancy** (multiple S3 buckets, Azure regions)
- **NFS multi-path** (same namespace via different mount points)

### Quick Example

```yaml
# Global configuration - all agents use these endpoints
multi_endpoint:
  strategy: round_robin
  endpoints:
    - "s3://192.168.1.10:9000/bucket/"
    - "s3://192.168.1.11:9000/bucket/"
    - "s3://192.168.1.12:9000/bucket/"

workload:
  - op: get
    path: "data/*.dat"
    weight: 100
    use_multi_endpoint: true  # Enable load balancing
```

**Load balancing strategies:**
- `round_robin`: Simple sequential rotation (default, predictable)
- `least_connections`: Adaptive routing to least-loaded endpoint

**Per-agent endpoint mapping** (static assignment):

```yaml
distributed:
  agents:
    - address: "host1:7167"
      multi_endpoint:
        endpoints: ["s3://10.0.1.10:9000/bucket/"]  # Host1 specific
    - address: "host2:7167"
      multi_endpoint:
        endpoints: ["s3://10.0.1.11:9000/bucket/"]  # Host2 specific
```

**Use case**: 4 test hosts × 2 endpoints/host = 8 storage IPs fully utilized.

**Output**: Per-endpoint statistics files (`workload_endpoint_stats.tsv`, `prepare_endpoint_stats.tsv`) for diagnosing load balancing effectiveness.

For complete multi-endpoint documentation including NFS examples, troubleshooting, and isolated vs shared storage modes, see **[CONFIG_SYNTAX.md § Multi-Endpoint Load Balancing](CONFIG_SYNTAX.md#multi-endpoint-load-balancing)**.

## YAML-Driven Stage Orchestration (v0.8.50+)

Define multi-stage test workflows beyond traditional prepare→execute→cleanup:

**v0.8.61+**: `distributed.stages` is **required** for distributed runs. Empty or missing stage lists are invalid.

### Legacy Config Conversion (v0.8.61+)

Use the built-in converter to update older YAML files with implicit stages:

```bash
# Convert a single file
sai3-bench convert --config legacy.yaml
sai3bench-ctl convert --config legacy.yaml

# Convert multiple files with a glob
sai3-bench convert --files "tests/configs/*.yaml"
sai3bench-ctl convert --files "tests/configs/*.yaml"
```

### Basic Concept

Instead of a single workload run, define **independent stages** with custom ordering:

```yaml
distributed:
  stages:
    - name: "preflight"
      order: 1
      completion: validation_passed
      config:
        type: validation
    
    - name: "prepare"
      order: 2
      completion: tasks_done
      config:
        type: prepare
    
    - name: "benchmark"
      order: 3
      completion: duration
      config:
        type: execute
        duration: "30m"
    
    - name: "cleanup"
      order: 4
      completion: tasks_done
      optional: true  # Allow test to succeed even if cleanup fails
      config:
        type: cleanup
```

**Stage types:**
- **Execute**: Run workload for fixed duration
- **Prepare**: Create baseline objects
- **Cleanup**: Delete objects
- **Validation**: Pre-flight configuration checks
- **Hybrid**: Duration OR task completion (whichever first)
- **Custom**: Run custom scripts/commands

**Completion criteria:**
- `duration`: Complete after time period (execute stages)
- `tasks_done`: Complete when all tasks finished (prepare/cleanup)
- `validation_passed`: Complete when checks pass (validation)
- `duration_or_tasks`: Whichever completes first (hybrid)
- `script_exit`: Complete when command exits (custom)

**Real-world use cases:**
- Multi-epoch ML training with checkpointing between stages
- Tiered testing (smoke test → medium test → full test)
- Complex workflows (generate → convert → validate → benchmark)

**Output**: Each stage produces numbered TSV file (`01_preflight_results.tsv`, `02_prepare_results.tsv`, etc.) preserving execution order in analysis tools.

For complete stage orchestration documentation including hybrid stages, custom commands, and default stage behavior, see **[CONFIG_SYNTAX.md § YAML-Driven Stage Orchestration](CONFIG_SYNTAX.md#yaml-driven-stage-orchestration)**.

## Barrier Synchronization (v0.8.25+)

Ensure all agents reach coordination points before proceeding. Critical for multi-stage workflows with timing precision.

**v0.8.61+**: Barriers are keyed by **numeric stage order** (stage index), not string names.

### Why Barriers Matter

**Without barriers:**
```
Agent 1: Prepare (30s) → Execute starts at T=30
Agent 2: Prepare (45s) → Execute starts at T=45
Result: 15-second timing skew ❌
```

**With barriers:**
```
Agent 1: Prepare (30s) → Wait at barrier
Agent 2: Prepare (45s) → Reach barrier
Both: Execute starts at T=45 (synchronized) ✅
```

### Configuration

```yaml
distributed:
  barrier_sync:
    enabled: true
    default_heartbeat_interval: 30  # Status report every 30s
    default_missed_threshold: 3     # Query after 3 missed heartbeats
    
    # Per-stage barrier override
    prepare:
      type: all_or_nothing         # All agents must complete
      agent_barrier_timeout: 600   # 10 minutes for large datasets
    
    execute:
      type: all_or_nothing
      agent_barrier_timeout: 300   # 5 minutes
    
    cleanup:
      type: best_effort            # Don't block on cleanup failures
```

**Barrier types:**
- `all_or_nothing`: ALL agents must reach barrier (strict)
- `majority`: >50% agents sufficient (fault-tolerant)
- `best_effort`: Proceed when stragglers fail liveness check (opportunistic)

**Scaling for large tests (300k+ directories):**
```yaml
barrier_sync:
  prepare:
    agent_barrier_timeout: 1800  # 30 minutes for massive prepare
    heartbeat_interval: 60       # Less frequent reports
```

For complete barrier synchronization documentation including timing parameters, troubleshooting, and large-scale test tuning, see **[CONFIG_SYNTAX.md § Barrier Synchronization](CONFIG_SYNTAX.md#barrier-synchronization)**.

## Timeout Configuration (v0.8.50+)

Prevent indefinite hangs in distributed testing with comprehensive timeout controls.

### Three Timeout Categories

**1. Agent operation timeouts** (per-stage):
```yaml
stages:
  - name: "prepare"
    timeout_secs: 600         # 10 minutes for data generation
  
  - name: "execute"
    timeout_secs: 3600        # 1 hour for workload
  
  - name: "cleanup"
    timeout_secs: 1800        # 30 minutes for deletion
```

**2. Barrier synchronization timeouts**:
```yaml
barrier_sync:
  prepare:
    agent_barrier_timeout: 600  # How long agents wait at barrier
```

**3. gRPC communication timeouts**:
```yaml
distributed:
  grpc_keepalive_interval: 30   # PING every 30 seconds
  grpc_keepalive_timeout: 10    # Wait 10s for PONG
```

**Default timeouts** (if not specified):
- Prepare: 600s (10 minutes)
- Execute: 3600s (1 hour)
- Cleanup: 3600s (1 hour)
- Validation: 300s (5 minutes)
- Barrier sync: 120s (2 minutes)
- gRPC keepalive: 30s interval + 10s timeout

**For slow operations (>60s latency):**
```yaml
distributed:
  grpc_keepalive_interval: 120  # Avoid spurious disconnects
```

For complete timeout documentation including best practices, timeout hierarchy, and troubleshooting, see **[CONFIG_SYNTAX.md § Timeout Configuration](CONFIG_SYNTAX.md#timeout-configuration)**.

# 2 Data Generation Methods

## Fill Patterns

For realistic storage performance testing, **always use `fill: random`** (the default).

### Available Fill Patterns

- **`random`** (default): ✅ **ALWAYS USE THIS** - Produces truly incompressible data for realistic storage testing
- **`zero`**: ❌ **DO NOT USE** - All-zero data (100% compressible, completely unrealistic for storage testing)

**Why this matters**: Storage systems with compression or deduplication perform very differently with incompressible vs compressible data. Using `zero` will show artificially high performance that doesn't represent real-world behavior.

### Configuration Example

```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/test-data/"
      count: 1000
      min_size: 1048576
      max_size: 1048576
      fill: random              # ✅ Default - use for all storage testing
```

# 3 Quick Start — Single Host (PLAINTEXT)
In one terminal:

## Run agent without TLS on port 7167
./target/release/sai3bench-agent --listen 127.0.0.1:7167
In another terminal:

## Controller talking to that agent (plaintext is default):
./target/release/sai3bench-ctl --agents 127.0.0.1:7167 ping

## Example GET workload (jobs = concurrency for downloads)
./target/release/sai3bench-ctl --agents 127.0.0.1:7167 get \
  --uri s3://my-bucket/path/ --jobs 8

# 4 Multi-Host (PLAINTEXT)
On each agent host (e.g., node1, node2):

./sai3bench-agent --listen 0.0.0.0:7167
From the controller host:

./sai3bench-ctl --agents node1:7167,node2:7167 ping

./sai3bench-ctl --agents node1:7167,node2:7167 get \
  --uri s3://my-bucket/data/ --jobs 16

# 5 TLS with Self‑Signed Certificates (No CA hassles)
You can enable TLS on the agent with an ephemeral self‑signed certificate
generated at startup. You do not need a public CA. The controller just needs
the generated cert to trust the agent connection.

## 5.1 Start the Agent with TLS and write the cert
Pick a DNS name (CN) you’ll use from the controller—typically the agent’s
resolvable hostname or IP. If you need multiple names or IPs, use --tls-sans.

### Example: agent runs on loki-node3, reachable by name and IP
Write cert & key into /tmp/agent-ca/  (for you to scp to controller)
./sai3bench-agent \
  --listen 0.0.0.0:7167 \
  --tls \
  --tls-domain loki-node3 \
  --tls-sans "loki-node3,127.0.0.1,10.10.0.23" \
  --tls-write-ca /tmp/agent-ca

This produces:
   /tmp/agent-ca/agent_cert.pem
   /tmp/agent-ca/agent_key.pem


**Tip:** --tls-domain is the CN; if --tls-sans is not specified,
it will be used as a single SAN. If --tls-sans is provided, the SANs
list replaces the default and should include all names (or IPs) you plan to
use to connect to this agent.

Copy the certificate to the controller host (key stays on the agent):

### From controller host:
scp user@loki-node3:/tmp/agent-ca/agent_cert.pem /tmp/agent_ca.pem

## 5.2 Connect from the Controller (TLS)
Single agent:

```
./sai3bench-ctl \
  --agents loki-node3:7167 \
  --agent-ca /tmp/agent_ca.pem \
  ping
```

If you connect by an alternate name or IP that’s in the SANs, you may need
--agent-domain to set the SNI / TLS server_name to match the certificate:

## Connecting to the agent by IP, telling TLS to expect "loki-node3" (in SANs)
```
./sai3bench-ctl \
  --agents 10.10.0.23:7167 \
  --agent-ca /tmp/agent_ca.pem \
  --agent-domain loki-node3 \
  ping
```

Multiple agents (all in TLS mode):

```
./sai3bench-ctl \
  --agents loki-node3:7167,loki-node4:7167 \
  --agent-ca /tmp/agent_ca.pem \
  ping
```
```
./sai3bench-ctl \
  --agents loki-node3:7167,loki-node4:7167 \
  --agent-ca /tmp/agent_ca.pem \
  get --uri s3://my-bucket/data/ --jobs 16
```

**Important:** When the agent is running with --tls, the controller must also use --tls --agent-ca <path>.
By default, both use plaintext (no flags needed).

# 6 Distributed Live Stats (v0.7.6+)

## Real-Time Progress Display

### Controller View (sai3bench-ctl)

When running distributed workloads with `sai3bench-ctl run`, you'll see a real-time progress display showing:

- **Progress bar**: Visual progress with elapsed/total seconds (e.g., `[=====>] 15/30s`)
- **Live metrics**: Aggregate stats updated every second across all agents
- **Microsecond precision**: All latency values shown in µs for accuracy
- **Agent count**: Number of active agents
- **Per-operation stats**: Separate lines for GET, PUT, META operations

### Agent Console View (sai3bench-agent)

Each agent also displays its own progress on its console (v0.8.3):

- **Prepare phase**: Progress bar showing file creation/discovery
  - Format: `[agent-id] [=====>] 45/90 objects`
- **Workload phase**: Live statistics spinner
  - Format: `[agent-id] 1234 ops/s | 12.3 MiB/s | avg 95ms`
  - Updates every 0.5 seconds with throughput and latency

This allows you to monitor individual agent progress when running agents in separate terminals or log files.

### Example Controller Output

```
=== Distributed Workload ===
Config: tests/configs/workload.yaml
Agents: 2
Start delay: 2s
Storage mode: local (per-agent)

Starting workload on 2 agents with live stats...

⏳ Waiting for agents to validate configuration...
  ✅ agent-1 ready
  ✅ agent-2 ready
✅ All 2 agents ready - starting workload execution

[========================================] 30/30s
2 agents
  GET: 19882 ops/s, 19.4 MiB/s (mean: 95µs, p50: 96µs, p95: 135µs)
  PUT: 8541 ops/s, 16.7 MiB/s (mean: 102µs, p50: 98µs, p95: 136µs)
✓ All 2 agents completed


=== Live Aggregate Stats (from streaming) ===
Total operations: 689656 GET, 296040 PUT, 0 META
GET: 19882 ops/s, 19.4 MiB/s (mean: 95µs, p50: 96µs, p95: 135µs)
PUT: 8541 ops/s, 16.7 MiB/s (mean: 102µs, p50: 98µs, p95: 136µs)
Elapsed: 35.00s

=== Distributed Results ===
[... per-agent results ...]
```

## Startup Handshake Protocol (v0.7.12)

The controller implements a sophisticated startup handshake with improved timing:

1. **Validation phase** (~40 seconds for 2 agents): Agents validate configuration
   - Checks file:// patterns match actual files
   - Verifies PUT operations have object sizes
   - Validates all required parameters
   - Timeout scales with agent count: 30s + (5s × agent_count)
2. **Ready reporting**: Each agent sends READY or ERROR status
3. **Error handling**: If any agent fails validation, controller displays errors and aborts
4. **Countdown display** (v0.7.12): Visual countdown shows time until workload starts
   - "⏳ Starting in 10s..." (counts down second by second)
   - Clear feedback that system is working, not hung
5. **Synchronized start**: All agents begin workload at exact same coordinated timestamp
6. **Fast coordinated start** (v0.7.12): Fixed 10-second delay (down from 30-50 seconds)
   - Plus 2-second user-configurable delay (default)
   - Total: ~12 seconds from agents ready to workload start

### Configuration Errors

If agents detect configuration issues, you'll see clear error messages:

```
⏳ Waiting for agents to validate configuration...
  ✅ agent-1 ready
  ❌ agent-2 error: Pattern 'data/*.dat' matches no files

❌ 1 agent(s) failed configuration validation:
  ❌ agent-2: Pattern 'data/*.dat' matches no files

Ready agents: 1/2
  ✅ agent-1

Error: 1 agent(s) failed startup validation
```

### v0.7.12 Startup Sequence

The improved startup sequence provides better visibility:

```
⏳ Waiting for agents to validate configuration...
  ✅ agent-1 ready
  ✅ agent-2 ready
✅ All 2 agents ready - starting workload execution

⏳ Starting in 12s...
⏳ Starting in 11s...
⏳ Starting in 10s...
...
⏳ Starting in 1s...
✅ Starting workload now!
```

## Adjusting Start Delay

The default start delay is 2 seconds (on top of the 10-second coordinated start). You can adjust it:

```bash
./sai3bench-ctl --agents node1:7167,node2:7167 \
  --start-delay 5 \  # 5s instead of 2s (total: 15s coordinated start)
  run --config workload.yaml
```

For more details on the implementation, see `docs/DISTRIBUTED_LIVE_STATS_IMPLEMENTATION.md`.

# 7 Examples for Workloads
GET (download) via controller
### PLAINTEXT (Default)
```
./sai3bench-ctl --agents node1:7167 get \
  --uri s3://my-bucket/prefix/ --jobs 16
```

### TLS
```
./sai3bench-ctl --agents node1:7167 \
  --agent-ca /tmp/agent_ca.pem \
  get --uri s3://my-bucket/prefix/ --jobs 16
```
--uri accepts a single object (s3://bucket/key), a prefix (s3://bucket/prefix/), or a simple glob under a prefix (e.g., s3://bucket/prefix/*).

--jobs controls per-agent concurrency for GET.
PUT (upload) via controller

## Create N objects of size S under bucket/prefix, using M concurrency per agent.

### PLAINTEXT
```
./sai3bench-ctl --agents node1:7167 put \
  --bucket my-bucket \
  --prefix test/ \
  --object-size 1048576 \
  --objects 100 \
  --concurrency 8
```

### TLS
```
./sai3bench-ctl --agents node1:7167 \
  --agent-ca /tmp/agent_ca.pem \
  put --bucket my-bucket \
  --prefix test/ \
  --object-size 1048576 \
  --objects 100 \
  --concurrency 8
```

# 8 Localhost Demo (No Makefile Needed)
### Terminal A — agent (PLAINTEXT)
```
./target/release/sai3bench-agent --listen 127.0.0.1:7167
```

### Terminal B — controller
```
./target/release/sai3bench-ctl --agents 127.0.0.1:7167 ping
```

```
./target/release/sai3bench-ctl --agents 127.0.0.1:7167 get \
  --uri s3://my-bucket/prefix/ --jobs 4
```

## For TLS on localhost

### Terminal A — agent with TLS & SANs covering "localhost" and "127.0.0.1"
```
./sai3bench-agent --listen 127.0.0.1:7167 --tls \
  --tls-domain localhost \
  --tls-sans "localhost,127.0.0.1" \
  --tls-write-ca /tmp/agent-ca
```


### Terminal B — controller
```
./sai3bench-ctl --agents 127.0.0.1:7167 \
  --agent-ca /tmp/agent-ca/agent_cert.pem \
  --agent-domain localhost \
  ping
```

# 9 Troubleshooting
TLS is enabled ... but --agent-ca was not provided
You're connecting to a TLS-enabled agent, but the controller is missing
--agent-ca. Provide the agent's agent_cert.pem or run the controller with
plaintext (default, no --tls) if the agent is also plaintext. Use --tls on both if the agent uses --tls.
h2 protocol error: http2 error / frame with invalid size

Most commonly a TLS name mismatch or wrong certificate. Ensure:
The controller uses --agent-ca that matches the agent's certificate.
The SNI matches (--agent-domain) a SAN entry on the agent certificate.
The address you dial (host/IP) is present in the SANs (or you provide
--agent-domain to override the SNI to a SAN value).

No objects found for GET
Verify the --uri prefix and that your AWS credentials (on the agent
hosts) allow ListObjectsV2 and GetObject.

Throughput lower than expected
Increase --jobs (GET) or --concurrency (PUT), and/or add more agents.
Verify network path and S3 region distance.
Check CPU/network utilization on agent hosts.

Agent startup validation timeout
One or more agents didn't respond within the validation window (3 seconds).
Check that:
- Agents are running and reachable
- Network connectivity is stable
- Agents have access to required files/resources
- Increase --start-delay if agents need more time

Configuration validation failed
Agents perform pre-flight validation before starting workload. Common issues:
- file:// patterns don't match any files: Verify path is correct and files exist
- PUT operation missing object_size: Add object_size to PUT operations
- Empty workload: Ensure workload array has at least one operation

## Error Handling and Agent Auto-Reset (v0.8.0+)

Agents implement comprehensive error handling with automatic recovery:

### Error Thresholds (Default Values)
- **max_total_errors**: 100 total errors before aborting
- **error_rate_threshold**: 5.0 errors/second triggers smart backoff
- **max_retries**: 3 retry attempts per operation (when retry_on_error=true)

These defaults can be overridden in your config YAML:
```yaml
error_handling:
  max_total_errors: 200
  error_rate_threshold: 10.0
  max_retries: 5
  retry_on_error: true
```

### Smart Backoff
When error rate exceeds threshold, agents skip operations to reduce system load,
allowing the backend to recover. Retries use exponential backoff.

### Agent Auto-Reset
After encountering errors, agents automatically reset to listening state.
This means agents accept new workload requests immediately without requiring restart.

**Example**: If agent encounters I/O errors during workload execution:
1. Agent reports errors to controller
2. Agent transitions: Failed → Idle state
3. Agent ready for next workload request
4. No manual restart required

### Verbosity Levels

Control error/retry logging with verbosity flags:

**Default** (no flags): Shows only critical failures and threshold warnings
```bash
./sai3bench-agent --listen 0.0.0.0:7167
```

**`-v` (info level)**: Adds retry attempt logging with 🔄 emoji
```bash
./sai3bench-agent --listen 0.0.0.0:7167 -v
# Output: 🔄 Retry 1/3 for operation get on s3://bucket/key
```

**`-vv` (debug level)**: Shows individual errors with full context
```bash
./sai3bench-agent --listen 0.0.0.0:7167 -vv
# Output: ❌ Error on get s3://bucket/key: Connection timeout (attempt 1/3)
```

**Best Practice**: Use `-v` for production monitoring, `-vv` for debugging specific issues.

### Operation Logging (v0.8.1+)

Capture detailed operation traces for performance analysis and replay using s3dlio oplogs.

**Oplog Format** (s3dlio v0.9.22+):
```
idx  thread  op  client_id  n_objects  bytes  endpoint  file  error  start  first_byte  end  duration_ns
```

**Key Fields**:
- `client_id`: Agent identifier (set automatically, or via SAI3_CLIENT_ID env var)
- `first_byte`: Approximate time-to-first-byte for GET operations (see limitations below)
- `start`/`end`: ISO 8601 timestamps (synchronized to controller time in distributed mode)
- `duration_ns`: Operation duration in nanoseconds

**Clock Synchronization** (Distributed Mode):
- Agent automatically calculates clock offset from controller's start_timestamp_ns
- All operation timestamps adjusted to controller's reference time
- Enables accurate cross-agent analysis even with clock skew

**Client Identification**:
- Standalone mode: Uses "standalone" or SAI3_CLIENT_ID env var
- Distributed mode: Uses agent_id from config (e.g., "agent-1", "agent-2")
- Enables per-agent filtering in merged oplogs

**First Byte Tracking** (Approximate):
- GET operations: first_byte ≈ end (when complete data available)
- PUT operations: first_byte = start (upload begins immediately)
- Metadata operations (LIST/HEAD/DELETE): first_byte empty (not applicable)
- **Limitation**: Current implementation captures when `get()` completes, not when first byte arrives
- **Use for**: Throughput analysis, relative comparisons, small object benchmarking
- **Don't use for**: Precise TTFB metrics on large objects (>10MB)
- See [s3dlio OPERATION_LOGGING.md](https://github.com/russfellows/s3dlio/blob/main/docs/OPERATION_LOGGING.md) for detailed explanation

**CLI Flag** (applies to all workloads on agent):
```bash
# Enable oplog via CLI flag
./sai3bench-agent --listen 0.0.0.0:7167 --op-log /data/oplogs/trace.tsv.zst
# Creates: /data/oplogs/trace-agent1.tsv.zst (agent_id automatically appended)
```

**YAML Config** (per-workload control, takes precedence over CLI):
```yaml
# Enable in config YAML
op_log_path: /shared/storage/oplogs/benchmark.tsv.zst

distributed:
  agents:
    - address: "node1:7167"
      id: agent1
    - address: "node2:7167"
      id: agent2
```

Results in per-agent files:
- `/shared/storage/oplogs/benchmark-agent1.tsv.zst`
- `/shared/storage/oplogs/benchmark-agent2.tsv.zst`

**Environment Variables** (s3dlio oplog settings):
```bash
# Optional: configure buffer size (default: 64KB)
export S3DLIO_OPLOG_BUF=131072

./sai3bench-agent --listen 0.0.0.0:7167 --op-log /data/oplogs/trace.tsv.zst
```

**Post-Processing: Sorting Oplogs**:

Operation logs are NOT sorted during capture due to concurrent writes. To sort chronologically:

```bash
# Sort by start timestamp (creates .sorted.tsv.zst files)
./sai3-bench sort --files /data/oplogs/trace-agent1.tsv.zst /data/oplogs/trace-agent2.tsv.zst

# Or in-place (overwrites originals)
./sai3-bench sort --files /data/oplogs/*.tsv.zst --in-place

# Validate sorting
./sai3-bench replay --op-log /data/oplogs/trace-agent1.sorted.tsv.zst --dry-run
```

**Benefits of Sorting**:
- Better compression (sorted files are ~30-40% smaller)
- Enables chronological replay for debugging
- Required for accurate latency analysis

**Oplog Analysis**:
```bash
# Decompress and view oplog
zstd -d < /data/oplogs/trace-agent1.tsv.zst | head -20

# Count operations
zstd -d < /data/oplogs/trace-agent1.tsv.zst | wc -l

# Sort operations by latency (column 13: duration_ns)
zstd -d < /data/oplogs/trace-agent1.tsv.zst | tail -n +2 | sort -t$'\t' -k13 -n | tail -10
```

**Use Cases**:
- **Performance Analysis**: Identify slow operations, latency percentiles per agent
- **Workload Replay**: Capture production traffic and replay at different speeds
- **Debugging**: Trace specific operations that failed or exceeded thresholds
- **Comparison**: Compare operation latencies across agents to identify hotspots

# 10 Replay Backpressure (v0.8.10+)

When replaying operation logs, the system may not sustain the original I/O rate. The **replay backpressure** feature gracefully handles this by detecting lag and switching to a best-effort mode that skips timing delays.

## Backpressure Modes

- **Normal Mode**: Strict timing - operations execute at recorded timestamps
- **Best-Effort Mode**: No timing delays - operations execute as fast as possible to catch up

## Configuration

Enable backpressure configuration via `--config`:

```bash
# With YAML config for backpressure settings
./target/release/sai3-bench replay \
  --op-log /data/oplogs/trace.tsv.zst \
  --config tests/configs/replay_backpressure.yaml
```

### YAML Configuration Options

```yaml
# tests/configs/replay_backpressure.yaml
replay:
  lag_threshold: 5s           # Switch to best-effort when lag exceeds this
  recovery_threshold: 1s      # Switch back to normal when lag drops below this
  max_flaps_per_minute: 3     # Max mode transitions before giving up
  drain_timeout: 10s          # Timeout waiting for in-flight ops on mode change
  max_concurrent: 16          # Maximum concurrent replay operations
```

### Configuration Fields

| Field | Default | Description |
|-------|---------|-------------|
| `lag_threshold` | 5s | Lag that triggers switch to best-effort mode |
| `recovery_threshold` | 1s | Lag that allows return to normal mode |
| `max_flaps_per_minute` | 3 | Maximum mode oscillations before permanent best-effort |
| `drain_timeout` | 10s | Wait time for in-flight operations during mode switch |
| `max_concurrent` | 16 | Parallel operation limit |

## Behavior

1. **Lag Detection**: System monitors difference between expected and actual operation time
2. **Mode Switch**: When lag exceeds `lag_threshold`, switches to best-effort (skips delays)
3. **Recovery**: When lag drops below `recovery_threshold`, returns to normal timing
4. **Flap Prevention**: After 3 transitions per minute, permanently stays in best-effort mode

## Output Statistics

Replay summary includes backpressure metrics:

```
=== Replay Statistics ===
Operations: 25000 completed (1024 errors)
Throughput: 1234.5 ops/s
Replay time: 30.5s vs original 28.2s (108.2%)

Mode transitions: 2
Peak lag: 3500ms
Best-effort time: 12.3s
Final mode: Normal
```

## Dry-Run Validation

Validate your config before running:

```bash
./target/release/sai3-bench replay \
  --op-log /data/oplogs/trace.tsv.zst \
  --config tests/configs/replay_backpressure.yaml \
  --dry-run

# Output:
# Dry-run mode - validating config...
#   lag_threshold: 5s
#   recovery_threshold: 1s
#   max_flaps_per_minute: 3
#   drain_timeout: 10s
#   max_concurrent: 16
# Replay config valid. 25000 operations would be replayed.
```

# 11 Notes and Best Practices
Use resolvable hostnames for agents and include them in --tls-sans when
using TLS. If connecting by IP from the controller, add that IP to
--tls-sans or set --agent-domain to a SAN value.
Keep the private key (agent_key.pem) on the agent host; only the cert
(agent_cert.pem) should be copied to the controller(s).
For repeatable test environments, you can pre-generate and persist the certs
(via --tls-write-ca) and reuse them.
Monitor live stats during execution to catch issues early (low throughput,
high latency, stalled agents).
All latency metrics are reported in microseconds (µs) for precision with
fast operations.

# 12 Running Tests
Unit + integration tests:

cargo test
The gRPC integration test starts a local agent, then checks controller
connectivity (plaintext). For full TLS tests between hosts, use the examples in
Sections 3–4.

# 13 Versioning
The agent reports its version on ping:

```
./sai3bench-ctl --agents node1:7167 ping
# connected to node1:7167 (agent version X.Y.Z)
```
Keep controller/agent binaries from the same source build when testing.

---

## Workload Config Examples

### Simple Single-Node Config

```yaml
target: "file:///tmp/benchmark/"
duration: "60s"
concurrency: 16

prepare:
  ensure_objects:
    - base_uri: "data/"
      count: 1000
      min_size: 1048576
      max_size: 1048576
      fill: random

workload:
  - op: get
    path: "data/*"
    weight: 70
  - op: put
    path: "data/"
    size_spec: 1048576
    weight: 30
```

```bash
sai3-bench run --config my-test.yaml --dry-run
sai3-bench run --config my-test.yaml
```

### Distributed Config

```yaml
target: "file:///shared/benchmark/"
duration: "120s"
concurrency: 32

perf_log:
  enabled: true
  interval: 1s

distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: random
  agents:
    - address: "host1:7167"
      id: "agent-1"
    - address: "host2:7167"
      id: "agent-2"

prepare:
  ensure_objects:
    - base_uri: "data/"
      count: 5000
      min_size: 524288
      max_size: 10485760
      fill: random

workload:
  - op: get
    path: "data/*"
    weight: 60
  - op: put
    path: "data/"
    size_spec: 2097152
    weight: 30
  - op: list
    path: "data/"
    weight: 10
```

```bash
# Start agents on test hosts
./target/release/sai3bench-agent --listen 0.0.0.0:7167

# Run from controller
./target/release/sai3bench-ctl run --config distributed-test.yaml
```

---

## Directory Tree Workloads

Test realistic shared filesystem scenarios with configurable directory hierarchies:

```yaml
prepare:
  directory_structure:
    width: 3              # Subdirectories per level
    depth: 2              # Tree depth (2 = 3 + 9 directories)
    files_per_dir: 10     # Files per directory
    distribution: bottom  # "bottom" (leaf only) or "all" (every level)
    dir_mask: "d%d_w%d.dir"

  ensure_objects:
    - base_uri: "file:///tmp/tree-test/"
      count: 0            # Files created by directory_structure above
      size_spec:
        type: uniform
        min: 4096         # 4 KiB
        max: 16384        # 16 KiB
      fill: random
      dedup_factor: 1
      compress_factor: 1
```

> **Fill pattern**: Always use `fill: random` for benchmarks. `zero` triggers storage dedup/compression and produces unrealistic results.

**Key notes**:
- `distribution: bottom` — files only in leaf directories (most realistic for filesystem workloads)
- `distribution: all` — files at every level
- `--dry-run` shows total directory/file counts and data size before execution
- Works with S3, Azure Blob, GCS (implicit directories), and all `file://` paths
- `TreeManifest` ensures collision-free file numbering across distributed agents

```bash
./sai3-bench run --config tree-test.yaml --dry-run
# Output: Total Directories: 12, Total Files: 60, Total Data: 600 KiB

./sai3-bench run --config tree-test.yaml
```

See [tests/configs/directory-tree/README.md](../tests/configs/directory-tree/README.md) for example configs.

---

## Workload Replay

Capture production I/O traces via s3dlio op-logs, then replay against any target with original
timing, accelerated speed, or remapped URIs.

### Capturing with s3dlio — Python

```python
import s3dlio

s3dlio.init_op_log("/tmp/production_trace.tsv.zst")

data = s3dlio.get("s3://bucket/model.bin")
s3dlio.put("s3://bucket/results/output.json", result_bytes)
files = s3dlio.list("s3://bucket/data/")

s3dlio.finalize_op_log()
```

### Capturing with s3dlio — Rust

```rust
use s3dlio::{init_op_logger, store_for_uri, LoggedObjectStore, global_logger};

init_op_logger("production_trace.tsv.zst")?;

let store = store_for_uri("s3://bucket/")?;
let logged_store = LoggedObjectStore::new(Arc::from(store), global_logger().unwrap());

let data = logged_store.get("s3://bucket/file.bin").await?;
```

> **Note**: sai3-bench can also capture op-logs during benchmark runs with `--op-log`, primarily for
> analyzing benchmark I/O patterns rather than capturing production workloads.

### Replaying Captured Traces

```bash
# Replay against test environment with original timing
sai3-bench replay --op-log /tmp/production_trace.tsv.zst --target "az://test-storage/"

# Replay at 5x speed for accelerated load testing
sai3-bench replay --op-log /tmp/production_trace.tsv.zst --speed 5.0
```

### Backpressure Configuration

When target storage can't sustain the recorded I/O rate:

```yaml
# replay_config.yaml
lag_threshold: 5s        # Switch to best-effort when lag exceeds this
recovery_threshold: 1s   # Switch back when lag drops below this
max_flaps_per_minute: 3  # Exit gracefully if oscillating too much
max_concurrent: 1000     # Maximum in-flight operations
drain_timeout: 10s       # Timeout for draining on exit
```

```bash
sai3-bench replay --op-log trace.tsv.zst --config replay_config.yaml --target "s3://bucket/"
```

### URI Remapping

```yaml
# remap.yaml — 1:1 bucket rename
rules:
  - match:
      bucket: "prod-bucket"
    map_to:
      bucket: "staging-bucket"
      prefix: "migrated/"
```

```bash
sai3-bench replay --op-log trace.tsv.zst --remap remap.yaml --target "s3://staging-bucket/"
```

**Remapping strategies**: 1→1 (rename), 1→N (fanout with `round_robin`/`sticky_key`), N→1 (consolidate), N→M (regex-based cross-cloud).

See [tests/configs/remap_examples.yaml](../tests/configs/remap_examples.yaml) for complete examples.

**Use Cases**: Pre-migration validation, performance regression testing, capacity planning, cross-cloud comparison.

---

## Storage Efficiency Testing

Validate vendor dedup/compression claims with controlled data patterns. Both `dedup_factor` and
`compress_factor` default to `1` (no dedup, no compression) if omitted.

### Default — No Dedup/Compression

```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/unique-media/"
      count: 500
      size_spec: 10485760   # 10 MB
      fill: random
      # dedup_factor: 1  (default — all blocks unique)
      # compress_factor: 1  (default — incompressible)
```

### 3:1 Deduplication

```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/vm-snapshots/"
      count: 100
      size_spec: 52428800   # 50 MB
      fill: random
      dedup_factor: 3       # 1/3 blocks unique, 2/3 duplicates
      compress_factor: 1
```

100 files × 50 MB = 5 GB logical → ~1.67 GB unique (3:1 dedup).

### Combined Dedup + Compression

```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/log-archives/"
      count: 200
      size_spec:
        type: uniform
        min: 5242880    # 5 MB
        max: 52428800   # 50 MB
      fill: random
      dedup_factor: 5   # 5:1 dedup — 1/5 unique
      compress_factor: 2  # 2:1 compression — 50% zeros
```

Avg 28.5 MB × 200 = 5.7 GB logical → ~1.14 GB unique → ~570 MB after compression.

### Ratio Reference

| Setting | Value | Meaning | Storage Impact |
|---------|-------|---------|----------------|
| `dedup_factor: 1` | 1:1 | All unique (default) | No savings |
| `dedup_factor: 3` | 3:1 | 1/3 unique | 67% savings |
| `dedup_factor: 5` | 5:1 | 1/5 unique | 80% savings |
| `compress_factor: 1` | 1:1 | Incompressible (default) | No savings |
| `compress_factor: 2` | 2:1 | 50% zeros | 50% savings |
| `compress_factor: 4` | 4:1 | 75% zeros | 75% savings |

> **Fill pattern**: Always use `fill: random`. `zero` triggers storage dedup/compression and produces unrealistic benchmark results.

---

## Realistic Size Distributions

```yaml
workload:
  - op: put
    path: "data/"
    weight: 100
    size_spec:
      type: lognormal    # many small files, few large (realistic)
      mean: 1048576      # 1 MB
      std_dev: 524288    # 512 KB
      min: 1024          # floor: 1 KB
      max: 10485760      # ceiling: 10 MB
    fill: random
```

**Distribution types**:
- `lognormal` — realistic; requires `mean` and `std_dev`
- `uniform` — even spread between `min` and `max`
- Fixed integer — e.g. `size_spec: 1048576`

Research shows object storage naturally follows lognormal distributions (many small configs/thumbnails, few large videos/backups).

---

## Distributed Testing — Advanced Patterns

### SSH Deployment

```bash
# One-time setup — configure passwordless SSH
sai3bench-ctl ssh-setup --hosts ubuntu@vm1,ubuntu@vm2,ubuntu@vm3

# Run distributed test — agents deploy automatically
sai3bench-ctl run --config distributed-workload.yaml
```

### Per-Agent Overrides

```yaml
distributed:
  agents:
    - address: "vm1.example.com"
      id: "us-west-agent"
      target_override: "s3://us-west-bucket/"
      concurrency_override: 128
      env: { AWS_PROFILE: "benchmark" }
    - address: "vm2.example.com"
      id: "us-east-agent"
      target_override: "s3://us-east-bucket/"

  ssh:
    enabled: true
    key_path: "~/.ssh/sai3bench_id_rsa"

  deployment:
    container_runtime: "docker"    # or "podman"
    image: "sai3bench:latest"
    network_mode: "host"
```

### Scale-Out vs Scale-Up

**Scale-Out** (multiple VMs — maximum bandwidth, fault tolerance):
```yaml
agents:
  - { address: "vm1:7167", id: "agent-1" }
  - { address: "vm2:7167", id: "agent-2" }
  # ... vm3-vm8
```

**Scale-Up** (single large VM — cost optimization, lower latency):
```yaml
agents:
  - { address: "big-vm:7167", id: "c1", listen_port: 7167 }
  - { address: "big-vm:7168", id: "c2", listen_port: 7168 }
  # ... c3-c8
```

Cloud automation scripts: `scripts/gcp_distributed_test.sh`, `scripts/cloud_test_template.sh`, `scripts/local_docker_test.sh`.

---

## I/O Rate Control

```yaml
io_rate:
  iops: 1000
  distribution: exponential   # Poisson arrivals (realistic)
                               # "uniform" — fixed intervals
                               # "deterministic" — precise timing
```

- Per-worker division: target IOPS is automatically split across concurrent workers
- Drift compensation via `tokio::time::Interval` for uniform distribution
- Zero overhead when `io_rate:` is omitted

See [docs/IO_RATE_CONTROL_GUIDE.md](IO_RATE_CONTROL_GUIDE.md) for detailed usage.

## Per-Operation Concurrency

```yaml
concurrency: 32   # global default

workload:
  - op: get
    path: "data/*"
    weight: 70
    concurrency: 64   # more GET workers
  - op: put
    path: "uploads/"
    weight: 30
    concurrency: 8    # fewer PUT workers
```
