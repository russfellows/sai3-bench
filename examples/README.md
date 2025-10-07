# sai3-bench Configuration Examples

This directory contains example configurations demonstrating various sai3-bench features.

## Quick Start Examples

### `mixed-workload-cloud.yaml`
**Purpose**: Production-ready mixed workload for cloud storage benchmarking

**Features**:
- Prepare stage creates 2,000 baseline objects
- 60% GET operations (read existing objects)
- 25% PUT operations (create new objects)
- 10% DELETE operations (remove objects)
- 5% STAT operations (query metadata)

**Usage**:
```bash
# For Google Cloud Storage
sai3-bench run --config examples/mixed-workload-cloud.yaml

# Customize the target in the YAML:
target: "gs://your-bucket-name/"  # Google Cloud Storage
target: "s3://your-bucket-name/"  # Amazon S3
target: "az://account/container/" # Azure Blob Storage
```

**Key Pattern**: PUT weight (25) > DELETE weight (10) ensures object pool doesn't deplete

---

### `all-operations.yaml`
**Purpose**: Comprehensive demonstration of all operation types

**Features**:
- All 5 operation types: GET, PUT, DELETE, STAT, LIST
- Shows glob pattern usage for GET/DELETE/STAT
- Shows directory listing with LIST
- Balanced weights to avoid race conditions

**Operations Explained**:

1. **GET** - Read object data
   - Uses glob patterns: `data/prepared-*.dat`
   - Pre-resolves pattern to actual object URIs
   - Randomly samples from resolved list
   
2. **PUT** - Write new objects
   - Auto-generates unique names: `obj_<random_u64>`
   - Can specify size or size distribution
   
3. **LIST** - List directory contents
   - Operates on directory paths: `data/`
   - No pattern resolution needed
   - Returns all objects in directory
   
4. **STAT** - Query object metadata
   - Uses glob patterns: `data/prepared-*.dat`
   - Pre-resolves pattern to actual object URIs
   - Returns object size and metadata
   
5. **DELETE** - Remove objects
   - Uses glob patterns: `data/prepared-*.dat`
   - Pre-resolves pattern to actual object URIs
   - Randomly deletes from resolved list

**Pattern Resolution**:
- GET, DELETE, STAT: Pre-resolve patterns at startup
- PUT: Generates new names dynamically
- LIST: No resolution needed (directory-based)

---

## Operation Types Deep Dive

### Pattern Resolution (GET, DELETE, STAT)

These operations support glob patterns with wildcards:

```yaml
workload:
  - op: get
    path: "bench/data/prepared-*.dat"  # ✅ Matches all .dat files
```

At startup, sai3-bench resolves patterns to actual object URIs:
```
Resolving 3 operation patterns (1 GET, 1 DELETE, 1 STAT)...
Found 2000 objects for GET pattern: gs://bucket/bench/data/prepared-*.dat
Found 2000 objects for DELETE pattern: gs://bucket/bench/data/prepared-*.dat
Found 2000 objects for STAT pattern: gs://bucket/bench/data/prepared-*.dat
```

During execution, each operation randomly samples from its pre-resolved list.

### PUT Operations

PUT operations create new objects with auto-generated names:

```yaml
workload:
  - op: put
    path: "bench/data/new-"      # Base path
    object_size: 1048576          # 1 MiB
```

Generated names: `gs://bucket/bench/data/new-obj_<random_u64>`

### LIST Operations

LIST operations work on directory prefixes:

```yaml
workload:
  - op: list
    path: "bench/data/"  # Lists all objects under this prefix
```

No pattern resolution - returns all objects in the directory.

---

## Weight Guidelines

### Avoiding Object Pool Exhaustion

When mixing operations that consume objects (DELETE) with those that read them (GET, STAT):

**Rule**: `PUT weight >= DELETE weight`

**Example (Safe)**:
```yaml
workload:
  - op: get
    weight: 60
  - op: put
    weight: 25  # Creates 25% of operations
  - op: delete
    weight: 10  # Deletes 10% of operations
```

**Example (Risky - Pool Depletes)**:
```yaml
workload:
  - op: get
    weight: 60
  - op: delete
    weight: 30  # Deletes more than PUT creates!
  - op: put
    weight: 10
```

### Realistic Workload Patterns

Common production ratios (based on MinIO Warp benchmarks):

**Read-Heavy** (70/20/10):
```yaml
- op: get
  weight: 70
- op: put
  weight: 20
- op: delete
  weight: 10
```

**Balanced** (60/25/10/5):
```yaml
- op: get
  weight: 60
- op: put
  weight: 25
- op: delete
  weight: 10
- op: stat
  weight: 5
```

**Write-Heavy** (40/50/10):
```yaml
- op: get
  weight: 40
- op: put
  weight: 50
- op: delete
  weight: 10
```

---

## Performance Considerations

### Concurrency Settings

Global concurrency (default: 32):
```yaml
concurrency: 32  # 32 parallel workers
```

Per-operation concurrency override:
```yaml
workload:
  - op: get
    path: "data/*.dat"
    weight: 60
    concurrency: 64  # Override for GET only
```

### Prepare Stage Performance

Prepare stage uses parallel execution (32 workers by default):
```yaml
prepare:
  ensure_objects:
    - base_uri: "s3://bucket/data/"
      count: 10000  # Creates 10K objects in parallel
```

**Expected throughput**:
- Small objects (100 KiB): 10,000-18,000 obj/s (file backend)
- Large objects (1 MiB): 700-800 obj/s (file backend)
- Cloud storage: Varies by latency (see docs/PREPARE_PERFORMANCE_FIX.md)

---

## Troubleshooting

### "No URIs found for GET/DELETE/STAT pattern"

Your pattern didn't match any objects:
```
Error: No URIs found for GET pattern: gs://bucket/data/*.dat
```

**Fix**: Verify objects exist and pattern is correct:
```bash
# Check what prepare creates
# Prepare creates: prepared-00000000.dat, prepared-00000001.dat, etc.

# Use correct pattern
path: "data/prepared-*.dat"  # ✅ Matches
path: "data/*.dat"            # ✅ Also matches
path: "data/obj-*.dat"        # ❌ No match - wrong prefix
```

### "File not found" during execution

This is expected when DELETE operations remove objects that GET/STAT are trying to access:

**Solution 1**: Reduce DELETE weight
```yaml
- op: delete
  weight: 5  # Lower weight = fewer deletions
```

**Solution 2**: Increase PUT weight to grow pool faster
```yaml
- op: put
  weight: 40  # Creates pool faster than DELETE removes
- op: delete
  weight: 10
```

**Solution 3**: Use separate object pools
```yaml
- op: get
  path: "data/prepared-*.dat"  # Read from prepared objects
- op: delete
  path: "data/temp-*.dat"      # Delete from different pool
```

---

## See Also

- [Main Documentation](../README.md)
- [Configuration Guide](../docs/USAGE.md)
- [Performance Tuning](../docs/PREPARE_PERFORMANCE_FIX.md)
- [Pattern Resolution Fix](../docs/PATTERN_RESOLUTION_FIX.md)
