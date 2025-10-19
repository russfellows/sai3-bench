# v0.6.9 Performance Benchmark Suite

This directory contains configs for benchmarking sai3-bench v0.6.9 improvements, specifically testing:
- s3dlio v0.9.9 enhanced buffer pool management
- Chunked reads for direct:// URIs (v0.6.9)
- Comparison of buffered vs. direct I/O performance

## Test Scenario

**Dataset**: 1 TB of 4 MB files (262,144 files)
**Location**: `/tmp/sai3bench-v069-1tb/`
**Purpose**: Compare read performance across different I/O methods

## Configuration Files

### 1. `v069_benchmark_prepare.yaml`
**Purpose**: Generate the 1TB test dataset
**Usage**:
```bash
sai3-bench run --config tests/configs/v069_benchmark_prepare.yaml
```

**What it does**:
- Creates 262,144 files of exactly 4 MB each
- Uses `direct://` URIs for generation (O_DIRECT writes)
- Zero-filled data for fast writes
- **Does NOT cleanup** - files remain for subsequent tests
- High concurrency (64) for faster generation

**Expected time**: ~10-30 minutes depending on storage speed

### 2. `v069_benchmark_read_buffered.yaml`
**Purpose**: Test buffered (paged) I/O read performance
**Usage**:
```bash
# Clear cache first (requires sudo externally)
# sudo sync && sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches'

sai3-bench run --config tests/configs/v069_benchmark_read_buffered.yaml
```

**What it does**:
- Reads all 262,144 files using `file://` URIs
- Uses standard buffered I/O (goes through OS page cache)
- **Optimization**: `page_cache_mode: sequential` for 2-3x performance boost
- Enables aggressive prefetching and large readahead
- 5 minute duration (300s)
- 64 concurrent workers
- Expected benefit from OS caching on repeated reads

**Performance Note**: The `sequential` page cache mode was added based on v0.6.8 learnings - it provides 2-3x faster sequential reads by enabling aggressive kernel readahead.

### 3. `v069_benchmark_read_direct.yaml`
**Purpose**: Test direct I/O read performance (v0.6.9 optimization)
**Usage**:
```bash
# Clear cache first (requires sudo externally)
# sudo sync && sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches'

sai3-bench run --config tests/configs/v069_benchmark_read_direct.yaml
```

**What it does**:
- Reads all 262,144 files using `direct://` URIs (O_DIRECT)
- Bypasses OS page cache completely
- Uses s3dlio v0.9.9 enhanced buffer pool
- 5 minute duration (300s)
- 64 concurrent workers
- **Note**: 4MB files use whole-file reads (chunking is for files >= 8MB)

## Benchmark Workflow

### Step 1: Prepare Test Data
```bash
./target/release/sai3-bench run --config tests/configs/v069_benchmark_prepare.yaml
```

Verify completion:
```bash
ls -lh /tmp/sai3bench-v069-1tb/data/ | head -10
du -sh /tmp/sai3bench-v069-1tb/
# Should show ~1TB
```

### Step 2: Test Buffered I/O (file://)
```bash
# Clear cache (externally with sudo)
sudo sync && sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches'

# Run buffered test
./target/release/sai3-bench run --config tests/configs/v069_benchmark_read_buffered.yaml
```

### Step 3: Test Direct I/O (direct://)
```bash
# Clear cache again (externally with sudo)
sudo sync && sudo sh -c 'echo 3 > /proc/sys/vm/drop_caches'

# Run direct I/O test
./target/release/sai3-bench run --config tests/configs/v069_benchmark_read_direct.yaml
```

### Step 4: Compare Results
Results are automatically exported to TSV files:
```bash
ls -lt sai3bench-*-results.tsv | head -3
```

Key metrics to compare:
- **Throughput**: MB/s (total_bytes / duration)
- **Latency**: p50, p95, p99 (in microseconds)
- **IOPS**: Total operations per second

## Comparing v0.6.9 vs v0.6.8

To benchmark improvements in v0.6.9:

1. **Build v0.6.8**:
```bash
git checkout v0.6.8
cargo build --release
cp target/release/sai3-bench ~/sai3-bench-v0.6.8
```

2. **Build v0.6.9**:
```bash
git checkout v0.6.9
cargo build --release
cp target/release/sai3-bench ~/sai3-bench-v0.6.9
```

3. **Run tests with both versions** using the same configs

## Expected Results

### v0.6.8 (Before Optimization)
- **direct://** - Slow performance due to whole-file reads without optimal buffering
- **file://** - Good performance with OS page cache

### v0.6.9 (After Optimization)
- **direct://** - **173x improvement** for large files (>8MB) with chunked reads
- **direct://** - Enhanced buffer pool for all direct I/O operations
- **file://** - Similar performance (no regression)

### Page Cache Optimization (v0.6.8+)
The buffered read config uses `page_cache_mode: sequential` which provides:
- **2-3x performance improvement** for large sequential reads
- Aggressive kernel prefetching and large readahead
- Optimal for streaming/scanning workloads (like our benchmark)
- Linux/Unix only (uses `posix_fadvise()` system calls)

**Why it matters**: Without this setting, the kernel uses default caching heuristics which may not be optimal for sequential access patterns. The `sequential` mode tells the kernel to:
1. Increase readahead window size
2. Prefetch more aggressively
3. Drop pages sooner (assumes no re-read)

### Notes
- 4MB files in this test use whole-file reads (below 8MB chunking threshold)
- Main v0.6.9 benefit is buffer pool improvements from s3dlio v0.9.9
- For testing chunked reads, use files >= 8MB (see below)

## Testing Chunked Reads (Files >= 8MB)

To specifically test v0.6.9's chunked read optimization, modify the prepare config:

```yaml
prepare:
  ensure_objects:
    - base_uri: "direct:///tmp/sai3bench-v069-chunked/"
      count: 131072  # 1TB / 8MB = 131,072 files
      size: 8388608  # 8 MB (will use chunked reads)
      fill: zero
```

With 8MB files:
- v0.6.9 will use 4MB chunks (2 chunks per file)
- Expected: Much better performance than v0.6.8's whole-file approach

## Cleanup

After benchmarking:
```bash
rm -rf /tmp/sai3bench-v069-1tb/
```

**Warning**: This will delete ~1TB of data!

## Disk Space Requirements

- **Test data**: ~1TB in `/tmp/sai3bench-v069-1tb/`
- **Ensure sufficient free space** before running prepare step
- Check with: `df -h /tmp`
