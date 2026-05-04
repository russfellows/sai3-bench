# Changelog

All notable changes to sai3-bench are documented in this file.

**For historical changes:**

- **v0.8.5 - v0.8.19**: See [archive/CHANGELOG_v0.8.5-v0.8.19.md](archive/CHANGELOG_v0.8.5-v0.8.19.md)
- **v0.1.0 - v0.8.4**: See [archive/CHANGELOG_v0.1.0-v0.8.4.md](archive/CHANGELOG_v0.1.0-v0.8.4.md)

## [0.8.97] - 2026-05-03

### Added

- **`prepare.key_prefix_shards: <N>` — namespace sharding for hash-based storage (issue #81)**

  Spreads prepared and PUT object keys across N hexadecimal directory shards to avoid
  concentrating metadata on a single director node (critical for VAST and similar
  hash-partitioned storage systems).

  When set to a value greater than 0 (recommended: `256`), every object key gains a
  two-hex-character prefix shard directory:

  ```text
  # key_prefix_shards: 0 (default — all keys share the same namespace prefix)
  prepared-00000000.dat
  prepared-00000001.dat
  ...

  # key_prefix_shards: 256 — 256 distinct shard directories
  00/prepared-00000000.dat
  a3/prepared-00000001.dat
  ff/prepared-00000002.dat
  ```

  The shard is derived deterministically from the object index (prepare phase) or from
  the random 64-bit object ID (workload PUT phase), so the distribution is uniform across
  all N shards.

  **Why it matters**: Storage systems that route namespace requests by key prefix hash
  (VAST, Weka, some S3-compatible clusters) will concentrate all metadata operations on
  one director node when all keys share the same long common prefix (e.g.
  `prepared-0000…`).  Setting `key_prefix_shards: 256` gives 256 distinct prefixes,
  distributing metadata load evenly across all director nodes.

  **Backward compatible**: Default is `0` (unchanged key format).  Existing configs and
  prepared datasets are unaffected.

  ```yaml
  prepare:
    key_prefix_shards: 256    # 256 hex shards: 00/…, 01/…, …, ff/…
    ensure_objects:
      - base_uri: "s3://bucket/data/"
        count: 1000000
        min_size: 1048576
        max_size: 1048576
  ```

- **`dynamic_put_pool: true` — live GET pool growth during mixed workloads (issue #82)**

  When a workload mixes PUT and GET operations, newly PUT objects are added to the GET
  selection pool in real time so GET workers can immediately access them.  This matches
  warp's mixed-mode behavior where the object population grows during the test.

  **Implementation**: `UriSource.full_uris` is now an `Arc<RwLock<Vec<String>>>` shared
  across all worker clones.  After each successful PUT, the new URI is appended to the
  matching GET pool entry via a write lock held for nanoseconds — negligible overhead
  against millisecond-scale network I/O.

  **Default is `false`** (frozen pool) to preserve benchmark reproducibility: two runs
  with the same pre-populated dataset produce identical GET pool membership regardless of
  PUT throughput variation.  Set to `true` for warp-style mixed workloads where organic
  object population growth is part of the test scenario.

  ```yaml
  dynamic_put_pool: true      # Add newly PUT objects to GET pool (warp mixed-mode parity)

  workload:
    - op: put
      path: "s3://bucket/data/new-"
      object_size: 1048576
      weight: 30
    - op: get
      path: "s3://bucket/data/prepared-*.dat"
      weight: 70
  ```

## [0.8.96] - 2026-04-28

### Fixed

- **Multi-endpoint routing for all operation types** — GET, LIST, STAT, and DELETE operations
  now correctly route through `MultiEndpointStore` when a `multi_endpoint:` block is present.
  Previously only PUT was routed through the store; all other op types fell back to a single
  endpoint (typically the first), silently defeating the purpose of multi-endpoint configuration.
  The fix pre-creates one shared `MultiEndpointStore` per run and shares it across all op types
  via a well-known internal key (`__multi_endpoint_store__`).

### Added

- **`S3_ENDPOINT_URIS` environment variable fallback** — a comma-separated list of fully-
  qualified URIs enables multi-endpoint load balancing at runtime without modifying any YAML
  file.  Priority order (highest wins):
  1. YAML `multi_endpoint:` block
  2. `S3_ENDPOINT_URIS` env var (new) — round-robin across all listed URIs
  3. `AWS_ENDPOINT_URL` — single-endpoint fallback
  4. AWS SDK default (`s3.amazonaws.com`)

  Whitespace around commas is trimmed.  At startup, a bordered banner shows the active source
  and lists all resolved endpoints.

  Example:

  ```bash
  export S3_ENDPOINT_URIS="s3://10.9.0.17:80/bucket/,s3://10.9.0.18:80/bucket/,s3://10.9.0.19:80/bucket/"
  sai3-bench run --config my_existing_config.yaml   # automatically uses all 3 endpoints
  ```

- **`multi_endpoint.endpoints` count validation** — the YAML parser now validates that the
  endpoint list has at least 1 entry and does not exceed `s3dlio::constants::MAX_ENDPOINTS`
  (32).  A clear error is returned at config-load time rather than at first I/O.

- **`S3_ENDPOINT_URIS` consistency tests** (5 new tests in `src/config_tests.rs`):
  - `test_s3_endpoint_uris_fallback_uses_all_4_endpoints`
  - `test_yaml_multi_endpoint_wins_over_s3_endpoint_uris`
  - `test_s3_endpoint_uris_whitespace_trimmed_consistently`
  - `test_s3_endpoint_uris_unset_falls_through_to_target`
  - `test_s3_endpoint_uris_valid_counts_match_s3dlio`

- **Validation banner for endpoint source** — `--dry-run` and startup validation now print
  which endpoint source is active (YAML block, `S3_ENDPOINT_URIS`, or `AWS_ENDPOINT_URL`),
  with a warning when `S3_ENDPOINT_URIS` contains only a single URI (likely misconfigured).

- **Example config: `tests/configs/multi_endpoint_s3_put_4endpoint.yaml`** — a ready-to-use
  PUT benchmark config targeting 4 S3-compatible endpoints (one bucket per endpoint URI),
  with instructions for dry-run and live execution.

- **`tests/configs/MULTI_ENDPOINT_README.md` expanded** — new "How to Use Multiple Endpoints"
  section at the top covering both configuration methods (YAML block and `S3_ENDPOINT_URIS`
  env var), priority rules, startup banner, and whitespace behaviour.

### Dependencies

- **`s3dlio`**: updated from v0.9.92 → **v0.9.96** — multi-endpoint correctness fixes,
  `S3_ENDPOINT_URIS` enforcement, new `s3-cli` options (`--endpoint-url`, `--region`,
  `--ca-bundle`), `MAX_ENDPOINTS` constant, `create_multi_endpoint_store_from_env()` Python API.
- **`s3dlio-oplog`**: updated from v0.9.92 → **v0.9.96** (kept in sync with s3dlio).

### Tests

- Total: **713 passing** (was 712)

## [0.8.94] - 2026-04-23

### Changed

- **jemalloc global allocator** — replaced the default glibc allocator with
  `tikv-jemallocator` (`#[global_allocator]`).  Profiling showed `malloc_consolidate` and
  allocator frames consuming ~55% of CPU cycles at high PUT concurrency; jemalloc eliminates
  arena contention and fragmentation under multi-threaded workloads.
  Expected gain: +10–25% throughput at t=32+.
  Measured gain: +3.6% at t=32 (31,492 → 32,634 ops/s, loopback, 1 KiB PUTs, 30 s).

### Dependencies

- **`s3dlio`**: updated from v0.9.90 → **v0.9.92** — unlimited connection pool
  (`DEFAULT_POOL_MAX_IDLE_PER_HOST = usize::MAX`), removed 32-thread runtime cap,
  `warmup_connection_pool()` and `configure_for_concurrency(n)` APIs.
- **`s3dlio-oplog`**: updated from v0.9.90 → **v0.9.92** (kept in sync with s3dlio).
- **`tikv-jemallocator = "0.6"`**: new dependency (jemalloc allocator).

## [0.8.92] - 2026-04-18

### Added

- **Credential forwarding from controller to agents** (`--env-file` / `--no-forward-env`) —
  eliminates the manual step of copying cloud credentials to each agent host.  The controller
  reads allow-listed credential variables and embeds them in the config YAML sent over gRPC;
  each agent applies them to its own environment before pre-flight validation and workload
  execution.

  - **Default behaviour**: forward any `AWS_*`, `GOOGLE_APPLICATION_CREDENTIALS`, and
    `AZURE_STORAGE_*` variables found in the controller's own environment.
  - **`--env-file <path>`**: read credentials from a `.env` file instead (file is parsed by
    `dotenvy`; the controller's own process environment is not modified).
  - **`--no-forward-env`**: disable all forwarding (use when agents already have credentials
    via IAM roles, Kubernetes Secrets, etc.).
  - **Security**: hard-coded allow-list prevents arbitrary env var injection.  Key names (never
    values) are logged at `info` level on both controller and agent for audit purposes.
    A plaintext-connection warning is printed when `--tls` is not active.
  - **Local env wins**: if a key already exists in the agent's environment, the forwarded value
    is silently skipped — local configuration always takes precedence.
  - **Never written to disk**: the `distributed_env` config field uses
    `skip_serializing_if = "is_empty"` so credentials never appear in YAML files saved to disk.
  - **Reference**: [docs/CREDENTIAL_FORWARDING.md](CREDENTIAL_FORWARDING.md)

- **Per-agent endpoint filtering in pre-flight validation** — `extract_object_storage_endpoints_from_config`
  now accepts `agent_id: Option<&str>` and returns **only the endpoints belonging to that agent**
  when a per-agent `multi_endpoint` block is present.  Previously the function ignored the agent
  ID and collected every endpoint from every agent, causing the agent to validate 64 endpoints
  across all agents' buckets (most of which it had no access to), producing ~64 spurious errors.

- **Error classification in object-storage pre-flight** — storage errors during endpoint
  validation are now classified into four categories with actionable remediation hints:
  - `[PERM]` — HTTP 403 / `AccessDenied` / `service error` (AWS SDK pattern for parsed S3 XML
    error responses; the 403 status code lives only in the Debug representation, so the
    classifier checks both Display and Debug output).
  - `[AUTH]` — HTTP 401 / `Unauthorized` / `Unauthenticated`.
  - `[CONF]` — HTTP 404 / `NoSuchBucket`.
  - `[NET]`  — all other connection failures.

- **Noise reduction: bucket grouping in pre-flight** — when multiple endpoints point to the same
  bucket (e.g. 16 IPs × 4 buckets = 64 endpoints), pre-flight tests one representative per
  bucket and emits a single `Bucket 'name' accessible — 16 endpoints` line instead of 16
  individual lines.  Failures are similarly grouped.

- **Agent version check before pre-flight** — the controller now pings all agents and prints a
  version table before starting pre-flight validation:

  ```text
  🔌 Agent Version Check
  agent-1 (host1:7167) ✅ v0.8.92
  agent-2 (host2:7167) ⚠️  v0.8.88 (controller is v0.8.92 — update recommended)
  ```

- **Distributed config validation: redundant top-level `multi_endpoint` warning** — emits a
  `Warning` when all agents have per-agent `multi_endpoint` blocks AND a top-level
  `multi_endpoint` is also present (the union-of-all-endpoints pattern that was the root cause
  of the 64-error pre-flight failure described above).

- **Distributed config validation: missing credentials hint** — when a config targets object
  storage (S3/GCS/Azure) with multiple agents and the controller's local environment has no
  cloud credentials, a `Warning` is emitted reminding operators that each agent must have
  credentials set independently (or to use `--env-file`).

- **HTTP/2 and h2c support via s3dlio v0.9.90** — the underlying s3dlio library now supports
  HTTP/2 for S3-protocol endpoints with automatic version negotiation:
  - **Auto mode (default)**: probes h2c (HTTP/2 prior-knowledge) on the first `http://`
    connection and falls back to HTTP/1.1 automatically if the server refuses.  `https://`
    endpoints negotiate HTTP/2 via TLS ALPN with no probe needed.
  - **Force h2c** (`S3DLIO_H2C=1` or `s3dlio_optimization.h2c: true`): always use HTTP/2 on
    plain-HTTP endpoints; `https://` still uses ALPN.
  - **Force HTTP/1.1** (`S3DLIO_H2C=0` or `s3dlio_optimization.h2c: false`): skip the probe,
    always use HTTP/1.1.
  - Optional flow-control tuning via `h2_adaptive_window`, `h2_stream_window_mb`, and
    `h2_conn_window_mb` YAML fields (all in `s3dlio_optimization`).
  - See [docs/S3DLIO_PERFORMANCE_TUNING.md](S3DLIO_PERFORMANCE_TUNING.md) for YAML examples.

### Fixed

- **`service error` classification** — AWS SDK Rust wraps HTTP 4xx responses in an opaque
  "service error" string in which the status code appears only in the `Debug` representation,
  not in `Display`.  The pre-flight error classifier was checking only `Display`, so all SDK
  service errors fell through to the generic `[NET]` bucket.  Now checks both.

- **False-pass in pre-flight for agents without per-agent endpoints** — older agent binaries
  (and agents with `target: null`) produced an empty endpoint list during pre-flight, which
  caused the validation to log "Skipping filesystem validation" and report success without
  actually testing any endpoints.  The root cause (ignoring `agent_id`) is now fixed.

### Tests

- **20 new unit tests** covering:
  - `bucket_label` — correct extraction for S3 with IP:port, standard, no-path, GCS, Azure,
    no-scheme, and host-no-path inputs
  - `classify_storage_error` — all four error categories plus network fallback
  - `is_credential_key` + `load_credentials_for_agents` — allow-list filtering, `.env` file
    parsing, error on missing file, GCP key handling
  - `extract_object_storage_endpoints_from_config` — per-agent override, global fallback,
    `file://` exclusion, no-agent-id aggregation
  - `apply_distributed_env` — sets missing vars, does not overwrite existing, empty-map no-op
  - `validate_distributed_config` — redundant multi_endpoint warning, partial-agents no-warning,
    credential hint for S3 without local creds, no hint for `file://` config

### Changed

- **`Config.distributed_env`** — new field on the shared `Config` struct used to carry forwarded
  credentials between controller and agents via the gRPC config YAML payload.  Serialised only
  when non-empty (`skip_serializing_if = "is_empty"`); defaults to an empty `HashMap` so older
  YAML configs remain fully compatible.

### Dependencies

- **`s3dlio`**: updated to v0.9.90 — HTTP/2 h2c support, ForceH2c routing fix, TLS test server,
  AIStore full TLS security, GCS delete error propagation, `list_containers()` Python API, and
  10 new unit tests.  See [s3dlio Changelog v0.9.90](https://github.com/russfellows/s3dlio/blob/main/docs/Changelog.md).
- **`dotenvy`**: promoted from dev-dependency to full dependency for controller `.env` file parsing.

---

## [0.8.90] - 2026-04-15

### Added

- **`enable_metadata_cache` config option** (`v0.8.89`) — new top-level YAML field that
  controls whether the internal Fjall KV metadata cache is created during prepare.
  The cache tracks per-object creation state and enables crash/resume for long-running
  prepares; it now has a documented sweet-spot and can be turned off for very large or
  simple workloads:

  ```yaml
  enable_metadata_cache: false   # disable for > ~1 Billion objects/batch
  ```

  - **Default: `true`** — fully backward-compatible, no config changes required for
    existing workloads.
  - **Sweet spot: 1 M – 1 B objects per batch** — crash-resume value exceeds overhead.
  - **Suggested off: > 1 B objects/batch** — KV store becomes impractically large
    (~3.4 GB per 50 M objects, ~15 s scan on resume, ~2 GB RAM spike at checkpoint).
  - Dry-run and preflight validation now print a clear banner showing whether the
    cache is enabled or disabled for the current run.

- **Populate ledger (`populate_ledger.tsv`)** (`v0.8.90`) — a new always-on TSV that
  records object creation counts and sizes after every prepare phase, independent of
  `enable_metadata_cache`.  At trillion-object scales, listing from storage to verify
  counts is infeasible; this ledger provides a lightweight ground-truth:
  - **Standalone**: `<results_dir>/populate_ledger.tsv`
  - **Distributed**: per-agent row + one `AGGREGATE` row summing all agents
  - Columns: `timestamp`, `test_name`, `agent_id`, `stage`, `objects_created`,
    `objects_existed`, `total_objects`, `total_bytes`, `avg_bytes`, `wall_seconds`,
    `ops_per_sec`
  - Sum across batches without listing:

    ```sh
    awk -F'\t' 'NR>1 && $3!="AGGREGATE" {sum += $5} END {print sum}' \
        sai3-*/populate_ledger.tsv
    ```

- **`dgen-data` crate for data generation** — `data_gen_pool.rs` has been rewritten
  to use the [`dgen-data`](https://crates.io/crates/dgen-data) crate (v0.2.3) in
  place of the internal `s3dlio::fill_controlled_data` call.  The new design uses a
  **rolling-pointer pool**: a single 1 MB buffer is generated once and successive PUT
  operations receive zero-copy `Bytes::slice()` windows into it.  A new buffer is only
  generated when the pool is exhausted or the dedup/compress/seed config changes.
  - Generator setup cost is paid **once per 1 MB** produced, not once per PUT.
  - No data copying for any object size ≤ 1 MB.
  - Objects > 1 MB are generated individually (PUT latency dominates; pool overhead
    is negligible at that size).
  - `dgen-data` is declared with `default-features = false` (excludes PyO3 Python
    bindings) and `features = ["thread-pinning"]` (optional CPU affinity via
    `core_affinity`; no hwloc dependency).

- **PUT latency split: internal setup vs. external I/O** — the workload engine now
  measures and reports two separate latency distributions for PUT operations:
  - **`Latency (I/O ext)`** — time spent inside `store.put()` (network/VFS round-trip).
  - **`Latency (setup int)`** — everything before the I/O call: size selection,
    data generation from the rolling pool, URI construction, and task scheduling.
    Stored at **nanosecond resolution** (HDR histogram lower-bound = 1 ns) so that
    sub-microsecond setup overhead is not truncated to zero.
  - `SizeSpec::Fixed` fast-path: skips `StdRng` construction entirely for fixed-size
    workloads, eliminating unnecessary overhead in the hot path.

- **Excel output now includes workload results** — `sai3bench-analyze` previously
  missed `workload_results.tsv` (the main workload output file) because it only looked
  for the legacy `results.tsv` and the new numbered-stage `03_execute_results.tsv`
  filenames.  `workload_results.tsv` is now discovered and written as a dedicated
  Excel tab alongside the prepare tab.

### Changed

- **Terminal output formatting** — all large integer and latency values in the
  terminal (not in TSV files) are now formatted for readability:
  - **Comma-separated integers**: ops counts, byte counts, latency values
    (e.g. `53,754`, `102,400,000`).
  - **Comma-separated floats**: ops/s throughput values (e.g. `36,184.43 ops/s`).
  - **Adaptive latency units** (5 significant digits, auto-scaled):
    values below 10,000 in their current unit are shown as comma-formatted integers;
    at ≥ 10,000 they convert up one tier (ns → µs → ms → s) with appropriate
    decimal places (e.g. `29,309µs` → `29.309ms`, `666,623µs` → `666.62ms`).
  - **Adaptive byte units** (`fmt_bytes`): same 5-sig-digit rule applied to byte
    totals in the populate ledger line (bytes → KiB → MiB → GiB → TiB).
  - TSV export files are **unchanged** — raw numeric columns are preserved exactly.

- **`results_dir.tsv_path()` returns `workload_results.tsv`** — the workload result
  TSV was already being written to this filename; `analyze.rs` is now aware of it.

### Dependencies

- Added `dgen-data = { version = "0.2.3", default-features = false, features = ["thread-pinning"] }`
- Removed direct dependency on `s3dlio::fill_controlled_data` for data generation

---

## [0.8.88] - 2026-04-10

### Changed

- **Default agent port changed from 7761 to 7167** to avoid conflicts with other
  tools that also use port 7761.  When a port conflict exists, agents silently fail
  to start and the controller reports "no response" from all agents.
  - New default: `sai3bench-agent --listen 0.0.0.0:7167`
  - Sequential ports for multi-agent-per-host setups: 7167, 7168, 7169, … 7174
  - All YAML configs, example files, documentation, and scripts updated accordingly
  - **Migration**: update any existing YAML configs or firewall rules that reference
    port 7761 → 7167 (or 7762 → 7168, etc.)

- **KV cache serialization migrated from JSON to postcard** — each `ObjectEntry`
  record stored in the fjall KV store is now encoded with
  [postcard](https://crates.io/crates/postcard) (compact binary, varint integers,
  no field-name overhead) instead of `serde_json`.
  - Per-entry size: **~156 bytes → ~68 bytes** (56% reduction)
  - Scan throughput: **1.6 M entries/s → 3.2 M entries/s** (2× speedup, release build)
  - No backward-compatibility concern — fjall KV stores are agent-local, created fresh
    per run, and never shared between binary versions.
  - Struct derives (`Serialize, Deserialize`) unchanged; only the two call sites
    (`to_bytes` / `from_bytes`) in `src/metadata_cache.rs` were updated.

### Added

- **Port-in-use detection on agent startup** — `sai3bench-agent` now probes the
  configured port before handing off to the gRPC server.  If the port is already
  occupied, a clear diagnostic message is printed showing the port number, likely
  causes, and remediation steps, then the process exits with a non-zero code instead
  of producing a cryptic tonic startup failure.

- **Preflight write probe** — when the workload contains any PUT or DELETE
  operations, `validate_object_storage()` now performs a write-probe cycle on each
  configured endpoint before the benchmark begins:
  1. PUT a fixed 24-byte sentinel string (`sai3bench-write-probe-v1`) under the key
     `sai3bench-preflight-probe-<8hex>.bin` (random hex suffix avoids collisions
     between concurrent probes).
  2. GET the object and verify the content byte-for-byte.
  3. DELETE the probe object.
  All probe failures are reported as `WARNING` (not hard errors), so the benchmark
  still runs if an endpoint cannot be verified — operators decide whether to abort.
  Read-only workloads skip the write probe entirely.

- **Preflight stage always occupies stage 0; YAML stages start at 1** — the agent
  state machine now reserves index 0 for the preflight validation stage.  The first
  YAML-defined stage (e.g. `prepare`) is stage 1, the second is stage 2, and so on.
  This makes the log output and controller/agent stage coordination unambiguous:
  - Log header: `=== Stage 1/N: prepare ===` (not `Stage 0`)
  - Barrier IDs sent to the controller are 0-based relative to the YAML stages
    (stage 1 → barrier 0, stage 2 → barrier 1, …) to stay compatible with the
    controller's own counter.
  - New state transition rule: `{0,"preflight",false} → {1,*,false}` is explicitly
    allowed, which was previously rejected as a same-index conflict.

- **KV cache coverage summary at startup** — after a KV cache checkpoint is restored,
  both `sequential.rs` and `parallel.rs` now log a one-line human-readable summary:

  ```text
  ⚡ KV cache checkpoint shows all 64032768 objects already Created — skipping LIST
  📊 Cache summary: 64032768 objects | 7.63 GiB total storage
  ```

  The storage total is accumulated in the same single fjall scan that counts objects
  by state, so there is zero additional I/O overhead.

- **`count_and_bytes_by_state()` on `EndpointCache`** — new method that walks the
  fjall keyspace once and returns both a `HashMap<ObjectState, usize>` (counts) and a
  `HashMap<ObjectState, u64>` (logical bytes) simultaneously.  Replaces the previous
  count-only scan; `CheckpointCoverage` gains a new `bytes_by_state` field populated
  from this single pass.

- **Progressive WARN safeguards for slow KV cache scans** — `count_and_bytes_by_state`
  now measures elapsed time every 50 000 entries (negligible overhead).  If the scan
  exceeds 10 s a `WARN` is emitted; another fires every 10 s thereafter, with the
  message escalating to a stronger tone at 30 s.  The scan never aborts — the warning
  explicitly notes "a LIST would take far longer."

- **KV cache coverage query from agent preflight** — `sai3bench-agent` now queries the
  on-disk KV cache checkpoint for each `ensure_objects` spec during the preflight phase
  and logs the result (✅ all created, ⚠️ partial with breakdown, ℹ️ no checkpoint).
  This is purely informational and does not change pass/fail behaviour.

- **18 new unit / integration tests** (17 active, 1 `#[ignore]`):
  - *`src/metadata_cache.rs`* — 12 new tests covering checkpoint round-trip, coverage
    queries (full and partial), multi-agent isolation, per-object lookup after restore,
    `get_objects_by_state` after restore, and the convenience `query_checkpoint_coverage`
    helper.  Includes `test_coverage_scan_timing_600k` (`#[ignore]`) which populates
    600 000 entries and asserts both scan passes complete in under 10 s.
  - *`src/prepare/tests.rs`* — 6 new integration tests exercising KV cache population
    during real sequential and parallel prepare runs across flat and tree object layouts
    (`test_kv_cache_flat_sequential_200`, `test_kv_cache_flat_parallel_variable_sizes`,
    `test_kv_cache_tree_sequential_192`, `test_kv_cache_tree_parallel_variable_sizes`,
    `test_sequential_prepare_populates_kv_cache`,
    `test_parallel_prepare_populates_kv_cache`).

---

## [0.8.86] - 2026-03-24

### Fixed

- **GCS `enable_range_downloads` was a no-op for GCS/Azure backends (bug + doc)**
  - `s3dlio_optimization.enable_range_downloads: true` set the env var
    `S3DLIO_ENABLE_RANGE_OPTIMIZATION=1`, but `GcsObjectStore` (and `AzureObjectStore`)
    never read that env var — only the S3 backend does.  Range parallelism was silently
    disabled for all GCS workloads regardless of the YAML setting.
  - Fixed in `src/workload.rs`: both `create_store_for_uri_with_config()` and
    `create_store_with_logger_and_config()` now read `S3DLIO_ENABLE_RANGE_OPTIMIZATION`
    and `S3DLIO_RANGE_THRESHOLD_MB` from the process environment (already set by
    `apply_request_tuning()` before any store is constructed) and bridge them into
    `GcsConfig { enable_range_engine: true, range_engine: { min_split_size: threshold } }`.
  - Priority order for GCS store creation:
    1. Explicit `range_engine:` YAML block (highest — unchanged)
    2. `s3dlio_optimization.enable_range_downloads` / `range_threshold_mb` (new fallback)
    3. Range disabled (default when neither is present)
  - An `info!` log confirms when the bridge activates:
    `GCS RangeEngine: enabled via s3dlio_optimization.enable_range_downloads (threshold N MiB)`
  - Root cause: `S3DLIO_ENABLE_RANGE_OPTIMIZATION` is an S3-specific mechanism;
    GCS/Azure use per-store struct config.  A future s3dlio release will make
    `GcsConfig::default()` read these env vars directly, making this bridge redundant.

- **Misleading range-download comment in `tests/configs/ai-ml/unet3d_1-host_gcs-rapid.yaml`**
  - Old comment: `# RAPID benefits from concurrent range GETs: enable with a low threshold`
    — implied the `s3dlio_optimization` block was wiring range downloads correctly for RAPID.
  - New comment accurately describes the sai3-bench bridge mechanism, and notes the
    ReadObject-per-chunk trade-off (RAPID bidi transport is bypassed per chunk).

- **Incorrect examples in `docs/GCS_INTEGRATION.md`**
  - Both RAPID quick-start examples showed `enable_range_downloads: false` with comment
    `# RAPID uses bidi streaming, not byte ranges`, which was misleading on two counts:
    the setting is a valid option (not forbidden), and its prior silently-broken behaviour
    was not documented.
  - Comments updated; a new **"Range Downloads with RAPID"** section added explaining the
    mechanism, GET-per-chunk trade-off, and a working configuration example.

- **Worker drain deadline anchored incorrectly (CRITICAL)**
  - `drain_deadline` was computed as `Instant::now() + drain_budget` at the moment workers
    were spawned, so the drain fired 150 s into execute while workers still had time remaining
  - Fixed: `drain_deadline = tokio::time::Instant::from_std(deadline) + drain_budget`
    anchors the drain window to the workers' actual end-of-workload deadline
  - **Impact**: Execute stage now runs for its full configured duration (e.g. 300 s)
    instead of being killed after `drain_budget` seconds (default 150 s)

### Added

- **Object manifest source reported in `--dry-run` output**
  - A new **"Object Manifest Source"** box is printed in `--dry-run` mode whenever
    the workload contains GET operations or a prepare stage.  It shows exactly how
    the agent will build the object-path manifest at runtime, so there are no surprises on
    large buckets:

  | Situation | What dry-run shows | Runtime behaviour |
  |---|---|---|
  | Prepare stage present | ✅ `prepare_objects()` builds manifest | Paths generated as objects are written |
  | No prepare stage + `directory_structure:` present | ✅ Arithmetic synthesis | Paths computed in milliseconds — **no LIST call** |
  | No prepare stage + no `directory_structure:` | ⚠️ Warning + guidance | Falls back to listing the bucket (may take hours for 50 M+ objects) |

  - The warning case also explains the exact fix: add a `prepare:` section with
    `directory_structure:` matching the layout of existing objects — a prepare **stage**
    is not required.

- **`glob_list_params()` — extracted safe listing helper**
  - Both `prefetch_uris_multi_backend()` and `prefetch_uris_multi_endpoint()` now call a
    shared `pub(crate) fn glob_list_params(uri) -> (&str, bool)` helper that computes
    the correct `(list_prefix, recursive)` pair for any glob URI.
  - Fixes the wildcard-contaminated-prefix bug (see Fixed section below).

- **12 new unit tests for glob listing and manifest synthesis** (`src/workload.rs`)
  - `old_code_produces_wildcard_prefix_for_deep_glob` — documents the pre-fix regression
  - `new_code_produces_clean_prefix_for_deep_glob` — proves the fix
  - `simple_filename_glob_unchanged` — proves no regression on simple `dir/*.dat` patterns
  - `wildcard_only_in_first_dir_component`, `double_star_glob_unet3d_variant`,
    `file_uri_flat_glob`, `relative_path_glob`, `star_at_root_no_slash_before_it`
  - `regex_matches_objects_returned_by_deep_listing` — proves GCS-returned URIs match the glob
  - `regex_does_not_match_wrong_prefix` — proves wrong-bucket URIs are rejected
  - `tree_manifest_paths_match_get_glob` — proves every manifest path matches the YAML GET glob
  - `tree_manifest_synthesised_without_io_matches_expected_count` — proves width=28 depth=2 files_per_dir=64 → 50,176 files with zero I/O

- **New section in `docs/YAML_DRIVEN_STAGE_ORCHESTRATION.md`: "GET-Only / Read-Only Workflows"**
  - Applies to **all object storage backends** (S3, GCS, Azure Blob, file, direct) — not
    provider-specific.
  - Explains the three manifest methods in priority order (prepare stage → arithmetic
    synthesis → object-storage listing) with a cost/trigger/use-case comparison table.
  - Documents how arithmetic synthesis works: `DirectoryTree::new()` is pure in-memory
    math — no I/O, completes in milliseconds regardless of object count.
  - Explains the critical rule: keep `prepare.directory_structure:` in the YAML even when
    the prepare *stage* is removed; the section costs nothing to parse and enables zero-cost
    manifest synthesis at startup.
  - Shows the `--dry-run` "Object Manifest Source" box output so users can verify which
    manifest method will be used before committing to a long benchmark run.
  - Provides a complete annotated GET-only YAML pattern (2-stage: preflight + execute).
  - Explains the listing fallback cost (2–4 hours for 50 M+ objects, per-call cloud charges)
    so users understand why adding `directory_structure:` matters.

### Fixed

- **GET-only / read-only stage YAML fails with "No URIs found" despite `--dry-run` passing**
  - When a `distributed.stages:` list has no prepare stage (e.g. a read-only re-run after
    data has already been written), `tree_manifest` remained `None`.  `workload::run()`
    then called `prefetch_uris_multi_backend()` with the full glob URI
    (e.g. `gs://bucket/unet3d/scan.d*_w*.dir/**/*`).

  - Two distinct bugs:
    1. **Wildcard in list prefix** — `rfind('/')` gave `list_prefix =
       "…/scan.d*_w*.dir/**/"` (contains `*`); GCS/S3/Azure treat prefix bytes literally
       and return 0 results in < 100 ms → immediate "No URIs found" error.
    2. **Non-recursive listing** — `store.list(prefix, false)` was used even for patterns
       spanning directory boundaries; deep objects are never returned by a flat list.
    3. **Listing never needed** — when `prepare.directory_structure` is present, object
       paths are fully deterministic and can be computed arithmetically.

  - **Fix 1 (avoid listing entirely)**: `src/bin/agent.rs` staged-workflow function now
    synthesises `TreeManifest` from `prepare.directory_structure` config before the stage
    loop when no prepare stage is present.  `DirectoryTree::new()` is pure in-memory
    arithmetic — completes in milliseconds for any dataset size; no GCS calls are made.
    `workload::run()` uses `PathSelector` (not listing) when `tree_manifest.is_some()`.

  - **Fix 2 (correct listing if it does happen)**: `glob_list_params()` helper fixes both
    listing bugs — safe prefix (strips to last `/` before first `*`) and `recursive=true`
    when `*` appears inside a directory component.

  - Workload start: duration, drain budget, total max wall time
  - Worker join phase: worker count, drain deadline offset
  - Agent execute stage: elapsed vs configured duration on both success and error paths
  - Controller barrier release and Prepare→Workload transition events
  - Enables post-mortem analysis without requiring trace-level logging

### Changed

- **s3dlio dependency**: v0.9.84 (already in place from v0.8.84 work)
  - GCS RAPID storage is fully functional with s3dlio v0.9.84
  - Both `BidiWriteObject` (PUT) and `BidiReadObject` (GET) verified working
    against Hyperdisk ML RAPID buckets at ~1.7–1.8 GiB/s sustained throughput
  - RAPID mode is auto-detected per bucket or can be forced via
    `s3dlio_optimization.gcs_rapid_mode: true` in the workload YAML

### Version

- Aligned with s3dlio v0.9.**86** (last-two-digits convention: sai3-bench 0.8.**86**)

---

## [0.8.70] - 2026-03-16

### Added

- **`channels_per_thread` YAML parameter for autotune**
  - New parameter in `AutotuneYaml` and `ControllerAutotuneConfig`
  - Expresses GCS gRPC subchannel count as a multiplier of thread count: `effective_channels = threads × channels_per_thread`
  - Example: `channels_per_thread: "1,2,4"` sweeps 1×, 2×, and 4× thread count as channel counts
  - Mutually exclusive with the existing `channels` (absolute count) parameter; an error is raised if both are specified
  - Both `TuneCase` and `CtTuneCase` gain a `channels_per_thread: Option<usize/u32>` field tracking the multiplier used
  - Result output shows `channels_per_thread` info alongside the effective channel count

- **Autotune dry-run mode** (`--dry-run` flag on `sai3-bench autotune`)
  - Validates configuration and prints the complete sweep plan without executing any trials
  - Shows: loop iteration order (outermost → innermost), dimension labels and values, total case count, total trial runs, estimated object I/O count, and a rough time estimate
  - Warns if the total run count exceeds 200
  - No trial count or time limit is enforced on actual runs; dry-run is the planning tool

- **Unit tests for `channels_per_thread` and dry-run infrastructure** (11 new tests across `src/main.rs` and `src/bin/controller.rs`)

### Changed

- **`sai3-bench autotune` CLI flags removed** — all tuning parameters are now YAML-only
  - Previously: individually specified via `--uri`, `--threads`, `--channels`, `--sizes`, etc.
  - Now: all parameters live in the YAML config file; use `--config <file>`
  - Retains `--config <path>` and adds `--dry-run`
  - Eliminates the ambiguous CLI-overrides-YAML merge logic

- **Autotune hard limit removed** — the previous 300-trial hard error is gone
  - The dry-run plan display (case count, total runs, rough time estimate) serves as the planning check

- **Distributed autotune example updated** (`examples/distributed-autotune-minimal.yaml`)
  - Documents `channels` vs `channels_per_thread` mutual exclusion
  - Adds commented-out `channels_per_thread` example

- **Distributed autotune YAML example**
  - Added `examples/distributed-autotune-minimal.yaml` for `sai3bench-ctl autotune`
  - Includes minimal matrix and optional native tuning fields

- **Native per-request tuning fields in controller↔agent RPCs**
  - `RunGetRequest`: `gcs_rapid_mode`, `gcs_channel_count`, `enable_range_downloads`, `range_threshold_mb`, `gcs_write_chunk_size_bytes`
  - `RunPutRequest`: `gcs_rapid_mode`, `gcs_channel_count`, `gcs_write_chunk_size_bytes`
  - Added tri-state enum for optional boolean overrides (`UNSPECIFIED`, `OFF`, `ON`)

### Changed

- **Controller autotune now sends tuning settings over RPC**
  - Tuning is no longer CLI-only/local for distributed sweeps
  - Controller matrix can include channels, range optimization, threshold, and chunk size

- **Controller autotune supports explicit `autotune:` YAML block**
  - All tuneable fields are grouped under `autotune` for clarity
  - Top-level autotune fields are not supported

- **Agent applies tuning per request before GET/PUT execution**
  - Maintains backward compatibility with `UNSPECIFIED`/zero values (no override)

### Validation

- Verified config dry-run paths still pass after RPC/schema changes:
  - `sai3-bench run --config tests/configs/test_fill_random.yaml --dry-run`
  - `sai3bench-ctl --agents 127.0.0.1:7167 run --config tests/configs/custom_stage_test.yaml --dry-run`

### Fixed

- **YAML autotune config: invalid enum values now produce an error instead of silently falling back to defaults**
  - `gcs_rapid_mode`, `optimize_for`, and `ops` fields in autotune YAML previously swallowed parse errors (`.ok()`)
  - Invalid values (e.g., `gcs_rapid_mode: typo`) now propagate as fatal errors with a clear message

### Tests

- Added 35 unit tests in `src/main.rs` for `sai3-bench autotune` helpers:
  - `resolve_util_uri`: flag-only, positional-only, both-same, both-different, neither
  - `parse_csv_usize` / `parse_csv_bool`: valid, spaces, multi, empty, invalid
  - `parse_sizes`: explicit list, dedup+sort, range (single/two/reversed/default steps)
  - `mbps_from_output`: basic, integer, last-match-wins, absent, wrong unit
  - `build_trial_uri`: glob, trailing-slash, bare URI, zero-padding
  - `parse_rapid_mode_str` / `parse_optimize_for_str` / `parse_tune_ops_str`: all variants + invalid
- Added 19 unit tests in `src/bin/controller.rs` for controller autotune helpers:
  - `parse_csv_u32`, `parse_csv_bool` (controller copy), `to_pb_toggle`
  - `split_put_uri`: trailing slash, no trailing slash, s3://, file://
  - `parse_sizes_from_config`: explicit list, range with 1 step, default range

- **`distributed.stages` with `barrier` blocks added to all AI/ML test configs**
  - All 7 files in `tests/configs/ai-ml/` now include explicit `stages:` with 3 stages
    (preflight, prepare, execute), each with `barrier: { type: all_or_nothing }`
  - Added `perf_log: { enabled: true, interval: 1s }` to every config
  - Added `force_overwrite: true` inside `prepare:` of every config
  - Files updated: `resnet50_1-host.yaml`, `resnet50_4-hosts.yaml`, `resnet50_8-hosts.yaml`,
    `unet3d_1-host.yaml`, `unet3d_2-hosts_quick-test.yaml`, `unet3d_4-hosts.yaml`,
    `unet3d_8-hosts.yaml`

### Tests

- **638 tests passing, 0 failing, 0 warnings** (final verified count for this release)

## [0.8.63] - 2026-02-23

**Multi-Endpoint Checkpoint Race Condition Fix + s3dlio Performance Tuning**

This release fixes a critical race condition in multi-endpoint checkpoint handling that caused workload aborts at 99% completion. It adds comprehensive s3dlio optimization support for large object workloads and upgrades s3dlio to v0.9.50 for enhanced performance.

### Fixed

- **Multi-endpoint checkpoint race condition (CRITICAL)**
  - Detects shared vs independent storage by comparing bucket/container names
  - Only creates one checkpoint for shared storage (avoids concurrent ObjectStore creation)
  - Prevents fatal "Failed to create object store" errors at 99% completion
  - Handles both IP addresses and DNS hostnames for load-balanced endpoints
  - **New functions**: `endpoints_share_storage()`, `extract_storage_location()` in `metadata_cache.rs`
  - **Impact**: Eliminates data loss in long-running prepare operations with multi-endpoint shared storage
  - Affected versions: v0.8.24 through v0.8.62

### Added

- **s3dlio optimization configuration**
  - New optional `s3dlio_optimization` config section for performance tuning
  - `s3dlio_range_concurrency`: Parallel range GET requests (default: 4)
  - `s3dlio_get_part_size_mb`: Range chunk size in MB (default: 8)
  - `s3dlio_multipart_chunk_size_mb`: Upload chunk size (default: 8)
  - `s3dlio_max_multipart_concurrency`: Upload parallelism (default: 8)
  - **Performance**: +76% GET throughput, +45% PUT throughput for large objects (≥64MB)
  - Applied in all binaries: `sai3-bench`, `sai3bench-agent`, `sai3bench-ctl`

- **Documentation enhancements**
  - `docs/S3DLIO_PERFORMANCE_TUNING.md`: Complete performance optimization guide (248 lines)
  - `docs/S3_MULTI_ENDPOINT_GUIDE.md`: Multi-endpoint best practices (322 lines)
  - `docs/PrepareWorkloadArchitecture_v0.8.62.md`: Pipeline architecture reference (299 lines)

- **Test configurations**
  - `tests/configs/s3dlio_optimization_example.yaml`: Performance tuning example
  - `tests/configs/distributed_4node_8endpoint_s3_test.yaml`: Multi-endpoint validation
  - `tests/configs/stephen_1node_2endpoints_s3.yaml`: Simple multi-endpoint setup

### Changed

- **s3dlio dependency upgraded to v0.9.50**
  - Enhanced range request handling for large objects
  - Improved multipart upload performance tuning
  - Better connection pooling for multi-endpoint configurations
  - Bug fixes for concurrent metadata operations

- **Storage topology detection**
  - 13 new tests for endpoint comparison logic (all pass in <1s)
  - 2 checkpoint resilience tests (marked `#[ignore]` for default runs)
  - Handles S3, Azure, GCS, file://, direct:// protocols
  - GCS bucket extraction fixed (hostname-based, not path-based)

- **Code quality improvements**
  - Cleaner emptiness checks (`.is_empty()` vs `len() > 0`)
  - Range `contains()` for bounds checking
  - Enhanced test infrastructure with `s3dlio_optimization` field consistency

### Testing

- **569 tests passing** (565 active + 4 ignored for performance)
- Zero warnings policy maintained
- All checkpoint race conditions validated

### Compatibility

- **Backward compatible** with v0.8.x configurations
- `s3dlio_optimization` section is optional
- Existing multi-endpoint setups work without changes
- No breaking API changes

---

## [0.8.62] - 2026-02-11

**Prepare Streaming + Perf Log Timing + Dry-Run Memory Sampling**

This release removes the large precompute memory spike in prepare, restores parallel size mixing while streaming, and adds visibility into dry-run sample memory. It also aligns perf-log timing with stage transitions and improves the analyze tooling.

### Added

- **Dry-run sample generation with memory/time reporting**
  - Always generates a fixed sample (100k) of prepare paths/sizes
  - Reports elapsed time and RSS delta to surface scaling risks

- **Per-agent perf-log export in analyze tool**
  - Adds worksheets for `agents/*/perf_log.tsv`
  - Ensures unique worksheet names and safe 31-char limits
  - Adds `--overwrite` flag and smarter default output naming

### Changed

- **Parallel prepare now streams and interleaves**
  - Interleaves `ensure_objects` entries to preserve mixed sizes
  - Uses deterministic on-the-fly size generation with bounded chunking
  - Keeps memory flat by dropping per-chunk task vectors

- **Sequential prepare no longer precomputes sizes**
  - Deterministic streaming generation for large datasets

- **Directory tree path resolution is now O(1)**
  - Avoids linear scans in `TreeManifest::get_file_path()`

- **Perf-log elapsed timing resets on stage transitions**
  - Controller and per-agent perf-log writers reset stage elapsed time on transitions
  - Ensures stage timing alignment without resetting counters

- **Directory tree counts applied to parsed configs**
  - `sai3-bench` and `sai3bench-ctl` now apply directory tree counts before dry-run and controller dispatch

### Testing

- **551 tests passing** (release profile)

---

## [0.8.61] - 2026-02-11

**Stage and Barrier Alignment + Test Reliability**

This release tightens distributed stage orchestration requirements, standardizes barrier identifiers, and improves test reliability and store setup behavior.

### Breaking

- **Distributed stages are required**
  - Implicit stage generation has been removed for distributed runs
  - Use the built-in `convert` command to update legacy YAML files

### Added

- **Config conversion command for legacy YAML files**
  - `sai3-bench convert --config <file.yaml>`
  - `sai3-bench convert --files <glob>`
  - `sai3bench-ctl convert --config <file.yaml>`
  - `sai3bench-ctl convert --files <glob>`

### Changed

- **Distributed stages are now required**
  - `distributed.stages` must be explicitly defined (empty list is invalid)
  - Removes legacy default stage generation for distributed runs
  - **Impact**: Configs must declare stage ordering explicitly for distributed mode

- **Barrier identifiers are numeric stage indices**
  - Barrier coordination uses stage order indices instead of string names
  - Aligns controller barrier messaging with stage transition sequencing

### Fixed

- **Store cache pre-creation for PUT operations**
  - Uses PUT-specific URI resolution to avoid metadata-only path handling
  - Prevents runtime panic when pre-creating stores for PUT workloads

- **Checkpoint tests made deterministic**
  - Sequential checkpoint overwrite/restore flow avoids cross-test interference

- **Performance test flakiness removed**
  - Validation now asserts correctness instead of timing comparisons

### Testing

- **547 tests passing** (release profile)

---

## [0.8.60] - 2026-02-10

**KV Cache Checkpoint Restoration - Complete Resume Capability**

This release completes the checkpoint implementation with automatic restoration on startup, enabling full resume capability after crashes or restarts. Checkpoints are now created AND restored for both standalone and distributed modes.

### Added

- **Checkpoint restoration on startup** (CRITICAL)
  - `EndpointCache::new()` now calls `try_restore_from_checkpoint()` BEFORE opening database
  - Downloads checkpoint from storage (`{endpoint}/.sai3-cache-agent-{id}.tar.zst`)
  - Extracts tar.zst to local cache location
  - Opens restored fjall database with all object states intact
  - **Impact**: Agents can resume long-running prepare operations after crashes/restarts
  - **Files changed**: `src/metadata_cache.rs` (+120 lines restoration logic)

- **Checkpoint creation for standalone mode**
  - `sai3-bench run` now passes `results_dir` and `config` to `prepare_objects()`
  - Periodic checkpoints created every 5 minutes (configurable via `cache_checkpoint_interval_secs`)
  - Works with all storage backends (file://, s3://, az://, gs://)
  - **Impact**: Single-node workloads can now resume after interruption
  - **Files changed**: `src/main.rs` (+2 lines enable cache)

### Fixed

- **Race condition in checkpoint creation** (CRITICAL)
  - Changed from `maybe_flush()` to guaranteed `force_flush()` before archiving
  - Added database file verification after flush to ensure files are on disk
  - Prevents corruption from intervening writes between flush and archive creation
  - **Impact**: Checkpoints now reliably contain all committed object states
  - **Files changed**: `src/metadata_cache.rs` (+25 lines verification)

- **Checkpoint extraction verification**
  - Added comprehensive diagnostics for restoration process
  - Verifies cache directory and database files exist after extraction
  - Logs restored object counts for validation
  - **Impact**: Easier debugging of restoration issues
  - **Files changed**: `src/metadata_cache.rs` (+35 lines diagnostics)

### Enhanced

- **9 comprehensive checkpoint tests** covering all scenarios:
  - Archive contains KV database files
  - Restoration from storage
  - No checkpoint on storage (graceful fallback)
  - Local cache newer than checkpoint (skip restore)
  - Agent ID isolation (separate checkpoints per agent)
  - Checkpoint overwrites (latest wins)
  - Storage location verification (checkpoints at storage URI, not cache location)
  - Large dataset (1000 objects)
  - Multiple restore cycles

### Testing

- **545 tests passing** (14 ignored performance tests)
  - All checkpoint restoration tests passing
  - All integration tests updated for new `prepare_objects()` signature
  - All doc tests updated for new `MetadataCache::new()` signature
  - **0 test failures, 0 compilation errors, 0 warnings**

### Configuration

**New top-level field:**

```yaml
cache_checkpoint_interval_secs: 300  # Default: 5 minutes, 0 = disabled
```

**Checkpoint locations:**

- `file:///path/` → `{path}/.sai3-cache-agent-{id}.tar.zst`
- `s3://bucket/` → `s3://bucket/.sai3-cache-agent-{id}.tar.zst`
- `az://container/` → `az://container/.sai3-cache-agent-{id}.tar.zst`
- `gs://bucket/` → `gs://bucket/.sai3-cache-agent-{id}.tar.zst`

---

## [0.8.53] - 2026-02-09

**Critical Fixes: Multi-Endpoint + Directory Tree Workloads**

This release fixes critical bugs affecting multi-endpoint configurations with directory tree structures, and enhances dry-run validation output for better visibility.

### Fixed

- **Multi-endpoint + directory tree workload routing** (CRITICAL)
  - GET/PUT/STAT/DELETE operations now correctly route to the endpoint where each file was created
  - Fixed round-robin endpoint calculation: extracts file index from filename, computes `endpoint_idx = file_idx % num_endpoints`
  - Applies to all tree mode operations in distributed workloads
  - **Impact**: Workloads that previously failed with 0 ops now execute correctly
  - Example: 2 agents × 2 endpoints = 4 total mount points, all files now accessible
  - **Files changed**: `src/workload.rs` (+94 lines endpoint routing logic)

- **"duplicate field `timeout_secs`" deserialization error in distributed validation stage**
  - Removed duplicate `timeout_secs` field from `StageConfig::Validation` variant
  - Configuration now uses single top-level `timeout_secs` field
  - **Impact**: Agents no longer crash on startup when parsing YAML with validation stages
  - **Files changed**: `src/config.rs` (-7 lines), `src/bin/agent.rs` (2 lines field access)

### Enhanced

- **Dry-run validation now shows complete file distribution across ALL endpoints**
  - Previously: Only showed first agent's first endpoint
  - Now: Displays all endpoints from all agents with full URIs
  - Shows round-robin pattern visualization (e.g., "indices 0,4,8,12 → endpoint1")
  - Displays first 2 sample files per endpoint with complete file:// / s3:// / az:// URIs
  - Grouped by agent for multi-agent configurations
  - **Impact**: Users can verify correct endpoint distribution before running expensive workloads
  - **Files changed**: `src/validation.rs` (+146 lines multi-endpoint display logic)

### Testing

- Verified with 2 agents × 2 endpoints = 4 total mount points
- 80 files distributed perfectly (20 per endpoint, round-robin by index)
- 11 GET operations succeeded (previously failed with 0 ops)
- Dry-run shows complete URI distribution across all 4 endpoints
- All existing tests passing (446 total)

---

## [0.8.52] - 2026-02-06

**Maintainability Release: Adaptive Retry + Code Refactoring + Enhanced UX**

This release improves code maintainability, enhances prepare phase resilience with adaptive retry strategies, and adds user-friendly numeric formatting across the codebase.

### Added

- **Adaptive retry strategy for prepare phase failures** (80-20 rule)
  - Intelligent retry based on failure rate:
    - `<20%` failures: Full retry (10 attempts) - likely transient issues
    - `20-80%` failures: Limited retry (3 attempts) - potential systemic problems
    - `>80%` failures: Skip retry - clear systemic failure, no point retrying
  - Deferred retry runs AFTER main prepare phase completes
  - More aggressive exponential backoff (500ms initial, 30s max, 2.0× multiplier)
  - Prevents "missing object" errors during execution phase
  - Eliminates fast path performance impact
  - **11 comprehensive unit tests** for all failure rate scenarios
  - Total test count: **446 tests passing** (+27 from v0.8.51)

- **Thousand separator formatting for numeric output**
  - Dry-run displays: `64,032,768 files` instead of `64032768 files`
  - TSV column headers: sizes like `1,048,576` for improved readability
  - All validation messages: clearer numeric output
  - Dependency: `num-format` crate (v0.4+)

- **YAML numeric input with thousand separators** (v0.8.52)
  - Support for commas, underscores, and spaces in YAML numbers
  - Examples: `count: 64,032,768`, `count: 64_032_768`, `count: 64 032 768`
  - Custom serde deserializers handle all three separator formats
  - Applies to all numeric fields: `count`, `min_size`, `max_size`, `width`, `depth`, `files_per_dir`
  - Backward compatible: plain numbers (`64032768`) still work
  - See [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for examples

- **Human-readable time units in YAML configuration** (v0.8.52)
  - Support for time unit suffixes: `s` (seconds), `m` (minutes), `h` (hours), `d` (days)
  - Examples: `duration: "5m"`, `timeout: "2h"`, `delay: "30s"`
  - Applies to all timeout/duration fields:
    - `duration`, `start_delay`, `post_prepare_delay`
    - `grpc_keepalive_interval`, `grpc_keepalive_timeout`
    - `agent_ready_timeout`, `query_timeout`, `agent_barrier_timeout`
    - `default_heartbeat_interval`, `default_query_timeout`
    - SSH `timeout` and all barrier sync timeouts
  - Backward compatible: plain integers still interpreted as seconds
  - **10 comprehensive unit tests** for duration parsing
  - See [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for examples

- **Configuration conflict detection warnings**
  - Warns when `cleanup: false` conflicts with YAML cleanup stage
  - Helps prevent unintended behavior in multi-stage configurations
  - Non-fatal: displays warning but continues execution

### Changed

- **Code refactoring: Split prepare.rs into 10 focused modules** (Phase 1)
  - Original: monolithic 4,778-line file
  - New structure: 10 modules averaging ~478 lines each
  - **Module breakdown:**
    - `error_tracking.rs` (181 lines) - PrepareErrorTracker, ListingErrorTracker
    - `retry.rs` (231 lines) - Adaptive retry with 80-20 rule
    - `metrics.rs` (87 lines) - PreparedObject, PrepareMetrics types
    - `listing.rs` (364 lines) - Distributed listing with progress
    - `sequential.rs` (587 lines) - Sequential prepare strategy
    - `parallel.rs` (812 lines) - Parallel prepare strategy
    - `directory_tree.rs` (709 lines) - Tree operations and agent assignment
    - `cleanup.rs` (382 lines) - Cleanup and verification
    - `tests.rs` (1,312 lines) - All 178 prepare phase tests
    - `mod.rs` (237 lines) - Public API and orchestration
  - **Benefits:**
    - Average module size reduced 10× (4,778 → ~478 lines)
    - Clear separation of concerns
    - Easier code navigation and maintenance
    - Test isolation in dedicated module
    - Improved IDE performance and analysis
  - **Verification:**
    - All 178 prepare tests passing
    - Zero compilation warnings
    - Build time unchanged (~12s)
    - No functionality changes - pure refactoring

- **Improved validation error messages**
  - All numeric values use thousand separators for readability
  - Directory structure validation shows formatted counts
  - Conflict warnings highlight specific configuration issues

### Fixed

- **Custom serde deserializers for numeric fields**
  - New `src/serde_helpers.rs` module with deserializer utilities
  - Applied to `DirectoryStructure` and `EnsureObjects` structs
  - Handles all separator formats transparently

### Documentation

- Updated [README.md](../README.md) with v0.8.52 feature descriptions
- Added thousand separator examples to [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md)
- All example configurations updated to show readable numeric format

### Migration Guide

**No breaking changes** - All updates are backward compatible.

**Optional enhancements:**

1. Use thousand separators in YAML configs for better readability:

   ```yaml
   prepare:
     ensure_objects:
       - count: 10,000,000  # More readable than 10000000
         min_size: 1,048,576  # Clearly shows 1 MiB
   ```

2. Review prepare phase retry behavior in logs - new adaptive strategy may change retry patterns
3. Monitor prepare phase warnings for configuration conflicts

---

## [0.8.51] - 2026-02-06

**Critical Release: Blocking I/O Fixes for Large-Scale Deployments**

This release addresses four critical executor starvation issues identified in production large-scale testing (>100K files). These fixes are essential for distributed deployments where agents must validate extensive directory structures and create millions of objects without blocking the async executor.

### Added

- **Comprehensive unit test suite for blocking I/O fixes** (12 new tests)
  - `test_agent_ready_timeout_default` - Verifies 120s default timeout
  - `test_agent_ready_timeout_custom` - Tests custom timeout parsing (60s-600s)
  - `test_agent_ready_timeout_scale_recommendations` - Scale-based timeout validation
  - `test_timeout_duration_conversion` - Duration conversion logic
  - `test_timeout_realistic_values` - Real-world scenarios (10K-1M files)
  - `test_backward_compatibility_no_timeout` - Default fallback behavior
  - `test_glob_does_not_block_executor` - Concurrent task progress during glob
  - `test_glob_large_directory` - 10K file glob without executor starvation
  - `test_prepare_yields_during_creation` - 500 object prepare with heartbeats
  - `test_prepare_sequential_yields` - Sequential mode yielding validation
  - `test_executor_responsiveness_large_prepare` - 1000 object integration test
  - `test_small_prepare_still_works` - Regression test for small workloads
  - Total test count: **419 tests passing**

### Changed

- **Configurable agent_ready_timeout** (default: 120s, was hardcoded 30s)
  - New configuration field: `distributed.agent_ready_timeout`
  - Allows agents time to complete glob validation at scale
  - Scale recommendations: 60s (small), 120s (medium), 300s (large), 600s (very large)
  - Prevents "Agent did not send READY within 30s" errors in large-scale tests
  - See [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for configuration details

- **Non-blocking glob operations** (spawn_blocking)
  - `agent.rs` line 3828: Moved blocking glob to thread pool
  - `controller.rs` line 4606: Moved blocking glob to thread pool
  - Prevents 5-30 second executor stalls during file path expansion
  - Critical for configurations with >100K files and glob patterns

- **Periodic yielding in prepare loops** (tokio::task::yield_now)
  - `prepare.rs` line 1269: Yield every 100 operations in parallel prepare
  - `prepare.rs` line 2011: Yield every 100 operations in sequential prepare
  - `prepare.rs` line 2999: Yield every 100 operations in cleanup
  - Allows heartbeats, READY signals, and stats updates during million-object operations
  - Uses `.is_multiple_of(100)` per clippy suggestion

### Fixed

- **Executor starvation during large-scale operations**
  - Fixed blocking glob preventing gRPC heartbeats
  - Fixed prepare phase blocking stats writer task
  - Fixed cleanup phase blocking barrier coordination
  - All fixes validated with concurrent task execution tests

- **Test fixture compilation errors**
  - Updated 5 test functions with new `agent_ready_timeout` field
  - `src/preflight/distributed.rs`: 4 test fixtures updated
  - `tests/distributed_config_tests.rs`: 1 test fixture updated
  - All 419 tests passing with zero compilation errors

### Documentation

- Updated [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) with agent_ready_timeout recommendations
- Added comprehensive blocking I/O fix documentation
- Design documents archived in [docs/archive/](archive/):
  - `BLOCKING_IO_FIXES_REQUIRED.md`
  - `LARGE_SCALE_TIMEOUT_ANALYSIS.md`
  - `TIMEOUT_FIX_IMPLEMENTATION.md`

### Migration Guide

**No breaking changes** - All fixes are backward compatible. Existing configurations will use default timeout values.

**Recommended actions for large-scale deployments:**

1. Add `agent_ready_timeout` to distributed config based on file count
2. Verify glob patterns resolve quickly or increase timeout accordingly
3. Monitor agent READY times in logs to tune timeout values

---

## [0.8.50] - 2026-02-05

**Major Release: YAML-Driven Stage Orchestration + Barrier Synchronization + Configurable Timeouts**

This release represents a fundamental architectural evolution of sai3-bench, transitioning from simple prepare→workload execution to a flexible, multi-stage orchestration framework with precise synchronization control and production-grade timeout management.

### Added

- **YAML-driven stage orchestration framework (Phases 1-3 complete)**
  - Multi-stage test definition via YAML configuration (preflight, prepare, execute, cleanup)
  - Each stage configurable with independent operations, durations, and targets
  - Support for 4 distinct stage types:
    - **Preflight**: Configuration validation (file existence, directory structure)
    - **Prepare**: Data preparation (object creation, tree building)
    - **Execute**: Performance workload (read/write/list operations)
    - **Cleanup**: Test teardown (directory removal, object deletion)
  - Automatic stage detection and execution ordering
  - Backward compatibility with legacy single-stage configurations

- **Numbered stage output format for Excel organization**
  - New TSV file naming: `NN_stagename_results.tsv` (e.g., `01_preflight_results.tsv`)
  - Stage numbers preserve execution order in multi-tab Excel workbooks
  - Enables clear visual progression: 01→02→03→04 in tab ordering
  - Each stage file includes descriptive comment header with phase information

- **Barrier synchronization framework**
  - Pre-stage barriers ensure all agents ready before stage execution
  - Post-stage barriers ensure stage completion before proceeding
  - Barrier timeout configuration per stage (default: 300 seconds)
  - Prevents timing skew in distributed multi-stage tests
  - Critical for coordinated multi-host testing scenarios
  - See [STAGE_ORCHESTRATION_README.md](STAGE_ORCHESTRATION_README.md) for architecture details

- **Comprehensive timeout configuration system**
  - **Agent operation timeouts**: Per-stage configurable (default: 600s for prepare, 3600s for execute/cleanup)
  - **Barrier synchronization timeouts**: Per-stage configurable (default: 300s)
  - **gRPC call timeouts**: Configurable at controller level (default: 300s)
  - **Health check timeouts**: Fixed 30s for rapid failure detection
  - Prevents indefinite hangs in distributed testing environments
  - Validates timeout values at config load time (≥60s minimum)
  - See [docs/TIMEOUT_CONFIGURATION.md](docs/TIMEOUT_CONFIGURATION.md) for complete reference

- **sai3-analyze multi-stage support**
  - Parses numbered stage TSV files: `01_preflight_results.tsv`, `02_prepare_results.tsv`, etc.
  - Excel tab naming: `workload-NN_stagename` (e.g., `test_barriers-01_preflight`)
  - Automatic comment line filtering (lines starting with `#`)
  - Stage number extraction for proper tab ordering
  - Backward compatibility with legacy `perf_log.tsv` format
  - Timestamp column formatting preserved (epoch → Excel datetime)

- **37 validated multi-stage YAML test configurations**
  - Comprehensive test coverage across all stage types:
    - 8 preflight validation tests (config checks, file existence)
    - 11 prepare tests (data generation, tree building, multi-endpoint)
    - 12 execute tests (workload runs, distributed operations, barriers)
    - 6 cleanup tests (directory removal, object deletion)
  - All configs validated with `--dry-run` before release
  - Pruned from 92 original configs to focus on essential test patterns
  - See `tests/configs/` directory for examples

- **Large-scale distributed testing validation**
  - Successfully tested with 300,000+ directories
  - Successfully tested with 64 million+ files
  - Proven barrier synchronization across multiple test hosts
  - Multi-stage execution with coordinated prepare→execute→cleanup workflows

### Changed

- **BREAKING: Stage-aware config structure**
  - Added top-level `stages` array to YAML configuration
  - Each stage requires: `name`, `stage_type`, and stage-specific configuration
  - Legacy single-stage configs still supported (auto-converted to single execute stage)
  - Stage types: `preflight`, `prepare`, `execute`, `cleanup`

- **BREAKING: Output file structure**
  - TSV files now numbered: `01_preflight_results.tsv` instead of `preflight_results.tsv`
  - Preserves execution order in multi-file analysis scenarios
  - Old format: `{stage}_results.tsv` → New format: `{NN}_{stage}_results.tsv`

- **Enhanced results directory organization**
  - Each test run creates timestamped directory: `sai3-YYYYMMDD-HHMM-{workload_name}/`
  - Stage results grouped within run directory
  - Comment headers in TSV files document stage execution details
  - Improved traceability for multi-stage test runs

- **Version bump: v0.8.26 → v0.8.50**
  - Reflects major architectural changes (stage orchestration framework)
  - Aligns with feature scope (not just incremental changes)

### Fixed

- **sai3-analyze stage name extraction**
  - Correctly strips `_results.tsv` suffix while preserving stage number prefix
  - Handles both new numbered format (`01_preflight_results.tsv`) and legacy format
  - Fixed parsing logic: iterate through chars to find first non-digit, then strip suffix
  - Tab names now properly formatted: `workload-01_preflight` (no timestamp clutter)

- **Comment line handling in TSV parsing**
  - Filters lines starting with `#` (descriptive headers)
  - Prevents parsing errors when stage metadata included in output files
  - Maintains backward compatibility with comment-free legacy files

- **Tab naming timestamp removal**
  - Removed timestamp logic from Excel tab names (user preference)
  - Timestamps retained in directory names for run identification
  - Tab format: `{workload}-{NN}_{stage}` for clarity and ordering

### Testing

- ✅ All 4 sai3-analyze unit tests passing:
  - Stage extraction from numbered filenames
  - Tab name generation (workload-stage format)
  - Tab name truncation (31-char Excel limit)
  - Legacy TSV parsing compatibility
  
- ✅ Real directory validation:
  - Tested with `sai3-20260205-1440-test_barriers_local/` (4 stage files)
  - Generated 11KB Excel workbook with 4 tabs
  - Tab ordering: 01_preflight → 02_prepare → 03_execute → 04_cleanup
  
- ✅ 37 YAML configs validated with `--dry-run`:
  - All preflight validation tests (8 configs)
  - All prepare stage tests (11 configs)
  - All execute stage tests (12 configs)
  - All cleanup stage tests (6 configs)
  
- ✅ Large-scale distributed testing:
  - 300,000+ directories with barrier synchronization
  - 64 million+ files across multi-host test environment
  - Multi-stage workflows (prepare → execute → cleanup)

### Documentation

- **New guides:**
  - [STAGE_ORCHESTRATION_README.md](STAGE_ORCHESTRATION_README.md) - Complete stage framework architecture
  - [docs/TIMEOUT_CONFIGURATION.md](docs/TIMEOUT_CONFIGURATION.md) - Comprehensive timeout reference
  - [docs/examples/multi_stage_workflow_example.yaml](docs/examples/multi_stage_workflow_example.yaml) - Full 4-stage example

- **Updated guides:**
  - [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) - Added `stages` array documentation
  - [YAML_MULTI_STAGE_GUIDE.md](YAML_MULTI_STAGE_GUIDE.md) - Stage orchestration patterns
  - [README.md](README.md) - Feature list, test count updates (pending)

- **Archived documentation:**
  - Created [archive/CHANGELOG_v0.8.5-v0.8.19.md](archive/CHANGELOG_v0.8.5-v0.8.19.md)
  - 933 lines of historical changes (15 versions)
  - Maintains complete project history

### Configuration Examples

**Multi-stage workflow with barriers:**

```yaml
stages:
  - name: preflight
    stage_type: preflight
    barrier_sync: true
    barrier_timeout_secs: 300
    checks:
      - file_exists: /mnt/storage/benchmark/
  
  - name: prepare
    stage_type: prepare
    barrier_sync: true
    agent_timeout_secs: 600
    workload:
      object_count: 10000
      object_size: 1048576
  
  - name: execute
    stage_type: execute
    barrier_sync: true
    agent_timeout_secs: 3600
    barrier_timeout_secs: 300
    workload:
      duration_secs: 300
      operations:
        - type: get
          weight: 80
        - type: put
          weight: 20
  
  - name: cleanup
    stage_type: cleanup
    barrier_sync: true
    workload:
      remove_objects: true
```

**Per-stage timeout customization:**

```yaml
# Global gRPC timeout
distributed:
  grpc_timeout_secs: 300

# Per-stage overrides
stages:
  - name: long_prepare
    stage_type: prepare
    agent_timeout_secs: 1800  # 30 minutes for large dataset
    barrier_timeout_secs: 600  # 10 minutes barrier sync
```

### Technical Details

- Stage orchestration leverages existing distributed testing infrastructure
- Barrier synchronization implemented via gRPC coordination messages
- Timeout handling propagates from controller through agents to s3dlio operations
- TSV comment headers use `#` prefix for metadata (filtered during analysis)
- Excel tab name format: max 31 chars, truncated with "..." suffix if needed
- sai3-analyze supports both new numbered format and legacy single-file format

### Dependencies

- s3dlio v0.9.16+ (git tag dependency, can use v0.9.27+ for latest features)
- Rust 1.75+ for Duration API and async timeout handling
- Compatible with all storage backends (S3, Azure, GCS, file://, direct://)

### Migration Notes

**From v0.8.23 and earlier:**

- Old single-stage configs still work (auto-converted to single execute stage)
- To use multi-stage features, restructure config with `stages` array
- sai3-analyze automatically detects numbered vs legacy TSV format
- No action required for existing test scripts or automation

**Excel workbook changes:**

- Tab names no longer include timestamps (cleaner, more readable)
- Tab names now include stage numbers for proper ordering (01_, 02_, etc.)
- Multi-stage tests produce multiple tabs per workload (one per stage)

---

## [0.8.23] - 2026-02-03

### Added

- **Distributed configuration pre-flight validation** (prevents common misconfigurations)
  - New `src/preflight/distributed.rs` validation module (315 lines, 4 comprehensive tests)
  - Detects `base_uri` specified in isolated mode with per-agent storage (the h2 protocol error bug)
  - Validates `shared_filesystem` semantics: warns if agents lack `multi_endpoint` in shared mode
  - Integrated into controller workflow - runs before agent pre-flight validation
  - Provides actionable error messages with configuration fix recommendations
  
- **EnsureSpec.base_uri made optional for isolated mode**
  - `base_uri` field now `Option<String>` (was `String`)
  - When `None` with `use_multi_endpoint: true`, each agent uses its first endpoint for listing
  - Enables correct distributed listing in isolated mode without base_uri conflicts
  - New `get_base_uri()` helper method for transparent fallback logic
  
- **Comprehensive test coverage for base_uri handling**
  - 10 unit tests in `src/config_tests.rs` covering all base_uri edge cases
  - 4 validation tests in `src/preflight/distributed.rs` for configuration scenarios
  - Fixed existing integration tests (3 files) to wrap base_uri in `Some()`
  - Real integration tests prove buggy config is blocked, fixed config passes

### Changed

- **Validation logic respects shared_filesystem semantics**
  - `shared_filesystem: true` - All agents access same storage (multi_endpoint optional)
  - `shared_filesystem: false` - Per-agent isolated storage (base_uri in isolated mode is error)
  - No assumptions about network topology or endpoint accessibility
  - Supports all valid deployment patterns: shared NFS, independent disks, mixed scenarios

### Fixed

- **h2 protocol errors in distributed isolated mode** (THE BUG)
  - Root cause: `base_uri` specified when agents have different `multi_endpoint` configurations
  - Listing phase used `base_uri` instead of each agent's first endpoint
  - Pre-flight now blocks this misconfiguration before runtime with clear fix recommendation
  - Example error: "1 agents cannot access 'file:///mnt/filesys1/benchmark/'"

### Testing

- **323 tests passing** (was 312)
  - 121 lib tests (10 new config tests, 1 integration test fix)
  - 202 integration tests (3 new distributed validation tests)
  - Validated with real agents: buggy config blocked, fixed config runs successfully

---

## [0.8.22] - 2026-01-29

### Added

- **Multi-endpoint statistics tracking** (endpoint-level performance visibility)
  - New `workload_endpoint_stats.tsv` file with per-endpoint metrics
  - New `prepare_endpoint_stats.tsv` file for data preparation phase
  - Columns: endpoint, operation_count, bytes, errors, latency percentiles (p50, p90, p99, p99.9)
  - Essential for diagnosing multi-endpoint load balancing effectiveness
  - Works with both global and per-agent endpoint configurations

- **Multi-endpoint load balancing for distributed storage systems**
  - Enables distributing I/O operations across multiple storage endpoints (IPs, mount points)
  - Perfect for multi-NIC storage systems (VAST, Weka, MinIO clusters)
  - Supports both global and per-agent endpoint configuration
  
- **Static per-agent endpoint mapping**
  - Assign specific endpoints to specific agents for optimal load distribution
  - Example: 4 test hosts × 2 endpoints/host = 8 storage IPs fully utilized
  - Prevents endpoint overlap in distributed testing scenarios
  
- **Multi-endpoint NFS support**
  - Load balance across multiple NFS mount points with identical namespaces
  - Works with any distributed filesystem presenting unified namespace (VAST, Weka, Lustre)
  
- **Configuration schema enhancements**
  - Added `multi_endpoint` field to top-level Config (global configuration)
  - Added `multi_endpoint` field to AgentConfig (per-agent override)
  - Strategy options: `round_robin` (simple), `least_connections` (adaptive)
  
- **New workload creation API**
  - Added `create_store_from_config()` function for multi-endpoint aware store creation
  - Respects configuration priority: per-agent > global > target fallback
  - Added `create_multi_endpoint_store()` internal helper function

- **Excel timestamp formatting** in sai3-analyze
  - Auto-detects timestamp columns by suffix (_ms,_us, _ns)
  - Converts epoch timestamps to Excel datetime format: "2026-01-29 22:21:51.000"
  - Column width: 22 for timestamps, 15 for regular columns
  - Fixes scientific notation display (1.77E+18 → readable dates)

- **Version option** for sai3-analyze
  - Added `-V` and `--version` flags
  - Displays: "sai3bench-analyze 0.8.22"

- **Comprehensive documentation consolidation**
  - Created [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md) (620+ lines)
    - Multi-host architecture with verified YAML examples
    - Multi-endpoint testing patterns (critical common use case)
    - Real-world examples: multi-region, multi-account, load balancing
    - Troubleshooting guide and performance analysis tools
  - Created [DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md)
    - Consolidated SSH_SETUP_GUIDE + CONTAINER_DEPLOYMENT_GUIDE + DOCKER_PRODUCTION_DEPLOYMENT
    - SSH deployment with automated ssh-setup command
    - Container deployment with host network mode
    - Production best practices (tmux + daemon patterns)
  - Updated [README.md](README.md) as clean documentation index
  - Reduced from 27+ documentation files to 9 essential docs
  - Moved 18 planning/design docs to archive/ directory

### Configuration Examples

**Global multi-endpoint** (all agents share endpoints):

```yaml
multi_endpoint:
  strategy: round_robin
  endpoints:
    - s3://192.168.1.10:9000/bucket/
    - s3://192.168.1.11:9000/bucket/
    - s3://192.168.1.12:9000/bucket/
```

**Per-agent static mapping** (each agent gets specific endpoints):

```yaml
distributed:
  agents:
    - address: "testhost1:7167"
      id: agent1
      multi_endpoint:
        endpoints:
          - s3://192.168.1.10:9000/bucket/
          - s3://192.168.1.11:9000/bucket/
    
    - address: "testhost2:7167"
      id: agent2
      multi_endpoint:
        endpoints:
          - s3://192.168.1.12:9000/bucket/
          - s3://192.168.1.13:9000/bucket/
```

**NFS multi-mount**:

```yaml
multi_endpoint:
  strategy: round_robin
  endpoints:
    - file:///mnt/nfs1/benchmark/
    - file:///mnt/nfs2/benchmark/
```

### Changed

- **Breaking: perf_log.tsv format** - Removed `is_warmup` column
  - Old format: 31 columns (with is_warmup)
  - New format: 30 columns (cleaner, less redundant)
  - sai3-analyze updated to handle both formats

- **Controller config serialization** - Per-agent YAML generation
  - Controller now serializes agent-specific YAML with only assigned endpoints
  - Lines 1272-1301 in `src/bin/controller.rs`
  - Prevents agents from seeing/accessing non-assigned endpoints
  - Critical for endpoint isolation in distributed testing

### Fixed

- **Critical: Per-agent endpoint isolation bug**
  - **Problem**: Controller sent same config YAML to all agents (all endpoints visible)
  - **Impact**: Each agent accessed ALL endpoints instead of only assigned ones
  - **Root cause**: Controller didn't override `multi_endpoint` in serialized agent config
  - **Fix**: Controller generates per-agent YAML with `multi_endpoint` override matching AgentConfig
  - **Validated**: Real distributed tests confirm Agent-1 only accesses endpoints A&B, Agent-2 only C&D

- **Excel timestamp scientific notation**
  - Timestamps no longer display as 1.77E+18
  - Auto-detection: Parses column names for time unit suffixes
  - Conversion: Applies correct divisor (1000 for ms, 1000000 for us, 1000000000 for ns)

### Validated

- ✅ Two distributed workload tests compared in Excel (multi_endpoint_comparison.xlsx)
- ✅ Endpoint stats files prove per-agent endpoint isolation
  - Agent-1: Only endpoints A & B in endpoint_stats.tsv
  - Agent-2: Only endpoints C & D in endpoint_stats.tsv
- ✅ All YAML examples verified with `--dry-run`:
  - `tests/configs/local_test_2agents.yaml` - 2 agents, 320 files, tree structure
  - `tests/configs/distributed_mixed_test.yaml` - 3 size groups (1KB, 128KB, 1MB)
  - `tests/configs/multi_endpoint_prepare.yaml` - Per-agent endpoints, 100 objects
  - `tests/configs/multi_endpoint_workload.yaml` - 20s duration, 75/20/5 GET/PUT/LIST
- ✅ 3-phase distributed test: prepare → replicate → workload (full validation)

### Technical Details

- Leverages s3dlio v0.9.37 zero-copy `Bytes` API (277 tests passing)
- Multi-endpoint support from s3dlio v0.9.14+ `MultiEndpointStore` with per-endpoint statistics
- Compatible with all storage backends (S3, Azure, GCS, file://, direct://)
- Requires identical namespace across all endpoints (same files accessible from each)
- Load balancing strategies implemented at s3dlio layer (zero overhead in sai3-bench)

### Test Configurations

- `tests/configs/local_test_2agents.yaml` - Basic 2-agent distributed test
- `tests/configs/distributed_mixed_test.yaml` - Mixed size workload (1KB, 128KB, 1MB)
- `tests/configs/multi_endpoint_prepare.yaml` - Per-agent endpoint assignment (Phase 1: prepare)
- `tests/configs/multi_endpoint_workload.yaml` - Multi-endpoint workload test (Phase 3)
- All configs validated with `--dry-run` before release

### Documentation

- See [DISTRIBUTED_TESTING_GUIDE.md](DISTRIBUTED_TESTING_GUIDE.md) for multi-endpoint architecture and examples
- See [DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md) for SSH and container deployment
- See [archive/MULTI_ENDPOINT_ENHANCEMENT_PLAN.md](archive/MULTI_ENDPOINT_ENHANCEMENT_PLAN.md) for design rationale
- See [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) for multi-endpoint configuration reference
- See [README.md](README.md) for documentation index

### Dependencies

- s3dlio v0.9.37 (zero-copy Bytes API)
- Compatible with s3dlio v0.9.16+ (multi-endpoint support)

---

## [0.8.21] - 2026-01-26

### Changed

- **BREAKING: Zero-copy API migration for s3dlio v0.9.36 compatibility**
  - Updated all PUT operations to accept `Bytes` instead of `&[u8]` (eliminates hidden memcpy)
  - Updated all GET operations to return `Bytes` instead of `Vec<u8>` (true zero-copy throughout)
  - Maintains s3dlio's zero-copy architecture: no data copies from storage backend to application

- **Enhanced data generation with fill_controlled_data()**
  - Replaced deprecated `generate_controlled_data_prand()` with high-performance `fill_controlled_data()`
  - Performance improvement: 86-163 GB/s (parallel) vs 3-4 GB/s (old sequential method)
  - Zero-copy workflow: `BytesMut` → in-place fill → `freeze()` to `Bytes`
  - All FillPattern variants now use zero-copy: Zero, Random, and Prand

- **Updated s3dlio dependency to v0.9.36**
  - API breaking change: `ObjectStore::put()` now takes `Bytes` for true zero-copy
  - New `fill_controlled_data()` function for in-place buffer filling
  - Deprecated `generate_controlled_data_prand()` removed from hot paths

### Performance Impact

- **Data generation**: 20-50x faster (86-163 GB/s vs 3-4 GB/s)
- **PUT operations**: Eliminated hidden `.to_vec()` copy (s3dlio v0.9.35 → v0.9.36 migration)
- **GET operations**: Eliminated all `.to_vec()` conversions (returned `Bytes` only used for `.len()`)
- **Memory efficiency**: No intermediate Vec<u8> allocations in hot path

---

## [0.8.20] - 2025-01-20

### Changed

- **Updated s3dlio dependency to v0.9.35**
  - Integrated hardware detection API for automatic optimal configuration
  - Per-run unique RNG seeding ensures non-repeating data patterns across distributed agents
  - Leverages 51.09 GB/s data generation performance from s3dlio v0.9.35
  - Zero-copy architecture with reduced memory overhead

- **Hardware-aware data generation**
  - Automatically detects NUMA nodes and CPU count at runtime
  - Configures optimal parallelism for data generation without manual tuning
  - Each workload run gets unique seed (agent_id + PID + nanosecond timestamp)
  - Eliminates data pattern repetition in multi-run and distributed scenarios

### Fixed

- **PerfLogEntry struct compatibility**
  - Added `get_mean_us`, `put_mean_us`, `meta_mean_us` fields to all test initializations
  - Updated field count assertions from 28 to 31 columns
  - Fixed doctest import in `set_global_rng_seed` example
