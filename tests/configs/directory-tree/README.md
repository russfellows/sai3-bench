# Directory Tree Test Configurations

This directory contains test configurations for the directory tree feature introduced in v0.7.0. These tests validate the creation and manipulation of hierarchical directory structures with files distributed according to various strategies.

## Testing Directories

**Primary Test Directory**: `/mnt/test` (dedicated device and mount point)
- All configs use `/mnt/test` for realistic I/O testing
- Provides consistent performance baseline
- Avoids memory caching effects from `/tmp` (may be tmpfs)

**Note**: For quick validation during development, you can temporarily change configs to use `/tmp`, but performance results will not be representative.

## Feature Overview

The directory tree feature enables:
- Hierarchical directory structure creation (configurable width × depth)
- File placement within directories with multiple distribution strategies
- Path selection for workload operations using various strategies
- Realistic filesystem testing scenarios

## Test Configurations

### 1. `tree_test_basic.yaml` - Basic Functionality Test
**Purpose**: Validates basic directory tree creation with uniform file sizes

**Configuration**:
- **Structure**: 3-way tree, 2 levels deep (9 leaf directories)
- **Files**: 5 files per leaf directory = 45 total files
- **Distribution**: `bottom` (files only in leaf directories)
- **Size**: Uniform 1-4 KB (1024-4096 bytes)
- **Fill**: Random data, no dedup/compression
- **Workload**: Mixed GET (50%), PUT (30%), STAT (20%)

**Validates**:
- Basic tree creation
- Bottom distribution strategy (files only at deepest level)
- Uniform size distribution
- Multi-operation workload with tree paths

**Expected Results**:
- 9 directories at depth 2
- 45 files total (5 per leaf directory)
- File sizes uniformly distributed 1.1K-4.0K

---

### 2. `tree_test_fixed_size.yaml` - Fixed Size Test
**Purpose**: Validates fixed-size file creation in tree structure

**Configuration**:
- **Structure**: 2-way tree, 2 levels deep (4 leaf directories)
- **Files**: 3 files per leaf directory = 12 total files
- **Distribution**: `bottom` (files only in leaf directories)
- **Size**: Fixed 8192 bytes (8 KB)
- **Fill**: Zero-filled, no dedup/compression
- **Workload**: MKDIR (30%), RMDIR (20%), GET (50%)

**Validates**:
- Fixed-size file generation in trees
- Zero-fill pattern
- Directory operations (MKDIR/RMDIR) combined with file operations
- Custom directory naming mask (`sai3.d%d_w%d`)

**Expected Results**:
- 4 leaf directories
- 12 files, all exactly 8.0K
- All files zero-filled

---

### 3. `tree_test_lognormal.yaml` - Lognormal Distribution Test
**Purpose**: Validates realistic file size distribution (many small, few large files)

**Configuration**:
- **Structure**: 4-way tree, 3 levels deep (64 leaf directories)
- **Files**: 10 files per directory across ALL levels
- **Distribution**: `all` (files at every directory level, not just leaves)
- **Size**: Lognormal (mean=1MB, stddev=512KB, range=1KB-10MB)
- **Fill**: Random data with dedup_factor=2, compress_factor=3
- **Workload**: GET only (100%)

**Validates**:
- Lognormal size distribution (realistic workload)
- "all" distribution strategy (files throughout tree, not just leaves)
- Deduplication and compression factors
- Large-scale tree (64+ directories, 840+ files)
- Custom directory naming (`level%d.width%d`)

**Expected Results**:
- 84 total directories (4 + 16 + 64 across 3 levels)
- 840 total files (10 per directory)
- File sizes concentrated around 300KB-2MB with some large outliers
- Realistic distribution: many small files, few large files

---

### 4. `tree_test_bottom.yaml` - Bottom Distribution Validation
**Purpose**: Comprehensive validation of bottom-only distribution strategy

**Configuration**:
- **Structure**: 3-way tree, 4 levels deep (81 leaf directories)
- **Files**: 5 files per leaf directory = 405 total files
- **Distribution**: `bottom` (files ONLY in leaf directories at depth 4)
- **Size**: Uniform 2-8 KB (2048-8192 bytes)
- **Fill**: Random data, no dedup/compression
- **Workload**: GET only (100%)

**Validates**:
- Strict "bottom" distribution (no files in intermediate directories)
- Deep tree structure (4 levels)
- Large-scale directory creation (120 total directories)
- Global file indexing (405 files with unique indices)

**Expected Results**:
- 120 total directories (1 + 3 + 9 + 27 + 81 across 4 levels + root)
- 405 files total (ONLY in 81 depth-4 directories)
- 0 files in directories at depth 1-3
- All depth-4 directories have exactly 5 files

---

## Distribution Strategies

### `bottom` (Leaf-Only)
- Files created ONLY in leaf directories (deepest level)
- Intermediate directories remain empty
- Use case: Simulating organized storage (data in leaves, directories for structure)
- Example: `tree_test_basic.yaml`, `tree_test_fixed_size.yaml`, `tree_test_bottom.yaml`

### `all` (Every Level)
- Files created in directories at ALL levels of the tree
- Root directory also gets files
- Use case: Realistic filesystem scenarios with mixed hierarchy
- Example: `tree_test_lognormal.yaml`

---

## Size Distribution Strategies

### Fixed Size
- All files exactly the same size
- Use case: Simplified testing, performance baseline
- Example: `tree_test_fixed_size.yaml` (8192 bytes)

### Uniform Distribution
- Files uniformly distributed within a range [min, max]
- Even probability across the range
- Use case: General-purpose testing
- Example: `tree_test_basic.yaml` (1024-4096 bytes)

### Lognormal Distribution
- Realistic distribution: many small files, few large files
- Parameters: mean, std_dev, min, max
- Use case: Real-world filesystem simulation
- Example: `tree_test_lognormal.yaml` (mean=1MB, range=1KB-10MB)

---

## Global File Indexing

All files in the tree are assigned unique global indices:
- Index format: `file_{global_idx:08}.dat`
- Example: `file_00000000.dat`, `file_00000001.dat`, ..., `file_00000404.dat`
- Prevents collisions in distributed scenarios
- Respects distribution strategy (bottom vs all)

**Path format**: `{dir_path}/{file_name}`
- Example: `sai3bench.d1_w0.dir/sai3bench.d2_w1.dir/file_00000015.dat`

---

## Running the Tests

### Prepare Only (Create Structure)
```bash
./target/release/sai3-bench run --config tests/configs/directory-tree/tree_test_basic.yaml --prepare-only
```

### Full Workload Run
```bash
./target/release/sai3-bench -v run --config tests/configs/directory-tree/tree_test_lognormal.yaml
```

### Verify Structure
```bash
# Count directories
find /mnt/test/sai3bench-tree-bottom -type d | wc -l

# Count files
find /mnt/test/sai3bench-tree-bottom -type f | wc -l

# Check file sizes
find /mnt/test/sai3bench-tree-lognormal -type f -exec ls -lh {} \; | awk '{print $5}' | sort -h

# Verify bottom distribution (leaf dirs only)
find /mnt/test/sai3bench-tree-bottom -name "*d4_*.dir" -type d -exec sh -c 'ls "$1" | wc -l' _ {} \;
```

---

## Test Coverage Summary

| Test Config | Tree Size | File Count | Distribution | Size Strategy | Validates |
|-------------|-----------|------------|--------------|---------------|-----------|
| `tree_test_basic.yaml` | 3×2 (9 leaves) | 45 | bottom | uniform 1-4KB | Basic functionality |
| `tree_test_fixed_size.yaml` | 2×2 (4 leaves) | 12 | bottom | fixed 8KB | Fixed sizes, MKDIR/RMDIR |
| `tree_test_lognormal.yaml` | 4×3 (64 leaves) | 840 | all | lognormal 1KB-10MB | Realistic workload, large scale |
| `tree_test_bottom.yaml` | 3×4 (81 leaves) | 405 | bottom | uniform 2-8KB | Strict bottom, deep tree |

---

## Regression Testing

These configurations serve as regression tests for:
1. **Directory tree creation** (`create_directory_tree()` in `src/workload.rs`)
2. **File distribution strategies** (`compute_file_ranges()` in `src/directory_tree.rs`)
3. **Path selection** (`PathSelector` in `src/workload.rs`)
4. **Size generation** (`SizeGenerator` integration)
5. **Global file indexing** (collision prevention)

**Validation checklist**:
- ✅ All tests build without warnings
- ✅ All tests create expected directory count
- ✅ All tests create expected file count
- ✅ File sizes match configured distribution
- ✅ Distribution strategy respected (bottom vs all)
- ✅ No file index collisions
- ✅ Workload operations complete successfully

---

## Future Enhancements

Potential additional test configurations:
- **Distributed tree test**: Multi-agent with path partitioning
- **Mixed depth test**: Variable depth across branches
- **Weighted selection test**: Validate weighted path selection strategy
- **Exclusive paths test**: Validate exclusive path assignment per agent
- **Large-scale test**: 10,000+ files for stress testing
- **S3/Azure/GCS variants**: Same tests on cloud backends

---

## Version History

- **v0.7.0** (October 2025): Initial directory tree feature and test suite
  - 4 comprehensive test configurations
  - 2 distribution strategies (bottom, all)
  - 3 size strategies (fixed, uniform, lognormal)
  - Global file indexing
  - Full integration testing
