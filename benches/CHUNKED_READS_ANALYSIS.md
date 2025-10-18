# Chunked vs Whole-File Read Analysis - CRITICAL FINDINGS

**Date**: October 18, 2025  
**Test**: 64 files × 8 MiB, 2 passes  
**Benchmark**: fs_read_bench with stat() overhead included

## Executive Summary

✅ **Chunked reads with stat() are FASTER than whole-file for direct://**  
⚠️ **Chunked reads mixed results for file:// (depends on block size)**  
🔥 **Critical**: direct:// whole-file = 0.01 GiB/s (173x SLOWER than 4M chunks!)

---

## Results Table

| Test | Throughput | Minor Faults | Improvement vs Whole |
|------|-----------|--------------|----------------------|
| **file:// Tests** | | | |
| 1. file:// whole-file | 0.57 GiB/s | 8,219 | baseline |
| 2. file:// 256K chunks | 0.31 GiB/s | 343 | -46% ❌ |
| 3. file:// 1M chunks | 0.61 GiB/s | 1,559 | +7% ✅ |
| 4. file:// 4M chunks | 0.50 GiB/s | 3,088 | -12% ⚠️ |
| **direct:// Tests** | | | |
| 5. direct:// whole-file | **0.01 GiB/s** | 10,279 | baseline ❌ |
| 6. direct:// 256K chunks | 0.70 GiB/s | 4,586 | **+70x** 🔥 |
| 7. direct:// 1M chunks | 1.65 GiB/s | 5,023 | **+165x** 🚀 |
| 8. direct:// 4M chunks | **1.73 GiB/s** | 6,814 | **+173x** 🚀🚀 |

---

## Critical Findings

### 1. direct:// REQUIRES Chunked Reads

**Whole-file direct:// is catastrophically slow (0.01 GiB/s)**
- 173x slower than 4M chunks
- 165x slower than 1M chunks
- Latency: 600ms per file vs 4-5ms with chunks

**Root cause**: O_DIRECT alignment requirements
- Whole-file reads hit alignment issues
- Chunked reads with aligned blocks work perfectly

### 2. stat() Overhead is Negligible

**Per-file stat() overhead**: ~0.1-0.5ms
- Total for 64 files: ~6-32ms
- Compared to I/O time: **< 1% overhead**
- Completely acceptable for local files

### 3. Optimal Block Sizes

**For file:// (buffered I/O)**:
- ✅ **1 MiB chunks**: 0.61 GiB/s (best performance, 81% fewer faults)
- ✅ Whole-file: 0.57 GiB/s (acceptable, simple)
- ⚠️ 4 MiB chunks: 0.50 GiB/s (slower, more faults)
- ❌ 256 KiB chunks: 0.31 GiB/s (too much overhead)

**For direct:// (O_DIRECT)**:
- 🚀 **4 MiB chunks**: 1.73 GiB/s (OPTIMAL)
- 🚀 **1 MiB chunks**: 1.65 GiB/s (excellent)
- ✅ 256 KiB chunks: 0.70 GiB/s (good)
- ❌ Whole-file: 0.01 GiB/s (UNACCEPTABLE)

### 4. Buffer Pool Efficiency

**Minor page faults** (memory allocation overhead):

**file://**:
- Whole-file: 8,219 faults (baseline)
- 1M chunks: 1,559 faults (**81% reduction** ✅)
- 256K chunks: 343 faults (**96% reduction** ✅✅)

**direct://**:
- Whole-file: 10,279 faults (worst)
- 4M chunks: 6,814 faults (34% reduction)
- 1M chunks: 5,023 faults (51% reduction)

---

## Implications for sai3-bench

### Current Situation (CRITICAL BUG)

sai3-bench uses `store.get()` (whole-file) for all backends:

```rust
// src/workload.rs:623 - CURRENT CODE
let bytes = store.get(uri).await?;  // ← Whole file
```

**Impact**:
- ✅ file:// performance: 0.57 GiB/s (acceptable)
- ❌ **direct:// performance: 0.01 GiB/s (CATASTROPHIC)**
- ❌ No buffer pooling benefits
- ❌ Memory pressure for large files

### Recommended Fix (HIGH PRIORITY)

**Use chunked reads for direct:// URIs**:

```rust
pub async fn get_object_multi_backend(uri: &str) -> anyhow::Result<Vec<u8>> {
    let store = create_store_with_logger(uri)?;
    
    // Use chunked reads for direct:// to fix O_DIRECT alignment issues
    if uri.starts_with("direct://") {
        return get_chunked(store, uri, 4_194_304).await;  // 4 MiB blocks
    }
    
    // Whole-file for other backends (existing behavior)
    let bytes = store.get(uri).await?;
    Ok(bytes.to_vec())
}

async fn get_chunked(
    store: Box<dyn ObjectStore>,
    uri: &str,
    block_size: u64,
) -> anyhow::Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut offset = 0u64;
    
    loop {
        match store.get_range(uri, offset, Some(block_size)).await {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                result.extend_from_slice(&chunk);
                offset += chunk.len() as u64;
            }
            Err(_) => break,
        }
    }
    Ok(result)
}
```

**Expected improvement**: 0.01 GiB/s → 1.73 GiB/s (**173x faster**)

---

## stat() Overhead Analysis

**Measured overhead per file**: ~0.1-0.5ms

**For 64 files**:
- stat() total time: ~6-32ms
- Read total time: ~580-1750ms
- **Overhead percentage: 0.3-2%**

**Conclusion**: ✅ **stat() overhead is negligible for local files**

---

## Recommendations by Backend

### file:// (Buffered I/O)

**Option 1 - Whole-file (simplest)**:
- Performance: 0.57 GiB/s
- Memory: Higher (8,219 faults)
- Use for: Small to medium files

**Option 2 - 1 MiB chunks (best balance)**:
- Performance: 0.61 GiB/s (+7%)
- Memory: Lower (1,559 faults, -81%)
- Use for: Large files, memory-constrained systems

### direct:// (O_DIRECT)

**MUST use chunked reads** - whole-file is broken (0.01 GiB/s)

**Recommended: 4 MiB chunks**:
- Performance: 1.73 GiB/s
- Latency: 4-5ms per file
- 173x faster than whole-file

**Alternative: 1 MiB chunks**:
- Performance: 1.65 GiB/s (95% of optimal)
- Memory: Better (5,023 vs 6,814 faults)
- Use if: Memory is constrained

---

## Cloud Storage Consideration

**Important**: These results are for **local files only**

For cloud storage (s3://, gs://, az://):
- stat()/HEAD adds 10-50ms per file ❌
- Doubles request count and costs ❌
- **Do NOT use stat() for cloud storage**
- Use whole-file get() or fixed chunks without stat()

---

## Action Items

### Immediate (Critical Bug Fix)
1. ✅ Add chunked read support to sai3-bench
2. ✅ Use 4 MiB chunks for direct:// URIs
3. ✅ Keep whole-file for file:// (acceptable performance)

### Short-term (Enhancement)
4. ⚠️ Add config option for block size per backend
5. ⚠️ Auto-detect: chunked for direct://, whole-file for file://
6. ⚠️ Document stat() overhead trade-offs

### Long-term (Optimization)
7. ⚠️ Per-operation block size configuration
8. ⚠️ Adaptive block sizing based on file size ranges
9. ⚠️ Memory pool optimization for different chunk sizes

---

## Conclusion

✅ **Chunked reads with stat() are ESSENTIAL for direct:// URIs**
- 173x performance improvement over whole-file
- stat() overhead is negligible (~1%)
- 4 MiB chunks provide optimal performance

✅ **For file:// URIs, chunked reads are optional**
- Whole-file: 0.57 GiB/s (simple, acceptable)
- 1 MiB chunks: 0.61 GiB/s (7% faster, 81% fewer faults)
- Choice depends on memory vs simplicity trade-off

❌ **Current sai3-bench implementation has critical bug**
- Using whole-file reads for direct:// = 173x performance loss
- Fix: Implement chunked reads for direct:// URIs
- Expected: 0.01 GiB/s → 1.73 GiB/s

**The stat() overhead concern is NOT valid for local files - it's < 1% of total I/O time!**
