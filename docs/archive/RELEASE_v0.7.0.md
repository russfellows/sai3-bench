# sai3-bench v0.7.0 Release Notes

**Release Date:** October 31, 2025  
**Release Type:** Major Feature Release

## 🌳 Overview

Version 0.7.0 introduces comprehensive filesystem testing capabilities with configurable directory tree workloads and full support for nested path operations across both traditional filesystems and object storage backends.

## ✨ Key Features

### Directory Tree Workloads

Create realistic hierarchical filesystem structures for testing:

```yaml
directory_tree:
  width: 3              # Subdirectories per level
  depth: 2              # Tree depth
  files_per_dir: 10     # Files per directory
  distribution: bottom  # "bottom" (leaf only) or "all" (every level)
  
  size:
    type: uniform
    min_size_kb: 4
    max_size_kb: 16
  
  fill: random          # Now the default (changed from zero)
```

**Documentation:** [Directory Tree Guide](docs/DIRECTORY_TREE_GUIDE.md) (30+ pages)

### Filesystem Operations

Full support for nested paths and directory operations:

- **Directory management:** MKDIR, RMDIR (filesystem backends)
- **Path enumeration:** LIST with prefix filtering (all backends)
- **Metadata queries:** STAT operations (all backends)
- **Cleanup operations:** DELETE files and objects (all backends)
- **Smart behavior:** Automatically skips mkdir for object storage (implicit directories)

**Documentation:** [Filesystem Testing Guide](docs/FILESYSTEM_TESTING_GUIDE.md) (20+ pages)

### Enhanced Dry-Run

Comprehensive pre-execution validation showing:

- Total directories count
- Total files count
- Total data size (bytes + human-readable)

```bash
./sai3-bench run --config tree-test.yaml --dry-run
# Output: Total Directories: 12, Total Files: 60, Total Data: 600 KiB (614400 bytes)
```

## 📊 Statistics

- **9,884 insertions, 81 deletions** across 49 files
- **101 tests passing** (100% pass rate, +60 from v0.6.11)
- **Zero compiler warnings**
- **50+ pages** of new documentation (2 comprehensive guides)
- **9 test configurations** for directory tree testing
- **6 test configurations** for filesystem operations

## 🧪 Validation

### Azure Blob Storage Testing

Comprehensive validation on Azure Blob Storage:

| Test | Files | Structure | Performance |
|------|-------|-----------|-------------|
| Bottom distribution | 90 | 9 leaf directories | ~11.64 ops/s |
| All-levels distribution | 60 | 12 total directories | ~11.04 ops/s |

**Latencies:**
- GET: mean ~900ms, p50 ~915ms, p95 ~1040ms
- PUT: mean ~450ms, p50 ~430ms, p95 ~620ms
- STAT: mean ~430ms, p50 ~430ms, p95 ~520ms

## 🔄 Backend Compatibility

| Backend | Directory Ops | Path Ops | Tree Workloads | Validation |
|---------|---------------|----------|----------------|------------|
| file:// | ✅ MKDIR/RMDIR | ✅ All | ✅ Full | Tested |
| direct:// | ✅ MKDIR/RMDIR | ✅ All | ✅ Full | Tested |
| s3:// | ⚠️ Implicit | ✅ All | ✅ Full | Compatible |
| az:// | ⚠️ Implicit | ✅ All | ✅ Full | **Validated** |
| gs:// | ⚠️ Implicit | ✅ All | ✅ Full | Compatible |

*⚠️ Implicit = Directories created automatically via key prefixes*

## ⚠️ Breaking Changes

### Default Fill Type: Zero → Random

**Change:** Configurations without explicit `fill:` now use `random` data instead of `zero`.

**Rationale:** Random fill provides realistic compression-resistant data for testing.

**Migration:**
```yaml
# If you need zero fill, explicitly specify:
fill: zero
```

### Object Storage MKDIR Behavior

**Change:** MKDIR operations automatically skipped for s3://, az://, gs:// (implicit directories).

**Impact:** Slight performance improvement, no functional change.

**Benefit:** Eliminates unnecessary API calls to cloud storage.

## 📚 Documentation

### New Guides

- **[Directory Tree Guide](docs/DIRECTORY_TREE_GUIDE.md)** - Complete guide for hierarchical filesystem testing
  - Configuration examples (uniform, lognormal, fixed sizes)
  - Distribution strategies (bottom, all-levels)
  - Performance considerations
  - Best practices and troubleshooting

- **[Filesystem Testing Guide](docs/FILESYSTEM_TESTING_GUIDE.md)** - Filesystem vs object storage operations
  - Nested path testing for object storage
  - Directory operations (MKDIR, RMDIR)
  - Path operations (LIST, STAT, DELETE)
  - Backend-specific behaviors

### Updated Documentation

- **[README.md](README.md)** - Updated for v0.7.0 with new features section
- **[CHANGELOG.md](docs/CHANGELOG.md)** - Comprehensive v0.7.0 release notes
- **[docs/README.md](docs/README.md)** - Updated documentation index

### Test Configurations

Nine example configurations in [tests/configs/directory-tree/](tests/configs/directory-tree/):

- `tree_test_basic.yaml` - Simple 2×2 tree
- `tree_test_bottom.yaml` - Bottom distribution (leaf only)
- `tree_test_all_levels.yaml` - All-levels distribution
- `tree_test_fixed_size.yaml` - Fixed size files
- `tree_test_lognormal.yaml` - Lognormal distribution
- `tree_test_io_operations.yaml` - Operation mix testing
- `tree_test_azure_blob.yaml` - Azure Blob example (**tested**)
- `tree_test_azure_all_levels.yaml` - Azure all-levels (**tested**)
- `tree_test_s3_example.yaml` - S3 template

**Documentation:** [Test Config README](tests/configs/directory-tree/README.md) (252 lines)

## 🚀 Quick Start

### Installation

```bash
# Clone repository
git clone https://github.com/russfellows/sai3-bench.git
cd sai3-bench

# Build
cargo build --release

# Run tests
cargo test
```

### Basic Directory Tree Test

```bash
# Create a simple tree workload config
cat > my-tree-test.yaml <<EOF
target: "file:///tmp/my-tree-test/"
duration: 30
concurrency: 8

directory_tree:
  width: 3
  depth: 2
  files_per_dir: 10
  distribution: bottom
  size:
    type: uniform
    min_size_kb: 4
    max_size_kb: 16
  fill: random

workload:
  - op: get
    weight: 60
  - op: put
    weight: 30
  - op: stat
    weight: 10
EOF

# Validate with dry-run
./target/release/sai3-bench run --config my-tree-test.yaml --dry-run

# Run the test
./target/release/sai3-bench run --config my-tree-test.yaml
```

### Azure Blob Storage Test

```bash
# Set credentials
export AZURE_STORAGE_ACCOUNT="your-account"
export AZURE_STORAGE_ACCOUNT_KEY="your-key"

# Run validated Azure test
./target/release/sai3-bench run \
  --config tests/configs/directory-tree/tree_test_azure_blob.yaml
```

## 🔗 Links

- **GitHub Repository:** https://github.com/russfellows/sai3-bench
- **Pull Request:** #33
- **Release Tag:** v0.7.0
- **Previous Release:** [v0.6.11](https://github.com/russfellows/sai3-bench/releases/tag/v0.6.11)

## 📦 Assets

- **Source code** (zip)
- **Source code** (tar.gz)
- **Linux binary** (x86_64) - `sai3-bench-v0.7.0-linux-x86_64` *(if available)*

## 🙏 Acknowledgments

Built on the [s3dlio Rust library](https://github.com/russfellows/s3dlio) for multi-protocol storage support.

## 📝 Full Changelog

See [CHANGELOG.md](docs/CHANGELOG.md) for complete version history from v0.1.0 to v0.7.0.

---

**For questions or issues:** https://github.com/russfellows/sai3-bench/issues  
**License:** GPL-3.0
