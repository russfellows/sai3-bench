# Data Generation and Storage Efficiency Testing

## Overview

sai3-bench supports configurable data generation to test storage deduplication and compression. Three fill patterns are available: `zero`, `random`, and `prand` (pseudo-random).

**⚠️ RECOMMENDATION**: Always use `fill: random` for storage performance testing. Both `zero` and `prand` produce artificially compressible data (100% and 90% respectively) that results in unrealistic performance measurements. Use `fill: prand` only when data generation CPU overhead is a proven bottleneck.

## Fill Patterns

| Pattern | Implementation | Compressibility | Performance | Parameters | Use Case |
|---------|---------------|-----------------|-------------|------------|----------|
| `zero` | `vec![0u8; size]` | 100% (22 bytes) | Fastest | size only | **⚠️ Unrealistic** - All zeros, extreme compression. Use only for specific zero-data testing. |
| `random` | `s3dlio::generate_controlled_data()` (new algorithm) | 0% (uncompressible) | 1954µs/op | size, dedup_factor, compress_factor | **✅ RECOMMENDED** - Truly incompressible data for realistic storage testing |
| `prand` | `s3dlio::generate_controlled_data_prand()` (old algorithm) | 87-90% (8995 bytes) | 1340µs/op | size, dedup_factor, compress_factor | **⚠️ NOT RECOMMENDED** - Faster CPU but artificially compressible, unrealistic for storage testing |

### Random vs Prand: Critical Performance Comparison

**Why compressibility matters**: Storage benchmarking should use **uncompressible data** to accurately measure storage system performance. Compressible data produces artificially high throughput and doesn't represent real-world workloads.

**Measured characteristics** (64KB samples with zstd -19):
- `random`: 0% compressed (65,549 bytes from 64KB) - ✅ **Perfect for storage testing**
- `prand`: 87-90% compressed (8,995 bytes from 64KB) - ❌ **Unrealistic, artificially high performance**
- `zero`: 100% compressed (22 bytes from 64KB) - ❌ **Completely unrealistic**

**Performance comparison** (prepare phase metrics):
```
Method   Latency     Throughput   Ops/sec   Compressibility   Storage Test Quality
random   1954µs      234 MiB/s    195/s     0% (realistic)    ✅ RECOMMENDED
prand    1340µs      254 MiB/s    197/s     90% (artificial)  ⚠️ USE WITH CAUTION
zero     2910µs      273 MiB/s    194/s     100% (artificial) ❌ NOT RECOMMENDED
```

**When to use `random` (default and recommended)**:
- ✅ All realistic storage performance testing
- ✅ Measuring true backend throughput without compression artifacts
- ✅ Comparing storage systems fairly
- ✅ Production-representative workloads
- ✅ Default choice for 99% of use cases

**When to use `prand` (use sparingly)**:
- ⚠️ ONLY when data generation CPU is the bottleneck (rare)
- ⚠️ Testing with known-compressible data patterns
- ⚠️ **Understand tradeoff**: 31% faster generation but produces unrealistic data
- ⚠️ Results NOT comparable to `random` fill tests

**Algorithm differences**:
- `random`: Xoshiro256++ RNG, no shared template → truly incompressible
- `prand`: ThreadRng with shared BASE_BLOCK template → cross-block patterns enable 90% compression

### Configuration Syntax

```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/path/"
      count: 1000
      min_size: 1048576
      max_size: 1048576
      fill: random            # ✅ RECOMMENDED - realistic data for storage testing
                              # "prand" for maximum CPU efficiency
                              # "zero" produces unrealistic compression ratios
      dedup_factor: 1         # default: 1 (all unique), applies to "random" and "prand"
      compress_factor: 1      # default: 1 (uncompressible), applies to "random" and "prand"
```

**Note**: While `fill` defaults to `zero`, you should **explicitly set `fill: random`** for realistic storage testing. The `zero` pattern is only appropriate when specifically testing all-zero data behavior. Use `fill: prand` when CPU efficiency is more important than data realism.

**Parameter defaults**: `dedup_factor` and `compress_factor` both default to 1. They are ONLY used when `fill: random` or `fill: prand`.

## Parameters

### `dedup_factor` (default: 1, only for `fill: random` or `fill: prand`)
Controls block-level deduplication:
- **1** = All unique blocks (no deduplication)
- **2** = 50% dedup ratio (blocks repeat 2x)
- **3** = 66.7% dedup ratio (blocks repeat 3x)
- **N** = (N-1)/N dedup ratio

### `compress_factor` (default: 1, only for `fill: random` or `fill: prand`)
Controls compressibility (zero-to-random byte ratio):
- **1** = Fully random (uncompressible)
- **2** = ~50% zeros (2:1 compression)
- **3** = ~66.7% zeros (3:1 compression)
- **N** = ~(N-1)/N zeros (N:1 compression)

## Examples

### Realistic Random Data (REQUIRED FOR STORAGE TESTING)
```yaml
- base_uri: "s3://bucket/random/"
  count: 1000
  min_size: 1048576
  max_size: 1048576
  fill: random              # ✅ ALWAYS USE THIS - produces truly incompressible data
  # dedup_factor: 1 (default)
  # compress_factor: 1 (default)
```

### Pseudo-Random (NOT RECOMMENDED FOR STORAGE TESTING)
```yaml
- base_uri: "s3://bucket/prand/"
  count: 1000
  min_size: 1048576
  max_size: 1048576
  fill: prand               # ⚠️ WARNING: 90% compressible, unrealistic performance
  # dedup_factor: 1 (default)  # Only use if data generation CPU is proven bottleneck
  # compress_factor: 1 (default)
```

### High Compressibility
```yaml
- base_uri: "s3://bucket/compressible/"
  count: 1000
  min_size: 1048576
  max_size: 1048576
  fill: random
  compress_factor: 3  # ~66.7% zeros
```

### Dedupable Data
```yaml
- base_uri: "s3://bucket/dedupable/"
  count: 1000
  min_size: 1048576
  max_size: 1048576
  fill: random
  dedup_factor: 2  # Blocks repeat 2x
```

### Both Dedup and Compression
```yaml
- base_uri: "s3://bucket/both/"
  count: 1000
  min_size: 1048576
  max_size: 1048576
  fill: random
  dedup_factor: 2
  compress_factor: 3
```

## Use in PUT Operations

PUT operations also support dedup/compress factors:

```yaml
workload:
  # Default: random uncompressible
  - op: put
    path: "data/"
    object_size: 1048576
    weight: 30
  
  # Compressible
  - op: put
    path: "data/"
    object_size: 1048576
    compress_factor: 5
    weight: 30
  
  # Dedupable
  - op: put
    path: "data/"
    object_size: 1048576
    dedup_factor: 4
    weight: 40
```

## Testing Storage Efficiency

### Validate Deduplication
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/baseline/"
      count: 100
      min_size: 10485760
      fill: random  # dedup_factor defaults to 1
    
    - base_uri: "s3://bucket/dedup2x/"
      count: 100
      min_size: 10485760
      fill: random
      dedup_factor: 2
```
Expected storage: baseline=1000 MiB, dedup2x=~500 MiB (with working dedup)

### Validate Compression
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/uncompressed/"
      count: 100
      min_size: 10485760
      fill: random  # compress_factor defaults to 1
    
    - base_uri: "s3://bucket/compress10x/"
      count: 100
      min_size: 10485760
      fill: random
      compress_factor: 10
```
Expected storage: uncompressed=1000 MiB, compress10x=~100 MiB (with working compression)

## See Also

- [CONFIG_SYNTAX.md](CONFIG_SYNTAX.md) - Complete YAML configuration reference
- [USAGE.md](USAGE.md) - General usage guide
- [CHANGELOG.md](CHANGELOG.md) - Version history (dedup/compress added in v0.5.3)
