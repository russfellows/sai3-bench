# RDF-Bench Feature Analysis for sai3-bench Integration

**Date**: October 29, 2025  
**Analyst**: GitHub Copilot  
**Purpose**: Identify advanced features in rdf-bench for potential integration into sai3-bench

## Executive Summary

RDF-Bench (formerly VDB-Bench) is a mature storage benchmarking tool with ~200 Java classes and native C/JNI code, offering significant capabilities beyond current sai3-bench features. This analysis identifies **15 major feature areas** where rdf-bench is more advanced, organized into three priority tiers.

**Key Finding**: Raw block I/O testing is rdf-bench's most distinctive capability, but many filesystem features are also more sophisticated than sai3-bench's current `file://` support.

---

## Priority 1: High-Value Features (Implement First)

### 1. ✅ **Raw Block I/O Testing (`block://` backend)**

**rdf-bench Capability:**
- Direct raw disk/LUN I/O using platform-specific native code
- Supports `/dev/sdX`, `/dev/rdsk/`, Windows drives
- No filesystem overhead - true block-level testing
- Critical for storage array, SAN, NVMe device testing

**sai3-bench Gap:**
- Only supports filesystem operations (`file://`, `direct://`, cloud backends)
- Cannot test raw block devices or LUNs
- Missing entire storage testing use case

**Implementation Path:**
```rust
// New backend: block://
// Example URI: block:///dev/sdb or block:///dev/nvme0n1

pub enum BackendType {
    // ... existing ...
    BlockDevice,  // NEW: Raw block I/O
}

// Implementation considerations:
// - Use libc::open() with O_DIRECT for Linux
// - Platform-specific block size detection (ioctl BLKGETSIZE64)
// - Aligned memory allocation for direct I/O (posix_memalign)
// - Support for range/offset/size parameters
// - Read-only safety checks (require explicit --allow-write flag)
```

**rdf-bench Code Reference:**
- `Jni/rdfblinux.c`: `file_read()`, `file_write()` with `pread64/pwrite64`
- `RdfBench/SD_entry.java`: Storage Definition with LUN support
- Native JNI for platform-specific block operations

**Complexity**: **Medium** (requires unsafe Rust, platform-specific code)  
**Impact**: **High** (enables entire new testing category)

---

### 2. ✅ **Advanced Data Validation & Corruption Detection**

**rdf-bench Capability:**
- **Sub-512 byte unique block generation** (critical enhancement)
- LBA stamping, checksums, key validation, owner validation
- Detects data corruption, silent data loss, cache coherency issues
- Journaling for crash consistency testing
- Cross-run validation (write on one system, verify on another)

**sai3-bench Gap:**
- No data validation beyond basic read/write
- Cannot detect corruption or verify data integrity
- No journaling or cross-run validation

**Implementation Path:**
```rust
// Data validation module (src/validation.rs)
pub struct DataBlock {
    magic: u64,           // 0xVAL1DAT3 constant
    lba: u64,             // Logical block address
    timestamp: u64,       // Write timestamp
    checksum: u32,        // CRC32 or xxHash
    pattern: Vec<u8>,     // LFSR-generated unique data
}

// Validation modes:
pub enum ValidationMode {
    None,                 // Current behavior
    Quick,                // LBA + checksum only
    Full,                 // All fields + pattern verification
}

// Configuration:
pub struct ValidationConfig {
    mode: ValidationMode,
    unique_block_size: usize,  // 512 bytes default (rdf-bench enhancement)
    journal_dir: Option<PathBuf>,  // For crash testing
}
```

**rdf-bench Code Reference:**
- `Jni/rdfb_dv.c`: Complete data validation engine (600+ lines)
- `Jni/tinymt64.c`: Mersenne Twister RNG for unique data generation
- Enhanced to 512-byte blocks (vs original 4KB) to prevent sub-block dedup

**Complexity**: **High** (requires cryptographic primitives, journaling)  
**Impact**: **High** (essential for production storage testing)

---

### 3. ✅ **Comprehensive Filesystem Operations**

**rdf-bench Capability:**
- **20+ filesystem operations**: create, read, write, delete, mkdir, rmdir, open, close, copy, move, setattr, getattr, access
- Directory tree operations (depth, width, files per directory)
- File distribution control (size distributions, placement)
- Shared filesystem testing (multi-host coordination)

**sai3-bench Gap:**
- Only basic operations: GET, PUT, LIST, DELETE, STAT
- No directory management (mkdir, rmdir)
- No file metadata operations (setattr, getattr, access)
- No file copy/move operations

**Implementation Path:**
```yaml
# Enhanced operations for file:// backend
workload:
  - op: mkdir
    path: "testdir/"
    weight: 5
    
  - op: rmdir
    path: "testdir/"
    weight: 5
    
  - op: copy
    source: "data/*.dat"
    destination: "backup/"
    weight: 10
    
  - op: move
    source: "tmp/*.log"
    destination: "archive/"
    weight: 5
    
  - op: setattr
    path: "data/*.dat"
    attributes:
      mode: "0644"
      mtime: "now"
    weight: 2
    
  - op: getattr
    path: "data/*"
    weight: 10
```

**rdf-bench Code Reference:**
- `RdfBench/Op*.java`: 20+ operation classes (OpCreate, OpMkdir, OpCopy, etc.)
- `RdfBench/FileAnchor.java`: Directory tree management
- `RdfBench/FileEntry.java`: File metadata and lifecycle

**Complexity**: **Medium** (extend OpSpec enum, implement operations)  
**Impact**: **Medium-High** (more realistic filesystem testing)

---

### 4. ✅ **Sophisticated Workload Patterns**

**rdf-bench Capability:**
- **Hot-banding**: Concentrate I/O on specific regions (e.g., 90% hits on 10% of storage)
- **Skip-sequential** (stride patterns): Read every Nth block
- **Cache hit simulation**: Control read hit percentage
- **Workload skewing**: Non-uniform access distributions
- **I/O rate curves**: Test across multiple rates in single run

**sai3-bench Gap:**
- Uniform random or sequential patterns only
- No cache hit control
- No hot-spot simulation
- Single concurrency level per run

**Implementation Path:**
```rust
// Enhanced workload patterns (src/patterns.rs)
pub enum AccessPattern {
    Random,              // Current: uniform random
    Sequential,          // Current: sequential
    HotBand {            // NEW: skewed access
        region_pct: f64,      // e.g., 10% of storage
        hit_pct: f64,         // e.g., 90% of accesses
    },
    SkipSequential {     // NEW: stride pattern
        stride: u64,          // Skip every N blocks
    },
    Zipfian {            // NEW: realistic skew
        theta: f64,           // Zipf exponent (0.8-1.2 typical)
    },
}

pub struct CacheSimulation {
    hit_pct: f64,        // Target cache hit percentage
    hit_region: u64,     // Size of "hot" data in cache
}
```

**rdf-bench Code Reference:**
- `RdfBench/WD_entry.java`: Workload patterns (hotband, stride, seekpct)
- Access pattern generation in native code

**Complexity**: **Medium** (mathematical distributions)  
**Impact**: **High** (enables realistic production workload simulation)

---

### 5. ✅ **Multi-Host Coordination & File Sharing**

**rdf-bench Capability:**
- **Shared filesystem testing**: Multiple hosts accessing same files
- **Distributed file creation**: Coordinated file naming to avoid collisions
- **Cross-host validation**: Write on host A, verify on host B
- SSH-based multi-host deployment (similar to sai3-bench v0.6.11)

**sai3-bench Gap:**
- Distributed mode exists (v0.6.0+) but assumes independent datasets
- No shared filesystem coordination
- Per-agent path isolation prevents shared access testing

**Implementation Path:**
```yaml
# Enhanced distributed config for shared storage
distributed:
  shared_storage: true  # NEW: Enable shared filesystem mode
  file_sharing:
    lock_protocol: "fcntl"  # or "none" for lock-free testing
    collision_avoidance: "host_prefix"  # or "atomic_rename"
  
  agents:
    - address: "host1:7761"
      # All agents work on same dataset (no path_prefix)
    - address: "host2:7761"
```

**rdf-bench Code Reference:**
- `RdfBench/FileAnchor.java`: File sharing logic
- Bug fix (GitHub Issue #1): File naming collision prevention

**Complexity**: **Medium-High** (coordination protocols, race conditions)  
**Impact**: **High** (critical for NFS, Lustre, GPFS testing)

---

## Priority 2: Medium-Value Features

### 6. ✅ **Data Deduplication Testing**

**rdf-bench Capability:**
- Explicit dedup ratio control (e.g., `dedupratio=2.5` for 40% savings)
- Dedupable block sets (hot sets, flipflop patterns)
- **512-byte unique blocks** to prevent sub-4KB deduplication
- Verify storage dedup claims with validation

**sai3-bench Current State:**
- `dedup_factor` parameter exists in config (v0.5.3+)
- Basic implementation in size_generator.rs
- Not fully utilized in workload generation

**Implementation Path:**
```rust
// Enhanced dedup control (src/dedup.rs)
pub struct DedupConfig {
    ratio: f64,           // Target dedup ratio (1.0 = no dedup)
    unit: usize,          // Dedupable block size (4KB, 8KB, etc.)
    unique_sets: usize,   // Number of unique blocks
    hot_sets: Vec<(usize, f64)>,  // (set_id, access_pct) for skewed access
}

// Data generation with controlled dedup:
pub fn generate_dedupable_data(size: usize, config: &DedupConfig) -> Vec<u8> {
    // Mix unique and duplicate blocks per config
}
```

**rdf-bench Code Reference:**
- `RdfBench/Dedup.java`: Dedup simulation engine
- `RdfBench/DedupBitMap.java`: Track duplicate blocks
- `Jni/rdfb_dv.c`: 512-byte unique block generation

**Complexity**: **Medium** (deterministic data generation)  
**Impact**: **Medium** (important for dedup storage testing)

---

### 7. ✅ **Data Compression Simulation**

**rdf-bench Capability:**
- `compratio` parameter: Generate compressible data
- Realistic compression patterns (not just zero-filled)
- Verify storage compression effectiveness

**sai3-bench Current State:**
- `compress_factor` parameter exists (v0.5.3+)
- Not fully implemented in data generation

**Implementation Path:**
```rust
// Compression simulation (src/compress.rs)
pub struct CompressConfig {
    ratio: f64,           // Target compression ratio (2.0 = 50% size)
    pattern: CompressPattern,
}

pub enum CompressPattern {
    ZeroFilled,           // Simple: X% zeros, Y% random
    Repeated,             // Repeated blocks
    LowEntropy,           // ASCII text-like data
}

pub fn generate_compressible_data(size: usize, config: &CompressConfig) -> Vec<u8> {
    // Generate data that compresses to target ratio
}
```

**rdf-bench Code Reference:**
- `RdfBench/CompressObject.java`: Compression simulation
- Pattern generation in data validation code

**Complexity**: **Low-Medium** (pattern generation)  
**Impact**: **Medium** (useful for all-flash arrays with compression)

---

### 8. ✅ **Response Time Histograms**

**rdf-bench Capability:**
- Detailed latency bucketing (microsecond granularity)
- Per-operation histograms
- HTML histogram reports

**sai3-bench Current State:**
- HDR histograms exist (v0.3.1+)
- Basic p50/p95/p99 reporting
- Could be more detailed

**Implementation Path:**
```rust
// Enhanced histogram reporting (src/histograms.rs)
pub struct HistogramReport {
    buckets: Vec<(u64, u64, u64)>,  // (min_us, max_us, count)
    percentiles: Vec<(f64, u64)>,    // More percentiles (p90, p99.9, p99.99)
}

// HTML report generation:
pub fn generate_html_histogram(hist: &Histogram<u64>, op: &str) -> String {
    // Beautiful histogram visualization
}
```

**rdf-bench Code Reference:**
- `RdfBench/Histogram.java`: Histogram management
- Native histogram updates in JNI code

**Complexity**: **Low** (enhance existing HDR histogram output)  
**Impact**: **Low-Medium** (better visualization)

---

### 9. ✅ **File Size Distributions**

**rdf-bench Capability:**
- Explicit file size distributions in directory trees
- Mixture of small and large files
- Realistic filesystem testing

**sai3-bench Current State:**
- Size distributions exist (v0.5.3+): Fixed, Uniform, Lognormal
- Well-implemented in `size_generator.rs`
- ✅ **Already competitive with rdf-bench**

**Gap Analysis**: **MINIMAL** - sai3-bench's lognormal distribution is actually more sophisticated than rdf-bench's fixed size arrays.

**Complexity**: N/A  
**Impact**: N/A (already implemented)

---

### 10. ✅ **I/O Priority Control**

**rdf-bench Capability:**
- Per-workload I/O priority settings
- Platform-specific priority APIs

**sai3-bench Gap:**
- No I/O priority control
- All operations equal priority

**Implementation Path:**
```rust
// I/O priority support (Linux-specific initially)
pub enum IoPriority {
    RealTime(u8),    // IOPRIO_CLASS_RT (0-7)
    BestEffort(u8),  // IOPRIO_CLASS_BE (0-7)
    Idle,            // IOPRIO_CLASS_IDLE
}

// Set via ioprio_set() syscall on Linux
#[cfg(target_os = "linux")]
pub fn set_io_priority(priority: IoPriority) -> Result<()> {
    // libc::syscall(SYS_ioprio_set, ...)
}
```

**rdf-bench Code Reference:**
- `RdfBench/WD_entry.java`: Priority field
- Platform-specific implementation

**Complexity**: **Low-Medium** (platform-specific syscalls)  
**Impact**: **Low** (niche use case)

---

## Priority 3: Lower-Value / Niche Features

### 11. ✅ **Trace Replay (Swat/External Traces)**

**rdf-bench Capability:**
- Replay I/O traces from external tools (Swat)
- Timing-faithful replay
- Workload characterization

**sai3-bench Current State:**
- Op-log replay exists (v0.4.0+)
- Can replay sai3-bench's own operations
- ✅ **Conceptually equivalent**

**Gap Analysis**: rdf-bench supports external trace formats, sai3-bench only replays its own logs. External format support could be added if needed.

**Complexity**: **Medium** (parse external formats)  
**Impact**: **Low** (niche use case)

---

### 12. ✅ **CPU & System Statistics**

**rdf-bench Capability:**
- kstat integration (Solaris/OpenSolaris)
- CPU utilization per workload
- Windows PDH (Performance Data Helper)
- NFS statistics collection

**sai3-bench Gap:**
- No integrated system statistics
- Relies on external monitoring (e.g., `top`, `sar`)

**Implementation Path:**
```rust
// System statistics module (src/sys_stats.rs)
#[cfg(target_os = "linux")]
pub struct SystemStats {
    cpu_usage: f64,
    io_wait: f64,
    context_switches: u64,
    // Parse from /proc/stat
}

// Periodic collection during workload
```

**Complexity**: **Medium** (platform-specific)  
**Impact**: **Low-Medium** (nice-to-have for correlation)

---

### 13. ✅ **Journal Recovery & Crash Testing**

**rdf-bench Capability:**
- Journal writes before execution
- Recover and validate after crash
- Detect incomplete/lost writes

**sai3-bench Gap:**
- No journaling infrastructure
- Cannot verify crash consistency

**Implementation Path:**
```rust
// Journal module (src/journal.rs)
pub struct JournalEntry {
    timestamp: u64,
    operation: OpType,
    uri: String,
    offset: u64,
    size: u64,
    checksum: u64,
}

// Write-ahead logging:
pub async fn journal_write(entry: &JournalEntry, journal_dir: &Path) -> Result<()> {
    // Sync journal before actual operation
}

// Recovery:
pub async fn recover_and_validate(journal_dir: &Path) -> Result<Vec<DataCorruption>> {
    // Read journal, check actual data
}
```

**rdf-bench Code Reference:**
- `RdfBench/JournalThread.java`: Journaling coordinator
- `RdfBench/Jnl_entry.java`: Journal entry structure

**Complexity**: **High** (crash consistency, fsync semantics)  
**Impact**: **Medium** (important for filesystem/database testing)

---

### 14. ✅ **Utility Functions**

**rdf-bench Capability:**
- `./vdbench print`: Print raw blocks from LUN/file
- `./vdbench sds`: Generate SD parameters
- `./vdbench compare`: Compare test results
- `./vdbench parse`: Parse flatfiles
- Compression/dedup simulators

**sai3-bench Current State:**
- `sai3-bench util` subcommand exists
- Basic utilities (health checks)
- Could be expanded

**Implementation Path:**
```rust
// Enhanced util subcommand
Commands::Util {
    #[command(subcommand)]
    util_command: UtilCommands,
}

enum UtilCommands {
    Health { uri: String },           // Existing
    Print { uri: String, offset: u64, length: u64 },  // NEW: Print blocks
    Compare { results_a: PathBuf, results_b: PathBuf },  // NEW: Compare runs
    Validate { data_dir: PathBuf },   // NEW: Offline validation
}
```

**Complexity**: **Low-Medium** (straightforward utilities)  
**Impact**: **Low** (convenience features)

---

### 15. ✅ **Parameter File Flexibility**

**rdf-bench Capability:**
- Complex parameter files with variables
- Nested definitions (SD, WD, RD hierarchy)
- Command-line overrides (`testsize=100g`)
- Looping constructs

**sai3-bench Current State:**
- YAML-based configuration (cleaner, more modern)
- No variable substitution
- No command-line overrides

**Implementation Path:**
```rust
// Variable substitution in YAML (using tera or handlebars)
// Example config with variables:
// target: "{{ storage_backend }}"
// workload:
//   - op: get
//     weight: {{ read_weight }}

// Parse with:
use tera::Tera;
let template = Tera::one_off(&yaml_content, &context, false)?;
```

**Complexity**: **Low** (template engine integration)  
**Impact**: **Low-Medium** (improved config flexibility)

---

## Implementation Roadmap

### Phase 1: Raw Block I/O Foundation (4-6 weeks)
**Goal**: Enable block device testing

1. **`block://` Backend** (2 weeks)
   - Linux implementation first (`/dev/sdX`, `/dev/nvme0n1`)
   - Platform detection (block size, device size via ioctl)
   - Safety checks (read-only by default, require `--allow-write`)

2. **Aligned I/O & Memory** (1 week)
   - posix_memalign for O_DIRECT requirements
   - Block-aligned offset enforcement
   - Error handling for misaligned operations

3. **Basic Block Workloads** (1 week)
   - Random read/write
   - Sequential read/write
   - Mixed workloads

4. **Documentation & Testing** (1 week)
   - Safety guidelines (WARNING: destructive operations)
   - Example configs for common scenarios
   - Unit tests with loop devices

### Phase 2: Data Validation (4-6 weeks)
**Goal**: Detect data corruption

1. **Validation Framework** (2 weeks)
   - DataBlock structure (LBA, timestamp, checksum, pattern)
   - Quick validation mode (LBA + checksum)
   - Full validation mode (all fields + pattern)

2. **Unique Data Generation** (1 week)
   - 512-byte unique blocks (rdf-bench enhancement)
   - LFSR or Mersenne Twister patterns
   - Integration with existing size_generator

3. **Cross-Run Validation** (1 week)
   - Metadata storage (written block inventory)
   - Validation pass (read + verify)
   - Error reporting (location, type, expected vs actual)

4. **Testing & Bug Fixes** (1 week)
   - Inject synthetic errors
   - Verify detection
   - Performance impact measurement

### Phase 3: Advanced Workload Patterns (3-4 weeks)
**Goal**: Realistic production simulation

1. **Hot-Banding** (1 week)
   - Access pattern skewing
   - Configurable hot region size and hit percentage

2. **Cache Hit Simulation** (1 week)
   - Working set size control
   - Re-read percentage

3. **Skip-Sequential & Stride** (1 week)
   - Non-contiguous sequential patterns
   - Configurable stride length

4. **Zipfian & Other Distributions** (1 week)
   - Statistical distribution library integration
   - Validation against real-world traces

### Phase 4: Filesystem Enhancements (3-4 weeks)
**Goal**: Complete filesystem testing

1. **Extended Operations** (2 weeks)
   - mkdir, rmdir, copy, move
   - setattr, getattr, access
   - Error handling for all operations

2. **Directory Tree Operations** (1 week)
   - Depth, width, files-per-directory control
   - Tree creation and teardown

3. **Shared Filesystem Testing** (1 week)
   - Multi-host coordination for shared storage
   - File naming collision avoidance
   - Lock protocol options (fcntl, none)

### Phase 5: Polish & Integration (2-3 weeks)

1. **Dedup & Compression** (1 week)
   - Complete dedup_factor implementation
   - Compression pattern generation

2. **Journaling** (1 week)
   - Basic write-ahead logging
   - Recovery and validation

3. **Documentation & Examples** (1 week)
   - Comprehensive docs for new features
   - Migration guide from rdf-bench
   - Performance comparison benchmarks

**Total Estimated Time**: 16-23 weeks (~4-6 months)

---

## Rust-Specific Advantages

While integrating rdf-bench features, sai3-bench will maintain Rust advantages:

1. **Memory Safety**: No JNI boundary, no GC pauses
2. **Performance**: Zero-cost abstractions, optimal codegen
3. **Modern Async**: Tokio for efficient concurrency
4. **Type Safety**: Compile-time guarantees for complex workloads
5. **Ecosystem**: Excellent crates for crypto, compression, networking

---

## Recommendations

### Must-Have (High ROI):
1. ✅ **Raw block I/O** - Enables entirely new testing category
2. ✅ **Data validation** - Essential for production storage testing
3. ✅ **Hot-banding & workload skewing** - Realistic workload simulation

### Should-Have (Medium ROI):
4. ✅ **Extended filesystem operations** - More complete filesystem testing
5. ✅ **Deduplication testing** - Important for modern storage
6. ✅ **Multi-host shared filesystem** - Critical for distributed FS (NFS, Lustre)

### Nice-to-Have (Lower ROI, implement if time permits):
7. ✅ **Compression simulation**
8. ✅ **Journaling & crash testing**
9. ✅ **System statistics collection**
10. ✅ **Enhanced histogram reporting**

### Skip (Low Value or Already Covered):
- Trace replay (external formats) - sai3-bench op-log replay is sufficient
- File size distributions - sai3-bench's lognormal distribution is already excellent
- kstat integration - platform-specific, limited value on Linux

---

## Conclusion

RDF-Bench offers 15 major feature areas beyond sai3-bench's current capabilities. The **raw block I/O testing** capability is the most distinctive and valuable, followed by **data validation** and **advanced workload patterns**. 

A phased implementation over 4-6 months would bring sai3-bench to feature parity with rdf-bench while maintaining Rust's performance and safety advantages.

**Next Steps**:
1. Review this analysis with stakeholders
2. Prioritize Phase 1 features (block I/O)
3. Create detailed technical specifications
4. Begin implementation with `block://` backend prototype
