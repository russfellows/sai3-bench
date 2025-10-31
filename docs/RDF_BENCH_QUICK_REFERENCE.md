# Quick Reference: rdf-bench vs sai3-bench

**Purpose**: Side-by-side comparison for users migrating from rdf-bench

---

## Feature Comparison Matrix

| Feature | rdf-bench | sai3-bench v0.6.11 | sai3-bench v0.8.0 (Planned) |
|---------|-----------|-------------------|---------------------------|
| **Storage Backends** |
| Raw block I/O | ✅ Full support | ❌ Not supported | ✅ Planned (v0.7.0) |
| Local filesystem | ✅ Full support | ✅ file:// | ✅ Enhanced ops |
| Direct I/O | ✅ Via flags | ✅ direct:// | ✅ Optimized |
| S3 | ❌ Not supported | ✅ Native | ✅ Native |
| Azure Blob | ❌ Not supported | ✅ Native | ✅ Native |
| Google Cloud Storage | ❌ Not supported | ✅ Native | ✅ Native |
| **File Operations** |
| Read | ✅ | ✅ GET | ✅ GET |
| Write | ✅ | ✅ PUT | ✅ PUT |
| Delete | ✅ | ✅ DELETE | ✅ DELETE |
| List | ✅ | ✅ LIST | ✅ LIST |
| Stat/Head | ✅ | ✅ STAT | ✅ STAT |
| Create | ✅ | ⚠️  Via PUT | ✅ CREATE |
| Mkdir | ✅ | ❌ | ✅ MKDIR |
| Rmdir | ✅ | ❌ | ✅ RMDIR |
| Copy | ✅ | ❌ | ✅ COPY |
| Move | ✅ | ❌ | ✅ MOVE |
| Set attributes | ✅ | ❌ | ✅ SETATTR |
| Get attributes | ✅ | ⚠️  Via STAT | ✅ GETATTR |
| Access check | ✅ | ❌ | ✅ ACCESS |
| **Data Validation** |
| LBA stamping | ✅ Full | ❌ | ✅ Planned |
| Checksums | ✅ Full | ❌ | ✅ Planned |
| Pattern verification | ✅ Full | ❌ | ✅ Planned |
| Sub-512B uniqueness | ✅ Enhanced | ❌ | ✅ Planned |
| Corruption detection | ✅ Full | ❌ | ✅ Planned |
| Journaling | ✅ Full | ❌ | ⚠️  Basic |
| Cross-run validation | ✅ Full | ❌ | ✅ Planned |
| **Workload Patterns** |
| Random access | ✅ | ✅ | ✅ |
| Sequential | ✅ | ✅ | ✅ |
| Skip-sequential | ✅ stride= | ❌ | ✅ Planned |
| Hot-banding | ✅ hotband= | ❌ | ✅ Planned |
| Cache hit simulation | ✅ rhpct= | ❌ | ✅ Planned |
| Workload skewing | ✅ | ❌ | ✅ Zipfian |
| I/O rate curves | ✅ iorate=(...) | ⚠️  Via concurrency | ⚠️  Manual |
| **Data Patterns** |
| Random data | ✅ | ✅ | ✅ |
| Zero-filled | ✅ | ✅ | ✅ |
| Compressible | ✅ compratio= | ⚠️  compress_factor | ✅ Enhanced |
| Deduplicatable | ✅ dedupratio= | ⚠️  dedup_factor | ✅ Enhanced |
| Validation patterns | ✅ | ❌ | ✅ |
| **Size Control** |
| Fixed size | ✅ | ✅ | ✅ |
| Size range | ✅ | ✅ Uniform | ✅ |
| Size distribution | ⚠️  Basic | ✅ Lognormal | ✅ |
| Per-file sizes | ✅ | ✅ | ✅ |
| **Distributed Testing** |
| Multi-host support | ✅ SSH | ✅ gRPC | ✅ gRPC |
| Shared filesystem | ✅ Full | ⚠️  Limited | ✅ Enhanced |
| Per-host config | ✅ | ✅ | ✅ |
| Results aggregation | ✅ | ✅ HDR histograms | ✅ |
| **Performance Metrics** |
| IOPS | ✅ | ✅ | ✅ |
| Throughput | ✅ | ✅ | ✅ |
| Latency (avg) | ✅ | ✅ | ✅ |
| Latency histograms | ✅ | ✅ HDR | ✅ Enhanced |
| Percentiles | ✅ p50/p95/p99 | ✅ p50/p95/p99 | ✅ More |
| Response time buckets | ✅ | ✅ 9 buckets | ✅ |
| CPU statistics | ✅ kstat/PDH | ❌ | ⚠️  Basic |
| NFS statistics | ✅ Solaris | ❌ | ❌ |
| **Results & Reporting** |
| HTML reports | ✅ Full | ❌ | ⚠️  Planned |
| CSV/TSV export | ✅ Flatfile | ✅ TSV | ✅ |
| Results comparison | ✅ compare tool | ❌ | ⚠️  Planned |
| Histogram visualization | ✅ HTML | ⚠️  Text | ✅ HTML |
| **Configuration** |
| Parameter files | ✅ .parm | ✅ .yaml | ✅ |
| Variable substitution | ✅ $vars | ❌ | ⚠️  Planned |
| Command-line override | ✅ | ⚠️  Limited | ✅ |
| Config validation | ✅ -s | ✅ --dry-run | ✅ |
| **Utility Functions** |
| Print blocks | ✅ vdbench print | ❌ | ⚠️  Planned |
| Trace replay | ✅ Swat | ✅ Op-log | ✅ |
| Dedup simulator | ✅ dsim | ❌ | ⚠️  Planned |
| Compress simulator | ✅ csim | ❌ | ⚠️  Planned |
| **Platform Support** |
| Linux | ✅ Full | ✅ Full | ✅ Full |
| Solaris | ✅ Full | ❌ | ❌ |
| Windows | ✅ Full | ⚠️  Limited | ⚠️  Basic |
| macOS | ✅ Full | ✅ Basic | ✅ Basic |
| AIX | ✅ | ❌ | ❌ |
| HP-UX | ✅ | ❌ | ❌ |

**Legend**:
- ✅ Full support
- ⚠️  Partial/limited support
- ❌ Not supported

---

## Command Comparison

### Simple Random I/O Test

**rdf-bench**:
```bash
cat > test.parm << EOF
sd=sd1,lun=/dev/sdb
wd=wd1,sd=sd1,xfersize=4k,rdpct=70
rd=run1,wd=wd1,iorate=1000,elapsed=60
