# io-bench Replay: Future Enhancements

**Base Version**: v0.4.0 (Simple replay with 1:1 remapping)  
**Document Purpose**: Track planned enhancements for future releases

## v0.4.1 - Advanced Remapping & Filtering

### Advanced Remapping Patterns
Based on warp-replay reference implementation:

#### 1:M Mapping (One-to-Many)
Distribute operations from single source to multiple targets with round-robin:
```yaml
remap:
  "s3://oldbucket/":
    - "s3://newbucket1/"
    - "s3://newbucket2/"
    - "s3://newbucket3/"
```

#### M:1 Mapping (Many-to-One)
Consolidate operations from multiple sources to single target:
```yaml
remap:
  "s3://bucket[1-5]/": "s3://consolidated/"  # Regex pattern
  "az://storage*/": "s3://unified/"          # Glob pattern
```

#### M:N Mapping (Many-to-Many)
Complex remapping with pattern matching:
```yaml
remap:
  patterns:
    - match: "s3://prod-*/data/"
      targets: ["az://backup1/", "az://backup2/"]
    - match: "s3://test-*/"
      targets: ["file:///tmp/test/"]
```

#### Sticky Mapping
Maintain object-to-target affinity with TTL caching:
```yaml
remap:
  sticky:
    enabled: true
    ttl: 30s  # Cache object->target mapping
  targets:
    - "s3://bucket1/"
    - "s3://bucket2/"
```

**Implementation Notes:**
- Add `--remap-config <file.yaml>` flag
- Use regex crate for pattern matching
- State management for sticky mappings (HashMap with TTL)
- Round-robin counter for 1:M distribution

### Operation Filtering

```bash
# Filter by operation type
io-bench replay --op-log ops.tsv.zst --ops GET,PUT

# Filter by size range
io-bench replay --op-log ops.tsv.zst --size-min 1MB --size-max 100MB

# Filter by time range
io-bench replay --op-log ops.tsv.zst --time-range 10s-20s
```

### Configurable Data Generation

```bash
# Control deduplication and compression
io-bench replay --op-log ops.tsv.zst \
  --dedup-factor 2 \     # 50% deduplication
  --compress-factor 3    # 3:1 compressibility
```

Uses s3dlio's existing facilities:
- `generate_controlled_data(size, dedup, compress)`
- `generate_controlled_data_streaming()` for large objects

### Other v0.4.1 Features
- Dry-run mode: `--dry-run` (validate without executing)
- Comparison mode: Generate diff between capture and replay logs
- Better progress bars with operation breakdown

**Estimated Effort**: 5-7 days

## v0.4.2 - Memory Optimization

### Streaming Op-Log Parsing
Current: Load entire op-log into Vec (fine for most workloads)  
Future: Stream operations for multi-GB logs

```rust
// Iterator-based parsing instead of Vec
pub fn parse_oplog_streaming(path: &Path) -> impl Iterator<Item = Result<OpLogEntry>>
```

Benefits:
- Constant memory usage regardless of log size
- Can start replay before parsing completes
- Handles arbitrarily large logs

**Estimated Effort**: 2-3 days

## v0.5.0 - Analysis & Comparison Tools

### Replay Statistics
Compare original capture vs replay execution:
```bash
io-bench replay --op-log original.tsv.zst \
  --stats-output comparison.json

# Generates:
# - Latency comparison (capture vs replay)
# - Throughput comparison
# - Error rate comparison
# - HDR histogram overlays
```

### Analysis Subcommand
```bash
# Analyze op-log without replaying
io-bench analyze --op-log ops.tsv.zst

# Output:
# - Operation counts by type
# - Size distribution
# - Concurrency levels over time
# - Latency percentiles by operation type
```

### Comparison Tool
```bash
# Compare two op-logs
io-bench compare --baseline original.tsv.zst --test replayed.tsv.zst

# Output:
# - Side-by-side latency comparison
# - Regression detection
# - Visualization data (JSON/CSV)
```

**Estimated Effort**: 5-7 days

## v0.6.0 - Distributed Replay

Coordinate replay across multiple agents (similar to existing gRPC infrastructure):

```bash
# Controller
iobench-ctl --agents host1:7761,host2:7761,host3:7761 \
  replay --op-log large-workload.tsv.zst \
  --partition-by client_id
```

Features:
- Automatic workload partitioning
- Synchronized start time across agents
- Aggregated statistics collection
- Fault tolerance

**Estimated Effort**: 10-14 days

## v0.7.0 - Workload Transformation

Transform workloads during replay:

```bash
# Scale up operations
io-bench replay --op-log ops.tsv.zst --scale 10x

# Substitute operations
io-bench replay --op-log ops.tsv.zst \
  --substitute "GET -> HEAD"  # Convert GETs to HEADs

# Synthetic variation
io-bench replay --op-log ops.tsv.zst \
  --jitter 10%  # Add 10% timing jitter
```

**Estimated Effort**: 7-10 days

## Implementation Priorities

### High Priority (v0.4.1)
1. Advanced remapping (most requested feature)
2. Operation filtering (simple, high value)
3. Configurable data generation (uses existing s3dlio APIs)

### Medium Priority (v0.4.2 - v0.5.0)
4. Streaming op-log parsing (needed for very large logs)
5. Replay statistics and comparison (performance validation)
6. Analysis tools (workload understanding)

### Lower Priority (v0.6.0+)
7. Distributed replay (for extreme scale)
8. Workload transformation (advanced use cases)

## Design Principles

1. **Backward Compatibility**: New flags optional, existing behavior preserved
2. **Fail-Fast Default**: Errors stop replay unless `--continue-on-error`
3. **s3dlio Integration**: Always use s3dlio facilities, never reinvent
4. **Universal Backend**: All features work across file://, direct://, s3://, az://
5. **Performance**: Microsecond timing precision maintained
6. **Simplicity First**: Start simple (v0.4.0), add complexity incrementally

## References

- **warp-replay**: https://github.com/russfellows/warp-replay
  - `cli/replay.go` - Absolute timeline scheduling
  - `pkg/config/config.go` - Remapping configuration
  - `pkg/state/state.go` - Sticky mapping implementation

- **s3dlio**: Data generation API
  - `src/data_gen.rs` - generate_controlled_data, streaming variants
  - Already integrated in v0.4.0

---

**Note**: This document will be updated as features are implemented and new requirements emerge.
