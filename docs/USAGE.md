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
- **Open firewall** for the agent port (default: `7761`)

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
  --listen <addr>       Listen address (default: 0.0.0.0:7761)
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
    - address: "node1.example.com:7761"
      id: "agent-1"
    - address: "node2.example.com:7761"
      id: "agent-2"
```

```bash
# No --agents flag needed
./sai3bench-ctl run --config workload.yaml
```

### Option 2: CLI Only (Quick Testing)
```bash
# Specify on command line
./sai3bench-ctl --agents node1:7761,node2:7761 run --config workload.yaml
```

### Option 3: Both (Config Takes Precedence)
```bash
# Config YAML agents override CLI agents
./sai3bench-ctl --agents localhost:7761,localhost:7762 run --config workload.yaml
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
    - address: "node1:7761"
      multi_endpoint:
        endpoints: ["file:///mnt/vast1/"]
    - address: "node2:7761"
      multi_endpoint:
        endpoints: ["file:///mnt/vast5/"]

prepare:
  ensure_objects:
    - base_uri: "file:///mnt/vast1/"  # ❌ BUG - only node1 can access this!
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
    - address: "node1:7761"
      multi_endpoint:
        endpoints: ["file:///mnt/shared/"]  # Different mount, same storage
    - address: "node2:7761"
      multi_endpoint:
        endpoints: ["file:///mnt/shared/"]

prepare:
  ensure_objects:
    - base_uri: "file:///mnt/shared/test/"  # ✅ OK - all agents can access
      use_multi_endpoint: true
```

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

## Run agent without TLS on port 7761
./target/release/sai3bench-agent --listen 127.0.0.1:7761
In another terminal:

## Controller talking to that agent (plaintext is default):
./target/release/sai3bench-ctl --agents 127.0.0.1:7761 ping

## Example GET workload (jobs = concurrency for downloads)
./target/release/sai3bench-ctl --agents 127.0.0.1:7761 get \
  --uri s3://my-bucket/path/ --jobs 8

# 4 Multi-Host (PLAINTEXT)
On each agent host (e.g., node1, node2):

./sai3bench-agent --listen 0.0.0.0:7761
From the controller host:

./sai3bench-ctl --agents node1:7761,node2:7761 ping

./sai3bench-ctl --agents node1:7761,node2:7761 get \
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
  --listen 0.0.0.0:7761 \
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
  --agents loki-node3:7761 \
  --agent-ca /tmp/agent_ca.pem \
  ping
```

If you connect by an alternate name or IP that’s in the SANs, you may need
--agent-domain to set the SNI / TLS server_name to match the certificate:

## Connecting to the agent by IP, telling TLS to expect "loki-node3" (in SANs)
```
./sai3bench-ctl \
  --agents 10.10.0.23:7761 \
  --agent-ca /tmp/agent_ca.pem \
  --agent-domain loki-node3 \
  ping
```

Multiple agents (all in TLS mode):

```
./sai3bench-ctl \
  --agents loki-node3:7761,loki-node4:7761 \
  --agent-ca /tmp/agent_ca.pem \
  ping
```
```
./sai3bench-ctl \
  --agents loki-node3:7761,loki-node4:7761 \
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
./sai3bench-ctl --agents node1:7761,node2:7761 \
  --start-delay 5 \  # 5s instead of 2s (total: 15s coordinated start)
  run --config workload.yaml
```

For more details on the implementation, see `docs/DISTRIBUTED_LIVE_STATS_IMPLEMENTATION.md`.

# 7 Examples for Workloads
GET (download) via controller
### PLAINTEXT (Default)
```
./sai3bench-ctl --agents node1:7761 get \
  --uri s3://my-bucket/prefix/ --jobs 16
```

### TLS
```
./sai3bench-ctl --agents node1:7761 \
  --agent-ca /tmp/agent_ca.pem \
  get --uri s3://my-bucket/prefix/ --jobs 16
```
--uri accepts a single object (s3://bucket/key), a prefix (s3://bucket/prefix/), or a simple glob under a prefix (e.g., s3://bucket/prefix/*).

--jobs controls per-agent concurrency for GET.
PUT (upload) via controller

## Create N objects of size S under bucket/prefix, using M concurrency per agent.

### PLAINTEXT
```
./sai3bench-ctl --agents node1:7761 put \
  --bucket my-bucket \
  --prefix test/ \
  --object-size 1048576 \
  --objects 100 \
  --concurrency 8
```

### TLS
```
./sai3bench-ctl --agents node1:7761 \
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
./target/release/sai3bench-agent --listen 127.0.0.1:7761
```

### Terminal B — controller
```
./target/release/sai3bench-ctl --agents 127.0.0.1:7761 ping
```

```
./target/release/sai3bench-ctl --agents 127.0.0.1:7761 get \
  --uri s3://my-bucket/prefix/ --jobs 4
```

## For TLS on localhost

### Terminal A — agent with TLS & SANs covering "localhost" and "127.0.0.1"
```
./sai3bench-agent --listen 127.0.0.1:7761 --tls \
  --tls-domain localhost \
  --tls-sans "localhost,127.0.0.1" \
  --tls-write-ca /tmp/agent-ca
```


### Terminal B — controller
```
./sai3bench-ctl --agents 127.0.0.1:7761 \
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
./sai3bench-agent --listen 0.0.0.0:7761
```

**`-v` (info level)**: Adds retry attempt logging with 🔄 emoji
```bash
./sai3bench-agent --listen 0.0.0.0:7761 -v
# Output: 🔄 Retry 1/3 for operation get on s3://bucket/key
```

**`-vv` (debug level)**: Shows individual errors with full context
```bash
./sai3bench-agent --listen 0.0.0.0:7761 -vv
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
./sai3bench-agent --listen 0.0.0.0:7761 --op-log /data/oplogs/trace.tsv.zst
# Creates: /data/oplogs/trace-agent1.tsv.zst (agent_id automatically appended)
```

**YAML Config** (per-workload control, takes precedence over CLI):
```yaml
# Enable in config YAML
op_log_path: /shared/storage/oplogs/benchmark.tsv.zst

distributed:
  agents:
    - address: "node1:7761"
      id: agent1
    - address: "node2:7761"
      id: agent2
```

Results in per-agent files:
- `/shared/storage/oplogs/benchmark-agent1.tsv.zst`
- `/shared/storage/oplogs/benchmark-agent2.tsv.zst`

**Environment Variables** (s3dlio oplog settings):
```bash
# Optional: configure buffer size (default: 64KB)
export S3DLIO_OPLOG_BUF=131072

./sai3bench-agent --listen 0.0.0.0:7761 --op-log /data/oplogs/trace.tsv.zst
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
./sai3bench-ctl --agents node1:7761 ping
# connected to node1:7761 (agent version X.Y.Z)
```
Keep controller/agent binaries from the same source build when testing.
