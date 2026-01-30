# sai3-bench Multi-Endpoint Feature Handoff

**Date:** January 29, 2026  
**Branch:** `feature/multi-endpoint-support`  
**Last Commit:** `c727eca` - feat(multi-endpoint): Complete per-endpoint statistics and TSV export

---

## Overview

This handoff covers the multi-endpoint feature implementation for sai3-bench. The feature enables:
1. **Shared storage mode** - All workers access all endpoints with load balancing
2. **Per-endpoint statistics** - Track ops, bytes, latency per endpoint
3. **TSV export** - Export endpoint stats to results_dir

The s3dlio dependency (v0.9.37) with URI rewriting support has been merged and released.

---

## Current Status

### ✅ Completed

| Component | Status | Notes |
|-----------|--------|-------|
| `use_multi_endpoint` flag on OpSpec | ✅ Done | PUT, GET, DELETE support |
| `ArcMultiEndpointWrapper` | ✅ Done | Shared Arc for stats access |
| Per-endpoint stats collection | ✅ Done | workload.rs, prepare.rs |
| Per-endpoint stats display | ✅ Done | Console output at completion |
| TSV export - workload phase | ✅ Done | `workload_endpoint_stats.tsv` |
| TSV export - prepare phase | ✅ Done | `prepare_endpoint_stats.tsv` |
| Config: endpoint_uris as Vec<String> | ✅ Done | Fixed relative path handling |
| s3dlio v0.9.37 dependency | ✅ Done | URI rewriting support |

### ⏳ Pending Before PR

| Task | Priority | Notes |
|------|----------|-------|
| Update Cargo.toml s3dlio dependency | ✅ DONE | Changed from path to git tag v0.9.37 |
| Final build verification | HIGH | Zero warnings required |
| Update version to v0.8.22 | ✅ DONE | Cargo.toml |
| Update CHANGELOG.md | ✅ DONE | v0.8.22 already documented |
| Update README.md | MEDIUM | Brief mention of new feature |
| Run test suite | HIGH | `cargo test` |
| Documentation review | MEDIUM | Check docs/MULTI_ENDPOINT_*.md |

---

## Files Changed

### Core Implementation
- [src/workload.rs](src/workload.rs) - Per-endpoint stats collection and display
- [src/prepare.rs](src/prepare.rs) - Per-endpoint stats during prepare, TSV export
- [src/config.rs](src/config.rs) - endpoint_uris as Vec<String>, relative path handling
- [src/tsv_export.rs](src/tsv_export.rs) - `write_endpoint_stats_tsv()` function
- [src/results_dir.rs](src/results_dir.rs) - Endpoint stats file path constants

### Binary Updates
- [src/main.rs](src/main.rs) - Pass results_dir for TSV export
- [src/bin/agent.rs](src/bin/agent.rs) - Pass results_dir
- [src/bin/controller.rs](src/bin/controller.rs) - Pass results_dir

### Config/Validation
- [src/validation.rs](src/validation.rs) - Skip file:// prefix check for endpoint_uris
- [Cargo.toml](Cargo.toml) - s3dlio dependency (✅ updated to v0.9.37 tag)

### Documentation
- [docs/MULTI_ENDPOINT_IMPLEMENTATION_STATUS.md](docs/MULTI_ENDPOINT_IMPLEMENTATION_STATUS.md)
- [docs/MULTI_ENDPOINT_ROUTING_ANALYSIS.md](docs/MULTI_ENDPOINT_ROUTING_ANALYSIS.md) (new)
- [tests/configs/multi_endpoint_nfs.yaml](tests/configs/multi_endpoint_nfs.yaml)

---

## Key Implementation Details

### 1. ArcMultiEndpointWrapper

New wrapper in `workload.rs` that allows `Arc<MultiEndpointStore>` to be used through the ObjectStore trait while preserving stats access:

```rust
struct ArcMultiEndpointWrapper {
    inner: Arc<MultiEndpointStore>,
}

impl ObjectStore for ArcMultiEndpointWrapper {
    // Delegates all methods to inner
}
```

### 2. use_multi_endpoint Flag

Added to all OpSpec variants:
```yaml
workload:
  - op: get
    use_multi_endpoint: true  # Routes through MultiEndpointStore
    weight: 100
```

When `true`:
- Uses `MultiEndpointStore` with round-robin load balancing
- Per-endpoint statistics tracked
- URIs rewritten to target selected endpoint (s3dlio v0.9.37)

When `false` (default):
- Partitioned mode - each worker uses single endpoint
- `endpoint_uri_list[worker_id % num_endpoints]`

### 3. TSV Export Format

```
endpoint	operations	bytes	avg_latency_ms	ops_per_sec	mb_per_sec
file:///nfs/ds1/	2500	10485760000	1.234	2027.3	8515.6
file:///nfs/ds2/	2500	10485760000	1.198	2089.7	8778.1
...
```

---

## Testing Verification

### Verified Working
```bash
# 4-endpoint shared NFS test
./target/release/sai3-bench run --config tests/configs/multi_endpoint_nfs.yaml

# Results:
# - 25% load distribution across endpoints (within ±5%)
# - Per-endpoint stats displayed at completion
# - TSV files generated in results_dir
```

### Test Config Used
```yaml
# /tmp/test_multi_ep.yaml
endpoint_uris:
  - file:///tmp/ep1/
  - file:///tmp/ep2/
  - file:///tmp/ep3/
  - file:///tmp/ep4/

prepare:
  - op: put
    use_multi_endpoint: true
    count: 1000
    size: 4194304

workload:
  - op: get
    use_multi_endpoint: true
    weight: 100
```

---

## Before Final Commit

### 1. Update Cargo.toml

Change s3dlio from local path to git tag:
```toml
# FROM:
s3dlio = { path = "../s3dlio" }

# TO:
s3dlio = { git = "https://github.com/russfellows/s3dlio.git", tag = "v0.9.37" }
```

### 2. Update Version

In `Cargo.toml`:
```toml
version = "0.8.20"  # was 0.8.19
```

### 3. Build and Test

```bash
cargo build --release  # Must have zero warnings
cargo test             # All tests pass
cargo clippy           # Zero warnings
```

### 4. Update CHANGELOG.md

Add entry for v0.8.22 with multi-endpoint feature documentation.

### 5. Commit and Push

```bash
git add -A
git commit -m "feat(multi-endpoint): Per-endpoint statistics and TSV export

- Add use_multi_endpoint flag to PUT/GET/DELETE operations
- Per-endpoint statistics: ops, bytes, latency, throughput
- TSV export: workload_endpoint_stats.tsv, prepare_endpoint_stats.tsv
- ArcMultiEndpointWrapper for shared stats access
- Requires s3dlio v0.9.37 for URI rewriting support

Tested with 4-endpoint shared NFS configuration.
25% load distribution verified across endpoints."

git push origin feature/multi-endpoint-support
```

---

## PR Description Template

```markdown
## Summary

Adds multi-endpoint support with per-endpoint statistics and TSV export.

## Features

- `use_multi_endpoint: true` flag on PUT/GET/DELETE operations
- Per-endpoint statistics (ops, bytes, avg_latency, throughput)
- TSV export to results_dir
- Shared storage mode: all workers access all endpoints

## Dependencies

- Requires s3dlio v0.9.37 (URI rewriting support)

## Testing

- 4-endpoint shared NFS configuration verified
- 25% load distribution across endpoints (within ±5%)
```

---

## Related

- **s3dlio v0.9.37** - Multi-endpoint URI rewriting (merged, released)
- **sai3-bench issue** - Multi-endpoint statistics request (link TBD)
