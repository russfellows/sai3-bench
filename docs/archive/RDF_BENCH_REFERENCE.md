# rdf-bench Reference for sai3-bench Users

**Purpose**: Comprehensive reference for users familiar with rdf-bench (Oracle VDB-Bench fork)  
**Last Updated**: October 31, 2025  
**sai3-bench Version**: v0.7.1

This document combines feature comparison, command mapping, and implementation status for users migrating from rdf-bench or comparing capabilities.

---

## Table of Contents

1. [Quick Reference](#quick-reference) - Command syntax and common patterns
2. [Feature Comparison Matrix](#feature-comparison-matrix) - Detailed capability comparison
3. [Implementation Status](#implementation-status) - What's implemented, planned, or out of scope
4. [Migration Guide](#migration-guide) - How to convert rdf-bench configs to sai3-bench

---

## Quick Reference

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

### Example Conversion

**rdf-bench config**:
```
sd=sd1,lun=/dev/sdb,size=100g
wd=wd1,sd=sd1,xfersize=4k,rdpct=70,seekpct=random
rd=run1,wd=wd1,iorate=1000,elapsed=60,interval=5
```

**rdf-bench command**:
```bash
./vdbench -f test.parm
```

**sai3-bench config** (`test.yaml`):
```yaml
target: "file:///testdata/"  # Or "direct://", "s3://", "az://", "gs://"
duration: 60s
concurrency: 8

# I/O rate control (v0.7.1+)
io_rate:
  iops: 1000
  distribution: exponential  # or uniform, deterministic

workload:
  - op: get
    path: "*"  # random selection
    weight: 70
    
  - op: put
    path: "*"
    size_spec: 4096
    weight: 30
```

**sai3-bench command**:
```bash
./sai3-bench run --config test.yaml
```

---

## Feature Comparison Matrix

| Feature | rdf-bench | sai3-bench v0.7.1 | Status/Notes |
|---------|-----------|-------------------|--------------|
| **Storage Backends** |
| Raw block I/O | ✅ Full support | ❌ Not supported | Planned - see BLOCK_IO_IMPLEMENTATION_PLAN.md |
| Local filesystem | ✅ Full support | ✅ `file://` | Full support |
| Direct I/O | ✅ Via flags | ✅ `direct://` | Full support with O_DIRECT |
| S3 | ❌ Not supported | ✅ Native | sai3-bench advantage |
| Azure Blob | ❌ Not supported | ✅ Native | sai3-bench advantage |
| Google Cloud Storage | ❌ Not supported | ✅ Native | sai3-bench advantage |
| **File Operations** |
| Read | ✅ | ✅ GET | Full parity |
| Write | ✅ | ✅ PUT | Full parity |
| Delete | ✅ | ✅ DELETE | Full parity |
| List | ✅ | ✅ LIST | Full parity |
| Stat/Head | ✅ | ✅ STAT | Full parity |
| Create | ✅ | ⚠️  Via PUT | Works differently |
| Mkdir | ✅ | ✅ MKDIR | ✅ v0.7.0+ |
| Rmdir | ✅ | ✅ RMDIR | ✅ v0.7.0+ |
| Copy | ✅ | ❌ | Planned for v0.8.0+ |
| Move | ✅ | ❌ | Planned for v0.8.0+ |
| Set attributes | ✅ | ✅ SETATTR | ✅ v0.7.0+ (metadata ops) |
| Get attributes | ✅ | ✅ GETATTR | ✅ v0.7.0+ (metadata ops) |
| Access check | ✅ | ✅ ACCESS | ✅ v0.7.0+ (metadata ops) |
| **Data Validation** |
| LBA stamping | ✅ Full | ❌ | Not planned (user decision) |
| Checksums | ✅ Full | ❌ | Not planned |
| Pattern verification | ✅ Full | ❌ | Not planned |
| Sub-512B uniqueness | ✅ Enhanced | ❌ | Not planned |
| Corruption detection | ✅ Full | ❌ | Not planned |
| Journaling | ✅ Full | ❌ | Not planned |
| Cross-run validation | ✅ Full | ❌ | Not planned |
| **Workload Patterns** |
| Random access | ✅ | ✅ | Full parity |
| Sequential | ✅ | ✅ | Full parity |
| Skip-sequential | ✅ stride= | ❌ | Planned for v0.8.0+ |
| Hot-banding | ✅ hotband= | ❌ | Planned for v0.8.0+ |
| Cache hit simulation | ✅ rhpct= | ❌ | Planned for v0.8.0+ |
| Workload skewing | ✅ | ❌ | Planned (Zipfian distribution) |
| I/O rate control | ✅ iorate= | ✅ io_rate | ✅ v0.7.1+ |
| **Data Patterns** |
| Random data | ✅ | ✅ | Full parity |
| Zero-filled | ✅ | ✅ | Full parity |
| Compressible | ✅ compratio= | ⚠️  compress_factor | Partial implementation |
| Deduplicatable | ✅ dedupratio= | ⚠️  dedup_factor | Partial implementation |
| Validation patterns | ✅ | ❌ | Not planned |
| **Size Control** |
| Fixed size | ✅ | ✅ | Full parity |
| Size range | ✅ | ✅ Uniform | Full parity |
| Size distribution | ⚠️  Basic | ✅ Lognormal | **sai3-bench advantage** |
| Per-file sizes | ✅ | ✅ | Full parity |
| **Directory Trees** |
| Width/depth control | ✅ | ✅ | ✅ v0.7.0+ |
| Files per directory | ✅ | ✅ | ✅ v0.7.0+ |
| Tree creation | ✅ | ✅ | ✅ v0.7.0+ |
| Path selection strategies | ✅ | ✅ | ✅ v0.7.0+ (random, partitioned, exclusive) |
| Shared filesystem testing | ✅ Full | ⚠️  Partial | Basic support, enhancement planned |
| **Distributed Testing** |
| Multi-host support | ✅ SSH | ✅ gRPC | **sai3-bench advantage** (modern protocol) |
| Shared filesystem | ✅ Full | ⚠️  Limited | Works, needs coordination enhancement |
| Per-host config | ✅ | ✅ | Full parity |
| Results aggregation | ✅ | ✅ HDR histograms | Full parity |
| **Performance Metrics** |
| IOPS | ✅ | ✅ | Full parity |
| Throughput | ✅ | ✅ | Full parity |
| Latency (avg) | ✅ | ✅ | Full parity |
| Latency histograms | ✅ | ✅ HDR | Full parity |
| Percentiles | ✅ p50/p95/p99 | ✅ p50/p95/p99/p99.9 | Full parity |
| Response time buckets | ✅ | ✅ 9 buckets | Full parity |
| CPU statistics | ✅ kstat/PDH | ❌ | Not planned (use external tools) |
| NFS statistics | ✅ Solaris | ❌ | Not planned |
| **Results & Reporting** |
| HTML reports | ✅ Full | ❌ | May add in future |
| CSV/TSV export | ✅ Flatfile | ✅ TSV | Full parity |
| Results comparison | ✅ compare tool | ❌ | May add utility |
| Histogram visualization | ✅ HTML | ⚠️  Text | Text output, HTML may come later |
| **Configuration** |
| Parameter files | ✅ .parm | ✅ .yaml | YAML more modern/readable |
| Variable substitution | ✅ $vars | ❌ | May add templating |
| Command-line override | ✅ | ⚠️  Limited | Basic support |
| Config validation | ✅ -s | ✅ --dry-run | Full parity |
| **Platform Support** |
| Linux | ✅ Full | ✅ Full | Full parity |
| Solaris | ✅ Full | ❌ | Not planned |
| Windows | ✅ Full | ⚠️  Limited | Basic support |
| macOS | ✅ Full | ✅ Basic | Basic support |

**Legend**:
- ✅ Full support / Implemented
- ⚠️  Partial/limited support
- ❌ Not supported / Not planned

---

## Implementation Status

### ✅ Fully Implemented (v0.7.1)

**Core Operations** (v0.1.0+):
- GET, PUT, LIST, DELETE, STAT operations
- file://, direct://, s3://, az://, gs:// backends
- Async I/O with tokio runtime
- Concurrent workers with semaphore control

**Size Distributions** (v0.5.3+):
- Fixed, Uniform, Lognormal distributions
- More sophisticated than rdf-bench
- Per-operation size specifications

**Distributed Testing** (v0.6.0+):
- gRPC-based multi-agent architecture
- Controller/agent model
- SSH automation for deployment
- Per-agent configuration

**Directory Trees** (v0.7.0+):
- Width/depth hierarchical structures
- Files per directory control
- Tree creation in prepare phase
- Path selection strategies (random, partitioned, exclusive, weighted)
- MKDIR, RMDIR, metadata operations

**Metadata Operations** (v0.7.0+):
- GETATTR, SETATTR, ACCESS operations
- Cloud storage metadata support
- Filesystem attribute management

**I/O Rate Control** (v0.7.1+):
- IOPS target configuration (max or fixed)
- Three distribution types: Exponential (Poisson), Uniform (fixed intervals), Deterministic (precise)
- Per-worker rate throttling
- Zero overhead when disabled
- See IO_RATE_CONTROL_GUIDE.md for details

**Performance** (v0.6.10+):
- HDR histogram latency tracking
- 9 response time buckets
- p50/p95/p99/p99.9 percentiles
- TSV results export
- Progress bars and real-time stats

### 🚧 Planned Features

**v0.8.0 Roadmap** - Sequential Access (Q1 2026):
- Sequential read/write patterns
- Skip-sequential (stride patterns)
- Hot-banding (concentrated access)
- Cache hit simulation
- Enhanced path selection for sequential access
- See V0.8.0_IMPLEMENTATION_PLAN.md

**Future Versions** (v0.9.0+):
- Block device support (block:// backend) - see BLOCK_IO_IMPLEMENTATION_PLAN.md
- File copy/move operations
- Enhanced compression/dedup simulation
- Zipfian and other statistical distributions
- HTML report generation

### ❌ Not Planned

**Data Validation** (User Decision):
- LBA stamping, checksums, pattern verification
- Journaling and crash consistency testing
- Corruption detection
- **Reason**: Complex feature, limited immediate need, users requiring this can use rdf-bench

**Platform-Specific Features**:
- Solaris support
- kstat integration (Solaris)
- Windows PDH integration
- AIX/HP-UX support
- **Reason**: Focus on Linux primary, cloud platforms

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

4. **Rate Control**
   - rdf-bench: iorate=1000 (global or per-WD)
   - sai3-bench: io_rate: { iops: 1000 } (top-level config)

### Common Scenarios

#### Scenario 1: Simple Random I/O

**rdf-bench**:
```
sd=sd1,lun=/testdata,openflags=o_direct
wd=wd1,sd=sd1,xfersize=4k,rdpct=70,seekpct=random
rd=run1,wd=wd1,iorate=max,elapsed=60,threads=8
```

**sai3-bench**:
```yaml
target: "direct:///testdata/"
duration: 60s
concurrency: 8

workload:
  - op: get
    path: "*"
    weight: 70
  - op: put
    path: "*"
    size_spec: 4096
    weight: 30
```

#### Scenario 2: Rate-Limited I/O (NEW in v0.7.1)

**rdf-bench**:
```
sd=sd1,lun=/testdata
wd=wd1,sd=sd1,xfersize=4k,rdpct=100
rd=run1,wd=wd1,iorate=1000,elapsed=60,threads=10
```

**sai3-bench**:
```yaml
target: "file:///testdata/"
duration: 60s
concurrency: 10

io_rate:
  iops: 1000
  distribution: exponential  # Realistic Poisson arrivals

workload:
  - op: get
    path: "*"
    size_spec: 4096
    weight: 100
```

#### Scenario 3: Directory Tree Testing

**rdf-bench**:
```
sd=sd1,lun=/shared
wd=wd1,sd=sd1,width=10,depth=3,files=100,operations=(create,mkdir)
rd=run1,wd=wd1,elapsed=30
```

**sai3-bench**:
```yaml
target: "file:///shared/"
duration: 30s
concurrency: 4

prepare:
  directory_structure:
    width: 10
    depth: 3
    files_per_directory: 100
    file_size_dist:
      type: Fixed
      size: 4096

workload:
  - op: mkdir
    path: "testdirs/*"
    weight: 5
  - op: put
    path: "testdirs/**/*"
    size_spec: 4096
    weight: 20
```

#### Scenario 4: Multi-Host Distributed

**rdf-bench**:
```
hd=default,system=host1,jvms=1
hd=host2,system=host2,jvms=1
sd=sd1,lun=/shared/data
wd=wd1,sd=sd1,xfersize=1m,rdpct=50
rd=run1,wd=wd1,elapsed=60
```

**sai3-bench**:
```yaml
target: "file:///shared/data/"
duration: 60s
concurrency: 8

distributed:
  role: controller
  agents:
    - address: "host1:7167"
    - address: "host2:7167"

workload:
  - op: get
    path: "*"
    weight: 50
  - op: put
    path: "*"
    size_spec: 1048576
    weight: 50
```

#### Scenario 5: Cloud Storage (sai3-bench Advantage)

**No rdf-bench equivalent** - rdf-bench doesn't support cloud storage

**sai3-bench**:
```yaml
target: "s3://my-bucket/test-data/"
duration: 60s
concurrency: 32

workload:
  - op: get
    path: "*"
    weight: 70
  - op: put
    path: "*"
    size_spec:
      type: Lognormal
      median: 1048576
      sigma: 1.5
    weight: 30
```

### Key Advantages by Tool

**rdf-bench advantages**:
- Data validation (LBA stamping, checksums)
- Raw block device testing
- Advanced workload patterns (hot-banding, cache simulation)
- Mature feature set (20+ years development)

**sai3-bench advantages**:
- Modern cloud storage support (S3, Azure, GCS)
- Rust safety and performance
- gRPC distributed testing (vs legacy protocols)
- Advanced size distributions (Lognormal)
- Modern async I/O architecture
- Progress visualization
- I/O rate control (v0.7.1+)

---

## Performance Comparison

### rdf-bench Reported Performance
- **Block I/O**: 500K+ IOPS on NVMe
- **File I/O**: Limited by filesystem overhead
- **Cloud**: Not supported

### sai3-bench Measured Performance
- **File I/O**: 100K+ IOPS on local SSD
- **Direct I/O**: 200K+ IOPS with O_DIRECT
- **Cloud Storage** (GCS same-region): 2.6 GB/s throughput
- **S3**: 1-2 GB/s depending on region/bandwidth
- **Block I/O**: Not yet implemented

---

## Questions & Support

For questions about:
- **rdf-bench migration**: Check examples in `tests/configs/` directory
- **Feature requests**: Open GitHub issue with rdf-bench feature comparison
- **Bug reports**: GitHub issues with reproduction config

**Documentation**:
- User Guide: USAGE.md
- Configuration Syntax: CONFIG_SYNTAX.md
- Distributed Testing: DISTRIBUTED_TESTING_GUIDE.md
- Directory Trees: DIRECTORY_TREE_GUIDE.md
- I/O Rate Control: IO_RATE_CONTROL_GUIDE.md (v0.7.1+)

**For rdf-bench-specific features not in sai3-bench**, continue using rdf-bench for those workloads. Both tools can coexist for different testing needs.

---

## Version History

- **v0.7.1** (Oct 2025): Added I/O rate control
- **v0.7.0** (Oct 2025): Directory trees, metadata operations
- **v0.6.11** (Oct 2025): SSH automation, config-driven agents
- **v0.6.0** (Sep 2025): Distributed testing via gRPC
- **v0.5.3** (Aug 2025): Size distributions
- **v0.3.1** (Jul 2025): HDR histograms
- **v0.1.0** (Jun 2025): Initial release

See CHANGELOG.md for complete version history.
