# Phase 2B: Full Tree Integration with Files

**Date:** October 29, 2025  
**Status:** In Planning  
**Goal:** Make directory_structure usable by creating FILES in the tree, not just empty directories

---

## Executive Summary

**Current State:** PathSelector exists but only used for MKDIR/RMDIR. Directory trees are created empty. GET/PUT/STAT/DELETE use old flat namespace (prepared-*.dat).

**Target State:** ALL operations use tree structure. Files created IN directories using global indexing. PathSelector returns full file paths for all operations.

**Critical Decision:** Empty directories are not a valid test. Each release must deliver something usable.

---

## Architecture Overview

### Current Flow (Incomplete)
```
prepare_objects():
  ├─ create_directory_tree() → TreeManifest (dirs only)
  └─ create flat prepared-*.dat objects (ignores tree)

workload::run():
  ├─ GET/PUT/STAT/DELETE → use prepared-*.dat (flat namespace)
  └─ MKDIR/RMDIR → use PathSelector (tree structure)
  
PROBLEM: Two separate namespaces!
```

### Target Flow (Complete)
```
prepare_objects():
  ├─ create_directory_tree() → TreeManifest
  └─ create files IN tree structure (d1/d2/d3/file_00001.dat)
      └─ uses global file indexing (modulo distribution)

workload::run():
  └─ ALL operations → use PathSelector
      ├─ select_directory() → picks directory based on strategy
      ├─ select_file_in_directory(dir) → picks file in that directory
      └─ returns full path: "d1_w1.dir/d2_w1.dir/d3_w1.dir/file_00001.dat"
```

---

## Implementation Tasks

### Task 1: Extend TreeManifest to Track Files

**File:** `src/directory_tree.rs`

**Add fields to TreeManifest:**
```rust
pub struct TreeManifest {
    // ... existing fields ...
    
    /// Files per directory (from config)
    pub files_per_dir: usize,
    
    /// File distribution strategy: "bottom" or "all"
    pub distribution: String,
    
    /// File naming pattern (e.g., "file_{:08}.dat")
    pub file_mask: String,
    
    /// Total files in tree (computed)
    pub total_files: usize,
    
    /// File index ranges per directory: HashMap<dir_path, (start_idx, end_idx)>
    /// Enables fast file selection: "which files live in this directory?"
    pub file_ranges: HashMap<String, (usize, usize)>,
}
```

**Add methods:**
```rust
impl TreeManifest {
    /// Compute file distribution across directories
    /// Returns global file index ranges for each directory
    pub fn compute_file_ranges(&mut self) {
        let mut global_idx = 0;
        
        for dir_path in &self.all_directories {
            let should_have_files = match self.distribution.as_str() {
                "bottom" => {
                    // Only deepest level directories have files
                    let depth = dir_path.matches('/').count() + 1;
                    depth == self.max_depth()
                }
                "all" => true,
                _ => false,
            };
            
            if should_have_files {
                let start_idx = global_idx;
                let end_idx = global_idx + self.files_per_dir;
                self.file_ranges.insert(dir_path.clone(), (start_idx, end_idx));
                global_idx = end_idx;
            }
        }
        
        self.total_files = global_idx;
    }
    
    /// Get file name for a global file index
    pub fn get_file_name(&self, global_idx: usize) -> String {
        format!("file_{:08}.dat", global_idx)
    }
    
    /// Get full path for a global file index
    pub fn get_file_path(&self, global_idx: usize) -> Option<String> {
        // Find which directory this file belongs to
        for (dir_path, (start, end)) in &self.file_ranges {
            if global_idx >= *start && global_idx < *end {
                let file_name = self.get_file_name(global_idx);
                return Some(format!("{}/{}", dir_path, file_name));
            }
        }
        None
    }
}
```

**Estimated Effort:** 2 hours

---

### Task 2: Extend PathSelector for File Selection

**File:** `src/workload.rs` (PathSelector section)

**Add methods:**
```rust
impl PathSelector {
    /// Select a file path (directory + file) based on strategy
    /// 
    /// Returns full relative path: "d1_w1.dir/d2_w1.dir/d3_w1.dir/file_00001.dat"
    pub fn select_file(&self) -> String {
        // 1. Select directory using existing strategy
        let dir = self.select_directory();
        
        // 2. Pick a file within that directory
        let file = self.select_file_in_directory(&dir);
        
        // 3. Return full path
        format!("{}/{}", dir, file)
    }
    
    /// Select a file within a specific directory
    fn select_file_in_directory(&self, dir_path: &str) -> String {
        // Get file range for this directory
        if let Some((start_idx, end_idx)) = self.manifest.file_ranges.get(dir_path) {
            // Pick random file in range
            let mut rng = rng();
            let global_idx = rng.random_range(*start_idx..*end_idx);
            return self.manifest.get_file_name(global_idx);
        }
        
        // Directory has no files (shouldn't happen with proper config)
        warn!("Directory {} has no files in manifest", dir_path);
        "fallback_file_00000.dat".to_string()
    }
    
    /// Select a directory that has files (respects distribution strategy)
    /// Used when operation REQUIRES files (GET/STAT/DELETE)
    pub fn select_directory_with_files(&self) -> String {
        // Filter to directories that actually have files
        let dirs_with_files: Vec<&String> = self.manifest.all_directories
            .iter()
            .filter(|d| self.manifest.file_ranges.contains_key(*d))
            .collect();
        
        if dirs_with_files.is_empty() {
            warn!("No directories with files in manifest");
            return self.select_directory();
        }
        
        // Apply strategy to filtered list
        match self.strategy {
            PathSelectionStrategy::Random => {
                let mut rng = rng();
                let idx = rng.random_range(0..dirs_with_files.len());
                dirs_with_files[idx].clone()
            }
            PathSelectionStrategy::Exclusive => {
                // Pick from assigned directories that have files
                let assigned = self.manifest.get_agent_dirs(self.agent_id);
                let assigned_with_files: Vec<String> = assigned.into_iter()
                    .filter(|d| self.manifest.file_ranges.contains_key(d))
                    .collect();
                    
                if assigned_with_files.is_empty() {
                    // Fallback to any directory with files
                    let mut rng = rng();
                    let idx = rng.random_range(0..dirs_with_files.len());
                    return dirs_with_files[idx].clone();
                }
                
                let mut rng = rng();
                let idx = rng.random_range(0..assigned_with_files.len());
                assigned_with_files[idx].clone()
            }
            // Similar logic for Partitioned and Weighted
            _ => {
                // For now, use simple random from filtered list
                let mut rng = rng();
                let idx = rng.random_range(0..dirs_with_files.len());
                dirs_with_files[idx].clone()
            }
        }
    }
}
```

**Estimated Effort:** 3 hours

---

### Task 3: Modify prepare_objects() to Create Files in Tree

**File:** `src/workload.rs` (prepare_objects function)

**Current behavior:**
```rust
// Creates flat namespace
for i in 0..spec.count {
    let uri = format!("{}prepared-{:08}.dat", base_uri, i);
    store.put(&uri, &data).await?;
}
```

**New behavior:**
```rust
// After creating directory tree
if let Some(ref manifest) = tree_manifest {
    info!("Creating files in directory tree: {} total files across {} directories",
        manifest.total_files, manifest.all_directories.len());
    
    // Create files using global indexing (prevents collision bug)
    let store = create_store_for_uri(&base_uri)?;
    
    for global_file_idx in 0..manifest.total_files {
        // Modulo distribution: agent_id = global_idx % num_agents
        let assigned_agent = global_file_idx % num_agents;
        
        if assigned_agent == agent_id {
            // This file belongs to us
            if let Some(file_path) = manifest.get_file_path(global_file_idx) {
                let full_uri = format!("{}/{}", base_uri.trim_end_matches('/'), file_path);
                
                // Generate file data using size_spec
                let size = size_generator.next_size();
                let data = generate_data(size, &spec.fill, spec.dedup_factor, spec.compress_factor)?;
                
                store.put(&full_uri, &data).await?;
                pb.inc(1);
            }
        }
    }
    
    pb.finish_with_message(format!("created {} files in tree structure", 
        manifest.total_files / num_agents));
}
```

**Key points:**
- Use global file indexing: `assigned_agent = idx % num_agents`
- This prevents collision bug (all agents creating file_00000)
- Each agent creates its modulo-assigned files
- Respects distribution strategy (bottom/all)

**Estimated Effort:** 4 hours

---

### Task 4: Update Workload Operations to Use PathSelector

**File:** `src/workload.rs` (workload::run function)

**For each operation, replace pre-resolved URIs with PathSelector:**

#### GET Operation
```rust
// OLD: Pre-resolved URI list
OpSpec::Get { .. } => {
    let src = pre.get_for_uri(&pattern)?;
    let uri = src.full_uris[rng.random_range(0..src.full_uris.len())].clone();
    // ...
}

// NEW: PathSelector with tree structure
OpSpec::Get { .. } => {
    let file_path = if let Some(ref selector) = path_selector {
        // Use tree structure
        selector.select_file()
    } else {
        // Fallback to pre-resolved URIs (backward compat)
        let src = pre.get_for_uri(&pattern)?;
        let uri = src.full_uris[rng.random_range(0..src.full_uris.len())];
        uri.strip_prefix(&base_uri).unwrap_or(uri).to_string()
    };
    
    let full_uri = format!("{}/{}", base_uri.trim_end_matches('/'), file_path);
    // ... perform GET ...
}
```

#### PUT Operation
```rust
// OLD: Random object name
OpSpec::Put { .. } => {
    let key = format!("obj-{:08}.dat", rng.random_range(0..1_000_000));
    let uri = format!("{}/{}", base_uri, key);
    // ...
}

// NEW: PathSelector chooses directory + file
OpSpec::Put { .. } => {
    let file_path = if let Some(ref selector) = path_selector {
        // Choose directory, generate unique file name
        let dir = selector.select_directory_with_files();
        let file_name = format!("file_{:016x}.dat", rng.random::<u64>());
        format!("{}/{}", dir, file_name)
    } else {
        // Fallback to random naming (backward compat)
        format!("obj-{:08}.dat", rng.random_range(0..1_000_000))
    };
    
    let full_uri = format!("{}/{}", base_uri.trim_end_matches('/'), file_path);
    // ... perform PUT ...
}
```

#### STAT Operation
```rust
OpSpec::Stat { .. } => {
    let file_path = if let Some(ref selector) = path_selector {
        selector.select_file()  // Pick existing file from tree
    } else {
        // Fallback to pre-resolved URIs
        let src = pre.stat_for_uri(&pattern)?;
        src.full_uris[rng.random_range(0..src.full_uris.len())].clone()
    };
    
    let full_uri = format!("{}/{}", base_uri.trim_end_matches('/'), file_path);
    // ... perform STAT ...
}
```

#### DELETE Operation
```rust
OpSpec::Delete { .. } => {
    let file_path = if let Some(ref selector) = path_selector {
        selector.select_file()  // Pick file from deletable pool in tree
    } else {
        // Fallback to pre-resolved delete list
        let src = pre.delete_for_uri(&pattern)?;
        src.full_uris[rng.random_range(0..src.full_uris.len())].clone()
    };
    
    let full_uri = format!("{}/{}", base_uri.trim_end_matches('/'), file_path);
    // ... perform DELETE ...
}
```

**Key Design Decision: Backward Compatibility**
- If `path_selector.is_none()` → use old pre-resolved URI behavior
- If `path_selector.is_some()` → use tree-based paths
- This allows mixed workloads: some operations use tree, others don't

**Estimated Effort:** 6 hours

---

### Task 5: Add Comprehensive Tests

**File:** `src/workload.rs` (tests module)

**Test cases:**

1. **File path generation**
```rust
#[test]
fn test_tree_manifest_file_ranges() {
    let config = DirectoryStructureConfig {
        width: 2,
        depth: 2,
        files_per_dir: 10,
        distribution: "bottom".to_string(),
        dir_mask: "d%d_w%d.dir".to_string(),
    };
    
    let tree = DirectoryTree::new(config).unwrap();
    let mut manifest = TreeManifest::from_tree(&tree);
    manifest.compute_file_ranges();
    
    // Verify file ranges computed correctly
    assert_eq!(manifest.total_files, 20); // 2 dirs at depth 2, 10 files each
    assert!(manifest.file_ranges.len() > 0);
}
```

2. **PathSelector file selection**
```rust
#[test]
fn test_path_selector_select_file() {
    // Create manifest with files
    let manifest = create_test_manifest_with_files();
    
    let selector = PathSelector::new(
        manifest,
        0,
        1,
        PathSelectionStrategy::Random,
        0.0
    );
    
    let file_path = selector.select_file();
    assert!(file_path.contains("/file_"));
    assert!(file_path.ends_with(".dat"));
}
```

3. **Global file indexing (no collisions)**
```rust
#[test]
fn test_global_file_indexing_no_collisions() {
    let manifest = create_test_manifest_with_files();
    let num_agents = 4;
    
    // Simulate each agent creating its assigned files
    let mut agent_files: Vec<Vec<usize>> = vec![vec![]; num_agents];
    
    for global_idx in 0..manifest.total_files {
        let assigned_agent = global_idx % num_agents;
        agent_files[assigned_agent].push(global_idx);
    }
    
    // Verify no overlaps
    for i in 0..num_agents {
        for j in (i+1)..num_agents {
            let set_i: HashSet<_> = agent_files[i].iter().collect();
            let set_j: HashSet<_> = agent_files[j].iter().collect();
            assert!(set_i.is_disjoint(&set_j), 
                "Agents {} and {} have overlapping file assignments", i, j);
        }
    }
}
```

4. **Integration test: Full tree with files**
```rust
#[tokio::test]
async fn test_create_tree_with_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    let base_uri = format!("file://{}", temp_dir.path().display());
    
    let config = PrepareConfig {
        ensure_objects: vec![],
        directory_structure: Some(DirectoryStructureConfig {
            width: 2,
            depth: 2,
            files_per_dir: 5,
            distribution: "bottom".to_string(),
            dir_mask: "d%d_w%d.dir".to_string(),
        }),
        // ...
    };
    
    let (prepared, manifest) = prepare_objects(&config, None).await.unwrap();
    
    assert!(manifest.is_some());
    let manifest = manifest.unwrap();
    
    // Verify files were created
    assert_eq!(manifest.total_files, 10); // 2 leaf dirs * 5 files
    
    // Verify files exist on disk
    for global_idx in 0..manifest.total_files {
        if let Some(file_path) = manifest.get_file_path(global_idx) {
            let full_path = format!("{}/{}", temp_dir.path().display(), file_path);
            assert!(std::path::Path::new(&full_path).exists(), 
                "File not created: {}", full_path);
        }
    }
}
```

**Estimated Effort:** 4 hours

---

### Task 6: Update Example Configs

**File:** `tests/configs/directory_tree_example.yaml`

**Create comprehensive example:**
```yaml
version: 1
duration: 10s
concurrency: 8
target: "file:///tmp/sai3bench-tree-test"

# Prepare phase: Create directory tree with files
prepare:
  directory_structure:
    width: 10           # 10 subdirectories per level
    depth: 3            # 3 levels deep
    files_per_dir: 100  # 100 files in each directory
    distribution: "bottom"  # Only leaf directories have files
    dir_mask: "sai3bench.d%d_w%d.dir"  # Directory naming pattern
  
  ensure_objects:
    - base_uri: "file:///tmp/sai3bench-tree-test"
      count: 0  # No flat objects, only tree files
  
  post_prepare_delay: 0

# Workload: Test all operations on tree structure
workload:
  # Read existing files from tree
  - spec:
      Get:
        path: "/"  # PathSelector will choose actual paths
    weight: 40
  
  # Create new files in tree
  - spec:
      Put:
        path: "/"
        object_size: 1048576  # 1MB files
    weight: 30
  
  # Stat files in tree
  - spec:
      Stat:
        path: "/"
    weight: 20
  
  # Delete files from tree
  - spec:
      Delete:
        path: "/"
    weight: 10

# Optional: Distributed config for multi-agent testing
distributed:
  shared_filesystem: true
  tree_creation_mode: coordinator  # Controller creates tree once
  path_selection: partitioned      # Reduce contention
  partition_overlap: 0.3           # 30% cross-agent access
  
  agents:
    - address: "node1:7761"
    - address: "node2:7761"
    - address: "node3:7761"
```

**Estimated Effort:** 1 hour

---

## Testing Strategy

### Unit Tests (Quick Feedback)
```bash
cargo test --lib tree_manifest
cargo test --lib path_selector
cargo test --lib global_file_indexing
```

### Integration Tests (Full Flow)
```bash
# Single-agent tree creation with files
cargo test --test '*' -- tree_with_files

# Multi-agent simulation (no collision)
cargo test --test '*' -- multi_agent_files
```

### Manual Smoke Tests
```bash
# Create tree and verify structure
./target/release/sai3-bench run --config tests/configs/directory_tree_example.yaml --prepare-only

# Check that files exist
find /tmp/sai3bench-tree-test -type f | wc -l  # Should match total_files

# Run workload and verify operations use tree paths
./target/release/sai3-bench run --config tests/configs/directory_tree_example.yaml
```

---

## Success Criteria

- ✅ **TreeManifest tracks files**: file_ranges, total_files computed correctly
- ✅ **PathSelector returns file paths**: select_file() method works
- ✅ **Files created in tree**: prepare phase creates d1/d2/file_00001.dat structure
- ✅ **Global file indexing**: No collisions across agents (modulo distribution)
- ✅ **All operations use tree**: GET/PUT/STAT/DELETE use PathSelector when available
- ✅ **Backward compatibility**: Non-tree workloads still work with pre-resolved URIs
- ✅ **Tests pass**: 15+ new tests for file integration
- ✅ **Example config works**: Can create tree with files and run workload

---

## Timeline

| Task | Effort | Cumulative |
|------|--------|------------|
| 1. Extend TreeManifest | 2 hrs | 2 hrs |
| 2. Extend PathSelector | 3 hrs | 5 hrs |
| 3. Modify prepare_objects() | 4 hrs | 9 hrs |
| 4. Update workload operations | 6 hrs | 15 hrs |
| 5. Add tests | 4 hrs | 19 hrs |
| 6. Update example configs | 1 hr | 20 hrs |
| **TOTAL** | **~2.5 days** | |

**With buffer for debugging and iteration: 3-4 days**

---

## Future Extensions (v0.8.0)

### Object Storage Support

Same PathSelector logic works for S3/Azure/GCS:

```yaml
target: "s3://my-bucket/tree-test"

prepare:
  directory_structure:
    width: 10
    depth: 3
    files_per_dir: 100
    distribution: "bottom"
```

Creates object keys:
```
s3://my-bucket/tree-test/d1_w1.dir/d2_w1.dir/d3_w1.dir/file_00000001.dat
s3://my-bucket/tree-test/d1_w1.dir/d2_w1.dir/d3_w1.dir/file_00000002.dat
...
```

**Benefits:**
- Test S3 ListObjects with prefix (simulates directory listing)
- Test metadata operations on cloud storage
- Measure prefix enumeration performance
- Test concurrent access to same prefix across agents

**Implementation:** Minimal changes - ObjectStore abstraction already handles this!

---

## Risks and Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| File collision bug (like rdf-bench) | High | Use global indexing from day 1 |
| Backward compatibility break | Medium | Keep fallback to pre-resolved URIs |
| Performance regression (extra path logic) | Low | PathSelector is cheap (HashMap lookup) |
| Config complexity | Medium | Provide clear examples and documentation |
| Testing coverage gaps | Medium | 15+ tests, manual smoke tests |

---

## Conclusion

This is the **right architecture** for v0.7.0. Empty directories are not a useful test - we need files to make directory structures meaningful. This implementation:

1. ✅ Delivers usable feature (files in tree, real workload testing)
2. ✅ Fixes collision bug from day 1 (global indexing)
3. ✅ Maintains backward compatibility (fallback to pre-resolved URIs)
4. ✅ Sets up clean extension to object storage (v0.8.0)
5. ✅ Matches original vision from gap analysis

**Let's build this!**
