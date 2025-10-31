# Directory Structure Generation for sai3-bench

## Problem Statement

Current implementation generates random directory names (`dir_<random>`), making it impossible to:
- Test specific directory removal scenarios
- Create deterministic, reproducible workloads
- Test operations on known directory structures
- Simulate realistic hierarchical filesystem layouts

## Inspiration: rdf-bench Approach

rdf-bench uses a width/depth model for directory tree generation:

### Formula
- **Width (W)**: Number of subdirectories per directory level
- **Depth (D)**: Number of levels in the tree
- **Total directories**: Σ(W^i) for i=1 to D
  - Example: W=4, D=3 → 4¹ + 4² + 4³ = 4 + 16 + 64 = 84 directories

### Directory Naming Pattern
- Format: `vdb.{depth}_{width}.dir`
- Examples:
  - Level 1: `vdb.1_1.dir`, `vdb.1_2.dir`, `vdb.1_3.dir`, `vdb.1_4.dir`
  - Level 2: `vdb.2_1.dir`, `vdb.2_2.dir`, ... (under each Level 1 dir)
  - Level 3: `vdb.3_1.dir`, `vdb.3_2.dir`, ... (under each Level 2 dir)

### File Distribution
- **dist=bottom**: Files only at deepest level (W^D directories × files_per_dir)
- **dist=all**: Files at all levels (total_directories × files_per_dir)

## Proposed sai3-bench Design

### Configuration Schema

```yaml
# New 'directory_structure' section in config
directory_structure:
  width: 4              # Subdirectories per level
  depth: 3              # Tree depth
  files_per_dir: 100    # Files in each directory
  distribution: "bottom"  # "bottom" or "all"
  dir_mask: "bench.%d_%d.dir"  # Optional custom naming
  
# Example: Creates 84 directories (4+16+64) with 6,400 files total
```

### Implementation Plan

#### Phase 1: Directory Tree Generation
1. **Add DirectoryStructure config struct** (`src/config.rs`)
   - `width`, `depth`, `files_per_dir`, `distribution`, `dir_mask`
   
2. **Create DirectoryTree struct** (`src/directory_tree.rs`)
   - Build tree structure from width/depth
   - Generate all directory paths up front
   - Calculate total directories and files
   - Provide path enumeration/selection methods

#### Phase 2: Prepare Phase Integration
3. **Update PrepareConfig** to support directory structure
   - Auto-generate directory hierarchy before workload
   - Create files according to distribution setting
   
4. **Add mkdir operation to prepare phase**
   - Create all directories using `store.mkdir()`
   - Then populate with files if specified

#### Phase 3: Workload Integration
5. **Update operation path resolution**
   - When `directory_structure` is set, select from known paths
   - Support patterns like `bench.2_*.dir` to select all level-2 dirs
   - Maintain backward compatibility with random generation

6. **Add directory-aware operations**
   - RMDIR: Select from known directory list
   - PUT: Select from known directory list for parent
   - LIST: Target specific directory levels

### Example Use Cases

#### Test Case 1: Deep Directory Tree Stress
```yaml
directory_structure:
  width: 10
  depth: 5
  files_per_dir: 0
  distribution: "all"
  # Creates 111,110 directories (10+100+1000+10000+100000)

workload:
  - op: mkdir
    path: "bench.*_*.dir"  # Create all directories
    weight: 50
  - op: list
    path: "bench.5_*.dir"  # List deepest level only
    weight: 30
  - op: rmdir
    path: "bench.5_*.dir"  # Remove leaf directories
    recursive: false
    weight: 20
```

#### Test Case 2: Realistic ML Dataset Layout
```yaml
directory_structure:
  width: 8               # 8 categories
  depth: 3               # category/split/batch
  files_per_dir: 1000    # 1000 samples per batch
  distribution: "bottom" # Files only at batch level
  # Example: imagenet/train/batch001/, imagenet/val/batch001/, etc.

workload:
  - op: get
    path: "bench.3_*.dir/"  # Read from batch directories
    weight: 80
  - op: list
    path: "bench.2_*.dir"   # List split directories
    weight: 10
  - op: stat
    path: "bench.*_*.dir"   # Stat all directories
    weight: 10
```

#### Test Case 3: Directory Removal Test
```yaml
directory_structure:
  width: 4
  depth: 2
  files_per_dir: 10
  distribution: "all"

prepare:
  create_directory_structure: true  # Auto-create tree

workload:
  # Try removing non-empty dirs (should fail)
  - op: rmdir
    path: "bench.1_*.dir"
    recursive: false
    weight: 25
    
  # Remove empty leaf dirs (should succeed)
  - op: delete
    path: "bench.2_*.dir/*"  # Delete all files first
    weight: 25
  - op: rmdir
    path: "bench.2_*.dir"
    recursive: false
    weight: 25
    
  # Force remove with recursive (should always work)
  - op: rmdir
    path: "bench.*_*.dir"
    recursive: true
    weight: 25
```

### Benefits

1. **Deterministic Testing**: Known paths enable reproducible tests
2. **Realistic Workloads**: Mimics real-world hierarchical structures
3. **Targeted Operations**: Can test specific directory levels
4. **Performance Analysis**: Measure impact of tree depth/width
5. **Compatibility**: Existing random generation still works

### Implementation Priority

- ✅ Phase 1: Core directory tree structure (new module)
- ⏳ Phase 2: Prepare phase integration
- ⏳ Phase 3: Workload operation updates

### Open Questions

1. **Pattern matching**: Should we support glob patterns (`*`) or regex?
2. **Selection strategy**: Round-robin, random, or sequential through directory list?
3. **Memory efficiency**: For W=10, D=6 = 1,111,110 directories - store all paths?
4. **Cross-operation coordination**: How to ensure RMDIR targets directories with known state?

### Next Steps

1. Implement `DirectoryTree` struct with path generation
2. Add unit tests for directory calculation formulas
3. Create example configs demonstrating width/depth usage
4. Add prepare phase directory tree creation
5. Update workload dispatcher to use structured paths when configured
