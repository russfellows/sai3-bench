# sai3-bench Configuration Syntax Reference

This document defines the correct YAML configuration syntax for sai3-bench workload configurations.

## Table of Contents

- [Configuration Validation](#configuration-validation)
- [Basic Structure](#basic-structure)
- [Multi-Endpoint Load Balancing](#multi-endpoint-load-balancing)
  - [Global Multi-Endpoint Configuration](#global-multi-endpoint-configuration)
  - [Per-Agent Endpoint Mapping](#per-agent-endpoint-mapping)
  - [Load Balancing Strategies](#load-balancing-strategies)
- [YAML-Driven Stage Orchestration](#yaml-driven-stage-orchestration)
  - [Stage Configuration](#stage-configuration)
  - [Stage Types](#stage-types)
  - [Completion Criteria](#completion-criteria)
  - [Multi-Stage Examples](#multi-stage-examples)
- [Barrier Synchronization](#barrier-synchronization)
  - [Global Barrier Configuration](#global-barrier-configuration)
  - [Per-Stage Barriers](#per-stage-barriers)
  - [Barrier Types](#barrier-types)
- [Timeout Configuration](#timeout-configuration)
  - [Agent Operation Timeouts](#agent-operation-timeouts)
  - [Barrier Synchronization Timeouts](#barrier-synchronization-timeouts)
  - [gRPC Communication Timeouts](#grpc-communication-timeouts)
- [Distributed Testing](#distributed-testing)
  - [Agent Configuration](#agent-configuration)
  - [SSH Deployment](#ssh-deployment)
  - [Path Selection Strategies](#path-selection-strategies)
- [Page Cache Control](#page-cache-control)
- [Operation Logging](#operation-logging)
- [Target URI](#target-uri)
- [Pattern Syntax](#pattern-syntax)
- [Operation Types](#operation-types)
- [Prepare Stage](#prepare-stage)

## Configuration Validation

Before running a workload, validate your YAML config file with the `--dry-run` flag:

```bash
# Parse config and display test summary (no execution)
sai3-bench run --config my-workload.yaml --dry-run
```

This will:
- ✅ Parse and validate YAML syntax
- ✅ Check for required fields and correct data types
- ✅ Display test configuration summary (duration, concurrency, backend)
- ✅ Show prepare phase details (if configured)
- ✅ List all workload operations with weights and percentages
- ✅ Report any configuration errors with clear messages

**Example output**:
```
✅ Config file parsed successfully: my-workload.yaml

┌─ Test Configuration ────────────────────────────────────────┐
│ Duration:     60s
│ Concurrency:  32 threads
│ Target URI:   s3://my-bucket/test/
│ Backend:      S3
└─────────────────────────────────────────────────────────────┘

┌─ Workload Operations ───────────────────────────────────────┐
│ 2 operation types, total weight: 100
│
│ Op 1: GET - 70.0% (weight: 70)
│       path: data/*
│
│ Op 2: PUT - 30.0% (weight: 30)
│       path: output/, size: 1048576 bytes
└─────────────────────────────────────────────────────────────┘
```

## Basic Structure

**Note: Human-Readable Time Units (v0.8.52+)**

All timeout and duration fields support convenient time unit suffixes for better readability:
- **Supported units**: `s` (seconds), `m` (minutes), `h` (hours), `d` (days)
- **Examples**: `"30s"`, `"5m"`, `"2h"`, `"1d"`
- **Backward compatible**: Plain integers still work (interpreted as seconds)
- **Applies to**: `duration`, `start_delay`, `grpc_keepalive_interval`, `grpc_keepalive_timeout`, `agent_ready_timeout`, `post_prepare_delay`, `query_timeout`, `agent_barrier_timeout`, and all barrier/SSH timeout fields

```yaml
# All of these are equivalent:
duration: "300s"    # 300 seconds
duration: "5m"      # 5 minutes = 300 seconds  
duration: 300       # Plain integer (backward compatible)
```

```yaml
# Global settings
target: "gs://bucket-name/"   # Base URI for all operations (optional)
duration: "60s"               # Test duration - supports time units: "5m", "2h" (or plain: 300)
concurrency: 32               # Number of parallel workers
page_cache_mode: auto         # Page cache hint for file:// URIs (optional)
                              # Values: auto, sequential, random, dontneed, normal
                              # Default: auto (Linux/Unix only, no-op on other platforms)
op_log_path: /data/oplog.tsv.zst  # s3dlio operation log path (optional, v0.8.1+)
                                   # For distributed agents, overrides CLI --op-log flag
                                   # Agent appends agent_id to prevent collisions
                                   # Supports S3DLIO_OPLOG_BUF env var (buffer size)
                                   # Note: Sorting requires post-processing (sai3-bench sort)

# Prepare stage (optional)
prepare:
  ensure_objects:
    - base_uri: "gs://bucket/data/"  # Optional (v0.8.23+) - omit in isolated mode with use_multi_endpoint
      count: 1000
      min_size: 1048576
      max_size: 1048576
      fill: random
      use_multi_endpoint: false      # Optional (v0.8.22+) - load balance across endpoints
  cleanup: true  # Remove prepared objects after test

# Workload operations
workload:
  - op: get
    path: "data/prepared-*.dat"  # Glob pattern
    weight: 60
  
  - op: put
    path: "data/new-"
    object_size: 1048576
    weight: 25
```

## Multi-Endpoint Load Balancing

**Introduced in v0.8.22**

Multi-endpoint configuration enables distributing I/O operations across multiple storage endpoints for improved performance and bandwidth utilization. This is particularly useful for:

- **Multi-NIC storage systems**: VAST, Weka, MinIO clusters with multiple network interfaces
- **Distributed object storage**: Multiple S3 endpoints, regional buckets
- **Multi-mount NFS**: Identical namespaces accessible via different mount points
- **Load balancing**: Horizontal scaling across multiple storage backend IPs

### Global Multi-Endpoint Configuration

Configure endpoints that all agents will use:

```yaml
# Global multi-endpoint configuration (applied to all agents)
multi_endpoint:
  strategy: round_robin           # Load balancing strategy
  endpoints:
    - "s3://192.168.1.10:9000/bucket/"
    - "s3://192.168.1.11:9000/bucket/"
    - "s3://192.168.1.12:9000/bucket/"
    - "s3://192.168.1.13:9000/bucket/"

# Workload operations
workload:
  - op: get
    path: "data/*.dat"
    weight: 100
    use_multi_endpoint: true      # Enable multi-endpoint for this operation
```

**Key requirements:**
- All endpoints must present **identical namespace** (same files accessible from each endpoint)
- Works with any storage backend: S3, Azure, GCS, file://, direct://
- `use_multi_endpoint: true` required in workload operations to enable load balancing

### Per-Agent Endpoint Mapping

**Static endpoint assignment** (each agent gets specific endpoints):

```yaml
# No global multi_endpoint - agents have individual configs
distributed:
  agents:
    - address: "host1:7761"
      id: "agent-1"
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - "s3://192.168.1.10:9000/bucket/"  # Agent 1 gets endpoints 10 & 11
          - "s3://192.168.1.11:9000/bucket/"
    
    - address: "host2:7761"
      id: "agent-2"
      multi_endpoint:
        strategy: round_robin
        endpoints:
          - "s3://192.168.1.12:9000/bucket/"  # Agent 2 gets endpoints 12 & 13
          - "s3://192.168.1.13:9000/bucket/"
```

**Use case**: 4 test hosts × 2 endpoints/host = 8 storage IPs fully utilized without overlap.

### Per-Agent Override

Mix global configuration with per-agent overrides:

```yaml
# Global config - most agents use these
multi_endpoint:
  strategy: round_robin
  endpoints:
    - "s3://10.0.1.10:9000/bucket/"
    - "s3://10.0.1.11:9000/bucket/"

distributed:
  agents:
    - address: "host1:7761"
      # Uses global multi_endpoint config
    
    - address: "host2:7761"
      multi_endpoint:               # Override: different endpoints for this agent
        strategy: least_connections
        endpoints:
          - "s3://10.0.2.10:9000/bucket/"
          - "s3://10.0.2.11:9000/bucket/"
```

### Load Balancing Strategies

**`round_robin`** (default):
- Simple sequential rotation through endpoints
- Predictable and deterministic behavior
- Best for: Homogeneous storage backends, testing consistency

```yaml
multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file:///mnt/nfs1/benchmark/"
    - "file:///mnt/nfs2/benchmark/"
```

**`least_connections`**:
- Routes to endpoint with fewest active requests
- Adaptive load balancing
- Best for: Heterogeneous backends, production-like scenarios

```yaml
multi_endpoint:
  strategy: least_connections
  endpoints:
    - "s3://fast-tier:9000/bucket/"
    - "s3://slow-tier:9000/bucket/"
```

### Multi-Endpoint with NFS

Load balance across multiple NFS mount points with identical namespaces:

```yaml
multi_endpoint:
  strategy: round_robin
  endpoints:
    - "file:///mnt/filesys1/benchmark/"
    - "file:///mnt/filesys2/benchmark/"
    - "file:///mnt/filesys3/benchmark/"
    - "file:///mnt/filesys4/benchmark/"

workload:
  - op: get
    path: "data/*.dat"
    weight: 60
    use_multi_endpoint: true
  
  - op: put
    path: "output/"
    object_size: 1048576
    weight: 40
    use_multi_endpoint: true
```

**Requirements for NFS**:
- All mount points must expose **same namespace** (same files via each mount)
- Common with: VAST, Weka, Lustre with multi-path access
- Each mount point typically routes to different storage backend IP

### Multi-Endpoint in Prepare Stage

Enable multi-endpoint load balancing during data preparation:

```yaml
prepare:
  ensure_objects:
    - base_uri: null                # Omit base_uri in isolated mode
      use_multi_endpoint: true      # Use endpoints from multi_endpoint config
      count: 10000
      min_size: 1048576
      max_size: 1048576
      fill: random

multi_endpoint:
  strategy: round_robin
  endpoints:
    - "s3://bucket1/path/"
    - "s3://bucket2/path/"
```

**When to use**:
- **Isolated mode**: Each agent prepares data in its own endpoint
- **Distributed data generation**: Spread prepare load across multiple endpoints
- **Per-agent storage**: Each agent has separate mount point or bucket

**When NOT to use**:
- **Shared storage**: All agents need access to same prepared data
  - Use `base_uri: "s3://shared-bucket/data/"` instead
  - Set `use_multi_endpoint: false`

### Endpoint Statistics

Multi-endpoint mode generates per-endpoint statistics files:

**Output files** (v0.8.22+):
- `workload_endpoint_stats.tsv` - Per-endpoint metrics for execute phase
- `prepare_endpoint_stats.tsv` - Per-endpoint metrics for prepare phase

**Columns**:
- `endpoint`: Endpoint URI
- `operation_count`: Number of operations to this endpoint
- `bytes`: Total bytes transferred
- `errors`: Error count
- `latency_p50_us`, `latency_p90_us`, `latency_p99_us`, `latency_p999_us`: Latency percentiles

**Use cases**:
- Diagnose load balancing effectiveness
- Identify slow endpoints
- Verify endpoint isolation in distributed tests

### Troubleshooting

**Problem**: All agents accessing same endpoints instead of assigned ones

**Cause**: Global `multi_endpoint` config sent to all agents

**Solution**: Use per-agent endpoint configuration:
```yaml
distributed:
  agents:
    - address: "host1:7761"
      multi_endpoint:
        endpoints: ["s3://ip1/", "s3://ip2/"]  # Agent 1 specific
```

**Problem**: "base_uri is required" error in prepare stage

**Fix**: Either set explicit base_uri OR use multi_endpoint:
```yaml
# Option 1: Explicit base_uri (shared storage)
prepare:
  ensure_objects:
    - base_uri: "file:///shared/data/"
      use_multi_endpoint: false

# Option 2: Multi-endpoint (isolated storage)
prepare:
  ensure_objects:
    - base_uri: null  # or omit entirely
      use_multi_endpoint: true
```

## YAML-Driven Stage Orchestration

**Introduced in v0.8.50 (Phases 1-3 complete)**

YAML-driven stage orchestration enables flexible, multi-stage test workflows beyond the traditional prepare→execute→cleanup pattern. Define custom stages with independent configurations, execution ordering, and synchronization barriers.

**v0.8.61+**: `distributed.stages` is **required** for distributed runs. Use the built-in converter for legacy YAML files with implicit stages.

```bash
sai3-bench convert --config legacy.yaml
sai3bench-ctl convert --config legacy.yaml
```

### Why Stage Orchestration?

**Traditional flow** (single execute stage):
```
Prepare → Execute → Cleanup
```

**Modern multi-stage flows**:
```
Preflight Validation → Prepare Dataset → Warmup → Execute Benchmark → Cooldown → Cleanup
```

**Real-world use cases**:
- **Multi-epoch training**: Separate stages for each training epoch with checkpointing
- **Tiered testing**: Preflight checks → Small-scale test → Full-scale test
- **Complex workflows**: Data generation → Format conversion → Validation → Execution

### Stage Configuration

**v0.8.61+**: `distributed.stages` is **required** for distributed runs. Empty or missing stage lists are invalid.

Stages are defined in the `distributed.stages` array:

```yaml
distributed:
  agents:
    - address: "host1:7761"
    - address: "host2:7761"
  
  # Define execution stages
  stages:
    - name: "preflight"
      order: 1
      completion: validation_passed
      barrier:
        type: all_or_nothing
        agent_barrier_timeout: 300
      timeout_secs: 300
      config:
        type: validation
    
    - name: "prepare"
      order: 2
      completion: tasks_done
      barrier:
        type: all_or_nothing
        agent_barrier_timeout: 600
      config:
        type: prepare
        expected_objects: 10000
    
    - name: "execute"
      order: 3
      completion: duration
      barrier:
        type: all_or_nothing
        agent_barrier_timeout: 300
      config:
        type: execute
        duration: "5m"
    
    - name: "cleanup"
      order: 4
      completion: tasks_done
      optional: true
      config:
        type: cleanup
        expected_objects: 10000
```

### Stage Types

#### Execute Stage

Runs read/write workload for fixed duration:

```yaml
- name: "benchmark"
  order: 1
  completion: duration
  config:
    type: execute
    duration: "10m"
```

**Use cases**: Performance testing, sustained load, throughput measurement

#### Prepare Stage

Creates baseline objects before workload:

```yaml
- name: "data-generation"
  order: 1
  completion: tasks_done
  config:
    type: prepare
    expected_objects: 50000  # Optional: for progress tracking
```

**Note**: Actual prepare configuration comes from top-level `prepare` section.

#### Cleanup Stage

Deletes objects after test completion:

```yaml
- name: "teardown"
  order: 4
  completion: tasks_done
  optional: true  # Allow test to succeed even if cleanup fails
  timeout_secs: 1800
  config:
    type: cleanup
    expected_objects: 50000
```

**Best practice**: Mark cleanup as `optional: true` to avoid masking test failures.

#### Validation Stage

Pre-flight configuration and environment checks:

```yaml
- name: "preflight"
  order: 1
  completion: validation_passed
  timeout_secs: 300
  config:
    type: validation
    timeout_secs: 300
```

**Validation checks** (performed at controller):
- Agent connectivity (gRPC health check)
- Configuration consistency across agents
- File/directory existence (if configured)

#### Hybrid Stage

Combines duration and task limits (whichever completes first):

```yaml
- name: "timed-prepare"
  order: 1
  completion: duration_or_tasks
  config:
    type: hybrid
    max_duration: "30m"
    expected_tasks: 100000
```

**Use case**: "Create 100k objects OR run for 30 minutes, whichever comes first"

#### Custom Stage

Execute custom scripts or commands:

```yaml
- name: "format-conversion"
  order: 2
  completion: script_exit
  timeout_secs: 3600
  config:
    type: custom
    command: "/usr/local/bin/convert-dataset"
    args: ["--input", "/data/raw", "--output", "/data/formatted"]
```

**Note**: Custom stages run on agent hosts, not controller.

### Completion Criteria

Each stage defines **how it knows when it's done**:

**`duration`**: Complete after fixed time period
```yaml
completion: duration
config:
  type: execute
  duration: "5m"
```

**`tasks_done`**: Complete when all tasks finished
```yaml
completion: tasks_done
config:
  type: prepare
  expected_objects: 10000
```

**`validation_passed`**: Complete when validation checks pass
```yaml
completion: validation_passed
config:
  type: validation
```

**`script_exit`**: Complete when external command exits
```yaml
completion: script_exit
config:
  type: custom
  command: "./my-script.sh"
```

**`duration_or_tasks`**: Complete when either criterion met (hybrid)
```yaml
completion: duration_or_tasks
config:
  type: hybrid
  max_duration: "1h"
  expected_tasks: 500000
```

### Multi-Stage Examples

**Multi-epoch ML training simulation**:

```yaml
distributed:
  stages:
    - name: "epoch-1"
      order: 1
      completion: duration
      config:
        type: execute
        duration: "10m"
    
    - name: "checkpoint-1"
      order: 2
      completion: tasks_done
      config:
        type: custom
        command: "save-checkpoint"
        args: ["--epoch", "1"]
    
    - name: "epoch-2"
      order: 3
      completion: duration
      config:
        type: execute
        duration: "10m"
    
    - name: "checkpoint-2"
      order: 4
      completion: tasks_done
      config:
        type: custom
        command: "save-checkpoint"
        args: ["--epoch", "2"]
```

**Tiered testing** (small → medium → large scale):

```yaml
distributed:
  stages:
    - name: "smoke-test"
      order: 1
      completion: duration
      config:
        type: execute
        duration: "1m"
    
    - name: "medium-test"
      order: 2
      completion: duration
      optional: true  # Allow skipping if smoke test fails
      config:
        type: execute
        duration: "5m"
    
    - name: "full-test"
      order: 3
      completion: duration
      config:
        type: execute
        duration: "30m"
```

### Default Stages (Removed)

Legacy default-stage generation was removed in v0.8.61. Distributed configs must define explicit stages. Use the config converter for older YAML files.

### Stage Ordering

**Critical**: Stages execute in **`order` field** sequence, NOT YAML position:

```yaml
stages:
  - name: "cleanup"
    order: 4         # Runs LAST (despite being first in YAML)
  
  - name: "prepare"
    order: 1         # Runs FIRST
  
  - name: "execute"
    order: 2         # Runs SECOND
```

**Best practice**: Use order values 1, 2, 3, 4... for clarity.

**Warning**: Duplicate order values cause validation error.

### Stage Output Files

Each stage produces numbered TSV results files:

```
sai3-20260205-1440-test_barriers/
├── 01_preflight_results.tsv
├── 02_prepare_results.tsv
├── 03_execute_results.tsv
└── 04_cleanup_results.tsv
```

**Benefits**:
- Tab ordering preserved in Excel (01→02→03→04)
- Clear execution progression
- Per-stage performance analysis

**Analysis tool**: `sai3-analyze` supports numbered stage files (v0.8.50+).

## Barrier Synchronization

**Introduced in v0.8.25**

Barrier synchronization ensures all agents reach coordination points before proceeding. Critical for:
- **Multi-stage workflows**: All agents must complete prepare before execute
- **Distributed testing**: Prevent timing skew between agents
- **Large-scale tests**: Coordinate 300k+ directory creation across hosts

**v0.8.61+**: Barriers are keyed by numeric **stage order** (stage index), not stage name strings.

### Why Barriers?

**Without barriers** (v0.8.24 and earlier):
```
Agent 1: Prepare (30s) → Execute starts at T=30
Agent 2: Prepare (45s) → Execute starts at T=45
Result: 15-second timing skew, invalid performance results
```

**With barriers** (v0.8.25+):
```
Agent 1: Prepare (30s) → Wait at barrier
Agent 2: Prepare (45s) → Reach barrier  
Both: Barrier released → Execute starts at T=45 (synchronized)
```

### Global Barrier Configuration

Enable barriers at distributed config level:

```yaml
distributed:
  # Global barrier settings
  barrier_sync:
    enabled: true                    # Enable barrier synchronization
    default_heartbeat_interval: 30   # Agent reports progress every 30s
    default_missed_threshold: 3      # Query agent after 3 missed heartbeats (90s)
    default_query_timeout: 10        # Wait 10s for query response
    default_query_retries: 2         # Retry query twice before giving up
```

**Applies to ALL stages** unless overridden by per-stage configuration.

### Per-Stage Barriers

Configure barriers independently for each stage:

```yaml
distributed:
  barrier_sync:
    enabled: true
    
    # Validation phase barrier
    validation:
      type: all_or_nothing
      heartbeat_interval: 30
      agent_barrier_timeout: 300
    
    # Prepare phase barrier (longer timeout for large datasets)
    prepare:
      type: all_or_nothing
      heartbeat_interval: 30
      agent_barrier_timeout: 600  # 10 minutes for 300k dirs
    
    # Execute phase barrier
    execute:
      type: all_or_nothing
      heartbeat_interval: 30
      agent_barrier_timeout: 300
    
    # Cleanup phase barrier (best effort)
    cleanup:
      type: best_effort           # Don't block on cleanup failures
      agent_barrier_timeout: 300
```

### Barrier Types

**`all_or_nothing`** (strict synchronization):
- **ALL agents** must reach barrier
- Missing agents cause entire workload to abort
- **Use case**: Critical stages where consistency required (prepare, execute)

```yaml
barrier:
  type: all_or_nothing
  agent_barrier_timeout: 300
```

**`majority`** (fault-tolerant):
- **>50% agents** must reach barrier
- Stragglers marked failed and excluded from next stage
- **Use case**: Large-scale tests where occasional agent failures acceptable

```yaml
barrier:
  type: majority
  agent_barrier_timeout: 600
```

**`best_effort`** (opportunistic):
- Proceed when liveness check fails on stragglers
- Stragglers continue independently (out of sync acceptable)
- **Use case**: Cleanup stages, non-critical operations

```yaml
barrier:
  type: best_effort
  agent_barrier_timeout: 180
```

### Barrier Timing Parameters

**`heartbeat_interval`**: How often agents report progress (seconds)
- Default: 30 seconds
- Recommendation: 30-60 for normal tests
- Too low: Excessive network traffic
- Too high: Slow failure detection

**`missed_threshold`**: Missed heartbeats before query
- Default: 3 (= 90 seconds with 30s interval)
- Calculation: `timeout_before_query = interval × threshold`

**`query_timeout`**: Timeout for explicit agent query (seconds)
- Default: 10 seconds
- How long to wait for query response before declaring failure

**`query_retries`**: Retries for agent query
- Default: 2 
- Total query time: `timeout × (retries + 1) = 10 × 3 = 30s`

**`agent_barrier_timeout`**: Agent-side wait timeout (seconds)
- Default: 120 seconds
- How long agent waits for barrier release from controller
- **Critical sizing**: Must be > `(heartbeat_interval × missed_threshold) + (query_timeout × query_retries)`
- Large-scale tests (300k+ dirs): Use 600+ seconds

### Barrier Configuration Examples

**Standard test** (normal synchronization):

```yaml
distributed:
  barrier_sync:
    enabled: true
    default_heartbeat_interval: 30
    default_missed_threshold: 3
    default_query_timeout: 10
    agent_barrier_timeout: 120  # 2 minutes
  
  agents:
    - address: "host1:7761"
    - address: "host2:7761"
```

**Large-scale test** (300k+ directories):

```yaml
distributed:
  barrier_sync:
    enabled: true
    
    prepare:
      type: all_or_nothing
      heartbeat_interval: 60          # Report less frequently
      missed_threshold: 5             # 5 × 60s = 5 minutes before query
      agent_barrier_timeout: 600      # 10 minutes for massive prepare
```

**Fault-tolerant distributed test**:

```yaml
distributed:
  barrier_sync:
    enabled: true
    
    prepare:
      type: all_or_nothing    # All agents must complete prepare
    
    execute:
      type: majority          # >50% agents sufficient for execute
    
    cleanup:
      type: best_effort       # Don't block on cleanup failures
```

### Barrier Troubleshooting

**Problem**: "Barrier timeout exceeded" error

**Causes**:
1. Agent still working (operation not complete)
2. Agent crashed/disconnected
3. `agent_barrier_timeout` too short for operation

**Solutions**:
```yaml
# For long-running operations (large prepare)
barrier:
  agent_barrier_timeout: 1800  # 30 minutes

# For flaky networks
barrier:
  heartbeat_interval: 15       # More frequent heartbeats
  query_retries: 5             # More retries
```

**Problem**: Agents out of sync despite barriers

**Cause**: Barriers disabled or misconfigured

**Fix**:
```yaml
distributed:
  barrier_sync:
    enabled: true  # Must be explicitly enabled!
```

## Timeout Configuration

**Introduced in v0.8.50, enhanced in v0.8.52 with human-readable time units**

Comprehensive timeout system prevents indefinite hangs in distributed testing. Three categories of timeouts:

1. **Agent operation timeouts**: How long agents wait for storage operations
2. **Barrier synchronization timeouts**: How long agents wait at coordination barriers
3. **gRPC communication timeouts**: How long controller waits for RPC responses

**Time Unit Support (v0.8.52+)**: All timeout fields support human-readable units:
- **Units**: `s` (seconds), `m` (minutes), `h` (hours), `d` (days)
- **Examples**: `"30s"`, `"5m"`, `"2h"`, `"1d"`
- **Backward compatible**: Plain integers still work (interpreted as seconds)

### Agent Operation Timeouts

Configure per-stage timeouts for agent operations (data creation, workload execution, cleanup):

```yaml
distributed:
  stages:
    - name: "prepare"
      order: 1
      completion: tasks_done
      timeout_secs: "10m"       # 10 minutes (600 seconds) - with time units
      config:
        type: prepare
    
    - name: "execute"
      order: 2
      completion: duration
      timeout_secs: "1h"        # 1 hour (3600 seconds) - with time units
      config:
        type: execute
        duration: "30m"          # Execute for 30 minutes
    
    - name: "cleanup"
      order: 3
      completion: tasks_done
      timeout_secs: 1800        # 30 minutes - plain integer still works
      config:
        type: cleanup
```

**Default timeouts** (if not specified):
- Prepare: 600 seconds (10 minutes)
- Execute: 3600 seconds (1 hour)
- Cleanup: 3600 seconds (1 hour)
- Validation: 300 seconds (5 minutes)

**Minimum timeout**: 60 seconds (validation enforced at config load)

### Barrier Synchronization Timeouts

Control how long agents wait at barriers (see [Barrier Synchronization](#barrier-synchronization)):

```yaml
distributed:
  barrier_sync:
    enabled: true
    
    # Using time units for better readability (v0.8.52+)
    prepare:
      agent_barrier_timeout: "10m"  # 10 minutes for large dataset prepare
    
    execute:
      agent_barrier_timeout: "5m"   # 5 minutes for execute barrier
    
    # Plain integers still work (interpreted as seconds)
    cleanup:
      agent_barrier_timeout: 180     # 3 minutes (180 seconds)
```

**Key insight**: Barrier timeout is **separate from** agent operation timeout:
- **Agent timeout**: How long agent works on operation
- **Barrier timeout**: How long agent waits for OTHER agents at barrier

### gRPC Communication Timeouts

Configure gRPC call timeouts at distributed config level:

```yaml
distributed:
  # Using time units (v0.8.52+) - more readable
  grpc_keepalive_interval: "30s"   # PING frames every 30 seconds
  grpc_keepalive_timeout: "10s"    # Wait 10s for PONG before disconnect
  agent_ready_timeout: "2m"        # 2 minutes for agents to send READY
  start_delay: "5s"                # 5 second delay before coordinated start
  
  # Or use plain integers (backward compatible, interpreted as seconds)
  # grpc_keepalive_interval: 30
  # grpc_keepalive_timeout: 10
  
  agents:
    - address: "host1:7761"
    - address: "host2:7761"
```

**Parameters**:

**`grpc_keepalive_interval`**: How often PING frames sent
- Default: 30 seconds
- Format: `"30s"`, `"1m"`, or plain integer `30`
- Use case: Detect dead connections quickly
- For slow operations (>30s): Increase to `"1m"` or `"2m"` to avoid spurious disconnects

**`grpc_keepalive_timeout`**: Wait time for PONG response
- Default: 10 seconds
- Format: `"10s"`, `"30s"`, or plain integer `10`
- Total disconnect detection time: `interval + timeout = 40s`
- **v0.8.51 Recommendation for large-scale**: Increase to `"30s"` for deployments with >100K files

**`agent_ready_timeout`**: *(v0.8.51 new)* Timeout for agents to send initial READY signal
- Default: 120 seconds (2 minutes)
- Format: `"2m"`, `"5m"`, `"10m"`, or plain integer `120`
- Agents perform config validation (including glob pattern expansion) before sending READY
- Large directory structures require longer timeouts
- **Recommendations by scale**:
  - Small (<10K files): 60 seconds
  - Medium (10K-100K files): 120 seconds (default)
  - Large (100K-1M files): 300 seconds (5 minutes)
  - Very large (>1M files): 600 seconds (10 minutes)

**Example for large-scale deployment**:
```yaml
distributed:
  grpc_keepalive_interval: 30
  grpc_keepalive_timeout: 30      # Increased from 10 for heavy I/O
  agent_ready_timeout: 300        # 5 min for large glob validation
```

### Health Check Timeouts

**Fixed at 30 seconds** (not configurable):

- Controller sends health check RPC to each agent
- 30-second timeout for response
- Failure indicates agent unreachable/crashed

**Use case**: Rapid failure detection during test startup.

### Timeout Configuration Best Practices

**For normal tests** (small datasets, fast storage):

```yaml
distributed:
  grpc_keepalive_interval: 30
  grpc_keepalive_timeout: 10
  
  stages:
    - name: "prepare"
      timeout_secs: 600      # 10 minutes
      barrier:
        agent_barrier_timeout: 300
    
    - name: "execute"
      timeout_secs: 3600     # 1 hour
      barrier:
        agent_barrier_timeout: 300
```

**For large-scale tests** (300k+ dirs, 64M+ files):

```yaml
distributed:
  grpc_keepalive_interval: 60    # Less frequent (long operations expected)
  grpc_keepalive_timeout: 20
  
  stages:
    - name: "prepare"
      timeout_secs: 3600          # 1 hour for massive dataset
      barrier:
        agent_barrier_timeout: 600  # 10 minutes for barrier sync
```

**For flaky networks**:

```yaml
distributed:
  grpc_keepalive_interval: 15    # Frequent keepalives
  grpc_keepalive_timeout: 30     # Generous timeout
  
  barrier_sync:
    default_query_retries: 5     # More retries
    default_query_timeout: 20    # Longer timeout per query
```

### Timeout Hierarchy

When multiple timeouts apply, the **most specific wins**:

1. **Per-stage timeout** (highest priority)
2. **Per-phase barrier timeout**
3. **Default barrier timeout**
4. **gRPC keepalive timeout**
5. **Health check timeout** (fixed 30s)

Example:
```yaml
distributed:
  barrier_sync:
    enabled: true
    default_heartbeat_interval: 30
    prepare:
      agent_barrier_timeout: 900  # Prepare-specific override
  
  stages:
    - name: "prepare"
      timeout_secs: 1800           # Agent operation timeout
      barrier:
        agent_barrier_timeout: 900  # Stage-specific barrier timeout (wins)
```

### Timeout Troubleshooting

**Problem**: "Agent operation timeout exceeded"

**Cause**: Agent operation (prepare/execute/cleanup) took longer than `timeout_secs`

**Solution**: Increase stage timeout:
```yaml
stages:
  - name: "prepare"
    timeout_secs: "1h"  # Was "10m" (600s), now 1 hour - using time units
```

**Problem**: "gRPC deadline exceeded"

**Cause**: gRPC call to agent timed out

**Solutions**:
1. Increase gRPC keepalive timings
2. Check network connectivity
3. Check agent is responsive (not hung)

```yaml
distributed:
  grpc_keepalive_interval: "1m"    # Was "30s", using time units
  grpc_keepalive_timeout: "30s"    # Was "10s"
  # Or use plain integers: 60 and 30
```

**Problem**: Operations take 60+ seconds but gRPC disconnects

**Cause**: gRPC keepalive interval too short for slow operations

**Solution**: Increase keepalive interval:
```yaml
distributed:
  grpc_keepalive_interval: "2m"  # 2 minutes for very slow operations (using time units)
  # Or: grpc_keepalive_interval: 120  # Plain integer (seconds) also works
```

## Distributed Testing

Distributed testing enables coordinated multi-host execution with automated deployment, per-agent customization, and sophisticated synchronization.

### Agent Configuration

Define agent addresses and customizations:

```yaml
distributed:
  shared_filesystem: true        # Storage shared across agents?
  tree_creation_mode: coordinator  # Who creates directory tree?
  path_selection: partitioned    # How agents select paths?
  
  # Time-related settings support human-readable units (v0.8.52+)
  start_delay: "30s"             # Delay before starting (or plain: 30)
  grpc_keepalive_interval: "1m"  # Keepalive interval (or plain: 60)
  agent_ready_timeout: "5m"      # Agent ready timeout (or plain: 300)
  
  agents:
    - address: "host1.example.com:7761"
      id: "node-1"
      concurrency_override: 64     # Override global concurrency
      target_override: "file:///mnt/filesys1/benchmark/"
      env:
        RUST_LOG: "debug"
        AWS_PROFILE: "benchmark"
      volumes:
        - "/mnt/nvme:/data"
        - "/tmp/results:/results:ro"
    
    - address: "host2.example.com:7761"
      id: "node-2"
      concurrency_override: 128
```

**Per-agent fields**:

- **`address`**: Agent hostname:port or IP:port
- **`id`**: Friendly identifier (default: derived from address)
- **`concurrency_override`**: Override global concurrency setting
- **`target_override`**: Override base target URI
- **`env`**: Environment variables injected into agent process/container
- **`volumes`**: Docker volume mounts (format: `"host:container"` or `"host:container:mode"`)
- **`path_template`**: Custom path template override
- **`listen_port`**: Agent listen port (SSH mode only, default: 7761)
- **`multi_endpoint`**: Per-agent multi-endpoint override (see [Multi-Endpoint](#multi-endpoint-load-balancing))
- **`kv_cache_dir`**: Per-agent KV cache directory override (v0.8.60+, see [KV Cache Isolation](#kv-cache-isolation))

### KV Cache Isolation

**Critical for accurate performance measurements** (v0.8.60+)

The metadata KV cache uses LSM (Log-Structured Merge-tree) operations that generate random small-block I/O. To prevent contaminating workload measurements, the cache is stored separately during preparation, then copied back to the storage under test.

**Global default** (all agents use same base directory):

```yaml
distributed:
  kv_cache_dir: "/mnt/local-ssd/kv-cache"  # Fast local storage
  agents:
    - address: "host1:7761"
      id: "agent-1"  # Uses /mnt/local-ssd/kv-cache/sai3-cache-agent-0-{hash}/
    - address: "host2:7761"
      id: "agent-2"  # Uses /mnt/local-ssd/kv-cache/sai3-cache-agent-1-{hash}/
```

**Per-agent override** (heterogeneous storage):

```yaml
distributed:
  kv_cache_dir: "/mnt/slow-disk/kv-cache"  # Default for most agents
  agents:
    - address: "host1:7761"
      id: "agent-1"
      kv_cache_dir: "/mnt/nvme/kv-cache"  # Fast local NVMe
    - address: "host2:7761"
      id: "agent-2"
      # Uses global default: /mnt/slow-disk/kv-cache/
```

**Default behavior** (no configuration):

```yaml
distributed:
  agents:
    - address: "host1:7761"
      # kv_cache_dir omitted → defaults to /tmp/sai3-cache-agent-0-{hash}/
```

**How it works**:

1. **During prepare**: KV cache writes go to isolated location (`kv_cache_dir` or `/tmp/`)
2. **After prepare**: Cache copied back to storage under test (e.g., `file:///mnt/test/.sai3-cache-agent-0/`)
3. **Benefits**: 
   - LSM operations (journals, compaction, version files) don't pollute workload I/O
   - Random small-block I/O isolated from sequential large-block testing
   - Accurate performance measurements for storage under test

**Recommended**: Use fast local SSD/NVMe for `kv_cache_dir` to avoid I/O bottlenecks during object creation.

### KV Cache Checkpointing

**Automatic data loss protection** (v0.8.60+)

Long-running prepare operations (e.g., creating 100M objects over 12 hours) risk losing hours of work if interrupted. Periodic checkpointing saves cache state to storage under test at regular intervals.

**Top-level configuration** (applies to standalone AND distributed modes):

```yaml
cache_checkpoint_interval_secs: 300  # Checkpoint every 5 minutes (DEFAULT)
```

**Standalone mode example** (`sai3-bench run`):

```yaml
target: "s3://my-bucket/test/"
cache_checkpoint_interval_secs: 600  # Checkpoint every 10 minutes
prepare:
  strategy: tree
  # ... prepare config ...
```

**Distributed mode example** (`sai3bench-ctl`):

```yaml
cache_checkpoint_interval_secs: 300  # All agents checkpoint every 5 minutes
distributed:
  agents:
    - address: "host1:7761"
      id: "agent-1"  # Checkpoints to .sai3-cache-agent-1.tar.zst
    - address: "host2:7761"
      id: "agent-2"  # Checkpoints to .sai3-cache-agent-2.tar.zst
```

**Disable periodic checkpointing** (only checkpoint at end):

```yaml
cache_checkpoint_interval_secs: 0  # Disabled
```

**How it works**:

1. **During prepare**: Background task creates tar.zst archive of cache every N seconds
2. **Cloud storage** (s3://, az://, gs://): Upload archive via ObjectStore::put()
3. **Filesystem** (file://, direct://): Write .tar.zst file to disk
4. **Agent-specific**: Each agent creates its own checkpoint (`.sai3-cache-agent-{id}.tar.zst`)
5. **At most 5 minutes of lost work** (default) if prepare crashes

**Benefits**:
- Protects against data loss in long-running prepares (hours, days)
- Works for all storage backends (filesystem and cloud)
- Automatic retry with exponential backoff (5 attempts)
- Minimal overhead (~3x compression from zstd)

**Default**: 300 seconds (5 minutes) - chosen to balance safety vs. overhead.

### SSH Deployment

Automated SSH deployment and agent lifecycle management:

```yaml
distributed:
  ssh:
    enabled: true
    user: "benchuser"
    key_path: "~/.ssh/id_ed25519"
    timeout: 10
    known_hosts: "~/.ssh/known_hosts"
  
  deployment:
    deploy_type: "binary"                      # or "docker"
    binary_path: "/usr/local/bin/sai3bench-agent"
    container_runtime: "docker"                # or "podman"
    image: "sai3bench:v0.8.50"
    network_mode: "host"
    pull_policy: "if_not_present"
  
  agents:
    - address: "host1"              # Hostname only in SSH mode
      listen_port: 7761
    - address: "host2"
      listen_port: 7761
```

**SSH configuration**:

- **`enabled`**: Enable SSH automation (default: false)
- **`user`**: SSH username (default: current user)
- **`key_path`**: SSH private key path (default: `~/.ssh/id_rsa`)
- **`timeout`**: SSH connection timeout seconds (default: 10)
- **`known_hosts`**: Known hosts file (empty string = disable host key checking, INSECURE!)

**Deployment types**:

**Binary mode** (`deploy_type: binary`):
- Executes `sai3bench-agent` binary directly on remote host
- Requires binary pre-installed at `binary_path`
- Simpler, lower overhead than containers

**Docker mode** (`deploy_type: docker`):
- Launches agent in container
- Requires Docker/Podman installed on remote hosts
- Automatic image pull based on `pull_policy`
- Supports volume mounts and environment variables

### Path Selection Strategies

Control how agents select paths during workload execution:

**`random`** (maximum contention):
```yaml
distributed:
  path_selection: random
```
- All agents pick any directory randomly
- Maximum metadata contention
- Use case: Stress testing shared filesystem metadata servers

**`partitioned`** (reduced contention):
```yaml
distributed:
  path_selection: partitioned
  partition_overlap: 0.3  # 30% chance to access other partitions
```
- Agents prefer `hash(path) % agent_id` directories
- `partition_overlap` controls cross-partition access (0.0-1.0)
- Use case: Realistic distributed workload with some sharing

**`exclusive`** (minimal contention):
```yaml
distributed:
  path_selection: exclusive
```
- Each agent ONLY uses assigned directories
- Zero contention (purely isolated)
- Use case: Pure performance testing without metadata contention

**`weighted`** (probabilistic mix):
```yaml
distributed:
  path_selection: weighted
  partition_overlap: 0.5  # 50% local, 50% random
```
- Probabilistic mix of partitioned and random access
- Controlled by `partition_overlap`

### Tree Creation Modes

Control who creates directory tree structure:

**`isolated`**: Each agent creates separate tree
```yaml
distributed:
  tree_creation_mode: isolated
  path_template: "agent-{id}/"
```
- Agent 1: Creates `agent-1/` tree
- Agent 2: Creates `agent-2/` tree
- Use case: Per-agent storage (isolated disks, separate buckets)

**`coordinator`**: Controller creates tree once
```yaml
distributed:
  tree_creation_mode: coordinator
```
- Controller creates tree before agents start
- Agents skip prepare phase (tree already exists)
- Use case: Shared storage with expensive tree creation

**`concurrent`**: All agents create same tree
```yaml
distributed:
  tree_creation_mode: concurrent
```
- All agents attempt same mkdir operations
- Idempotent mkdir handles race conditions
- Use case: Shared storage with cheap idempotent mkdir

### Distributed Testing Example

Complete distributed test configuration:

```yaml
distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator
  path_selection: partitioned
  partition_overlap: 0.2
  start_delay: 2
  grpc_keepalive_interval: 30
  grpc_keepalive_timeout: 10
  
  barrier_sync:
    enabled: true
    default_heartbeat_interval: 30
    default_missed_threshold: 3
  
  ssh:
    enabled: true
    user: "benchuser"
    key_path: "~/.ssh/id_ed25519"
  
  deployment:
    deploy_type: "binary"
    binary_path: "/usr/local/bin/sai3bench-agent"
  
  agents:
    - address: "node1"
      id: "agent-1"
      concurrency_override: 64
    
    - address: "node2"
      id: "agent-2"
      concurrency_override: 64
    
    - address: "node3"
      id: "agent-3"
      concurrency_override: 128
  
  stages:
    - name: "preflight"
      order: 1
      completion: validation_passed
      timeout_secs: 300
      config:
        type: validation
    
    - name: "prepare"
      order: 2
      completion: tasks_done
      timeout_secs: 1800
      barrier:
        type: all_or_nothing
        agent_barrier_timeout: 600
      config:
        type: prepare
        expected_objects: 100000
    
    - name: "execute"
      order: 3
      completion: duration
      timeout_secs: 3600
      barrier:
        type: all_or_nothing
        agent_barrier_timeout: 300
      config:
        type: execute
        duration: "30m"
    
    - name: "cleanup"
      order: 4
      completion: tasks_done
      optional: true
      timeout_secs: 1800
      config:
        type: cleanup

target: "file:///mnt/shared/benchmark/"
duration: "30m"
concurrency: 64

prepare:
  ensure_objects:
    - base_uri: "file:///mnt/shared/benchmark/data/"
      count: 100000
      min_size: 1048576
      max_size: 1048576
      fill: random

workload:
  - op: get
    path: "data/*.dat"
    weight: 70
  
  - op: put
    path: "output/"
    object_size: 1048576
    weight: 30
```



## Page Cache Control (file:// URIs only)

The `page_cache_mode` field controls filesystem page cache behavior for `file://` URIs using `posix_fadvise()` hints on Linux/Unix systems.

### Supported Modes

- **`auto`** (default): Automatically selects based on file size
  - Sequential hints for files ≥ 64MB (better for large scans)
  - Random hints for files < 64MB (better for small random access)

- **`sequential`**: Optimize for sequential reads
  - Enables aggressive prefetching and large readahead
  - Best for: Large file scans, streaming workloads, backup/restore
  - Performance impact: 2-3x faster for large sequential reads

- **`random`**: Optimize for random access
  - Minimizes prefetching to reduce wasted I/O
  - Best for: Database-like workloads, small file random access
  - Performance impact: Better latency for small random reads

- **`dontneed`**: Drop pages from cache after read
  - Prevents cache pollution from benchmark data
  - Best for: Large dataset testing where you don't want to evict useful data
  - Performance impact: Keeps working set cache clean

- **`normal`**: Use default kernel behavior
  - No specific hints, kernel decides caching strategy
  - Best for: Mixed workloads or when unsure

### Platform Support

- **Linux/Unix**: Full support via `posix_fadvise()` system calls
- **Windows/Other**: Gracefully ignored (no-op, no errors)

### Configuration Examples

**Global configuration** (applies to all operations):

```yaml
target: "file:///data/benchmark/"
duration: "60s"
concurrency: 32
page_cache_mode: sequential  # All file operations use sequential hints

workload:
  - op: get
    path: "large-files/*.dat"
    weight: 100
```

**Per-operation override** (advanced):

```yaml
target: "file:///data/"
page_cache_mode: auto  # Default for all operations

workload:
  - op: get
    path: "sequential-data/*.dat"
    weight: 50
    # Inherits 'auto' mode
  
  - op: get
    path: "random-data/*.dat"
    weight: 50
    # Note: Per-op override not yet supported, uses global setting
```

### Performance Guidelines

**Sequential Mode** - Use when:
- Reading large files (>64MB) sequentially
- Streaming or backup/restore workloads
- High throughput is more important than latency

**Random Mode** - Use when:
- Accessing many small files randomly
- Database-like access patterns
- Low latency is more important than throughput

**DontNeed Mode** - Use when:
- Testing with large datasets that shouldn't stay in cache
- Benchmarking cold-cache scenarios
- Preventing cache pollution

**Auto Mode** - Use when:
- Mixed file sizes in workload
- Unsure of access pattern
- Want sensible defaults

### Technical Notes

- Only applies to `file://` URIs (not S3, Azure, GCS, or `direct://`)
- Requires s3dlio v0.9.7 or later
- Hints are advisory; kernel may ignore them under memory pressure
- No impact on correctness, only performance
- Dry-run mode displays current page cache configuration

## Operation Logging (v0.8.1+)

The `op_log_path` field enables s3dlio operation trace logging for detailed performance analysis and workload replay.

### Basic Configuration

```yaml
# Enable operation logging
op_log_path: /data/oplogs/benchmark.tsv.zst

target: "s3://my-bucket/data/"
duration: "60s"
concurrency: 32

workload:
  - op: get
    path: "objects/*"
    weight: 70
  - op: put
    path: "uploads/"
    object_size: 1048576
    weight: 30
```

### Distributed Agents

For distributed workloads, each agent automatically appends its `agent_id` to the filename to prevent collisions:

```yaml
op_log_path: /shared/storage/oplogs/trace.tsv.zst

distributed:
  agents:
    - address: "node1:7761"
      id: agent1
    - address: "node2:7761"
      id: agent2
```

**Results in**:
- `/shared/storage/oplogs/trace-agent1.tsv.zst` (operations from node1)
- `/shared/storage/oplogs/trace-agent2.tsv.zst` (operations from node2)

### Precedence Rules

- **YAML `op_log_path`** takes precedence over agent CLI `--op-log` flag
- Allows per-workload oplog control in distributed environments
- If both specified, YAML config wins

### Environment Variables

s3dlio oplog environment variables:

```bash
# Configure buffer size (default: 64KB)
export S3DLIO_OPLOG_BUF=131072

# Configure compression level (default: 3)
export S3DLIO_OPLOG_ZSTD_LEVEL=5
```

**Note**: Operation logs are NOT sorted during capture. Use post-processing:
```bash
# Sort by start timestamp after capture
./sai3-bench sort --files /data/oplog.tsv.zst
```

### Oplog Format

Operation logs are TSV (tab-separated values) with zstd compression:

```
idx  thread  op  client_id  n_objects  bytes  endpoint  file  error  start  first_byte  end  duration_ns
0    123     PUT            1          1048576 s3://    bucket/key       2025-11-21T...           2025-11-21T...  1234567
1    456     GET            1          1048576 s3://    bucket/key       2025-11-21T...           2025-11-21T...  987654
```

**Fields**:
- `idx`: Operation sequence number
- `thread`: Thread/worker ID
- `op`: Operation type (GET, PUT, DELETE, LIST, etc.)
- `bytes`: Data transferred
- `endpoint`: Storage backend URI
- `file`: Object/file path
- `start/end`: ISO8601 timestamps
- `duration_ns`: Latency in nanoseconds

### Analysis Examples

```bash
# Decompress and view first 20 operations
zstd -d < /data/oplogs/trace-agent1.tsv.zst | head -20

# Count total operations
zstd -d < /data/oplogs/trace-agent1.tsv.zst | wc -l

# Find slowest operations (if sorted)
zstd -d < /data/oplogs/trace-agent1.tsv.zst | sort -t$'\t' -k12 -n | tail -10

# Filter by operation type
zstd -d < /data/oplogs/trace-agent1.tsv.zst | awk -F'\t' '$3 == "GET"'
```

### Use Cases

- **Performance Analysis**: Identify slow operations, latency distribution per agent
- **Workload Replay**: Capture production traffic and replay at different speeds
- **Debugging**: Trace specific operations that failed or exceeded thresholds
- **Comparison**: Compare operation latencies across agents to identify hotspots
- **Optimization**: Analyze access patterns for caching or prefetching strategies

## Target URI

The `target` field sets the base URI for all operations. Paths in workload operations are relative to this base.

```yaml
target: "gs://my-bucket/test/"

workload:
  - op: get
    path: "data/*.dat"  # Resolves to: gs://my-bucket/test/data/*.dat
```

**Supported schemes**:
- `file://` - Local filesystem
- `direct://` - Direct I/O (high performance local)
- `s3://` - Amazon S3 or S3-compatible
- `az://` - Azure Blob Storage
- `gs://` or `gcs://` - Google Cloud Storage

## Pattern Syntax

### Glob Patterns (with wildcards)

sai3-bench uses **glob patterns** with `*` wildcards for matching multiple objects:

```yaml
workload:
  - op: get
    path: "data/prepared-*.dat"  # ✅ Matches: prepared-00000000.dat, prepared-00000001.dat, etc.
  
  - op: delete
    path: "archive/*"             # ✅ Matches all files in archive/ directory
```

### What's NOT Supported

**Brace expansions** (bash-style) are **NOT supported**:

```yaml
workload:
  - op: get
    path: "obj_{00000..19999}"    # ❌ ERROR: This is NOT supported
```

### Pattern Resolution Behavior

Different operations handle patterns differently:

| Operation | Pattern Support | Behavior |
|-----------|----------------|----------|
| **GET** | ✅ Glob patterns | Pre-resolves at startup, samples randomly |
| **DELETE** | ✅ Glob patterns | Pre-resolves at startup, samples randomly |
| **STAT** | ✅ Glob patterns | Pre-resolves at startup, samples randomly |
| **PUT** | ❌ No patterns | Generates unique names dynamically |
| **LIST** | ❌ No patterns | Operates on directory prefixes |

## Operation Types

### GET - Read Objects

Read existing objects. Requires pattern that matches existing objects.

```yaml
- op: get
  path: "data/prepared-*.dat"  # Pattern matching existing objects
  weight: 60                   # Relative weight (60% of operations)
  concurrency: 64              # Optional: override global concurrency
```

**Pattern resolution**:
```
Resolving 1 GET operation patterns...
Found 2000 objects for GET pattern: gs://bucket/data/prepared-*.dat
```

### PUT - Write Objects

Create new objects with auto-generated unique names.

```yaml
- op: put
  path: "output/"              # Base path (object names auto-generated)
  object_size: 1048576         # Fixed size (1 MiB)
  weight: 25

# OR with size distribution:
- op: put
  path: "output/"
  size_distribution:
    type: lognormal
    mean: 1048576
    std_dev: 524288
    min: 1024
    max: 10485760
  weight: 25
```

**Generated names**: `gs://bucket/output/obj_<random_u64>`

**Size specifications**:

1. **Fixed size** (backward compatible):
```yaml
object_size: 1048576  # Always 1 MiB
```

2. **Uniform distribution**:
```yaml
size_distribution:
  type: uniform
  min: 1048576   # 1 MiB
  max: 10485760  # 10 MiB
```

3. **Lognormal distribution** (realistic):
```yaml
size_distribution:
  type: lognormal
  mean: 1048576      # Average: 1 MiB
  std_dev: 524288    # Std dev: 512 KiB
  min: 1024          # Min: 1 KiB
  max: 10485760      # Max: 10 MiB
```

### DELETE - Remove Objects

Delete existing objects matching a pattern.

```yaml
- op: delete
  path: "temp/prepared-*.dat"  # Pattern matching objects to delete
  weight: 10
```

**Important**: Keep DELETE weight lower than PUT weight to avoid exhausting object pool.

### STAT - Query Metadata

Query object metadata (size, modification time, etc.) without downloading content.

```yaml
- op: stat
  path: "data/prepared-*.dat"  # Pattern matching objects to stat
  weight: 5
```

### LIST - List Directory

List all objects in a directory/prefix.

```yaml
- op: list
  path: "data/"        # Directory to list (no glob pattern needed)
  weight: 10
```

**Note**: LIST operates on directory prefixes, not individual objects. No pattern resolution occurs.

## Prepare Stage

The prepare stage creates baseline objects before the workload begins. It supports two strategies for object creation: **sequential** (default) and **parallel**.

### Configuration Options

```yaml
prepare:
  # Prepare strategy: controls how objects are created
  # Values: sequential (default), parallel
  prepare_strategy: parallel  # Optional, defaults to "sequential"
  
  # Delay after prepare completes (seconds) - for cloud storage eventual consistency
  # Default: 0 (no delay)
  # Recommended: 2-5 for cloud storage (S3, GCS, Azure)
  post_prepare_delay: 5
  
  # Cleanup-only mode: skip workload and only run cleanup (v0.8.7+)
  # Use this to delete objects from interrupted benchmarks without re-running prepare
  cleanup_only: true  # Optional, defaults to false
  
  # Cleanup mode: error handling strategy for deletions (v0.8.7+)
  # Values: strict, tolerant (default), best_effort
  # - strict: Fail on any error (including "not found")
  # - tolerant: Ignore "not found" errors, fail on other errors  
  # - best_effort: Log all errors but continue (never fail)
  cleanup_mode: tolerant
  
  # Skip verification: don't list existing objects before prepare (v0.8.7+)
  # When true: Generate object list from config (fast, no storage listing)
  # When false: List existing objects first (expensive for large datasets)
  # Recommended: true for cleanup-only mode (avoids N×listing in distributed mode)
  skip_verification: true
  
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 2000            # Create 2000 objects
      min_size: 1048576      # Minimum size: 1 MiB
      max_size: 1048576      # Maximum size: 1 MiB (same = fixed size)
      fill: random           # Fill pattern: random (default) or zero (DO NOT USE)
      dedup_factor: 1        # Deduplication factor (1 = no dedup)
      compress_factor: 1     # Compression factor (1 = no compression)
  cleanup: true              # Remove prepared objects after test
```

**Note: Thousand Separators in YAML Numbers (v0.8.52+)**

For improved readability, numeric values support thousand separators (commas, underscores, or spaces):

```yaml
prepare:
  directory_structure:
    width: 100
    depth: 3
    files_per_dir: 10,000     # Comma separator (more readable than 10000)
    distribution: uniform
  
  ensure_objects:
    - count: 1,000,000         # 1 million objects
      min_size: 1,048,576      # 1 MiB (1024 × 1024)
      max_size: 104,857,600    # 100 MiB
    
    # Alternative: underscore separators (common in Rust/Python)
    - count: 500_000
      min_size: 524_288        # 512 KiB
    
    # Alternative: space separators (common in some locales)
    - count: 250 000
      min_size: 262 144        # 256 KiB
```

### Cleanup-Only Mode (v0.8.7+)

Run cleanup without prepare or workload phases - useful for cleaning up after interrupted benchmarks:

```yaml
duration: "0s"  # Skip workload
workload: []    # No operations

prepare:
  cleanup_only: true         # Skip prepare, only run cleanup
  cleanup_mode: tolerant     # Ignore "not found" errors
  skip_verification: true    # Don't list objects (use config to generate list)
  cleanup: true              # Required for cleanup to execute
  ensure_objects:
    - base_uri: "s3://bucket/test/"
      count: 1000000          # Number of objects that were created
      size_spec: {fixed: 2048}  # Not used for cleanup, but required
```

**When to use**:
- Interrupted benchmarks left objects behind
- Need to clean up distributed prepare phase across multiple agents
- Testing cleanup logic independently

**Performance notes**:
- `skip_verification: true` is **highly recommended** for large datasets
  - Avoids expensive storage listing (can take 30+ minutes for millions of objects)
  - Uses deterministic algorithm to generate deletion list from config
  - Each agent knows exactly which objects to delete (via modulo distribution)
- `skip_verification: false` lists existing objects first
  - Expensive: N agents × full object listing
  - Only use for small datasets or when unsure which objects exist

### Prepare Strategies

#### Sequential Strategy (Default)
Processes each `ensure_objects` entry in order. Creates all objects for the first entry, then moves to the second, etc.

**Characteristics**:
- **Predictable ordering**: Objects are numbered sequentially within each entry
- **Deterministic**: Same config always produces same order
- **Best for**: Workloads requiring specific object ordering or when debugging

**Example**:
```yaml
prepare:
  prepare_strategy: sequential  # Default, can be omitted
  ensure_objects:
    - base_uri: "s3://bucket/small/"
      count: 100
      size_spec: {fixed: 32KB}
    - base_uri: "s3://bucket/large/"
      count: 50
      size_spec: {fixed: 1MB}
```
Creates: All 100 small objects first, then all 50 large objects.

#### Parallel Strategy
Interleaves all `ensure_objects` entries for maximum throughput. Shuffles object sizes across all entries to avoid clustering.

**Characteristics**:
- **Better throughput**: Better storage pipeline utilization
- **Size mixing**: Each directory gets a mix of all file sizes
- **Best for**: Large-scale prepare operations where order doesn't matter

**Example**:
```yaml
prepare:
  prepare_strategy: parallel
  ensure_objects:
    - base_uri: "s3://bucket/dir1/"
      count: 1000
      size_spec: {uniform: {min: 1KB, max: 1MB}}
    - base_uri: "s3://bucket/dir2/"
      count: 1000
      size_spec: {uniform: {min: 1KB, max: 1MB}}
```
Creates: Mixes sizes from both entries, avoiding all 32KB objects followed by all 1MB objects.

### Live Performance Monitoring (v0.7.2+)

During prepare execution, you'll see real-time performance statistics:

```
[00:00:09] [████████] 5000/5000 objects 32 workers | 464 ops/s | 487.0 MiB/s | avg 58.2ms
```

After completion, a comprehensive performance summary is displayed:

```
Prepare Performance:
  Total ops: 5000 (3709.58 ops/s)
  Total bytes: 5242880000 (5000.00 MiB)
  Throughput: 3709.58 MiB/s
  Latency: mean=6.09ms, p50=4.11ms, p95=16.91ms, p99=42.49ms
```

Metrics are also exported to `prepare_results.tsv` for machine-readable analysis.

### Multiple Object Sets

You can prepare multiple object sets with different characteristics:

```yaml
prepare:
  prepare_strategy: parallel      # Use parallel for better throughput
  post_prepare_delay: 3           # Wait 3 seconds after creating objects
  ensure_objects:
    - base_uri: "gs://bucket/small/"
      count: 10000
      min_size: 1024
      max_size: 102400
    - base_uri: "gs://bucket/large/"
      count: 100
      min_size: 104857600    # 100 MiB
      max_size: 1073741824   # 1 GiB
```

**Post-Prepare Delay**: The `post_prepare_delay` field controls how long to wait after creating objects before starting the workload. This is essential for cloud storage backends that have eventual consistency:
- **Local storage** (`file://`, `direct://`): 0 seconds (no delay needed)
- **Cloud storage** (S3, GCS, Azure): 2-5 seconds recommended
- **Large object counts** (>1000 objects): 5-10 seconds recommended

The delay only applies if new objects were created. If all objects already existed, no delay occurs.

### Object Naming

Prepare creates objects named `prepared-NNNNNNNN.dat` where N is zero-padded 8-digit number.

Examples:
- `prepared-00000000.dat`
- `prepared-00000001.dat`
- `prepared-00001234.dat`

**Match your workload patterns accordingly**:
```yaml
prepare:
  post_prepare_delay: 3
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 1000

workload:
  - op: get
    path: "data/prepared-*.dat"  # ✅ Matches prepare naming
```

## Size Distributions

Available in both prepare stage and PUT operations.

### Fixed Size
```yaml
object_size: 1048576  # Always exactly 1 MiB
```

### Uniform Distribution
Evenly distributed between min and max:
```yaml
size_distribution:
  type: uniform
  min: 1048576
  max: 10485760
```

### Lognormal Distribution
Realistic distribution (many small, few large files):
```yaml
size_distribution:
  type: lognormal
  mean: 1048576
  std_dev: 524288
  min: 1024
  max: 10485760
```

## Weight System

Weights determine the relative frequency of operations:

```yaml
workload:
  - op: get
    weight: 60   # 60% of operations
  - op: put
    weight: 25   # 25% of operations
  - op: delete
    weight: 10   # 10% of operations
  - op: stat
    weight: 5    # 5% of operations
```

**Best practices**:
1. Ensure `PUT weight ≥ DELETE weight` to avoid exhausting object pool
2. Total weights don't need to sum to 100 (they're relative)
3. Use realistic ratios based on production workloads

## Concurrency Control

### Global Concurrency
```yaml
concurrency: 32  # 32 parallel workers for all operations
```

### Per-Operation Concurrency
```yaml
workload:
  - op: get
    path: "data/*.dat"
    weight: 60
    concurrency: 64  # Override: use 64 workers for GET operations
```

## Complete Example

```yaml
# Production-like mixed workload for Google Cloud Storage
target: "gs://production-bucket/benchmarks/"
duration: "5m"
concurrency: 32

prepare:
  ensure_objects:
    - base_uri: "gs://production-bucket/benchmarks/data/"
      count: 5000
      min_size: 1048576
      max_size: 1048576
      fill: random
  cleanup: true

workload:
  # Read existing objects (60%)
  - op: get
    path: "data/prepared-*.dat"
    weight: 60
  
  # Create new objects (25%)
  - op: put
    path: "data/new-"
    size_distribution:
      type: lognormal
      mean: 1048576
      std_dev: 524288
      min: 1024
      max: 10485760
    weight: 25
  
  # Delete objects (10%)
  - op: delete
    path: "data/prepared-*.dat"
    weight: 10
  
  # Query metadata (5%)
  - op: stat
    path: "data/prepared-*.dat"
    weight: 5
```

## Common Pitfalls

### ❌ Using Brace Expansions
```yaml
- op: get
  path: "obj_{00000..19999}"  # ERROR: Not supported
```

**Fix**: Use glob patterns:
```yaml
- op: get
  path: "prepared-*.dat"  # ✅ Correct
```

### ❌ DELETE Weight Too High
```yaml
workload:
  - op: put
    weight: 10
  - op: delete
    weight: 30  # ERROR: Deletes faster than PUT creates!
```

**Fix**: Ensure PUT ≥ DELETE:
```yaml
workload:
  - op: put
    weight: 30
  - op: delete
    weight: 10  # ✅ Correct
```

### ❌ Pattern Doesn't Match Prepared Objects
```yaml
prepare:
  ensure_objects:
    - base_uri: "gs://bucket/data/"
      count: 1000  # Creates: prepared-NNNNNNNN.dat

workload:
  - op: get
    path: "data/obj-*.dat"  # ERROR: No match!
```

**Fix**: Match the prepare naming:
```yaml
workload:
  - op: get
    path: "data/prepared-*.dat"  # ✅ Correct
```

## See Also

- [Usage Guide](USAGE.md) - Getting started with sai3-bench
- [Data Generation Guide](DATA_GENERATION.md) - Fill patterns, deduplication, and compression testing
- [Examples Directory](../examples/) - Complete example configurations
- [CHANGELOG](CHANGELOG.md) - Version history and breaking changes
