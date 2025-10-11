# Data Generation and Storage Efficiency Testing

## Overview

sai3-bench supports configurable data generation to test storage deduplication and compression. Two fill patterns are available: `zero` and `random`.

**⚠️ RECOMMENDATION**: Use `fill: random` for realistic storage testing. The `zero` fill pattern produces artificially high compression ratios and does not represent real-world data. Only use `fill: zero` if you specifically need to test all-zero data behavior.

## Fill Patterns

| Pattern | Implementation | Parameters | Use Case |
|---------|---------------|------------|----------|
| `zero` | `vec![0u8; size]` | size only | **⚠️ Unrealistic** - All zeros, extreme compression. Use only for specific zero-data testing. |
| `random` | `s3dlio::generate_controlled_data()` | size, dedup_factor, compress_factor | **✅ RECOMMENDED** - Realistic data for storage testing |

### Configuration Syntax

```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/path/"
      count: 1000
      min_size: 1048576
      max_size: 1048576
      fill: random            # ✅ RECOMMENDED - realistic data for storage testing
                              # "zero" produces unrealistic compression ratios
      dedup_factor: 1         # default: 1 (all unique), only applies to "random"
      compress_factor: 1      # default: 1 (uncompressible), only applies to "random"
```

**Note**: While `fill` defaults to `zero`, you should **explicitly set `fill: random`** for realistic storage testing. The `zero` pattern is only appropriate when specifically testing all-zero data behavior.

**Parameter defaults**: `dedup_factor` and `compress_factor` both default to 1. They are ONLY used when `fill: random`.

## Parameters

### `dedup_factor` (default: 1, only for `fill: random`)
Controls block-level deduplication:
- **1** = All unique blocks (no deduplication)
- **2** = 50% dedup ratio (blocks repeat 2x)
- **3** = 66.7% dedup ratio (blocks repeat 3x)
- **N** = (N-1)/N dedup ratio

### `compress_factor` (default: 1, only for `fill: random`)
Controls compressibility (zero-to-random byte ratio):
- **1** = Fully random (uncompressible)
- **2** = ~50% zeros (2:1 compression)
- **3** = ~66.7% zeros (3:1 compression)
- **N** = ~(N-1)/N zeros (N:1 compression)

## Examples

### Realistic Random Data (Recommended) - No Dedup, No Compression
```yaml
- base_uri: "s3://bucket/random/"
  count: 1000
  min_size: 1048576
  max_size: 1048576
  fill: random              # ✅ RECOMMENDED - this should be your default
  # dedup_factor: 1 (default)
  # compress_factor: 1 (default)
  fill: random
  # dedup_factor: 1 (default, not required)
  # compress_factor: 1 (default, not required)
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
