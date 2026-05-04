# Block Storage Testing — Gap Analysis and Feature Roadmap

**Date**: April 2026  
**Author**: Analysis based on review of rdf-bench, elbencho, and sai3-bench source code  
**Purpose**: Identify block storage testing capabilities present in rdf-bench and elbencho that are absent from sai3-bench, and propose a high-level roadmap for closing those gaps.

---

## 1. Tool Overviews

### 1.1 rdf-bench

rdf-bench is a Java workload generator originally developed by Henk Vandenbergh at Oracle, now maintained as a community fork. Its core design is built around three definition types:

- **SD (Storage Definition)**: Identifies the storage target. Can be a raw block device (`lun=/dev/sdb`), a regular file, or the anchor point for an FSD filesystem test. The `lun=` parameter is the critical path to block device testing.
- **FSD (Filesystem Definition)**: Filesystem workload generator — controls directory/file depth, width, file count, and size.
- **WD (Workload Definition)**: Binds a workload profile to one or more SDs: `xfersize`, `seekpct` (0 = sequential, 100 = pure random), `rdpct` (read percentage), access range (`range=`), hot-banding.
- **RD (Run Definition)**: Specifies how long to run and at what rate: `iorate=max` or a specific number, `elapsed=`, `interval=`, `warmup=`.

The JNI layer (`Jni/rdfblinux.c`) implements the actual I/O: `open64()` with configurable flags (`O_DIRECT`, `O_SYNC`, `O_DSYNC`), then `pread64()`/`pwrite64()` for random seeks, or sequential offset tracking in the Java layer. Block device sizes are discovered via `fstat()`. The key architectural point is that rdf-bench operates on a **single large file or device partitioned across worker threads**, not on many separate named objects.

**Block-specific capabilities in rdf-bench:**
- Raw block device (`/dev/sdX`, `/dev/raw/rawN`) access via `pread64`/`pwrite64` JNI
- Configurable `O_DIRECT`, `O_SYNC`, `O_DSYNC`, `fsync`-on-close flags
- Sequential and random access patterns over a defined LBA range
- Hot-banding (concentrating I/O on a small subset of the address space)
- Stride-based seeks (min/max stride between I/Os)
- Read/write mix (`rdpct=`) within a single open file handle
- Cache-hit simulation (`rhpct`/`whpct`) — produces working set effects
- Data validation with 512-byte block granularity (TinyMT64 RNG-based)
- Journaling for crash recovery and data validation across runs
- Multi-host distributed testing (rsh/ssh-based controller/worker model)
- Workload replay from captured traces
- HTML-format performance reports with per-interval histogram data

### 1.2 elbencho

elbencho is a modern C++17 benchmark written by Sven Breuner. It handles three storage tiers: block devices, large shared files, and many small files, plus S3 objects. Block device and large shared file testing are its strongest domain.

**Block-specific capabilities in elbencho:**
- Direct block device path (`/dev/nvme0n1`, `/dev/sdb`, etc.) — detected and opened with `O_RDWR` (no truncation), size discovered via `ioctl(BLKGETSIZE64)`
- `O_DIRECT` (bypasses page cache) with alignment validation and configurable bypass (`--nodiocheck`)
- **Async I/O via libaio**: configurable I/O queue depth (`--iodepth`); iodepth ≥ 2 activates asynchronous submission through the Linux AIO kernel interface
- Sequential and fully random offsets; random has pluggable algorithms (fast/balanced/strong) and configurable alignment (`--norandalign`)
- Backward sequential I/O (`--backward`)
- Read/write mix on the same device simultaneously: percentage-based (`--rwmixpct`) or thread-split (`--rwmixthr` + `--rwmixthrpct`)
- mmap I/O (`--mmap`) with `madvise()` hints (`seq`, `rand`, `willneed`, `dontneed`, `hugepage`)
- `fadvise()` hints for non-mmap paths (`--fadv`)
- Per-thread per-second rate limiting (`--limitread`, `--limitwrite`)
- NUMA zone binding (`--zones`) and per-core CPU affinity (`--cores`)
- File preallocation via `posix_fallocate()` (`--preallocfile`)
- Page cache drop via `/proc/sys/vm/drop_caches` between phases (`--dropcache`)
- Block variance control for compression/deduplication defeat (`--blockvarpct`)
- GPU/CUDA buffer I/O via `cuFile` API (GPUDirect Storage, optional build feature)
- HDFS support (optional build feature)
- Live CSV and JSON output; per-second interval stats
- "First Done" vs "Last Done" result columns (stonewalling vs tail phases)
- Distributed service mode (`--service` / `--hosts`) with per-host result aggregation
- Infinite loop mode (`--infloop`) for sustained load testing

### 1.3 sai3-bench (Current State)

sai3-bench is built around the **s3dlio** object/file storage library and operates with a fundamentally different execution model from rdf-bench and elbencho. Rather than I/O threads seeking within a large shared file, sai3-bench creates, retrieves, and deletes **named objects** or **named files** using an async Tokio runtime. Its operation types are:

`GET`, `PUT`, `LIST`, `STAT`, `DELETE`, `MKDIR`, `RMDIR`

**What sai3-bench does well today:**
- Multi-protocol: `s3://`, `az://`, `gs://`, `file://`, `direct://`
- Multi-endpoint load balancing with round-robin and least-connections strategies
- Distributed execution via gRPC controller/agent architecture (SSH deploy, coordinated start/stop)
- YAML-driven workload composition with weighted operation mix
- Directory tree workloads (`TreeManifest`) and metadata-cache-backed prepare/verify/cleanup
- Realistic object size distributions (lognormal, uniform, constant)
- Deduplication and compression control for object data generation
- Workload replay (oplog capture + replay with URI remapping)
- Rate control (`io_rate`) and warmup exclusion (`warmup_period`)
- HDR latency histograms with percentile reporting
- Per-second performance logging (`perf_log.tsv`) with 31-column schema
- CPU utilization monitoring

**What `direct://` is and is not:**  
The `direct://` URI scheme in sai3-bench maps to s3dlio's `FileSystemObjectStore`, which uses `O_DIRECT` when opening files. It opens individual named files with `O_DIRECT | O_RDWR`. This is **file-level direct I/O**, not raw block device I/O. Each "object" is still a separate named file in a directory tree. There is no concept of a single large device or file that is shared and seeked by multiple threads at arbitrary offsets.

---

## 2. Block Storage Feature Gap Analysis

The following table summarizes capabilities present in rdf-bench and/or elbencho that are absent or incomplete in sai3-bench:

| Feature | rdf-bench | elbencho | sai3-bench | Notes |
|---------|-----------|----------|------------|-------|
| Raw block device target (`/dev/nvme0n1`) | ✅ (lun=) | ✅ | ❌ | Fundamental gap |
| Single large shared file, multi-thread seeks | ✅ | ✅ | ❌ | Different I/O model |
| Configurable O_DIRECT with alignment enforcement | ✅ | ✅ | Partial (`direct://`) | sai3 has O_DIRECT on files only |
| O_SYNC / O_DSYNC write durability flags | ✅ | ✅ (--sync) | ❌ | Not exposed |
| fsync-on-close | ✅ | ✅ | ❌ | Not exposed |
| Async I/O queue depth (libaio/io_uring) | ❌ | ✅ (--iodepth) | ❌ | Major gap for NVMe IOPS |
| Sequential I/O within single file/device | ✅ | ✅ | ❌ | Objects are named, not seeked |
| Random seek I/O within single file/device | ✅ | ✅ | ❌ | No LBA/offset model |
| Backward sequential I/O | ❌ | ✅ | ❌ | |
| Stride-based random access | ✅ | ❌ | ❌ | Min/max stride between seeks |
| Hot-banding (working set restriction) | ✅ | ❌ | ❌ | Concentrate I/O on hot region |
| Simultaneous read/write mix to same target | ✅ | ✅ | Partial | sai3 has GET+PUT but different paths |
| mmap I/O | ❌ | ✅ | ❌ | |
| fadvise() / madvise() hints | ❌ | ✅ | Partial (`page_cache_mode`) | sai3 has posix_fadvise for file:// |
| NUMA zone binding | ❌ | ✅ | ❌ | |
| CPU core affinity | ❌ | ✅ | ❌ | |
| Page cache drop between phases | ❌ | ✅ | ❌ | |
| Device size auto-discovery | ✅ | ✅ | ❌ | |
| File preallocation (posix_fallocate) | ❌ | ✅ | ❌ | |
| Block variance for dedup/compress defeat | Partial | ✅ | ✅ (object level) | sai3 has dedup/compress factors |
| Data validation (write+read verify) | ✅ | ✅ (--verify) | ❌ | No write-then-read integrity check |
| GPU/CUDA buffer testing | ❌ | ✅ | ❌ | |
| "First Done" / stonewalling result | ❌ | ✅ | ❌ | |
| Cache-hit simulation | ✅ | ❌ | ❌ | |
| Workload curve (iorate steps) | ✅ | ❌ | Partial (manual) | |
| Configurable result intervals (live CSV/JSON) | ✅ | ✅ | ✅ (perf_log) | sai3 has TSV, not JSON/CSV |

---

## 3. Gap Details and Implementation Considerations

### 3.1 Gap 1: Block Device and Large Shared File Target

**Current state**: sai3-bench has no concept of a block device path or a single large file that multiple threads share and seek within. Every operation creates or fetches a named object.

**What's needed**: A new backend or target type — tentatively `block://` or `raw://` — that:
- Accepts a device path (`/dev/nvme0n1`) or a large pre-existing file (`/mnt/data/testfile`)
- Auto-discovers device/file size via `ioctl(BLKGETSIZE64)` or `fstat()`
- Opens with `O_RDWR | O_DIRECT` (and optionally `O_SYNC`/`O_DSYNC`)
- Divides the address space across worker threads (each thread owns a contiguous slice)
- Issues `pread()`/`pwrite()` at calculated offsets rather than opening new files

**Implementation path**: This likely requires a new `BlockDeviceStore` in s3dlio or a sai3-bench-native block I/O engine outside of the object store abstraction. The s3dlio `ObjectStore` trait (`get()`, `put()`, `list()`, etc.) is semantically unsuited to block-level offset I/O, so a parallel trait or a separate code path would be needed.

**Complexity**: High. Requires new CLI/YAML concepts (device path, offset ranges, block sizes as distinct from object sizes) and does not map cleanly to the existing `OpSpec` enum.

---

### 3.2 Gap 2: Async I/O Queue Depth (libaio / io_uring)

**Current state**: sai3-bench uses Tokio async I/O. For object storage (S3/GCS/Azure) this is entirely network-bound and Tokio is the right model. For local NVMe testing, however, the kernel I/O path matters: to saturate modern NVMe SSDs (1–7 million IOPS), you need either many threads or a kernel-bypass queue depth much deeper than one I/O per thread.

**What's needed**: For block device and large-file testing:
- `io_uring` based I/O engine (preferred over libaio on modern Linux) with configurable queue depth per thread
- Or libaio (`io_setup` / `io_submit` / `io_getevents`) as a fallback
- YAML parameter: `io_depth: 16` or similar

**Implementation path**: The `tokio-uring` crate provides io_uring integration for Tokio. Alternatively, the `rio` or `iou` crates expose raw io_uring. For maximum NVMe IOPS, synchronous libaio via the `libaio` crate may be simpler to implement correctly. Either approach would be limited to block device and large-file target types.

**Complexity**: High. Requires new crate dependencies and a parallel execution engine for the block storage path.

---

### 3.3 Gap 3: Sequential and Random I/O Within a Fixed Address Space

**Current state**: sai3-bench's GET and PUT operate on named objects of configurable sizes. There is no notion of seeking to offset X within a large file and reading N bytes.

**What's needed**:
- Sequential mode: threads advance their offset by `block_size` after each I/O, wrapping or stopping at end of file/device
- Random mode: threads generate random aligned offsets within their assigned range for each I/O
- Configuration: `block_size: 4k`, `io_pattern: sequential | random`, optional `random_amount` (limit total random I/O bytes)
- Address range restriction for hot-banding: `offset_start: 0`, `offset_end: 10g`

**Complexity**: Medium-high (requires the block device backend from Gap 1 as a prerequisite).

---

### 3.4 Gap 4: Write Durability Flags (O_SYNC, O_DSYNC, fsync)

**Current state**: The `direct://` backend uses `O_DIRECT`. Neither `O_SYNC` nor `O_DSYNC` are exposed, and there is no fsync-on-close option.

**What's needed**:
- YAML config option, e.g., `write_flags: [o_direct, o_sync]`  
- Applied via `fcntl()` or by passing the flags to `open()`
- For file backends, this can be added independently of the full block device work

**Complexity**: Low-medium. The s3dlio `FileSystemObjectStore` would need a `write_flags` config field, and sai3-bench's config would expose it.

---

### 3.5 Gap 5: Simultaneous Read/Write Mix on the Same Target

**Current state**: sai3-bench supports a weighted mix of GET and PUT operations in the `workload:` array, but GET reads objects that PUT previously created (different names, different sizes). There is no mode where reads and writes occur at the same offsets of the same large file.

**What's needed** (for block/large-file mode):
- `io_mix: 70r/30w` or similar — percentage of block I/Os that are reads vs. writes
- Thread-split model: some threads issue reads, others writes, all within the same address space
- Required for accurate RAID and cache-effect testing

**Complexity**: Medium, but depends on Gap 1 (block device backend).

---

### 3.6 Gap 6: Data Integrity Verification

**Current state**: sai3-bench has no write-then-read integrity checking. Data patterns are generated for dedup/compression testing but are not stored and verified on readback.

**What's needed**:
- Write a known pattern keyed by LBA (offset + sequence number), similar to elbencho's `--verify` or rdf-bench's data validation engine
- On subsequent reads, regenerate the expected pattern from the same seed and verify it matches
- Report miscompares with LBA/offset details
- Optionally embed a per-block checksum (CRC32 or BLAKE3) into the last N bytes of each block for fast detection without full re-generation

**Note on data generation**: sai3-bench already uses `dgen-data` (the author's own high-performance data generation crate, among the fastest available for this purpose). LBA-seeded pattern generation for integrity verification can be built on top of the same infrastructure — no additional RNG crates are needed. The only new dependency is a fast checksum crate (`crc32fast` or `blake3`) if per-block checksums are desired.

**Complexity**: Medium. The challenge is coordinating write seed/sequence state with read verification state across runs (persisted seed tracking per LBA range).

---

### 3.7 Gap 7: NUMA and CPU Affinity

**Current state**: sai3-bench spawns Tokio worker threads but does not pin them to CPU cores or NUMA zones.

**What's needed**:
- YAML option: `numa_zones: [0, 1]` or `cpu_cores: [0-7]`
- Applied via `sched_setaffinity()` (Linux) before each worker starts its I/O loop
- Important for benchmarking asymmetric hardware (e.g., NUMA-sensitive NVMe placement)

**Complexity**: Low-medium. Linux-specific. The `core_affinity` crate provides the required API.

---

### 3.8 Gap 8: Page Cache Management

**Current state**: sai3-bench has `posix_fadvise()` hints via `page_cache_mode`. There is no option to drop caches between phases.

**What's needed**:
- `drop_caches: true` config option — writes `3` to `/proc/sys/vm/drop_caches` between phases (requires root)
- File sync (`sync` syscall) before cache drop, to ensure dirty data is on disk
- Useful for cold-cache read benchmarks and between write and read phases

**Complexity**: Low (Linux-specific, privileged operation).

---

### 3.9 Gap 9: mmap I/O

**Current state**: No mmap support.

**What's needed**:
- `io_engine: mmap` config option
- Memory-map the target file, then use `memcpy()`-based access for reads/writes
- Optional `madvise()` hints for sequential/random workloads
- Only useful for filesystem targets (not block devices or object stores)

**Complexity**: Medium. Requires careful handling of file extension semantics (mmap cannot extend a file), SIGBUS protection, and virtual address space limits at large sizes.

---

### 3.10 Gap 10: "Stonewalling" Result Columns (First Done vs. Last Done)

**Current state**: sai3-bench reports aggregate throughput over the total `duration:` window. It does not distinguish between the throughput while all workers are active ("first done") and the tail phase when some workers have finished.

**What's needed**:
- Track the timestamp when the first and last worker completes its assigned work
- Report two result columns: `FirstDone_MBPS` and `LastDone_MBPS`
- Relevant only in fixed-work-quantity mode, not time-limited mode

**Complexity**: Low-medium. Requires tracking per-worker completion time.

---

## 4. Recommended Rust Crates by Gap

The following crates address specific implementation gaps. Where `dgen-data` already covers a need (data pattern generation), no additional crate is needed.

| Gap | Feature | Recommended Crate(s) | Notes |
|-----|---------|----------------------|-------|
| Gap 1 & 4 | Block device open, `O_DIRECT`/`O_SYNC`, `BLKGETSIZE64` ioctl | `nix`, `libc` | `nix` covers most flags; `libc` for anything `nix` hasn't yet abstracted |
| Gap 2 | Async I/O queue depth (io_uring) | `tokio-uring`, `io-uring` | `tokio-uring` first (Tokio-compatible); raw `io-uring` if finer queue control needed |
| Gap 6 | Per-block checksum for integrity verification | `crc32fast`, `blake3` | `blake3` is fastest with AVX-512; `crc32fast` is simpler; data *generation* is already covered by `dgen-data` |
| Gap 7 | CPU core pinning | `core_affinity` | Simple cross-platform API for `sched_setaffinity` |
| Gap 7 | NUMA topology discovery | `hwloc` | Auto-detect NUMA zones and PCIe proximity; heavier dependency, opt-in |
| Gap 8 | Page cache drop, `posix_fadvise`, `fsync` | `nix` | Same crate as Gap 1; no additional dependency |
| Gap 9 | mmap I/O with `madvise` hints | `memmap2` | Community standard; handles safety complexity and `madvise` (seq/rand) |

### Notes on `tokio-uring` vs. raw `io-uring`

- **`tokio-uring`**: Integrates directly with the existing Tokio runtime. Lowest friction for sai3-bench since everything is already Tokio-based. Requires Linux kernel ≥ 5.10 for production stability.
- **`io-uring` (raw)**: Maximum control over submission/completion queues; useful if `tokio-uring`'s abstraction imposes overhead at multi-million IOPS. Would require a parallel thread pool outside of Tokio for the block engine.
- **Synchronous `pread`/`pwrite` via `nix` in `spawn_blocking`**: Simplest starting point; correct for latency characterization; not sufficient to saturate modern NVMe at low thread counts.

**Recommended progression**: `nix`/`pread`/`pwrite` first (get correctness), then `tokio-uring` for queue depth.

---

## 5. Feature Priority Matrix

Below is a suggested priority ranking for implementing block storage features, based on practical impact and implementation effort:

| Priority | Feature | Effort | Impact | Rationale |
|----------|---------|--------|--------|-----------|
| P1 | Block device / large shared file backend | High | Very High | Unlocks all other block features; makes NVMe/SSD testing possible |
| P2 | Sequential and random seek I/O (depends on P1) | Med-High | Very High | Core block storage test patterns |
| P2 | Async I/O queue depth via io_uring (depends on P1) | High | High | Required to saturate modern NVMe at low thread counts |
| P3 | Read/write mix on same target (depends on P1) | Med | High | Needed for RAID/cache and mixed workload benchmarks |
| P3 | Data integrity verification | Med | High | Important for correctness testing, not just performance |
| P4 | Write durability flags (O_SYNC, O_DSYNC, fsync) | Low-Med | Med | Can be added to file:// independently; useful before full block work |
| P5 | Page cache drop between phases | Low | Med | Clean-cache testing; simple but privileged |
| P5 | NUMA / CPU core affinity | Low-Med | Med | Relevant for high-thread-count NVMe testing on multi-socket systems |
| P6 | mmap I/O engine | Med | Low-Med | Niche use case; adds complexity for limited gain vs. O_DIRECT |
| P6 | Stonewalling result columns | Low | Low-Med | Cosmetic; more useful in fixed-work than time-limited mode |
| P7 | GPU/CUDA support | Very High | Low | Highly specialized; not a common benchmark target |

---

## 6. Architectural Considerations for Implementation

### 6.1 The Core Impedance Mismatch

The fundamental challenge is that sai3-bench's architecture is built around the `ObjectStore` trait, which maps operations to named objects (`get(uri)`, `put(uri, data)`, `list(prefix)`). Block device I/O is fundamentally different — it operates on a single address space divided by byte offsets. Forcing block device semantics through the `ObjectStore` trait would be awkward at best.

The cleanest approach is a **parallel code path** via a dedicated `BlockEngine` module:
- Keep the existing `ObjectStore`-based path for `file://`, `direct://`, `s3://`, `az://`, `gs://`
- Add a new `BlockEngine` trait for block device and large-file random/sequential I/O; it treats the target as a single linear address space of N blocks, which matches the mental model of both rdf-bench and elbencho
- The YAML config gains a `block_target:` section (distinct from `workload:`) that activates the block path; the existing `OpSpec` enum is not extended
- `dgen-data` is used for fast, LBA-seeded data pattern generation within the block engine, consistent with how it is used for object workloads

### 6.2 io_uring vs. Synchronous pread/pwrite

Recommended progression:
1. **Phase 1 — `nix` + `pread`/`pwrite` in `spawn_blocking`**: Simplest to implement; correct; adequate for latency and moderate throughput testing.
2. **Phase 2 — `tokio-uring`**: Adds configurable queue depth per thread; integrates with existing Tokio runtime; required to saturate modern NVMe at low thread counts.
3. **Phase 3 (optional) — raw `io-uring`**: Only if `tokio-uring`'s abstraction imposes measurable overhead at extreme IOPS; requires a separate thread pool bypassing Tokio.

**libaio is not recommended**: `io_uring` is strictly superior on Linux ≥ 5.10, and `tokio-uring` already provides a high-level Rust API. Adding a libaio dependency alongside io_uring adds complexity with no benefit.

### 6.3 YAML Config Schema Extension (Proposed)

A new `block:` config section would be needed alongside the existing `workload:` array, or the existing `OpSpec` enum could be extended with `BlockRead` and `BlockWrite` variants. The latter is less disruptive to existing infrastructure but requires careful handling of path semantics (block device path vs. URI).

Example of a potential YAML schema for block storage:

```yaml
# Block device benchmark (proposed - not yet implemented)
duration: 300s
concurrency: 16

block_target:
  path: /dev/nvme0n1      # Or: /mnt/data/testfile.bin
  io_pattern: random      # sequential | random
  io_direction: read      # read | write | readwrite
  rw_mix_pct: 70          # % reads when io_direction: readwrite
  block_size: 4k
  io_depth: 16            # Async queue depth per thread (default: 1)
  open_flags: [o_direct]  # o_direct | o_sync | o_dsync
  random_seed: 42
  address_range:
    start: 0
    end: 100g             # Restrict I/O to first 100 GiB (hot-banding)

perf_log:
  interval: 1s
```

### 6.4 Scope Question: sai3-bench vs. s3dlio

Block device I/O does not belong in the `s3dlio` library (which is an object/file **storage abstraction** focused on cloud and local file I/O). Block device testing is purely a sai3-bench-level concern and should be implemented entirely within sai3-bench, with no s3dlio involvement. The `dgen-data` crate is used directly by the block engine for data generation.

---

## 8. Implementation Reference Guide

This section is written for an implementer starting fresh. Each subsection cites the exact file and pattern from rdf-bench or elbencho, then maps it to the idiomatic Rust/Tokio equivalent for sai3-bench. The goal is not to port code line-by-line but to extract the **design decisions** that were made by both teams and apply them as our own.

---

### 8.1 Block Device Opening and Size Discovery

**rdf-bench reference:** `Jni/rdfblinux.c` — `file_open()` (lines 67–97) and `file_size()` (lines 42–63)

```c
// rdf-bench: open64 with caller-supplied flags bitmask
fd = open64(filename, (O_RDWR | O_CREAT) | open_flags, 0666);

// rdf-bench: size via fstat (works for regular files; returns 0 for block devs)
rc = fstat(fhandle, &xstat);
filesize = xstat.st_size;
```

**elbencho reference:** `source/ProgArgs.cpp` — block device size detection (lines 2057–2083)

```cpp
// elbencho: detects BenchPathType_BLOCKDEV, then uses lseek(SEEK_END) for size
// (NOT ioctl(BLKGETSIZE64) — lseek(SEEK_END) on a block device returns its byte size)
off_t blockdevSize = lseek(fd, 0, SEEK_END);
lseek(fd, 0, SEEK_SET); // seek back to start
```

**Key design decision**: elbencho's block type detection happens during argument parsing by `stat()`-ing each path and checking `S_ISBLK`. It sets a `BenchPathType_BLOCKDEV` enum. The same file descriptor is then opened `O_RDWR` without `O_CREAT` and without any truncation.

**Rust/sai3-bench mapping:**
```rust
// Idiomatic Rust equivalent using nix crate
use nix::fcntl::{open, OFlag};
use nix::sys::stat::{fstat, SFlag};

// Detect block device
let stat = nix::sys::stat::stat(path)?;
let is_block_dev = SFlag::from_bits_truncate(stat.st_mode).contains(SFlag::S_IFBLK);

// Open with O_DIRECT (mandatory for block benchmarking to bypass page cache)
let flags = OFlag::O_RDWR | OFlag::O_DIRECT;
// For write durability: OFlag::O_SYNC or OFlag::O_DSYNC added conditionally

// Size discovery (works for both block devs and regular files)
// For block devs: lseek(SEEK_END) returns device size (same as elbencho)
// For files: fstat gives st_size
let file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
let size = file.seek(SeekFrom::End(0))?; // works for both block dev and file
```

---

### 8.2 Open Flags Model

**rdf-bench reference:** `RdfBench/OpenFlags.java` (lines 27–50, 118–240)

rdf-bench separates flags into two groups:
1. **`openflags`** — flags passed directly to `open()`: `O_DIRECT`, `O_SYNC`, `O_DSYNC`, `O_RSYNC` (as integer bitmask)
2. **`otherflags`** — post-open behavior: `FSYNC_ON_CLOSE` (line 33), `SOL_CLEAR_CACHE`

`fsync` is NOT an open flag — it is a close-time action triggered by `FSYNC_ON_CLOSE`. The two groups are stored separately and tested with `isOther(FSYNC_ON_CLOSE)` at close time.

**Rust/sai3-bench mapping:**
```rust
// In sai3-bench block config: separate the two categories
#[derive(Debug, Deserialize, Clone)]
pub struct BlockOpenFlags {
    pub o_direct: bool,          // maps to O_DIRECT
    pub o_sync: bool,            // maps to O_SYNC  
    pub o_dsync: bool,           // maps to O_DSYNC
    pub fsync_on_close: bool,    // fsync(fd) at end of phase (NOT an open flag)
}
// At close time: if fsync_on_close { nix::unistd::fsync(fd)?; }
```

The rdf-bench precedent is important: `fsync` as a workload-level option (on every close) produces very different semantics from `O_SYNC` (every write). Both must be exposed independently.

---

### 8.3 Offset Generation: Sequential and Random

**rdf-bench reference:** `RdfBench/ActiveFile.java` — `setNextSequentialLba()` (lines 319–359) and `setNextRandomLba()` (lines 362–421)

- **Sequential**: `next_lba += prev_xfer` after each I/O; check `next_lba >= max_lba` to detect end-of-range; workers track their own `next_lba` state per open file handle (no shared state needed for sequential)
- **Random**: `long blocks = max_lba / xfersize; blocks *= seek_rand.nextDouble(); next_lba = blocks * xfersize;` — block-aligned random in range `[0, max_lba)`
- **Range restriction** (`lowrange`, `highrange` in `WD_entry.java` lines 42–43): applied by restricting `max_lba` and `start_lba` at worker setup time — the offset generator itself doesn't need to know about ranges

**elbencho reference:** `source/toolkits/offsetgen/OffsetGenerator.h` — `OffsetGenSequential` class (lines 48–100)

elbencho uses a clean **trait object** (abstract class) pattern:
```cpp
class OffsetGenerator {
    virtual uint64_t getNextOffset() = 0;
    virtual size_t getNextBlockSizeToSubmit() const = 0;
    virtual uint64_t getNumBytesLeftToSubmit() const = 0;
    virtual void addBytesSubmitted(size_t numBytes) = 0;
};
```
`rwBlockSized()` and `aioBlockSized()` both call `rwOffsetGen->getNextOffset()` polymorphically — no conditional logic in the hot I/O loop itself. This is exactly the Rust trait pattern.

**Rust/sai3-bench mapping:**
```rust
pub trait OffsetGen: Send {
    fn next_offset(&mut self) -> u64;
    fn next_block_size(&self) -> usize;
    fn bytes_remaining(&self) -> u64;
    fn mark_submitted(&mut self, n: usize);
    fn is_done(&self) -> bool { self.bytes_remaining() == 0 }
}

pub struct SeqOffsetGen {
    current: u64,
    end: u64,       // exclusive upper bound (worker's slice of address space)
    block_size: usize,
}
impl SeqOffsetGen {
    fn new(start: u64, end: u64, block_size: usize) -> Self { ... }
}

pub struct RandOffsetGen {
    start: u64,
    end: u64,
    block_size: usize,
    // NOTE: dgen-data already provides fast RNG; use that here for seed-based repeatability
    rng: dgen_data::BlockRng,  // or expose a seed-based u64-range interface
}
```

**Critical rdf-bench insight**: each worker operates on its own **non-overlapping slice** of the device. The device is divided: `worker_start = (device_size / n_workers) * worker_id`. This means no locking between workers on the sequential path. For random, elbencho allows overlapping access across the full device (workers may read/write the same offsets), which is the more common NVMe test.

---

### 8.4 Synchronous pread/pwrite Loop

**rdf-bench reference:** `Jni/rdfblinux.c` — `file_read()` (lines 125–145) and `file_write()` (lines 150–170)

```c
// rdf-bench pread64 call (with buffer guard bytes at start/end for integrity check)
prepare_read_buffer(env, buffer, length);  // sets guard pattern
int rc = pread64((int) fhandle, (void*) buffer, (size_t) length, (off64_t) seek);
// check rc == length, then check_read_buffer() for guard corruption
```

**elbencho reference:** `source/workers/LocalWorker.cpp` — `rwBlockSized()` (lines ~1670–1820)

elbencho's pread/pwrite loop features:
1. `rwOffsetGen->getNumBytesLeftToSubmit()` controls the loop — not a fixed iteration count
2. The RW-mix decision happens per-I/O: `( (workerRank + numIOPSSubmitted) % 100) < rwMixReadPercent` (line 1709-1710)  — deterministic, no atomics needed for the percentage check
3. Function pointer dispatch: `((*this).*funcPositionalRead)(fileHandleIdx, buf, blockSize, offset)` — the actual syscall is chosen at phase init time, not each loop iteration
4. Latency is measured per I/O with `std::chrono::steady_clock`

**Rust/sai3-bench mapping:**
```rust
// In spawn_blocking (Phase 1 implementation):
async fn run_block_worker(cfg: BlockWorkerConfig, mut offset_gen: Box<dyn OffsetGen>) {
    tokio::task::spawn_blocking(move || {
        let fd = open_block_fd(&cfg.path, &cfg.open_flags)?;
        let mut buf = AlignedBuffer::new(cfg.block_size, 512); // 512-byte aligned for O_DIRECT
        
        while !offset_gen.is_done() && !cfg.stop_signal.is_set() {
            let offset = offset_gen.next_offset();
            let block_size = offset_gen.next_block_size();
            let t0 = Instant::now();
            
            // RW-mix: deterministic per-IO decision (no atomics needed)
            let is_read = if cfg.rw_mix_read_pct > 0 {
                (cfg.worker_rank + cfg.iops_submitted) % 100 < cfg.rw_mix_read_pct as u64
            } else {
                cfg.is_read_phase
            };
            
            let n = if is_read {
                nix::sys::uio::pread(fd, &mut buf.as_mut_slice()[..block_size], offset as i64)?
            } else {
                // fill buffer from dgen-data with LBA-seeded pattern
                dgen_data::fill_block(&mut buf, offset, &cfg.dgen_params);
                nix::sys::uio::pwrite(fd, &buf.as_slice()[..block_size], offset as i64)?
            };
            
            let latency_us = t0.elapsed().as_micros();
            cfg.stats.record(n, latency_us, is_read);
            offset_gen.mark_submitted(n);
            cfg.iops_submitted += 1;
        }
        
        if cfg.open_flags.fsync_on_close { nix::unistd::fsync(fd)?; }
        Ok(())
    }).await?
}
```

---

### 8.5 Async I/O Queue Depth (libaio → tokio-uring)

**elbencho reference:** `source/workers/LocalWorker.cpp` — `initLibAio()` (lines 456–476) and `aioBlockSized()` (lines ~1820–2100)

The libaio pattern is a **two-phase loop**:

**Phase 1 — seed** (lines ~1826–1865): Fill the submission queue to `maxIODepth`:
```cpp
while(rwOffsetGen->getNumBytesLeftToSubmit() && (numPending < maxIODepth)) {
    libaioContext.iocbPointerVec[ioVecIdx] = &libaioContext.iocbVec[ioVecIdx];
    ((*this).*funcAioRwPrepper)(&iocbVec[ioVecIdx], fd, ioBufVec[ioVecIdx], blockSize, offset);
    iocbVec[ioVecIdx].data = (void*)ioVecIdx;   // tag with buffer index for completion matching
    io_submit(ioContext, 1, &iocbPointerVec[ioVecIdx]);
    numPending++;
}
```

**Phase 2 — completion + resubmit** (lines ~1867–2060): Wait for one or more completions, then immediately resubmit:
```cpp
while(numPending) {
    int eventsRes = io_getevents(ioContext, 1, AIO_MAX_EVENTS, ioEvents, &timeout);
    for(int i = 0; i < eventsRes; i++) {
        size_t ioVecIdx = (size_t)ioEvents[i].data;  // recover buffer index
        numBytesDone += ioEvents[i].res;
        
        if(rwOffsetGen->getNumBytesLeftToSubmit()) {
            // reuse iocb: prepare and resubmit immediately
            ((*this).*funcAioRwPrepper)(ioEvents[i].obj, fd, ioBufVec[ioVecIdx], newBlockSize, newOffset);
            io_submit(ioContext, 1, &iocbPointerVec[ioVecIdx]);
        } else {
            numPending--;  // no more to submit; drain the queue
        }
    }
}
```

**Key design decisions from elbencho libaio:**
- Each in-flight I/O has its own **buffer** (`ioBufVec[ioVecIdx]`) — `ioBufVec.count == iodepth`
- `iocb.data` carries the buffer index back through the completion event — no lookup table needed
- `io_getevents` timeout (1 second) prevents deadlock; the loop checks `checkInterruptionRequest()` on timeout
- `io_queue_init(maxIODepth, &ioContext)` call is paired with `io_queue_release()` in uninit

**tokio-uring mapping for sai3-bench:**
```rust
// Phase 2 implementation: tokio-uring provides the same two-phase pattern natively
use tokio_uring::fs::File;

async fn run_block_worker_async(cfg: BlockWorkerConfig, mut offset_gen: Box<dyn OffsetGen>) {
    tokio_uring::start(async {
        let file = File::open(&cfg.path).await?;
        
        // Allocate iodepth buffers (one per in-flight I/O)
        let mut bufs: Vec<AlignedBuffer> = (0..cfg.io_depth)
            .map(|_| AlignedBuffer::new(cfg.block_size, 512))
            .collect();
        
        let mut pending: Vec<Option<tokio_uring::BufResult<usize, AlignedBuffer>>> = 
            vec![None; cfg.io_depth];
        
        // Phase 1: seed submissions
        for slot in 0..cfg.io_depth {
            if offset_gen.is_done() { break; }
            let offset = offset_gen.next_offset();
            let buf = bufs[slot].take();
            // tokio-uring passes ownership of buf, returns it on completion
            pending[slot] = Some(file.read_at(buf, offset).await);
        }
        // Phase 2: completion + resubmit (tokio-uring handles the uring SQ/CQ internally)
    });
}
```

**Note on io_depth buffer ownership**: tokio-uring (and io_uring in general) requires that the buffer remain valid until the kernel signals completion — this is why both elbencho and tokio-uring use **per-slot owned buffers**, not a single shared buffer. In Rust, `tokio_uring`'s API enforces this via ownership transfer: `read_at(buf, offset)` takes `buf` and returns it in the `BufResult`. This prevents the class of bugs that libaio has historically caused.

---

### 8.6 Read/Write Mix Dispatch

**elbencho reference:** `source/workers/LocalWorker.cpp` — `initPhaseFunctionPointers()` (lines ~1193–1330) and `rwBlockSized()` (lines 1709–1712)

elbencho uses two strategies for RW mix:
1. **Percentage-based** (`--rwmixpct`): per-I/O modular arithmetic: `(workerRank + numIOPSSubmitted) % 100 < rwMixReadPercent`. All threads do both reads and writes against the same file; the mix is achieved across the stream, not between threads.
2. **Thread-split** (`--rwmixthr`): a configurable number of threads run in `BenchPhase_READFILES` mode while the rest run in `BenchPhase_CREATEFILES` mode. A `RateLimiterRWMixThreads` object (file: `source/toolkits/RateLimiterRWMixThreads.cpp`) rate-balances the two groups to achieve the target mix.

The function pointer `funcPositionalWrite` / `funcPositionalRead` is set once at phase start in `initPhaseFunctionPointers()` and called in the hot loop. This avoids branch prediction misses in tight I/O loops — valuable at very high IOPS.

**rdf-bench reference:** `RdfBench/WD_entry.java` line 62: `double readpct = 100` — the workload definition carries `rdpct` which is applied in the Java I/O loop similarly.

**Rust/sai3-bench mapping**: Use an enum to select the I/O mode at worker initialization:
```rust
enum IoDirection { Read, Write, ReadWriteMix { read_pct: u8 } }

// Hot loop: single branch-predicted match on the enum
match &cfg.io_direction {
    IoDirection::Read => pread(fd, &mut buf, offset)?,
    IoDirection::Write => { fill_buf(&mut buf, offset); pwrite(fd, &buf, offset)? },
    IoDirection::ReadWriteMix { read_pct } => {
        if (worker_rank + iops_done) % 100 < *read_pct as u64 {
            pread(fd, &mut buf, offset)?
        } else {
            fill_buf(&mut buf, offset);
            pwrite(fd, &buf, offset)?
        }
    }
}
```

---

### 8.7 NUMA and CPU Affinity

**elbencho reference:** `source/workers/Worker.cpp` — `applyNumaAndCoreBinding()` (lines 102–148) and `source/toolkits/NumaTk.h` — `bindToNumaZone()` (lines 122–135) and `bindToCPUCores()` (lines 272–293)

```cpp
// Worker rank determines which zone/core from the configured list (round-robin)
int zoneNum = numaZonesVec[workerRank % numaZonesVec.size()];
NumaTk::bindToNumaZone(std::to_string(zoneNum));  // calls numa_run_on_node_mask()

int coreNum = cpuCoresVec[workerRank % cpuCoresVec.size()];
NumaTk::bindToCPUCore(coreNum);  // calls sched_setaffinity() with CPU_SET
```

**Key design decision**: binding is **round-robin per worker rank** — if you have 4 workers and 2 NUMA zones, workers 0 and 2 bind to zone 0, workers 1 and 3 bind to zone 1. This is the standard distribution model and is trivial to implement.

**Rust/sai3-bench mapping** using `core_affinity` crate:
```rust
// In spawn_blocking or thread spawn, called before any I/O
fn apply_cpu_binding(worker_rank: usize, cpu_cores: &[usize]) {
    if cpu_cores.is_empty() { return; }
    let core_id = cpu_cores[worker_rank % cpu_cores.len()];
    let core = core_affinity::CoreId { id: core_id };
    core_affinity::set_for_current(core);  // calls sched_setaffinity internally
}

// NUMA: use hwloc crate or raw libc::numa_run_on_node
// numa_zones: [0, 1] in YAML → same round-robin per worker_rank
```

**Note**: `applyNumaAndCoreBinding()` is called at the very start of `preparePhase()` — **before** any I/O buffer allocation. This ensures that memory allocated by the worker (including its I/O buffers) is placed on the correct NUMA node. Getting this order wrong (bind after alloc) defeats the purpose of NUMA binding.

---

### 8.8 Page Cache Drop

**elbencho reference:** `source/workers/LocalWorker.cpp` — `anyModeDropCaches()` (lines 7788–7830)

```cpp
// Only the first worker of this instance does this — serial kernel spinlock issue
if(workerRank != progArgs->getRankOffset()) {
    workerGotPhaseWork = false;
    return;
}
std::string dropCachesPath = "/proc/sys/vm/drop_caches";
int fd = open(dropCachesPath.c_str(), O_WRONLY);
write(fd, "3", 1);  // "3" drops page cache + dentries + inodes
close(fd);
```

This runs as a **named benchmark phase** (`BenchPhase_DROPCACHES`), not as part of a workload operation. The coordinator sequences it between the write phase and read phase automatically.

**Rust/sai3-bench mapping**: A dedicated phase in the block engine runner:
```rust
async fn drop_page_cache() -> anyhow::Result<()> {
    // Only worker_rank == 0 executes this
    let mut f = tokio::fs::OpenOptions::new()
        .write(true)
        .open("/proc/sys/vm/drop_caches")
        .await?;
    f.write_all(b"3").await?;
    // sync all dirty pages first (elbencho runs sync before dropcaches)
    nix::unistd::sync();
    Ok(())
}
```

In the block YAML config: `drop_caches_between_phases: true` triggers this between the write and read phases automatically.

---

### 8.9 mmap I/O

**elbencho reference:** `source/workers/LocalWorker.cpp` — `mmapReadWrapper()` and `mmapWriteWrapper()` (lines 2723–2741), `source/toolkits/FileTk.h` — `mmapAndMadvise()`

```cpp
// elbencho mmap wrappers: I/O is just memcpy to/from the mapped region
ssize_t LocalWorker::mmapReadWrapper(..., off_t offset) {
    memcpy(buf, &(fileHandles.mmapVec[fileHandleIdx][offset]), nbytes);
    return nbytes;
}
ssize_t LocalWorker::mmapWriteWrapper(..., off_t offset) {
    memcpy(&(fileHandles.mmapVec[fileHandleIdx][offset]), buf, nbytes);
    return nbytes;
}
```

The mmap is set up via `FileTk::mmapAndMadvise()` — `mmap(MAP_SHARED)` + `madvise(MADV_SEQUENTIAL|MADV_RANDOM|...)` — and stored in `fileHandles.mmapVec`. The `mmapReadWrapper`/`mmapWriteWrapper` function pointers are then dropped in to replace the `preadWrapper`/`pwriteWrapper` pointers — the hot loop is identical for both engines.

**Critical constraint** (elbencho `ProgArgs.cpp` line 1249): `iodepth > 1` is incompatible with mmap. `madvise()` is the performance knob for mmap, not queue depth.

**Rust/sai3-bench mapping** using `memmap2` crate:
```rust
use memmap2::{MmapMut, MmapOptions};

let file = std::fs::OpenOptions::new().read(true).write(true).open(&path)?;
let mut mmap = unsafe { MmapOptions::new().map_mut(&file)? };
mmap.advise(memmap2::Advice::Sequential)?;  // MADV_SEQUENTIAL

// I/O becomes memcpy:
fn mmap_read(mmap: &Mmap, buf: &mut [u8], offset: u64) {
    let src = &mmap[offset as usize..offset as usize + buf.len()];
    buf.copy_from_slice(src);
}
fn mmap_write(mmap: &mut MmapMut, buf: &[u8], offset: u64) {
    let dst = &mut mmap[offset as usize..offset as usize + buf.len()];
    dst.copy_from_slice(buf);
}
```

---

### 8.10 Stonewalling ("First Done" vs "Last Done")

**elbencho reference:** `source/Statistics.h` — `PhaseResults` (lines 15–30), `source/workers/WorkersSharedData.cpp` — `incNumWorkersDoneUnlocked()` (lines 19–30), `source/workers/Worker.cpp` — `finishPhase()` (lines 453–474)

```cpp
// Statistics.h: dual result sets
uint64_t firstFinishUSec;  // stonewall time (first worker to complete)
uint64_t lastFinishUSec;   // tail time (all workers complete)
LiveOps opsStoneWallTotal;     // bytes/IOPS at stonewall
LiveOps opsStoneWallPerSec;    // throughput at stonewall

// WorkersSharedData.cpp: stonewall triggered on first done
void WorkersSharedData::incNumWorkersDoneUnlocked(bool triggerStoneWall) {
    numWorkersDone++;
    if(triggerStoneWall)
        cpuUtilFirstDone.update();  // captures CPU util snapshot at stonewall
    if(numWorkersDone == progArgs->getNumThreads())
        cpuUtilLastDone.update();
    condition.notify_all();
}
```

The `triggerStoneWall` flag is set when the calling worker has actually done work (i.e., not a no-op worker that got zero bytes assigned). The `elapsedUSecVec[0]` captures each worker's finish time, allowing the coordinator to identify min/max finish times in `Statistics::calcPhaseResults()`.

**Rust/sai3-bench mapping**: sai3-bench already has a `CompletionTracker` pattern for distributed mode. For block storage:
```rust
pub struct StoneWallTracker {
    first_done_at: Option<Instant>,
    last_done_at: Option<Instant>,
    ops_at_stonewall: AtomicU64,  // bytes done across all workers at first-done time
    total_ops: AtomicU64,
    done_count: AtomicUsize,
    n_workers: usize,
}

impl StoneWallTracker {
    pub fn worker_done(&self, worker_bytes: u64) {
        let prev = self.done_count.fetch_add(1, Ordering::SeqCst);
        let total_now = self.total_ops.fetch_add(worker_bytes, Ordering::SeqCst) + worker_bytes;
        if prev == 0 {  // first finisher
            self.ops_at_stonewall.store(total_now, Ordering::Relaxed);
            // capture first_done_at
        }
        if prev + 1 == self.n_workers {  // last finisher
            // capture last_done_at
        }
    }
}
// Report FirstDone_MBPS = ops_at_stonewall / (first_done_at - phase_start)
// Report LastDone_MBPS = total_ops / (last_done_at - phase_start)
```

---

### 8.11 Data Integrity Verification Pattern

**elbencho reference:** `source/workers/LocalWorker.cpp` — `preWriteIntegrityCheckFillBuf()` (lines ~2100–2150)

```cpp
// elbencho: fills buffer with (offset + salt) as uint64_t checksum values
void LocalWorker::preWriteIntegrityCheckFillBuf(char* buf, ..., off_t fileOffset) {
    const uint64_t checkSumSalt = progArgs->getIntegrityCheckSalt();
    // For each 8-byte aligned slot: value = checkSumStartOffset + checkSumSalt
    // Non-aligned blocks are handled by copying only the relevant bytes
}
```

**rdf-bench reference:** `Jni/rdfb_dv.c` (data validation C layer) and `Jni/tinymt64.c` — TinyMT64 RNG seeded per-block. rdf-bench stores the RNG seed that produced each 512-byte block, allowing later verification by regenerating from seed.

**Rust/sai3-bench mapping** (using dgen-data for generation, crc32fast for verification):
```rust
// Write side: dgen-data fills buffer seeded by LBA (offset)
dgen_data::fill_aligned(&mut buf, offset /* LBA seed */, &cfg.dgen_params);

// Optional: embed CRC32 of first (block_size - 4) bytes into last 4 bytes
if cfg.embed_checksum {
    let data_len = buf.len() - 4;
    let crc = crc32fast::hash(&buf[..data_len]);
    buf[data_len..].copy_from_slice(&crc.to_le_bytes());
}

// Read side: regenerate expected pattern and compare
let mut expected = AlignedBuffer::new(cfg.block_size, 512);
dgen_data::fill_aligned(&mut expected, offset, &cfg.dgen_params);
if cfg.embed_checksum {
    let data_len = expected.len() - 4;
    let stored_crc = u32::from_le_bytes(buf[data_len..].try_into()?);
    let computed_crc = crc32fast::hash(&buf[..data_len]);
    if stored_crc != computed_crc {
        return Err(IntegrityError { offset, stored: stored_crc, computed: computed_crc });
    }
}
```

The critical design point (from both tools): the buffer fill is **deterministic from the offset alone** — no cross-run state needed. `dgen-data` already provides LBA-seeded generation. The only new code is the CRC embed/verify wrapper.

---

### 8.12 sai3-bench Architecture Integration Map

The table below maps each new capability to its place in the sai3-bench codebase, including the new files that need to be created and the existing files that need modification.

| Component | Location | What to Add/Change |
|-----------|----------|--------------------|
| Config | `src/config.rs` | Add `pub block: Option<BlockConfig>` field to `Config`; new `BlockConfig`, `BlockOpenFlags`, `BlockOffsetPattern` structs |
| Block engine | `src/block_engine/mod.rs` (**new**) | `BlockWorker`, `OffsetGen` trait + `SeqOffsetGen` + `RandOffsetGen`, `StoneWallTracker` |
| Buffer mgmt | `src/block_engine/aligned_buf.rs` (**new**) | `AlignedBuffer` — 512/4096-byte aligned memory via `std::alloc::Layout` (required for O_DIRECT) |
| Phase runner | `src/block_engine/runner.rs` (**new**) | `run_block_phase()` — opens device, spawns workers (spawn_blocking), collects stats |
| CLI | `src/main.rs` | Add `block` subcommand that parses YAML and calls `run_block_phase()` |
| Stats output | `src/perf_log.rs` | Add `first_done_mbps` and `last_done_mbps` columns (Gap 10) |
| Cargo.toml | `Cargo.toml` | Add: `nix = "0.29"` (O_DIRECT, pread/pwrite, fsync), `core_affinity = "0.8"` (CPU pinning), `crc32fast = "1"` (optional checksums), `memmap2 = "0.9"` (optional mmap); Phase 2: `tokio-uring = "0.5"` |

**Dependency note**: `nix`, `core_affinity`, and `crc32fast` have no build-time complications. `tokio-uring` requires Linux kernel ≥ 5.10 and will fail to compile on macOS — gate it with `#[cfg(target_os = "linux")]`. `hwloc` requires the system `libhwloc` to be installed and should be an optional Cargo feature.

---

To implement these block-storage enhancements in **sai3-bench** and its core library **s3dlio**, you aren't just adding a few functions; you are effectively building a second "engine" alongside the existing object-based one.

Because `sai3-bench` is designed for object/file workflows (named resources), and block storage requires offset-based access on a fixed address space, the implementation will be largely additive.

### Summary Estimation Table

| Category | New Lines (Est.) | Changed Lines (Est.) | Primary Location |
| :--- | :--- | :--- | :--- |
| **Core Block Engine** | 550 | 120 | `sai3-bench/src/workload/` |
| **io_uring Integration** | 450 | 30 | `s3dlio/src/backends/block/` |
| **Config & YAML Schema** | 180 | 100 | `sai3-bench/src/config/` |
| **Integrity & Pattern Gen** | 220 | 20 | `sai3-bench/src/verify/` |
| **System Utils (NUMA/Drop)**| 120 | 10 | `sai3-bench/src/utils/` |
| **Stats & Reporting** | 80 | 60 | `sai3-bench/src/report/` |
| **Totals** | **1,600** | **340** | |

---

### 1. Breakdown of New Code (~1,600 lines)

#### A. Block Workload Driver (~550 LOC)
The current `sai3-bench` worker loop is likely an async `while` loop performing `get()` or `put()` on a specific URI. For block storage, you need a new worker type that:
* Pre-calculates offset ranges for each thread (slicing the LUN).
* Manages a ring buffer (if using `io_uring`).
* Handles sequential/random logic and stride-based seek calculation.
* **Location**: `sai3-bench` needs a new module like `src/workload/block_engine.rs`.

#### B. `s3dlio` Block Backend (~450 LOC)
You need to add a `BlockDeviceStore` to `s3dlio`. 
* Uses `nix` for `ioctl(BLKGETSIZE64)` and `open(O_DIRECT)`.
* Provides the glue between the high-level `BlockEngine` and the Linux kernel.
* **Location**: `s3dlio/src/backends/block.rs`.

#### C. Data Integrity Module (~220 LOC)
To implement `rdf-bench` style validation, you need a deterministic pattern generator.
* Implementation of a PCG-based generator that uses `LBA + Seed` as the state.
* Logic to interleave `Verify` ops after `Write` ops.
* **Location**: `sai3-bench/src/verify.rs`.

#### D. Hardware & Cache Utils (~120 LOC)
Small but critical utilities for:
* Pinning threads using `core_affinity`.
* Dropping caches via `/proc/sys/vm/drop_caches`.
* Detecting NUMA topology.

---

### 2. Breakdown of Changed Code (~340 lines)

#### A. Config Parsing (~100 LOC)
Your existing `Config` and `OpSpec` structs need to support the new `block://` URI scheme and its parameters (queue depth, block size, etc.). 
* **Files**: `sai3-bench/src/config/workload.rs` and `sai3-bench/src/config/args.rs`.

#### B. Workload Dispatcher (~120 LOC)
The main entry point where threads are spawned needs to branch:
```rust
// Conceptual change in main worker spawn logic
if workload.uri.starts_with("block://") {
    spawn_block_worker(config).await?;
} else {
    spawn_object_worker(config).await?;
}
```

#### C. Reporting & Stats (~60 LOC)
Adding "Stonewalling" support. 
* The `Summary` struct needs two new timestamps: `first_worker_done` and `last_worker_done`.
* The TSV/Console output logic needs to be updated to calculate throughput based on these specific windows.

---

### Implementation Advice
The most complex part will be the **Asynchronous Queue Depth (io_uring)**. 

Since `sai3-bench` is built on a standard `Tokio` runtime, you will face a choice:
1.  **The Easy Path**: Use `tokio-uring`. It integrates well but requires you to use its specific `Runtime`. This will involve changing ~100 lines in your `main.rs` to initialize the `tokio_uring` runtime instead of standard `tokio`.
2.  **The Performance Path**: Run a dedicated thread pool for block I/O that uses raw `io-uring` submission queues, bypassing the `Tokio` executor for the actual I/O calls to minimize overhead. This is what `elbencho` essentially does in C++.

**Recommendation**: Start with the **Data Integrity** and **Write Durability Flags** (Gaps 4 & 6). These are the "lowest-hanging fruit" and can be implemented in the existing `direct://` and `file://` backends with minimal architectural changes.

One relevant follow-up: We are planning to implement the `io_uring` support specifically for Linux only

---

## 9. Summary

sai3-bench is currently a strong performer for **object and filesystem benchmarking**, covering S3, Azure Blob, GCS, local files, and distributed multi-node scenarios. It has no block storage testing capability whatsoever — the `direct://` backend is O_DIRECT file I/O over named files, not raw block device access.

To reach parity with rdf-bench and elbencho for block storage:

1. **The most important single feature is a block device / large shared file backend** (Gap 1). Without it, none of the other block storage features are meaningful. This is a significant architectural addition that does not map cleanly to the existing `ObjectStore` abstraction.

2. **io_uring / async I/O queue depth** (Gap 2) is the second critical piece for NVMe testing. Simple synchronous `pread`/`pwrite` in Tokio `spawn_blocking` can serve as a starting point.

3. **Sequential and random seek patterns** (Gap 3) are straightforward once the block backend exists.

4. All other gaps (durability flags, NUMA affinity, mmap, etc.) are secondary concerns that add value but are not blockers.

rdf-bench and elbencho each took years of development to reach their current block storage capabilities. Implementing a credible block storage engine in sai3-bench is a multi-sprint project. Given that sai3-bench's primary value proposition is multi-protocol object/file storage testing and its distributed gRPC architecture, a pragmatic approach would be to implement a basic block device engine (sequential + random, O_DIRECT, synchronous pread/pwrite) that handles the 90% use case (NVMe throughput and IOPS characterization), and leave advanced features like io_uring, NUMA affinity, and mmap for a later phase.
