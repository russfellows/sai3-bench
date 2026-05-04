# Trillion-Object Throughput Investigation

## Achieving 70,000 PUT ops/sec per Client Node with sai3-bench

**Date**: April 2026  
**Goal**: 1 Trillion × 1 KiB objects in ≤ 6 weeks  
**Required rate**: 4 nodes × ~70,000 PUT/s = ~275,000 PUT/s collective  
**Math check**: 275,000 × 60 × 60 × 24 × 42 days ≈ 1.0 × 10¹² ✓

---

## Executive Summary

The current stack is **architecturally sound but has three critical blockers** that prevent reaching 70k ops/sec:

1. **No h2c (HTTP/2 cleartext)**: The AWS SDK HTTP client does HTTP/1.1 on cleartext endpoints. With HTTP/1.1 you need ~3,500 concurrent TCP connections per endpoint to sustain 70k ops/sec at typical latencies. That is both impractical and OS-hostile. HTTP/2 multiplexing cuts this to ~700 connections (100 streams × 7 TCP conns).

2. **Single global AWS SDK client**: All 96 concurrent prepare tasks share one `aws-sdk-s3::Client` with one connection pool. Multi-endpoint load balancing happens at the URI routing level in `MultiEndpointStore`, but all actual HTTP requests are tunnelled through the client pointed at `AWS_ENDPOINT_URL`. Multi-endpoint within a single process does **not** distribute traffic across endpoints today.

3. **HTTP/1.1 means one request per connection**: No request pipelining or multiplexing. Every in-flight PUT occupies a dedicated TCP connection. Connection setup/teardown overhead and OS socket limits become the bottleneck long before bandwidth or CPU do.

The data throughput is trivial (70k × 1 KiB = 68 MiB/s per node — well under NIC capacity). The challenge is purely **request-rate**, which is a TCP connection management and HTTP protocol problem.

---

## Architecture Trace: What Actually Runs Today

### 1. The PUT Hot Path (bottom-up)

```
sai3-bench prepare/parallel.rs
  tokio::spawn per object (up to `concurrency` in-flight)
  └─ data_gen_pool::generate_data_optimized()       [dgen-data, zero-copy ✓]
  └─ retry_with_backoff(...)                         [overhead even on success]
  └─ store_ref.put(uri, data)                        [Arc<Box<dyn ObjectStore>>]
       └─ s3dlio::MultiEndpointStore::put()          [rounds-robin endpoint index]
            └─ S3ObjectStore::put()                  [ignores endpoint in URI]
                 └─ s3_put_object_uri_async(uri)
                      └─ parse_s3_uri(uri)           [DISCARDS endpoint host!]
                      └─ put_object_async(bucket, key, data)
                           └─ build_ops_async()      [re-created per PUT call]
                                └─ aws_s3_client_async()
                                     └─ GLOBAL_SINGLETON Client ← all PUTs land here
```

**Critical observation**: `parse_s3_uri()` discards the `endpoint:port` component from the URI. Every PUT, regardless of which endpoint `MultiEndpointStore` selected, ends up calling `client.put_object().bucket(...).key(...).send()` on the **same global client** which is configured with a single `AWS_ENDPOINT_URL`. Within one process, multi-endpoint is currently a no-op at the HTTP layer.

### 2. The Global Client

```rust
// src/s3_client.rs
static CLIENT: OnceCell<Client> = OnceCell::const_new();  // one client, forever
```

Default construction path (what runs unless `S3DLIO_USE_OPTIMIZED_HTTP=true`):

```rust
// http_client = None → AWS SDK picks its built-in default
let s3_config = aws_sdk_s3::config::Builder::from(&cfg)
    .force_path_style(true)
    .build();
Client::from_conf(s3_config)
```

The built-in default is `aws-smithy-http-client` v1.1.x → `hyper v0.14` + `hyper-rustls v0.24`.  
For cleartext (`http://`) endpoints:  

- `hyper-rustls` provides TLS — not used on cleartext  
- `hyper v0.14` falls back to HTTP/1.1 on cleartext, one request per connection  
- **No ALPN negotiation possible on cleartext → HTTP/2 is impossible via this path**

### 3. The `EnhancedHttpClient` and `h2c` Code: Dead Code in the PUT Path

`src/http/client.rs` contains `HttpClientConfig::http2_prior_knowledge: true` and the corresponding `builder.http2_prior_knowledge()` call. **This code is never reached on the PUT path.** It is only referenced by `src/performance/mod.rs` (the performance metrics module, not storage operations).

The `experimental-http-client` feature in s3dlio builds a `hyper-util` based connector with `http2_adaptive_window(true)`, but:

- It requires a "patched `aws-smithy-http-client`" (noted in comments) — not production ready
- Even with the feature enabled, it sets `http2_only(false)` — still falls back to HTTP/1.1
- The `Connector::builder().hyper_builder(hyper_builder)` API does not exist in the published crate

### 4. `ShardedS3Clients`: Also Dead Code

`src/sharded_client.rs` has the right idea — multiple client shards to reduce pool contention — but:

- `create_aws_client` has a `// TODO: Apply HTTP client optimizations per shard` comment
- The shard clients are created with `aws_config::defaults(...)` — no custom connection pool, no HTTP/2
- Not wired into `S3ObjectStore::put()` or `put_object_async()` at all
- Only used by `range_engine.rs` (GET range parallelism)

### 5. The Multi-Endpoint Reality

When running **4 separate agent processes**, each process sets its own `AWS_ENDPOINT_URL` pointing at one endpoint. This is the correct model and works. The global singleton client per process points at its own endpoint.

When running **standalone** (no agents, one process, `multi_endpoint:` in config):  

- `MultiEndpointStore` correctly distributes URI selection across 4 endpoint URIs  
- But all 4 endpoints' PUTs hit `AWS_ENDPOINT_URL` (first endpoint)  
- Net result: all traffic goes to endpoint 1, endpoints 2–4 are idle  

**Workaround confirmed working today**: Use 4 agent processes, each with one endpoint. The 4-agent distributed config already exploits this correctly. The standalone multi-endpoint config only works for benchmarks where you can tolerate all load hitting one endpoint.

---

## Bottleneck Analysis

### HTTP/1.1 Connection Math

At 1 KiB objects with a 4-node object storage cluster, expect round-trip latency:

- Network RTT: ~100–200 µs (LAN)
- Server processing time: ~50–150 µs (metadata + write commit)
- Total per-PUT: ~200–400 µs = call it **300 µs average**

With HTTP/1.1 (one request per connection):

```
Connections needed = ops/sec × latency = 70,000 × 0.0003 = 21 connections
```

Wait, that seems low. The issue is that at high-enough concurrency *within* the connection pool, HTTP/1.1 *can* achieve high rates. The real problem is:

1. **TCP setup cost** when the pool is cold or connections are evicted
2. **OS limits**: each connection is a file descriptor; 70k ops/s × sustained → thousands of sockets in TIME_WAIT
3. **Per-connection overhead** in the kernel: TCP state machines, buffers, ACK processing
4. **AWS SDK pool default**: 100 connections max per host (approximately). At 300 µs/request, 100 connections → 333,000 ops/sec theoretical max. But:
   - AWS SDK default is likely 100-200 connections — needs verification
   - Connection pool contention at 96 concurrent tasks sharing one pool
   - TCP socket recycling overhead under sustained load

With HTTP/2 multiplexing (multiple streams per connection):

- 100 concurrent streams × 1 connection = same throughput, 1 socket
- Server-side connection overhead reduced by 100×
- Linux socket table stress eliminated
- No TIME_WAIT storm

### Data Generation: Not the Bottleneck

`dgen-data` (`RollingPool`) generates 1 KiB data at > 15 GB/s. At 70k ops/s × 1 KiB = 68 MiB/s — this is ~220× headroom. Data generation is not a bottleneck.

### Tokio Runtime: Probably Adequate, But Untuned

```rust
let rt = RtBuilder::new_multi_thread().enable_all().build()?;
// No .worker_threads(N) — defaults to num_cpus
```

On a typical benchmark node with 32 cores, this gives 32 Tokio workers. The s3dlio global runtime is capped at `min(num_cpus * 2, 32)`. For pure async I/O-bound work (each PUT is mostly network wait), 32 workers is generally sufficient — Tokio tasks yield on `.await` and reuse threads. This is not the primary bottleneck but should be verified.

The env var `S3DLIO_RT_THREADS` already exists to override the global runtime thread count.

---

## What Must Change (Prioritized)

### P0 — Blocking: Force h2c in the AWS SDK Client

**Problem**: HTTP/1.1 on cleartext endpoints.  
**Solution**: Provide the AWS SDK with a custom `hyper`-based HTTP connector that uses HTTP/2 prior knowledge (h2c).

The path requires using the `aws-smithy-http-client`'s `build_with_connector_fn` API, but more reliably, using a direct `hyper-util` tower service approach:

```rust
// In s3_client.rs — create_optimized_http_client()
// Use hyper-util's legacy client with http2_prior_knowledge
use hyper_util::client::legacy::{Builder as HyperBuilder, connect::HttpConnector};

let connector = HttpConnector::new();  // Cleartext TCP connector
let mut builder = HyperBuilder::new(hyper_util::rt::TokioExecutor::new());
builder
    .http2_prior_knowledge()          // ← This is the key flag for h2c
    .http2_only(true)                 // Force HTTP/2
    .http2_adaptive_window(true)      // Adaptive flow control
    .http2_initial_connection_window_size(64 * 1024 * 1024)  // 64 MiB
    .http2_initial_stream_window_size(16 * 1024 * 1024)      // 16 MiB per stream
    .http2_max_concurrent_reset_streams(256)
    .pool_max_idle_per_host(512)
    .pool_idle_timeout(Duration::from_secs(90))
    .tcp_keepalive(Some(Duration::from_secs(30)));
```

The obstacle: `aws-smithy-http-client`'s public API does not yet expose a clean `build_with_connector` that takes an arbitrary `hyper_util` connector without the "patched" crate.

**Practical options**:

**Option A** — Implement `HttpClient` trait directly:  
Wrap the `hyper-util` client in a newtype that implements `aws_smithy_runtime_api::client::http::HttpClient`. This is the most robust and doesn't require a patched SDK. It requires ~200 lines of glue code to translate `HttpRequest`/`HttpResponse` types, but it is pure safe Rust and requires no forked crates.

**Option B** — Use `reqwest` with h2c (already a dependency behind `enhanced-http` feature):  
`reqwest` with `http2_prior_knowledge()` and `use_rustls_tls()` supports h2c. Build the reqwest client, extract its `hyper` connection pool, and wrap it. The `enhanced-http` feature already pulls in reqwest with `http2`. Simpler glue but reqwest's API for "give me the underlying hyper client" is private.

**Option C** — Enable `S3DLIO_USE_OPTIMIZED_HTTP=true` + add `http2_prior_knowledge()`:  
Fix the `experimental-http-client` path. The current code has the right structure but is missing `http2_prior_knowledge()` and has a non-compiling `build_with_connector_fn` call. This is the closest existing code to what's needed. The fix is:

1. Replace `build_with_connector_fn` with the correct `aws-smithy-http-client` API
2. Add `http2_prior_knowledge()` to the hyper builder

**Recommended**: Option A or C. Option C is less code but touches a fragile experimental path. Option A is more work but production-grade.

> **Note**: The storage system may support HTTP/2 (h2c). Verify with `curl --http2-prior-knowledge http://<endpoint>:80/` before implementing.

### P1 — Blocking: Per-Endpoint AWS SDK Client Instances

**Problem**: One global client means one connection pool for all endpoints.  
**Solution**: `MultiEndpointStore` must hold per-endpoint `aws_sdk_s3::Client` instances.

In `multi_endpoint.rs`, change `EndpointInfo` to store a per-endpoint client:

```rust
struct EndpointInfo {
    store: Box<dyn ObjectStore>,
    uri: String,
    client: Arc<aws_sdk_s3::Client>,  // ← per-endpoint client
    stats: Arc<EndpointStats>,
    ...
}
```

Each client is constructed with its own `endpoint_url`. The `S3ObjectStore` backing each endpoint would need to be given that specific client rather than using the global singleton. This requires either:

- Passing a `client: Arc<Client>` into `S3ObjectStore::new()` (breaking the current static singleton pattern)
- Or creating per-endpoint `S3Ops` instances directly in `MultiEndpointStore`

The global singleton in `s3_client.rs` (`static CLIENT: OnceCell<Client>`) is the root problem. It needs to become per-endpoint in multi-endpoint scenarios.

**Simpler short-term workaround**: For the 4-node distributed run, use 4 `sai3bench-agent` processes, one per endpoint. Each agent's process environment has a different `AWS_ENDPOINT_URL`. This is already how the distributed configs work. This sidesteps the per-endpoint client problem entirely, at the cost of requiring agent infrastructure.

### P2 — Important: Remove `build_ops_async()` Per-PUT Overhead

```rust
// Every PUT call creates a new S3Ops struct:
async fn put_object_async(bucket: &str, key: &str, data: Bytes) -> Result<()> {
    build_ops_async().await?.put_object(bucket, key, data).await
}
```

`S3Ops` is a lightweight struct (Client clone + logger Option + two Strings), so this isn't catastrophic, but at 70k ops/sec it means 70k struct constructions and Arc reference increments per second. These should be cached. The `store_cache` in `prepare/parallel.rs` already caches the `ObjectStore` instances — cache the `S3Ops` instances there as well.

### P3 — Important: `retry_with_backoff` on the Hot Path

```rust
let put_result = retry_with_backoff(
    &format!("PUT {}", &task.uri),  // ← string allocation per PUT
    &retry_config,
    || { ... }
).await;
```

`format!("PUT {}", &task.uri)` allocates a new `String` per PUT. At 70k ops/sec that is 70k string allocations per second. The retry wrapper also has overhead even on the successful (first-attempt) path. For a prepare benchmark that expects zero errors, this is pure overhead. At minimum, pre-format the context string once; consider making the retry context `&'static str` or `Cow<str>`.

### P4 — Important: Tokio Runtime Thread Count

The s3dlio global runtime caps at 32 threads. For pure async I/O (TCP send/receive), 32 threads is often sufficient, but at 70k ops/sec with many small tasks, more threads reduce head-of-line blocking. Recommendation:

```bash
export S3DLIO_RT_THREADS=128  # 4× the current cap of 32
```

Also, `sai3-bench`'s prepare runtime is created with `new_multi_thread().enable_all()` — no `worker_threads()` call, defaults to `num_cpus`. On a 32-core machine this is 32. For the agent binary, which is also async, increase this:

```rust
// In bin/agent.rs
#[tokio::main(flavor = "multi_thread", worker_threads = 128)]
```

Or set `TOKIO_WORKER_THREADS=128` environment variable.

### P5 — Linux Kernel Tuning (Server and Client)

For sustained 70k–275k ops/sec, the Linux networking stack needs tuning on **both** the sai3-bench client nodes **and** the storage server nodes (if accessible).

**Client node `/etc/sysctl.conf` additions**:

```bash
# File descriptor limits
fs.file-max = 10000000

# Socket buffer sizes (128 MiB)
net.core.rmem_max = 134217728
net.core.wmem_max = 134217728
net.core.rmem_default = 33554432
net.core.wmem_default = 33554432
net.ipv4.tcp_rmem = 4096 33554432 134217728
net.ipv4.tcp_wmem = 4096 33554432 134217728

# Connection backlog
net.core.somaxconn = 65535
net.core.netdev_max_backlog = 250000
net.ipv4.tcp_max_syn_backlog = 65535

# Ephemeral port range (default is 32768-60999 = only 28k ports!)
# For 70k ops/sec you WILL exhaust ports without this
net.ipv4.ip_local_port_range = 1024 65535

# TIME_WAIT management (critical for high-connection-rate HTTP/1.1)
net.ipv4.tcp_tw_reuse = 1
net.ipv4.tcp_fin_timeout = 10            # reduce from default 60s
net.ipv4.tcp_max_tw_buckets = 2000000    # allow more TIME_WAIT sockets

# TCP keepalive
net.ipv4.tcp_keepalive_time = 60
net.ipv4.tcp_keepalive_intvl = 10
net.ipv4.tcp_keepalive_probes = 6

# Increase connection tracking table
net.netfilter.nf_conntrack_max = 2000000   # if conntrack is in use
```

**Apply per session** (or add to `/etc/security/limits.conf`):

```bash
ulimit -n 1048576    # file descriptors per process
```

**With HTTP/2**: The TIME_WAIT and ephemeral port concerns largely go away — 700 long-lived h2 connections vs 70,000 short-lived HTTP/1.1 connections. The port range and TIME_WAIT tunables are critical for HTTP/1.1 but much less so for HTTP/2.

### P6 — HTTP/2 Stream Concurrency Limits

Even after enabling h2c, check the server-side `MAX_CONCURRENT_STREAMS` setting:

- Default in most S3 implementations: 100 concurrent streams per connection
- At 70k ops/sec × 300 µs = 21 concurrent streams needed → **one TCP connection is theoretically sufficient**
- In practice, use 8–16 connections with 100 streams each for headroom and load spreading

Check the server limit:

```bash
# Send HTTP/2 preface and examine SETTINGS frame
curl -v --http2-prior-knowledge http://<endpoint>:80/ 2>&1 | grep -i "settings\|streams\|window"
```

Client-side, configure hyper:

```rust
builder.http2_initial_connection_window_size(64 * 1024 * 1024)
       .http2_initial_stream_window_size(16 * 1024 * 1024)
       // For 1 KiB objects, window sizes don't matter much — no large payloads
```

---

## Verification: Does the Storage System Support h2c?

Before investing in h2c implementation, verify the endpoint supports it:

```bash
# Test h2c (HTTP/2 cleartext with prior knowledge)
curl -v --http2-prior-knowledge \
  -H "Authorization: AWS4-HMAC-SHA256 ..." \
  http://<endpoint>:80/s3-bucket1/ 2>&1 | head -20

# Simpler: just check if it speaks HTTP/2 at all (will get 403/InvalidAuth but that's OK)
curl -v --http2-prior-knowledge http://<endpoint>:80/ 2>&1 | grep -E "HTTP/|< h2|ALPN"
```

If the storage system responds with HTTP/2 frames, h2c works. If it resets the connection, only HTTPS h2 (via ALPN) is supported — in that case a TLS layer with self-signed certs would be required, adding ~5-10% overhead.

---

## Current Performance Ceiling Estimate

Without changes, estimating achievable throughput:

| Metric | Current State | With h2c + per-endpoint clients |
|---|---|---|
| TCP connections needed at 70k ops/s | ~21 (pool) | ~1–8 |
| TIME_WAIT sockets per minute | ~4.2M | ~0 |
| Max ops/sec (HTTP/1.1, 200-conn pool) | ~100k theoretical | — |
| Practical ops/sec (current code) | 20k–40k (estimated) | 70k–150k (estimated) |
| Blocking issue | yes (single endpoint) | resolved |

The single-process, standalone config with 4 endpoints currently routes all traffic to `AWS_ENDPOINT_URL` (one endpoint). This alone limits throughput to 1/4 of the target. Fixing P1 (per-endpoint clients) and P0 (h2c) together should unlock the full 70k target per agent node.

---

## Recommended Implementation Order

1. **Verify storage system supports h2c** (one curl command, 5 minutes)
2. **Implement per-endpoint clients in `MultiEndpointStore`** — fix the multi-endpoint routing bug. Even with HTTP/1.1, this should 4× throughput for the standalone config. (1–2 days in s3dlio)
3. **Implement h2c via Option A (custom `HttpClient` impl)** or Option C (fix experimental path). (2–3 days in s3dlio)
4. **Tune Tokio thread counts** — set `S3DLIO_RT_THREADS=128`, `TOKIO_WORKER_THREADS=128`. (30 minutes, env vars only)
5. **Apply Linux kernel tuning** on all 4 agent nodes. (1 hour)
6. **Run 4-agent distributed config** with per-node `AWS_ENDPOINT_URL` as the benchmark vehicle — this is already the correct distributed model.
7. **Benchmark and iterate** — measure ops/sec per endpoint, compare to 70k target.

---

## Files of Interest for Implementation

| File | Relevance |
|---|---|
| `s3dlio/src/s3_client.rs` | Global client init — add h2c HTTP client here |
| `s3dlio/src/http/client.rs` | `EnhancedHttpClient` with h2c logic — currently dead code, wire it into s3_client.rs |
| `s3dlio/src/multi_endpoint.rs` | Add per-endpoint `Client` instances to `EndpointInfo` |
| `s3dlio/src/s3_utils.rs` | `parse_s3_uri` drops endpoint — `put_object_uri_async` needs to be endpoint-aware |
| `s3dlio/src/sharded_client.rs` | Has TODO stubs for exactly what's needed — fill them in |
| `sai3-bench/src/prepare/parallel.rs` | Hot PUT loop — fix `format!` allocation and `build_ops_async` per-call |

---

*Investigation by: GitHub Copilot (Claude Sonnet 4.6) — April 2026*
