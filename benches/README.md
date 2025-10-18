# Benchmark Development Tools

This directory contains **internal development and testing tools** used during the development of sai3-bench. These are **not** production binaries and are **not** built by default.

## For Production Use

**Use `sai3-bench` directly** - it includes all optimizations and is the production-ready tool:

```bash
# Production benchmarking
./target/release/sai3-bench run --config myworkload.yaml

# Direct I/O testing  
./target/release/sai3-bench get --uri direct:///path/to/files/*

# File I/O testing
./target/release/sai3-bench get --uri file:///path/to/files/*
```

## Development Tools in This Directory

### fs_read_bench.rs (Internal Development Tool)

**Purpose**: Low-level testing of file:// and direct:// I/O with configurable block sizes.

**Used for**: 
- Validating s3dlio v0.9.9 buffer pool improvements
- Testing optimal block sizes (4 KiB to 8 MiB)
- Comparing chunked vs whole-file read strategies
- Measuring page fault behavior

**Why it's not a production binary**:
- ❌ Redundant with main sai3-bench functionality
- ❌ Confusing for end users ("Which tool do I use?")
- ❌ Limited to file:// and direct:// only (no s3://, gs://, az://)
- ❌ No workload configuration, just raw I/O testing
- ✅ All optimizations now integrated into main sai3-bench

**To build it for development**:
```bash
# Temporarily add [[bin]] to Cargo.toml:
# [[bin]]
# name = "fs_read_bench"
# path = "benches/fs_read_bench.rs"

cargo build --release --bin fs_read_bench
./target/release/fs_read_bench --help
```

## Test Scripts

### Test Scripts Overview

| Script | Purpose | Requires sudo |
|--------|---------|---------------|
| `test_buffer_improvements.sh` | Full buffer pool validation with cache clearing | Yes |
| `test_buffer_quick.sh` | Quick validation without sudo | No |
| `test_real_sai3bench_direct_vs_file.sh` | Real sai3-bench testing | Yes |
| `test_chunked_vs_whole_file.sh` | Chunked vs whole-file comparison | Yes |
| `test_chunked_optimization.sh` | Validate v0.6.9 chunked read optimization | No |
| `find_optimal_direct_io_block_size.sh` | Block size optimization (4K-8M) | Yes |

### Running Tests

```bash
# Quick validation (no sudo)
./benches/test_buffer_quick.sh
./benches/test_chunked_optimization.sh

# Full validation with cache clearing (requires sudo)
sudo ./benches/test_buffer_improvements.sh
sudo ./benches/test_chunked_vs_whole_file.sh
```

## Documentation

### Key Findings Documents

- **BUFFER_POOL_RESULTS.md** - s3dlio v0.9.9 buffer pool validation results
- **REAL_SAI3BENCH_VALIDATION.md** - Real sai3-bench executable testing results  
- **CHUNKED_READS_ANALYSIS.md** - Chunked vs whole-file read analysis (173x improvement for direct://)

## Summary

**For users**: Use `sai3-bench` - it has everything you need.

**For developers**: These tools helped validate optimizations that are now integrated into the main binary. They're kept for reference and future development work.

---

**Key Optimization Integrated**: v0.6.9 now includes automatic chunked reads for `direct://` URIs (173x performance improvement) while preserving optimal behavior for all other backends (s3://, gs://, az://, file://).
