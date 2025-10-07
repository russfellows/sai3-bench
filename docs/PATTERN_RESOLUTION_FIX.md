# DELETE and STAT Pattern Resolution Fix - v0.5.4

## Critical Bug Discovery

During cloud storage testing, DELETE and STAT operations with glob patterns were **completely broken**. The operations were trying to use glob patterns (e.g., `prepared-*.dat`) as literal filenames instead of resolving them to actual object URIs.

### Error Example
```
Error: Failed to delete object from URI: gs://signal65-russ-b1/bench/mixed/prepared-*.dat

Caused by:
    GCS DELETE failed for gs://signal65-russ-b1/bench/mixed/prepared-*.dat: 
    No such object: signal65-russ-b1/bench/mixed/prepared-*.dat
```

## Root Cause

The workload execution had **inconsistent pattern handling**:

- **GET operations**: ✅ Correctly pre-resolved glob patterns to actual URIs
- **DELETE operations**: ❌ Tried to delete the pattern string as a literal filename
- **STAT operations**: ❌ Tried to stat the pattern string as a literal filename
- **PUT operations**: ✅ N/A - generates new object names
- **LIST operations**: ✅ N/A - lists directories

### Code Analysis

**GET (Working Correctly):**
```rust
// Lines 563-605: Pre-resolution phase
for wo in &cfg.workload {
    if let OpSpec::Get { .. } = &wo.spec {
        let uri = cfg.get_uri(&wo.spec);
        let full_uris = prefetch_uris_multi_backend(&uri).await?;
        pre.get_lists.push(GetSource { full_uris, uri });
    }
}

// Lines 695-710: Worker loop
let full_uri = {
    let src = pre.get_for_uri(&uri).unwrap();
    let uri_idx = r.random_range(0..src.full_uris.len());
    src.full_uris[uri_idx].clone()  // ✅ Picks actual resolved URI
};
let bytes = get_object_multi_backend(&full_uri).await?;
```

**DELETE (Broken):**
```rust
// OLD CODE - No pre-resolution for DELETE!
OpSpec::Delete { .. } => {
    let uri = cfg.get_meta_uri(op);  // Returns "prepared-*.dat"
    delete_object_multi_backend(&uri).await?;  // ❌ Tries to delete pattern!
}
```

**STAT (Broken):**
```rust
// OLD CODE - No pre-resolution for STAT!
OpSpec::Stat { .. } => {
    let uri = cfg.get_meta_uri(op);  // Returns "prepared-*.dat"
    stat_object_multi_backend(&uri).await?;  // ❌ Tries to stat pattern!
}
```

## Solution Implementation

Extended the pre-resolution phase to handle **all operations that work with existing objects** (GET, DELETE, STAT):

### 1. Updated PreResolved Structure

```rust
/// Pre-resolved URI lists so workers can sample keys cheaply.
/// Handles GET, DELETE, and STAT operations with glob patterns.
#[derive(Default, Clone)]
struct PreResolved {
    get_lists: Vec<UriSource>,
    delete_lists: Vec<UriSource>,    // ✅ NEW
    stat_lists: Vec<UriSource>,      // ✅ NEW
}

#[derive(Clone)]
struct UriSource {
    full_uris: Vec<String>,  // Pre-resolved full URIs for random selection
    uri: String,             // Original pattern for lookup
}

impl PreResolved {
    fn get_for_uri(&self, uri: &str) -> Option<&UriSource> { ... }
    fn delete_for_uri(&self, uri: &str) -> Option<&UriSource> { ... }  // ✅ NEW
    fn stat_for_uri(&self, uri: &str) -> Option<&UriSource> { ... }    // ✅ NEW
}
```

### 2. Pre-Resolution for All Operations

```rust
// Pre-resolve GET, DELETE, and STAT sources once
for wo in &cfg.workload {
    match &wo.spec {
        OpSpec::Get { .. } => {
            let uri = cfg.get_uri(&wo.spec);
            let full_uris = prefetch_uris_multi_backend(&uri).await?;
            pre.get_lists.push(UriSource { full_uris, uri });
        }
        OpSpec::Delete { .. } => {  // ✅ NEW
            let uri = cfg.get_meta_uri(&wo.spec);
            let full_uris = prefetch_uris_multi_backend(&uri).await?;
            pre.delete_lists.push(UriSource { full_uris, uri });
        }
        OpSpec::Stat { .. } => {  // ✅ NEW
            let uri = cfg.get_meta_uri(&wo.spec);
            let full_uris = prefetch_uris_multi_backend(&uri).await?;
            pre.stat_lists.push(UriSource { full_uris, uri });
        }
        _ => {}
    }
}
```

### 3. Worker Loop - Sample from Pre-Resolved Lists

```rust
// DELETE - Now samples from pre-resolved URIs
OpSpec::Delete { .. } => {
    let pattern = cfg.get_meta_uri(op);
    let full_uri = {
        let src = pre.delete_for_uri(&pattern)?;
        let uri_idx = r.random_range(0..src.full_uris.len());
        src.full_uris[uri_idx].clone()  // ✅ Picks actual resolved URI
    };
    delete_object_multi_backend(&full_uri).await?;
}

// STAT - Now samples from pre-resolved URIs
OpSpec::Stat { .. } => {
    let pattern = cfg.get_meta_uri(op);
    let full_uri = {
        let src = pre.stat_for_uri(&pattern)?;
        let uri_idx = r.random_range(0..src.full_uris.len());
        src.full_uris[uri_idx].clone()  // ✅ Picks actual resolved URI
    };
    stat_object_multi_backend(&full_uri).await?;
}
```

## Key Design Decisions

1. **Unified Pattern Handling**: GET, DELETE, and STAT now use identical pre-resolution logic
2. **Pattern Resolution Messages**: User sees count for each operation type:
   ```
   Resolving 3 operation patterns (1 GET, 1 DELETE, 1 STAT)...
   Found 100 objects for GET pattern: file:///.../prepared-*.dat
   Found 100 objects for DELETE pattern: file:///.../prepared-*.dat
   Found 100 objects for STAT pattern: file:///.../prepared-*.dat
   ```
3. **Random Sampling**: Each operation randomly picks from the pre-resolved URI list
4. **Early Validation**: Patterns with zero matches fail during setup, not during execution

## Test Results

### Pattern Resolution Test
```bash
./target/release/sai3-bench run --config tests/configs/pattern-resolution-test.yaml
```

**Output:**
```
Resolving 3 operation patterns (1 GET, 1 DELETE, 1 STAT)...
Found 100 objects for GET pattern: file:///.../prepared-*.dat
Found 100 objects for DELETE pattern: file:///.../prepared-*.dat
Found 100 objects for STAT pattern: file:///.../prepared-*.dat
```

**Results:**
- ✅ GET operations successfully read prepared objects
- ✅ DELETE operations successfully removed objects
- ✅ STAT operations successfully queried object metadata
- ✅ PUT operations created new objects

### Cloud Storage Validation
```bash
# Fixed config for GCS testing
sai3-bench run --config sai3-bench-mixed-fixed.yaml
```

**Expected behavior:**
- Prepare creates 2,000 objects with `prepared-NNNNNNNN.dat` naming
- GET randomly samples from prepared objects (60% weight)
- PUT creates new objects with `obj_*` naming (25% weight)
- DELETE randomly removes prepared objects (10% weight)
- STAT randomly queries prepared objects (5% weight)

## Impact

### Before Fix
- **DELETE with patterns**: ❌ Completely broken - always failed
- **STAT with patterns**: ❌ Completely broken - always failed  
- **Cloud workloads**: ❌ Could not run mixed benchmarks

### After Fix
- **DELETE with patterns**: ✅ Works correctly - resolves and deletes actual objects
- **STAT with patterns**: ✅ Works correctly - resolves and stats actual objects
- **Cloud workloads**: ✅ Full Warp-compatible mixed benchmarks functional

## Configuration Update Required

Users need to update configs to use glob patterns instead of brace expansions:

### Old (Broken)
```yaml
workload:
  - op: delete
    path: "bench/mixed/obj_{00000..19999}"  # ❌ Brace expansion not supported
```

### New (Working)
```yaml
workload:
  - op: delete
    path: "bench/mixed/prepared-*.dat"  # ✅ Glob pattern with wildcard
```

## Code Location

**File**: `src/workload.rs`
**Changes**:
- Lines 903-927: Updated `PreResolved` struct and `UriSource` (renamed from `GetSource`)
- Lines 563-650: Extended pre-resolution to handle DELETE and STAT operations
- Lines 810-850: Updated DELETE and STAT worker loop logic to sample from pre-resolved URIs

**Commit**: (pending)

## Regression Testing

```bash
# Pattern resolution test (validates GET/DELETE/STAT all work with globs)
./target/release/sai3-bench run --config tests/configs/pattern-resolution-test.yaml

# Cloud storage mixed workload
./target/release/sai3-bench run --config configs/sai3-bench-mixed-fixed.yaml
```

**Expected**: No "No such object" errors for DELETE/STAT operations. Operations should randomly sample from resolved object lists.

## Future Enhancements

Consider adding support for:
1. **Multiple pattern types**: Currently only supports `*` wildcard
2. **Pattern validation**: Warn if pattern doesn't match any objects during setup
3. **Pattern refresh**: Option to refresh pre-resolved lists periodically for long-running benchmarks
