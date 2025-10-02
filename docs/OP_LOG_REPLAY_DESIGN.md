# Op-Log Replay Feature Design

**Status**: Planned for v0.4.0  
**Created**: October 1, 2025  
**Dependencies**: s3dlio v0.8.12 op-log capture (✅ implemented in v0.3.2)

## Overview

The op-log replay feature will enable faithful reproduction of captured workloads, allowing performance comparison across different storage backends, configurations, or time periods. This complements the existing op-log capture capability introduced in v0.3.2.

## Current Implementation (v0.3.2)

### Op-Log Capture ✅
- **Functionality**: `--op-log <PATH>` flag captures all storage operations to a zstd-compressed TSV file
- **Format**: Always `.tsv.zst` (zstd compressed)
- **Backends**: Works across all 4 backends (file://, direct://, s3://, az://)
- **Scope**: Captures operations from all commands: get, put, delete, list, stat, run

### TSV Format Specification
```
idx     thread  op      client_id       n_objects       bytes   endpoint        file    error   start   first_byte      end     duration_ns
```

**Column Descriptions:**
- `idx` - Sequential operation number (0-based)
- `thread` - Thread/worker ID that executed the operation
- `op` - Operation type: GET, PUT, DELETE, LIST, HEAD/STAT, COPY
- `client_id` - Internal client identifier
- `n_objects` - Number of objects involved (typically 1, or count for LIST)
- `bytes` - Data transferred in bytes (0 for metadata operations)
- `endpoint` - Storage backend URL scheme (file://, s3://, az://, direct://)
- `file` - Full object path/key
- `error` - Error message if operation failed (empty for success)
- `start` - Operation start timestamp (RFC3339)
- `first_byte` - First byte received/sent timestamp (RFC3339)
- `end` - Operation completion timestamp (RFC3339)
- `duration_ns` - Total operation duration in nanoseconds

**Example Entry:**
```
59338   17170585774888887431    GET     1       10      file:// /tmp/s3bench-test/data/temp_file1.txt    2025-10-01T22:51:59.331821968Z  2025-10-01T22:51:59.331909172Z  87204
```

## Proposed Replay Feature (v0.4.0)

### Command Structure
```bash
io-bench replay --op-log <PATH> [OPTIONS]
```

### Core Replay Modes

#### 1. **Sequential Replay** (Default)
Execute operations in exact `idx` order, one at a time.
```bash
io-bench replay --op-log workload.tsv.zst --mode sequential
```
- **Use Case**: Debugging, precise reproduction
- **Concurrency**: 1 (single-threaded)
- **Timing**: As fast as possible, no delays

#### 2. **Parallel Replay**
Execute all operations respecting original concurrency patterns.
```bash
io-bench replay --op-log workload.tsv.zst --mode parallel --concurrency 4
```
- **Use Case**: Performance testing, throughput measurement
- **Concurrency**: Configurable (default: auto-detect from thread IDs in log)
- **Timing**: As fast as possible, operations may overlap

#### 3. **Timing-Faithful Replay**
Replay operations respecting original timestamps and inter-operation delays.
```bash
io-bench replay --op-log workload.tsv.zst --mode timing-faithful [--speed-multiplier 2.0]
```
- **Use Case**: Realistic workload simulation, pattern preservation
- **Concurrency**: Matches original (derived from thread IDs)
- **Timing**: Respects original delays between operations
- **Option**: `--speed-multiplier` to speed up/slow down (e.g., 2.0 = 2x faster, 0.5 = half speed)

### Key Configuration Options

#### Backend Retargeting
Allow replaying captured operations against a different backend:
```bash
# Captured from file://, replay to S3
io-bench replay --op-log file_workload.tsv.zst --target s3://my-bucket/prefix/

# Captured from S3, replay to Azure
io-bench replay --op-log s3_workload.tsv.zst --target az://storage-account/container/
```

**Path Translation Rules:**
- Original: `file:///tmp/s3bench-test/data/obj_001.dat`
- With `--target s3://my-bucket/test/`
- Result: `s3://my-bucket/test/data/obj_001.dat`

**Considerations:**
- Strip original endpoint prefix, preserve relative path structure
- Option to preserve full path vs. flatten: `--preserve-paths` (default) vs. `--flatten-paths`

#### Operation Filtering
Replay only specific operation types or patterns:
```bash
# Only GET operations
io-bench replay --op-log workload.tsv.zst --ops GET

# Only GET and PUT
io-bench replay --op-log workload.tsv.zst --ops GET,PUT

# Only operations on specific paths
io-bench replay --op-log workload.tsv.zst --path-filter "*/data/*.dat"

# Skip failed operations from original capture
io-bench replay --op-log workload.tsv.zst --skip-errors
```

#### Concurrency Override
Override original concurrency for parallel replay:
```bash
# Force 8 concurrent workers regardless of original
io-bench replay --op-log workload.tsv.zst --concurrency 8

# Auto-detect from thread IDs in log (default)
io-bench replay --op-log workload.tsv.zst --concurrency auto
```

#### Replay Logging
Create new op-log during replay for comparison:
```bash
io-bench replay --op-log original.tsv.zst --replay-log new.tsv.zst
```

**Comparison Use Case:**
```bash
# Capture baseline on file://
io-bench -v --op-log baseline.tsv.zst run --config workload.yaml

# Replay to S3, capture new timings
io-bench replay --op-log baseline.tsv.zst --target s3://bucket/ --replay-log s3_replay.tsv.zst

# Compare performance (future tool)
io-bench compare --baseline baseline.tsv.zst --replay s3_replay.tsv.zst
```

### Data Handling for PUT Operations

**Challenge**: Replay needs data to PUT, but op-log only records size.

**Proposed Solutions:**

1. **Random Data Generation** (Default)
   ```bash
   io-bench replay --op-log workload.tsv.zst --put-data random
   ```
   - Generate random data matching original byte size
   - Fast, no external dependencies
   - Data content differs from original

2. **Zero-Filled Data**
   ```bash
   io-bench replay --op-log workload.tsv.zst --put-data zeros
   ```
   - Generate zero-filled buffers
   - Fastest option, minimal CPU
   - Useful for testing write throughput

3. **Reference Data Directory** (Future)
   ```bash
   io-bench replay --op-log workload.tsv.zst --put-data-from /path/to/data/
   ```
   - Look up original files by path
   - Requires preserved data from original capture
   - Exact reproduction of original data

4. **Skip PUT Operations**
   ```bash
   io-bench replay --op-log workload.tsv.zst --ops GET,DELETE
   ```
   - Filter out PUTs entirely
   - Focus on read/metadata workloads

### Error Handling

#### Skip vs. Fail
```bash
# Stop on first error (default)
io-bench replay --op-log workload.tsv.zst

# Continue on errors, report at end
io-bench replay --op-log workload.tsv.zst --continue-on-error

# Skip operations that failed in original capture
io-bench replay --op-log workload.tsv.zst --skip-original-errors
```

#### Dry Run Mode
```bash
io-bench replay --op-log workload.tsv.zst --dry-run
```
- Parse and validate op-log
- Print operation summary
- Don't execute any operations
- Useful for testing retargeting logic

### Output and Metrics

#### Progress Reporting
- Operation-based progress bar showing `current_idx / total_ops`
- Live metrics: current throughput, operations/sec
- ETA based on replay mode (faster for parallel than timing-faithful)

#### Summary Report
```
=== Replay Summary ===
Op-log: workload.tsv.zst
Original capture: 5.06s (59,338 ops)
Replay time: 3.42s
Replay mode: parallel (concurrency: 4)
Target: s3://my-bucket/test/

Operations replayed: 59,338
  GET: 41,634 (0.58 MB)
  PUT: 17,704 (17.29 MB)
  Skipped: 0
  Errors: 3

Throughput: 17,344 ops/s (5.21 MB/s)
Latency comparison:
  GET: p50=142µs (+44% vs. original 98µs), p95=289µs (+87% vs. original 154µs)
  PUT: p50=856µs (+756% vs. original 100µs), p95=2.1ms (+126% vs. original 928µs)
```

## Implementation Roadmap

### Phase 1: Basic Replay (v0.4.0-alpha)
- [ ] TSV parsing (handle zstd decompression)
- [ ] Sequential replay mode
- [ ] Backend retargeting (basic path translation)
- [ ] Random data generation for PUT operations
- [ ] Basic error handling (stop on error)
- [ ] Progress bar and summary metrics

### Phase 2: Advanced Replay (v0.4.0-beta)
- [ ] Parallel replay mode
- [ ] Concurrency auto-detection and override
- [ ] Operation filtering (--ops, --path-filter)
- [ ] --continue-on-error support
- [ ] --replay-log for performance comparison
- [ ] Dry-run mode

### Phase 3: Timing Replay (v0.4.0)
- [ ] Timing-faithful replay mode
- [ ] Speed multiplier support
- [ ] Delay calculation between operations
- [ ] Thread/worker mapping from original thread IDs

### Phase 4: Comparison Tools (v0.4.1+)
- [ ] `io-bench compare` command
- [ ] Side-by-side latency histograms
- [ ] Performance regression detection
- [ ] CSV/JSON export for analysis tools

## Design Questions for User Input

### 1. Timing Behavior
**Q**: Should replay respect original timing (delays between ops) or just execute all ops as fast as possible?

**A**: Support both via `--mode` flag:
- `sequential`: As fast as possible, one at a time
- `parallel`: As fast as possible, concurrent
- `timing-faithful`: Respect original delays (with optional speed multiplier)

**Default**: `parallel` (most useful for performance testing)

### 2. Backend Retargeting
**Q**: Should we allow retargeting to a different backend/URI?

**A**: Yes, via `--target` flag. Critical for comparing backends.

**Path Translation**: Strip original endpoint, preserve relative path structure by default.

### 3. Concurrency Control
**Q**: Should replay use the same concurrency as captured, or allow overriding?

**A**: Both:
- `--concurrency auto` (default): Detect from thread IDs in log
- `--concurrency N`: Override with specific value
- Mode-dependent: sequential=1, parallel=auto, timing-faithful=original

### 4. Replay Logging
**Q**: Should replay create a new op-log to compare performance differences?

**A**: Yes, via `--replay-log` flag. Essential for before/after analysis.

**Comparison workflow**:
1. Capture baseline: `--op-log baseline.tsv.zst`
2. Replay with logging: `--replay-log new.tsv.zst`
3. Compare (future): `io-bench compare --baseline baseline.tsv.zst --replay new.tsv.zst`

### 5. PUT Data Source
**Q**: How to handle PUT operations that need data?

**A**: Multiple strategies via `--put-data` flag:
- `random` (default): Generate random data
- `zeros`: Zero-filled buffers (fastest)
- `from=<dir>`: Load from directory (future, exact reproduction)
- Or filter them out with `--ops GET,DELETE`

## Testing Strategy

### Unit Tests
- TSV parsing with various formats
- Path translation logic for retargeting
- Concurrency detection from thread IDs
- Timing calculation for faithful replay

### Integration Tests
1. **Capture & Replay Loop**
   ```bash
   # Capture a workload
   io-bench --op-log capture.tsv.zst run --config test.yaml
   # Replay it back to same backend
   io-bench replay --op-log capture.tsv.zst
   # Verify operation count matches
   ```

2. **Cross-Backend Replay**
   ```bash
   # Capture from file://
   io-bench --op-log file.tsv.zst get --uri file:///tmp/test/*
   # Replay to S3
   io-bench replay --op-log file.tsv.zst --target s3://test-bucket/
   ```

3. **Filtered Replay**
   ```bash
   # Capture mixed workload
   io-bench --op-log mixed.tsv.zst run --config mixed.yaml
   # Replay only GETs
   io-bench replay --op-log mixed.tsv.zst --ops GET
   ```

### Performance Validation
- Verify parallel replay achieves expected concurrency
- Confirm timing-faithful replay matches original timing patterns
- Benchmark replay overhead (parsing, scheduling)

## Future Enhancements (Post v0.4.0)

### Advanced Filtering
- Time-based filtering: `--time-range 10s-20s` (replay operations between 10-20 seconds)
- Size-based filtering: `--size-range 1KB-1MB` (replay only objects in size range)
- Error filtering: `--replay-only-errors` (re-attempt failed operations)

### Workload Transformation
- Scale operations: `--scale 2.0` (replay 2x the operations)
- Operation substitution: `--substitute GET=HEAD` (test metadata performance)
- Path randomization: `--randomize-paths` (avoid cache effects)

### Distributed Replay
- Multi-agent replay for large-scale testing
- Coordinate timing across multiple nodes
- Aggregate results from distributed replay

### Analysis Integration
- Export to Prometheus format
- Grafana dashboard templates
- Jupyter notebook integration for analysis

## Related Documentation
- [CHANGELOG.md](CHANGELOG.md) - Version history
- [USAGE.md](USAGE.md) - General usage guide
- [CONFIG.sample.yaml](CONFIG.sample.yaml) - Workload configuration examples
- [copilot-instructions.md](../.github/copilot-instructions.md) - Development guide

## References
- s3dlio op-log implementation: https://github.com/russfellows/s3dlio (v0.8.12+)
- HDR Histogram for metrics: https://github.com/HdrHistogram/HdrHistogram_rust
- Indicatif for progress bars: https://github.com/console-rs/indicatif
