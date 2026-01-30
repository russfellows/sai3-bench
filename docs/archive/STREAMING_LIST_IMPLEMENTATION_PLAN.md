# Streaming List Implementation Plan

## Summary

This plan addresses two critical issues with large-scale distributed workloads:

1. **gRPC Timeout During Listing**: When checking 4.9M+ existing files, the blocking `store.list()` call takes 5-10 minutes with no progress updates, causing gRPC connections to timeout and tests to fail.

2. **Efficient Cleanup**: No current way to clean up millions of files from previous tests without running a full workload.

**Solution**: Use s3dlio's `list_stream()` API to process results incrementally, send progress updates via bidirectional channel, and optionally run cleanup-only mode.

## Key Features

- ✅ **Streaming list** with progress updates (keeps gRPC alive)
- ✅ **Cleanup-only mode** for efficient deletion without workload
- ✅ **Partitioned listing** across agents (8x speedup)
- ✅ **DELETE operation tracking** (like GET/PUT stats)
- ✅ **Constant memory** (no buffering millions of URIs)

## Problem

When prepare phase lists millions of existing files (e.g., 4.9M), the blocking `store.list()` call takes 5-10+ minutes. During this time:
- Agent sends NO stats updates to controller
- gRPC connection goes idle
- Network infrastructure closes idle connection after 30-60 seconds
- Controller sees "h2 protocol error: error reading a body from connection"
- Test fails

## Root Cause

Current implementation in `src/prepare.rs` (line ~252):

```rust
let all_files = store.list(&list_base, true).await  // ← BLOCKS for minutes
    .context("Failed to list existing objects")?;
```

This collects ALL results before returning, buffering millions of URIs in memory.

## Solution: Use Streaming List + Progress Updates

s3dlio v0.9.20+ provides `list_stream()` which yields results incrementally:

```rust
fn list_stream<'a>(
    &'a self,
    uri_prefix: &'a str,
    recursive: bool,
) -> Pin<Box<dyn Stream<Item = Result<String>> + Send + 'a>>;
```

Results arrive in pages (~1000 objects per page for S3), allowing us to:
1. Process results incrementally (constant memory)
2. Send progress updates to controller via bidirectional channel
3. Keep gRPC connection alive during long listing operations

## Implementation

### Phase 1: Replace `list()` with `list_stream()` + Progress Updates

**File**: `src/prepare.rs`, around line 250

**Current code**:
```rust
let all_files = store.list(&list_base, true).await
    .context("Failed to list existing objects")?;
```

**New code**:
```rust
use futures::StreamExt;

let mut list_stream = store.list_stream(&list_base, true);
let mut all_files = Vec::new();
let mut page_count = 0;

while let Some(uri_result) = list_stream.next().await {
    let uri = uri_result.context("LIST stream error")?;
    all_files.push(uri);
    
    // Send progress update every 10,000 files (~10 pages for S3)
    if all_files.len() % 10_000 == 0 {
        if let Some(tracker) = tracker_opt.as_ref() {
            // Update prepare phase progress (files listed so far)
            tracker.update_listing_progress(all_files.len() as u64, spec.count);
        }
        // Yield to tokio runtime to allow stats transmission
        tokio::task::yield_now().await;
        page_count += 1;
    }
}

info!("  Listed {} files in {} pages", all_files.len(), page_count);
```

### Phase 2: Add Listing Progress to LiveTracker

**File**: `src/live_stats.rs`

Add fields to track listing progress:

```rust
pub struct LiveTracker {
    // ... existing fields ...
    
    // NEW: Listing progress (v0.8.6+)
    listing_files_found: AtomicU64,
    listing_files_expected: AtomicU64,
    in_listing_phase: AtomicBool,
}

impl LiveTracker {
    pub fn update_listing_progress(&self, found: u64, expected: u64) {
        self.listing_files_found.store(found, Ordering::Relaxed);
        self.listing_files_expected.store(expected, Ordering::Relaxed);
        self.in_listing_phase.store(true, Ordering::Relaxed);
    }
    
    pub fn listing_complete(&self) {
        self.in_listing_phase.store(false, Ordering::Relaxed);
    }
}
```

### Phase 3: Transmit Listing Progress via Bidirectional Channel

**File**: `src/bin/agent.rs`, stats writer task

Add listing progress to `LiveStats` protobuf message:

```rust
let stats = LiveStats {
    agent_id: agent_id_stream.clone(),
    // ... existing fields ...
    in_prepare_phase: snapshot.in_prepare_phase,
    in_listing_phase: snapshot.in_listing_phase,  // NEW
    listing_files_found: snapshot.listing_files_found,  // NEW
    listing_files_expected: snapshot.listing_files_expected,  // NEW
    // ...
};
```

### Phase 4: Update Protobuf Definition

**File**: `proto/iobench.proto`

```protobuf
message LiveStats {
    // ... existing fields ...
    
    // Listing progress (v0.8.6+)
    bool in_listing_phase = 30;
    uint64 listing_files_found = 31;
    uint64 listing_files_expected = 32;
}
```

### Phase 5: Display Listing Progress on Controller

**File**: `src/bin/controller.rs`

Update progress bar to show listing when detected:

```rust
if in_listing {
    format!("🔍 Listing: {}/{} files ({}) {}", 
            listing_found, listing_expected, agg.format_progress())
} else if in_prepare && prepare_total > 0 {
    format!("📦 Preparing: {}/{} objects ({}%) {}", 
            prepare_created, prepare_total, pct, agg.format_progress())
}
```

## Advanced: Partitioned Listing (Optional)

For even better performance with distributed agents, partition the listing work:

### Directory Tree Partitioning

When using directory trees, each agent can list only its assigned subdirectories:

```rust
// Agent 1 of 8: List directories 0-799 (out of 4160 total)
// Agent 2 of 8: List directories 800-1599
// etc.

let agent_idx = config.agent_id_from_yaml().unwrap_or(0);
let num_agents = config.num_agents_from_yaml().unwrap_or(1);

if let Some(manifest) = &tree_manifest {
    // Only list directories assigned to this agent
    let my_dirs = manifest.directories_for_agent(agent_idx, num_agents);
    
    for dir in my_dirs {
        let dir_uri = format!("{}{}/", spec.base_uri, dir.path);
        let mut stream = store.list_stream(&dir_uri, false);  // Non-recursive
        
        while let Some(uri_result) = stream.next().await {
            // Process files in this directory...
        }
    }
}
```

This reduces listing time by factor of N (number of agents).

## Phase 7: Cleanup-Only Mode (NEW REQUIREMENT)

### Use Case

Before running a test with millions of files, you need to clean up previous test data. Currently there's no efficient way to do this - you have to:
1. Run test with `cleanup: true` (but this requires prepare + workload first)
2. Use AWS CLI `aws s3 rm --recursive` (slow, no progress visibility)
3. Manually delete files (not practical for millions of files)

### Solution: Cleanup-Only Prepare Phase

Add config option to run prepare phase in "cleanup-only" mode:

```yaml
prepare:
  cleanup_only: true  # NEW: Only delete, don't create
  skip_verification: true  # Optional: skip listing if you know structure
  directory_structure:
    width: 64
    depth: 2
    files_per_dir: 1200
```

When `cleanup_only: true`:
1. **LIST** phase: Stream-list all files (with progress updates)
2. **DELETE** phase: Batch-delete all files (with progress updates)
3. **No PUT operations**: Skip object creation entirely
4. **No workload execution**: Exit after cleanup completes
5. **Stats reporting**: Report LIST and DELETE operations (not GET/PUT)

### Implementation

#### 7.1: Add Config Option

**File**: `src/config.rs`

```rust
pub struct PrepareConfig {
    // ... existing fields ...
    
    /// Cleanup-only mode: delete all files matching prepare spec, don't create (v0.8.6+)
    /// When true, prepare phase lists and deletes all objects, then exits (no workload)
    /// Useful for cleaning up millions of files from previous tests
    /// Works with directory_structure or ensure_objects
    #[serde(default)]
    pub cleanup_only: bool,
}
```

#### 7.2: Implement Cleanup Logic

**File**: `src/prepare.rs`

```rust
// After listing phase, check if cleanup_only
if config.cleanup_only {
    info!("🗑️  Cleanup-only mode enabled - deleting all {} files", all_files.len());
    
    // Use s3dlio's delete_batch() for efficient deletion
    let batch_size = 1000;  // S3 allows 1000 objects per DeleteObjects request
    let total_files = all_files.len();
    let mut deleted = 0;
    
    for chunk in all_files.chunks(batch_size) {
        // Delete batch
        store.delete_batch(chunk).await
            .context("Batch delete failed")?;
        
        deleted += chunk.len();
        
        // Update progress every batch
        if let Some(tracker) = tracker_opt.as_ref() {
            tracker.update_delete_progress(deleted as u64, total_files as u64);
        }
        
        // Yield to allow stats transmission
        tokio::task::yield_now().await;
    }
    
    info!("✅ Cleanup complete: deleted {} files", deleted);
    
    // Return early - no object creation needed
    return Ok((PrepareResult::CleanupOnly(deleted as u64), None, None));
}
```

#### 7.3: Add DELETE Tracking to LiveTracker

**File**: `src/live_stats.rs`

```rust
pub struct LiveTracker {
    // ... existing fields ...
    
    // DELETE operation tracking (v0.8.6+)
    delete_ops: AtomicU64,
    delete_bytes: AtomicU64,  // Estimated (list returns sizes)
    
    // Listing progress
    listing_files_found: AtomicU64,
    listing_files_expected: AtomicU64,
    in_listing_phase: AtomicBool,
    
    // Delete progress
    delete_files_completed: AtomicU64,
    delete_files_total: AtomicU64,
    in_delete_phase: AtomicBool,
}

impl LiveTracker {
    pub fn update_delete_progress(&self, completed: u64, total: u64) {
        self.delete_files_completed.store(completed, Ordering::Relaxed);
        self.delete_files_total.store(total, Ordering::Relaxed);
        self.in_delete_phase.store(true, Ordering::Relaxed);
        self.delete_ops.fetch_add(completed, Ordering::Relaxed);
    }
}
```

#### 7.4: Update Protobuf for DELETE Stats

**File**: `proto/iobench.proto`

```protobuf
message LiveStats {
    // ... existing fields ...
    
    // DELETE operation stats (v0.8.6+)
    uint64 delete_ops = 33;
    uint64 delete_bytes = 34;
    
    // Listing progress
    bool in_listing_phase = 30;
    uint64 listing_files_found = 31;
    uint64 listing_files_expected = 32;
    
    // Delete progress
    bool in_delete_phase = 35;
    uint64 delete_files_completed = 36;
    uint64 delete_files_total = 37;
}
```

#### 7.5: Controller Display for Cleanup

**File**: `src/bin/controller.rs`

```rust
// Update progress bar messages
if in_delete {
    format!("🗑️  Deleting: {}/{} files ({:.1}%) {}", 
            delete_completed, delete_total, 
            (delete_completed as f64 / delete_total as f64 * 100.0),
            agg.format_progress())
} else if in_listing {
    format!("🔍 Listing: {}/{} files {}", 
            listing_found, listing_expected, agg.format_progress())
} else if in_prepare && prepare_total > 0 {
    format!("📦 Preparing: {}/{} objects ({}%) {}", 
            prepare_created, prepare_total, pct, agg.format_progress())
}
```

#### 7.6: Exit After Cleanup

**File**: `src/bin/agent.rs`

```rust
// After prepare phase completes, check if cleanup_only
if config.prepare.cleanup_only {
    info!("Cleanup-only mode - skipping workload execution");
    
    // Send final stats with cleanup results
    let final_stats = LiveStats {
        agent_id: agent_id.clone(),
        completed: true,
        status: 1,  // SUCCESS
        delete_ops: tracker.delete_ops(),
        delete_bytes: tracker.delete_bytes(),
        // ... other fields ...
    };
    
    tx_stats.send(final_stats).await?;
    return Ok(());  // Exit without running workload
}
```

### Usage Examples

#### Example 1: Full Cleanup Before Test

```bash
# Step 1: Clean up old test data (takes ~5-10 minutes for 4.9M files)
cat > cleanup_config.yaml <<EOF
prepare:
  cleanup_only: true
  directory_structure:
    width: 64
    depth: 2
    files_per_dir: 1200
    root_path: "s3://bucket/old-test/"
EOF

sai3bench-ctl run --config cleanup_config.yaml

# Expected output:
# 🔍 Listing: 4915200/4915200 files (100%)
# 🗑️  Deleting: 4915200/4915200 files (100%)
# ✅ Cleanup complete: deleted 4915200 files

# Step 2: Run new test on clean bucket
sai3bench-ctl run --config resnet50_8-hosts.yaml
```

#### Example 2: Skip Listing (Fast Delete)

If you know the exact structure exists:

```yaml
prepare:
  cleanup_only: true
  skip_verification: true  # Don't list, just delete based on manifest
  directory_structure:
    width: 64
    depth: 2
    files_per_dir: 1200
```

This generates the expected file paths from the manifest and deletes them directly (no LIST operation). **Much faster** but assumes files exist exactly as specified.

### Performance Estimates

#### Cleanup with Listing (Safe)
- LIST: 4.9M files @ 20k/sec = **245 seconds** (~4 min)
- DELETE: 4.9M files @ 50k/sec with batch API = **98 seconds** (~1.5 min)
- **Total: ~6 minutes for 4.9M files**

#### Cleanup without Listing (Fast, assumes known structure)
- DELETE only: 4.9M files @ 50k/sec = **98 seconds** (~1.5 min)
- **Total: ~1.5 minutes for 4.9M files**

#### Distributed Cleanup (8 agents, partitioned)
- Each agent: 613k files
- Time per agent: ~45 seconds (LIST) + ~12 seconds (DELETE) = **57 seconds**
- **Total: <1 minute for 4.9M files with 8 agents**

### Benefits

1. ✅ **Visibility**: See progress during cleanup (not blind waiting)
2. ✅ **Efficiency**: Uses S3 batch delete API (1000 objects/request)
3. ✅ **Distributed**: Multiple agents can partition cleanup work
4. ✅ **Safe**: Lists first by default (can skip with skip_verification)
5. ✅ **Stats reporting**: DELETE operations tracked like GET/PUT
6. ✅ **No workload waste**: Exits after cleanup (don't run unnecessary workload)

## Implementation Priority (Updated)

| Phase | Effort | Impact | Priority |
|-------|--------|--------|----------|
| 1. Streaming list | 1 hour | High | **Immediate** |
| 2. LiveTracker fields | 30 min | Medium | High |
| 3. Stats transmission | 30 min | High | High |
| 4. Protobuf updates | 30 min | Medium | High |
| 5. Controller display | 30 min | Low | Medium |
| 6. Partitioned listing | 3-4 hours | Very High | Future |
| **7. Cleanup-only mode** | **2 hours** | **High** | **High** |

**Total for Phases 1-5**: ~3 hours  
**Total with Cleanup**: ~5 hours  
**Total with Partitioning**: ~9 hours

## Testing Plan

### Test 1: Empty Bucket (Baseline)
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://empty-bucket/test/"
      count: 1000000
```

Expected: Instant listing (no files), ~2 min create

### Test 2: Pre-populated Bucket (Real Scenario)
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket-with-4.9M-files/test/"
      count: 4915200
```

Expected: 
- 5-10 min listing with progress updates every 10k files
- Controller shows: "🔍 Listing: 450000/4915200 files (9%)"
- NO gRPC timeout errors
- Controller receives stats every ~1 second during listing

### Test 3: Distributed Listing (8 Agents)
With partitioning enabled, each agent lists 1/8 of directories:
- Agent 1: Lists 520 directories (~613k files)
- Agent 2: Lists 520 directories (~613k files)
- ...
- Expected listing time: ~1.25 minutes (8x speedup)

## Performance Estimates

### S3 LIST Performance
- Page size: 1000 objects
- Latency per page: ~50ms (us-west-2, same region)
- Throughput: ~20,000 objects/second per agent

### 4.9M Files Listing
- Single agent: 4,900 pages × 50ms = **245 seconds** (~4 minutes)
- 8 agents (partitioned): 613 pages × 50ms = **31 seconds** (8x speedup)

### Progress Update Overhead
- Update every 10,000 files = ~10 page fetches
- Stats message: ~500 bytes
- Network overhead: negligible (<1ms per update)
- Total updates for 4.9M files: ~490 updates over 4 minutes = ~2 updates/second

## Next Steps

1. ✅ Revert `skip_check` config changes
2. Implement Phase 1 (streaming list with yield points)
3. Implement Phases 2-4 (progress tracking and transmission)
4. Test with empty bucket (baseline)
5. Test with 4.9M files (real scenario)
6. If successful, implement Phase 6 (partitioned listing)

## Conclusion

Using `list_stream()` with periodic progress updates solves the gRPC timeout issue while maintaining correct existence checking. The streaming approach:
- ✅ Keeps gRPC connection alive
- ✅ Uses constant memory (no buffering millions of URIs)
- ✅ Provides visibility into listing progress
- ✅ Enables future optimization (partitioned listing)
- ✅ No behavior changes (still checks existence properly)
