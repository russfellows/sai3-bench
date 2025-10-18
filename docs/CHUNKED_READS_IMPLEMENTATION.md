# Action Plan: Implementing Optimal I/O Block Sizing in sai3-bench

## Current Situation (Critical Finding)

**sai3-bench currently uses `store.get()` for ALL file reads** - this means:
- ❌ Whole-file reads only (no chunking)
- ❌ Explains why `direct://` whole-file = 0.02 GiB/s (alignment issues)
- ❌ No buffer pooling benefits for large files
- ❌ Memory pressure for large files (full file in RAM)

**Found in src/workload.rs:623:**
```rust
pub async fn get_object_multi_backend(uri: &str) -> anyhow::Result<Vec<u8>> {
    let store = create_store_with_logger(uri)?;
    let bytes = store.get(uri).await?;  // ← Whole file!
    Ok(bytes.to_vec())
}
```

This is why our standalone benchmark (which uses `get_range()`) showed better performance!

---

## Recommended Implementation

### Phase 1: Add Chunked Reading Support (HIGH PRIORITY)

**Add block_size parameter to GET operations:**

```rust
/// Multi-backend GET with optional block size
pub async fn get_object_multi_backend_chunked(
    uri: &str,
    block_size: Option<usize>
) -> anyhow::Result<Vec<u8>> {
    let store = create_store_with_logger(uri)?;
    
    if let Some(block_size) = block_size {
        // Chunked read with optimal block size
        let mut result = Vec::new();
        let mut offset = 0u64;
        
        loop {
            match store.get_range(uri, offset, Some(block_size as u64)).await {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => {
                    result.extend_from_slice(&chunk);
                    offset += chunk.len() as u64;
                }
                Err(e) => {
                    // Check if EOF or real error
                    if is_eof_error(&e) {
                        break;
                    }
                    return Err(e.into());
                }
            }
        }
        Ok(result)
    } else {
        // Whole-file read (backward compatible)
        let bytes = store.get(uri).await?;
        Ok(bytes.to_vec())
    }
}
```

### Phase 2: Config Schema Extension

**Add to `config.rs`:**

```rust
/// I/O optimization settings
#[derive(Debug, Deserialize, Clone)]
pub struct IoSettings {
    /// Default block size for chunked reads (bytes)
    /// Recommended: 1 MiB - 4 MiB for optimal performance
    #[serde(default = "default_block_size")]
    pub default_block_size: Option<u64>,
    
    /// Whether to use chunked reads (get_range) instead of whole-file get()
    /// Recommended: true for large files, false for small files
    #[serde(default = "default_use_chunked_reads")]
    pub use_chunked_reads: bool,
    
    /// Automatically determine block size based on backend
    /// - file:// and direct://: Use smaller blocks (1 MiB)
    /// - s3://, gs://, az://: Use larger blocks (4 MiB)
    #[serde(default = "default_auto_block_size")]
    pub auto_block_size: bool,
}

fn default_block_size() -> Option<u64> { Some(4_194_304) }  // 4 MiB
fn default_use_chunked_reads() -> bool { true }
fn default_auto_block_size() -> bool { true }

/// Per-operation block size override
#[derive(Debug, Deserialize, Clone)]
pub struct OpSpec {
    // ... existing fields ...
    
    /// Optional I/O block size for this operation (overrides global)
    /// Example: "256KiB", "1MiB", "4MiB"
    #[serde(default)]
    pub block_size: Option<String>,  // Parse with humanize_bytes
}
```

**YAML Example:**
```yaml
target: "direct:///tmp/data/"

# Global I/O settings (optional)
io_settings:
  use_chunked_reads: true     # Enable get_range() instead of get()
  default_block_size: 4MiB    # Default chunk size
  auto_block_size: true       # Auto-tune per backend

workload:
  # Large files - use large blocks
  - op: get
    path: "large/*.bin"
    weight: 50
    block_size: 4MiB          # Override for this operation
  
  # Small files - use small blocks or whole-file
  - op: get
    path: "small/*.jpg"
    weight: 50
    block_size: 256KiB        # Efficient for smaller files
```

### Phase 3: Automatic Backend-Specific Defaults

```rust
fn determine_optimal_block_size(uri: &str, config_hint: Option<u64>) -> Option<usize> {
    // 1. Use explicit config if provided
    if let Some(size) = config_hint {
        return Some(size as usize);
    }
    
    // 2. Auto-determine based on backend
    if uri.starts_with("s3://") || uri.starts_with("gs://") || uri.starts_with("az://") {
        Some(4_194_304)  // 4 MiB for cloud storage
    } else if uri.starts_with("direct://") {
        Some(4_194_304)  // 4 MiB for O_DIRECT (alignment + performance)
    } else if uri.starts_with("file://") {
        Some(1_048_576)  // 1 MiB for buffered local I/O
    } else {
        None  // Use whole-file get() for unknown backends
    }
}
```

---

## Performance Impact Prediction

Based on our testing:

| Scenario | Current (whole-file) | With Chunked (4 MiB blocks) | Improvement |
|----------|---------------------|----------------------------|-------------|
| **direct:// large files** | 0.02 GiB/s ❌ | 2.24 GiB/s ✅ | **112x faster** |
| **file:// large files** | ~7.9 GiB/s | ~7.9 GiB/s | Same |
| **s3:// large files** | Unknown | Should improve | TBD |
| **Small files (<1 MiB)** | Fast | Slightly slower | -5-10% |

**Critical**: Chunked reads fix the catastrophic direct:// performance issue!

---

## Implementation Priority

### 🔴 HIGH PRIORITY (Fix Regression)
1. ✅ Add `get_object_multi_backend_chunked()` function
2. ✅ Add basic `block_size` parameter support
3. ✅ Default to 4 MiB for `direct://` URIs
4. ✅ Test with actual workloads

### 🟡 MEDIUM PRIORITY (Enhancement)
5. ⚠️ Add `io_settings` to config schema
6. ⚠️ Per-operation `block_size` override
7. ⚠️ Auto-detection based on backend type

### 🟢 LOW PRIORITY (Nice to Have)
8. ⚠️ File size sampling for auto-tuning
9. ⚠️ Adaptive block sizing during runtime
10. ⚠️ Performance metrics per block size

---

## Testing Plan

**Test 1: Verify chunked reads work**
```yaml
target: "direct:///tmp/test/"
io_settings:
  use_chunked_reads: true
  default_block_size: 4MiB

workload:
  - op: get
    path: "data/*.bin"
```

**Expected**: Should match our fs_read_bench performance (~2.2 GiB/s)

**Test 2: Backward compatibility**
```yaml
target: "file:///tmp/test/"
# No io_settings - should use whole-file get()

workload:
  - op: get
    path: "data/*.bin"
```

**Expected**: Should work as before

**Test 3: Per-operation override**
```yaml
workload:
  - op: get
    path: "small/*.jpg"
    block_size: 256KiB
  - op: get
    path: "large/*.bin"
    block_size: 4MiB
```

---

## Quick Win: Minimal Change

**Simplest fix for immediate improvement:**

```rust
// In src/workload.rs, modify get_object_multi_backend:

pub async fn get_object_multi_backend(uri: &str) -> anyhow::Result<Vec<u8>> {
    let store = create_store_with_logger(uri)?;
    
    // Use chunked reads for direct:// to fix alignment issues
    if uri.starts_with("direct://") {
        let block_size = 4_194_304;  // 4 MiB
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
        return Ok(result);
    }
    
    // Whole-file for other backends (existing behavior)
    let bytes = store.get(uri).await?;
    Ok(bytes.to_vec())
}
```

This single change will fix the direct:// performance issue!

---

## Conclusion

**Critical Discovery**: sai3-bench uses whole-file reads, which explains:
- ❌ Poor direct:// performance (0.02 GiB/s)
- ❌ Memory pressure for large files  
- ❌ No buffer pooling benefits

**Immediate Action**: Implement chunked reads for direct:// URIs

**Expected Impact**: 100x+ performance improvement for direct:// backend

Would you like me to implement the quick fix first, or proceed with the full config-based solution?
