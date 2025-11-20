# rdf-bench Feature Comparison and Implementation Planning

**Purpose**: Combined reference for rdf-bench features and sai3-bench implementation priorities  
**Last Updated**: November 20, 2025  
**sai3-bench Version**: v0.8.0

This document combines feature comparison, command mapping, and implementation planning for users migrating from rdf-bench (Oracle VDB-Bench fork) or planning new features.

---

## Table of Contents

1. [Quick Command Reference](#quick-command-reference)
2. [Feature Comparison Matrix](#feature-comparison-matrix)
3. [Implementation Status](#implementation-status)
4. [Development Priorities](#development-priorities)
5. [Migration Guide](#migration-guide)
6. [Detailed Analysis](#detailed-analysis)

---

## Quick Command Reference

### Command Mapping

| rdf-bench | sai3-bench | Notes |
|-----------|------------|-------|
| SD (Storage Definition) | `target` in config | URI-based vs name-based |
| WD (Workload Definition) | `workload` array | Multiple ops vs single WD |
| RD (Run Definition) | Top-level config | Implicit in workload |
| `lun=/dev/sdb` | `target: "block:///dev/sdb"` | URI scheme (future) |
| `xfersize=4k` | `size_spec: 4096` | Bytes, not KB shorthand |
| `rdpct=70` | `weight: 70` (get) / `weight: 30` (put) | Explicit op weights |
| `iorate=1000` | `io_rate: { iops: 1000 }` | ✅ v0.7.1+ |
| `threads=8` | `concurrency: 8` | Same concept |
| `-v` | `--validate` | Data validation (future) |
| `elapsed=60` | `duration: 60s` | Same |
| `interval=5` | `interval: 5s` | Same (for metrics) |

---

## Feature Comparison Matrix

| Feature | rdf-bench | sai3-bench v0.8.0 | Priority |
|---------|-----------|-------------------|----------|
| **Storage Backends** |
| Raw block I/O | ✅ Full support | ❌ Not supported | See BLOCK_IO_IMPLEMENTATION_PLAN.md |
| Local filesystem | ✅ Full support | ✅ `file://` | Full parity |
| Direct I/O | ✅ Via flags | ✅ `direct://` | Full parity |
| S3 | ❌ Not supported | ✅ Native | sai3-bench advantage |
| Azure Blob | ❌ Not supported | ✅ Native | sai3-bench advantage |
| Google Cloud Storage | ❌ Not supported | ✅ Native | sai3-bench advantage |
| **File Operations** |
| Read | ✅ | ✅ GET | Full parity |
| Write | ✅ | ✅ PUT | Full parity |
| Delete | ✅ | ✅ DELETE | Full parity |
| List | ✅ | ✅ LIST | Full parity |
| Stat/Head | ✅ | ✅ STAT | Full parity |
| Copy | ✅ | ❌ | v0.9.0 planned |
| Move | ✅ | ❌ | v0.9.0 planned |
| Mkdir/Rmdir | ✅ | ✅ | v0.7.0+ |
| Metadata ops | ✅ | ✅ | v0.7.0+ |
| **Workload Patterns** |
| Random access | ✅ | ✅ | Full parity |
| Sequential | ✅ | ✅ | Full parity |
| I/O rate control | ✅ iorate= | ✅ io_rate | v0.7.1+ |
| GET size control | ✅ xfersizes | ❌ | **HIGH PRIORITY** for v0.8.1 |
| PUT size control | ✅ xfersizes | ✅ distributions | Full parity |
| Sequential/Random I/O within files | ✅ fileio= | ❌ | **MEDIUM-HIGH** for v0.8.1 |
| **Distributed Testing** |
| Multi-host support | ✅ SSH | ✅ gRPC | sai3-bench advantage |
| **Metrics** |
| Latency histograms | ✅ Basic | ✅ HDR | sai3-bench advantage |
| System metrics | ✅ CPU/kstat | ❌ | **HIGH PRIORITY** for v0.8.1 |

---

## Implementation Status

### ✅ Fully Implemented (v0.8.0)

**Core Operations**:
- GET, PUT, LIST, DELETE, STAT, MKDIR, RMDIR
- file://, direct://, s3://, az://, gs:// backends
- Async I/O with tokio runtime
- Concurrent workers with semaphore control

**State Machines** (v0.8.0):
- 5-state agent model (Idle/Ready/Running/Failed/Aborting)
- 9-state controller model with transition validation
- Auto-reset on errors (agents accept new requests immediately)

**Error Handling** (v0.8.0):
- Configurable thresholds (MAX_ERRORS, MAX_ERROR_RATE, MAX_RETRIES)
- Smart backoff on high error rates
- Retry logging at different verbosity levels

**I/O Rate Control** (v0.7.1):
- IOPS target configuration (max or fixed)
- Three distributions: Exponential (Poisson), Uniform, Deterministic
- Per-worker rate throttling

**Directory Trees** (v0.7.0):
- Width/depth hierarchical structures
- Path selection strategies (random, partitioned, exclusive)

### 🚧 Planned Features

**v0.8.1 High Priority** (Q4 2025):
1. **GET I/O Size Control** - Byte range requests for cloud storage
   - Use cases: CDN caching, video streaming, progressive loading
   - Cloud native: S3/Azure/GCS Range headers
   
2. **System Metrics** - CPU, memory, network stats
   - Universal value: Useful for ALL workloads
   - rdf-bench parity: Matches CpuStats.java capabilities

3. **Sequential/Random I/O within files** - fileio= equivalent
   - Use cases: VM images, search engines, analytics, HPC post-processing
   - Implementation: Uniform random 4KB-1MB I/O sizes

**v0.9.0 Medium Priority** (Q1 2026):
4. **File Operations** - Copy, Move for cloud workflows
5. **Strided Access** - HPC hyperslab reads
6. **Enhanced Dedup** - Advanced tracking

### ❌ Not Planned

- Data validation (LBA stamping, checksums) - User decision
- Platform-specific features (Solaris kstat, Windows PDH)
- Block device support initially (may reconsider for v1.0+)

---

## Development Priorities

### Why GET I/O Size Control is High Priority

**Current Gap**: sai3-bench GET always reads full object (no size control)

**Real-world use cases**:
- **CDN edge caching**: Read first 256KB headers only
- **Video streaming**: Progressive loading with Range requests
- **Image optimization**: Load preview thumbnails before full image
- **Log analysis**: Read last N bytes of log files
- **Header inspection**: Check file magic bytes without full download

**Cloud storage native**: All major providers support Range headers
```yaml
# Proposed v0.8.1 syntax
workload:
  - op: get
    path: "videos/*.mp4"
    size_spec:
      type: Uniform
      min: 262144    # 256KB chunks
      max: 1048576   # 1MB chunks
    weight: 70
```

### Why System Metrics are High Priority

**Current limitation**: No CPU/memory correlation with I/O performance

**Use cases**:
- Identify CPU bottlenecks during high-throughput tests
- Correlate memory pressure with latency spikes
- Detect network saturation
- Compare system impact across backends (S3 vs file://)

**Implementation approach**:
- Linux /proc/stat for CPU utilization
- /proc/meminfo for memory stats
- Export alongside I/O metrics in TSV

### Why Sequential/Random I/O is Medium-High Priority

**Revised assessment**: Initially underestimated - many workload categories need this

**Applications using random I/O within files**:
1. VM/container images (VMDK, QCOW2) - Guest OS block-level I/O
2. Search engines (Lucene, Elasticsearch) - Random segment lookups
3. Key-value stores (RocksDB, LevelDB) - SSTable reads
4. Analytics (Parquet, ORC) - Column chunk access
5. Media streaming - HTTP Range for scrubbing
6. Genomics (BAM/CRAM), GIS (GeoTIFF) - Tile/region queries
7. HPC post-processing - Hyperslab reads, strided access

**Implementation phases**:
1. Uniform random (4KB-1MB I/O sizes, configurable)
2. Strided access patterns (HPC-specific)
3. Hotset/Zipfian (cache behavior simulation)

---

## Migration Guide

### Conceptual Differences

1. **URI-Based Targets**
   - rdf-bench: Named storage definitions (SD=sd1)
   - sai3-bench: Direct URI specification (target: "file:///path/")

2. **Workload Specification**
   - rdf-bench: Separate WD/RD definitions
   - sai3-bench: Unified workload array with weights

3. **Operation Weights vs Percentages**
   - rdf-bench: rdpct=70 (70% reads)
   - sai3-bench: weight: 70 for get, weight: 30 for put

### Example Conversion

**rdf-bench config**:
```
sd=sd1,lun=/testdata,openflags=o_direct
wd=wd1,sd=sd1,xfersize=4k,rdpct=70,seekpct=random
rd=run1,wd=wd1,iorate=1000,elapsed=60,threads=8
```

**sai3-bench config**:
```yaml
target: "direct:///testdata/"
duration: 60s
concurrency: 8

io_rate:
  iops: 1000
  distribution: exponential

workload:
  - op: get
    path: "*"
    weight: 70
  - op: put
    path: "*"
    size_spec: 4096
    weight: 30
```

---

## Detailed Analysis

### Scope Note

This comparison focuses on **rdf-bench file workloads (fwd=)** vs sai3-bench. Block device features (wd=) are documented but not compared since sai3-bench doesn't support block I/O yet.

**rdf-bench has two workload modes**:
- `fwd=` (file workloads) - Filesystem and file I/O operations
- `wd=` (block workloads) - Raw block device I/O

### Architecture Comparison

| Aspect | rdf-bench | sai3-bench |
|--------|-----------|------------|
| **Language** | Java + C/JNI | Rust (async/await) |
| **Lines of Code** | ~200+ Java classes | ~20 Rust modules |
| **Age** | 20+ years | 2 years |
| **Threading** | Thread-per-worker | Tokio async runtime |
| **Code Quality** | Legacy patterns | Modern idioms, zero warnings |

### Operations Count

**rdf-bench file workloads**: 15 operations
- read, write, mkdir, rmdir, copy, move, create, delete
- getattr, setattr, access, open, close, put, get

**sai3-bench**: 7 operations
- Get, Put, List, Stat, Delete, Mkdir, Rmdir

**Gap**: Copy, Move, extended metadata operations

### Critical Feature Gaps

**rdf-bench fwd= has (sai3-bench lacks)**:
1. GET I/O size control (xfersizes for reads) - **CRITICAL**
2. System metrics (CPU, kstat, NFS) - **HIGH PRIORITY**
3. Sequential/Random I/O within files (fileio=) - **MEDIUM-HIGH**
4. 15 file operations vs 7 - **MODERATE**

**sai3-bench has (rdf-bench lacks)**:
1. Cloud storage (S3, Azure, GCS) - **CRITICAL**
2. PUT size distributions (lognormal, exponential) - **CRITICAL**
3. Modern async architecture - **MAJOR**
4. HDR histograms with 9 size buckets - **MAJOR**
5. gRPC distribution vs Java sockets - **MODERATE**
6. State machines with auto-reset (v0.8.0) - **NEW**
7. Comprehensive error handling (v0.8.0) - **NEW**

---

## Use Case Recommendations

### Use rdf-bench File Workloads When:
- ✅ Testing filesystem I/O with specific read sizes
- ✅ Need sequential vs random I/O patterns within files
- ✅ File copy and move operations required
- ✅ NFS testing with NFS-specific metrics
- ✅ Solaris kstat integration needed

### Use sai3-bench When:
- ✅ Testing cloud storage (S3, Azure, GCS)
- ✅ Modern distributed object storage (MinIO, Ceph)
- ✅ Need high throughput async I/O
- ✅ Want standard YAML configuration
- ✅ Size-bucketed latency analysis required
- ✅ Robust error handling with auto-reset
- ✅ State machine visibility for debugging

---

## Version History

- **v0.8.0** (Nov 2025): State machines, error handling, signal handling
- **v0.7.1** (Oct 2025): I/O rate control
- **v0.7.0** (Oct 2025): Directory trees, metadata operations
- **v0.6.0** (Sep 2025): Distributed testing via gRPC
- **v0.5.3** (Aug 2025): Size distributions
- **v0.1.0** (Jun 2025): Initial release

See CHANGELOG.md for complete version history.

---

**Analysis Methodology**: 
- Examined 200+ Java classes in rdf-bench
- Analyzed 20 Rust modules in sai3-bench
- Cross-referenced feature claims with implementations
- Verified all claims against actual code (not documentation)

**Last Updated**: November 20, 2025 - Combined from RDF_BENCH_REFERENCE.md and rdf-bench-vs-sai3-bench-comparison.md for planning purposes.
