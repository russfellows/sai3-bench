# Block I/O Implementation Plan for sai3-bench

**Feature**: Raw block device I/O support (`block://` backend)  
**Priority**: P1 - High Value  
**Estimated Effort**: 4-6 weeks  
**Target Version**: v0.7.0

---

## Overview

Enable sai3-bench to perform raw block I/O operations on physical disks, NVMe devices, and LUNs without filesystem overhead. This is critical for storage array testing, NVMe performance characterization, and SAN validation.

**Example Usage**:
```bash
# Test NVMe device raw performance
./sai3-bench run --config nvme_test.yaml

# Where nvme_test.yaml contains:
# target: "block:///dev/nvme0n1"
# workload:
#   - op: get
#     path: "*"    # Random reads across entire device
#     weight: 70
#   - op: put
#     path: "*"    # Random writes
#     size_spec: 4096
#     weight: 30
```

---

## Architecture Design

### 1. Backend Type Extension

```rust
// src/workload.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    S3,
    Azure,
    Gcs,
    File,
    DirectIO,
    BlockDevice,  // NEW
}

impl BackendType {
    pub fn from_uri(uri: &str) -> Self {
        if uri.starts_with("block://") {
            BackendType::BlockDevice
        }
        // ... existing logic ...
    }
}
```

### 2. New Module: `src/block_device.rs`

```rust
// src/block_device.rs
//
// Raw block device I/O operations using libc
//

use anyhow::{anyhow, bail, Context, Result};
use std::os::unix::io::{AsRawFd, RawFd};
use std::fs::{File, OpenOptions};
use std::path::Path;

/// Block device handle with metadata
pub struct BlockDevice {
    file: File,
    path: String,
    size_bytes: u64,
    block_size: u32,
    read_only: bool,
}

impl BlockDevice {
    /// Open block device with safety checks
    /// 
    /// # Safety
    /// - Requires root or appropriate permissions
    /// - Read-only by default to prevent accidental data loss
    /// - Use `allow_write: true` to enable write operations
    pub fn open(path: &str, allow_write: bool) -> Result<Self> {
        let device_path = Path::new(path);
        
        // Validate it's a block device
        let metadata = std::fs::metadata(device_path)
            .with_context(|| format!("Cannot access block device: {}", path))?;
        
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::FileTypeExt;
            if !metadata.file_type().is_block_device() {
                bail!("Path is not a block device: {}", path);
            }
        }
        
        // Open with O_DIRECT for true raw I/O
        let file = OpenOptions::new()
            .read(true)
            .write(allow_write)
            .custom_flags(libc::O_DIRECT | libc::O_SYNC)
            .open(device_path)
            .with_context(|| format!("Failed to open block device: {}", path))?;
        
        let fd = file.as_raw_fd();
        
        // Get device size via ioctl
        let size_bytes = Self::get_device_size(fd)?;
        
        // Get logical block size
        let block_size = Self::get_block_size(fd)?;
        
        info!("Opened block device: {} (size: {} bytes, block_size: {} bytes)", 
              path, size_bytes, block_size);
        
        if !allow_write {
            warn!("Block device opened in READ-ONLY mode. Use --allow-write to enable writes.");
        }
        
        Ok(BlockDevice {
            file,
            path: path.to_string(),
            size_bytes,
            block_size,
            read_only: !allow_write,
        })
    }
    
    /// Get device size using BLKGETSIZE64 ioctl (Linux)
    #[cfg(target_os = "linux")]
    fn get_device_size(fd: RawFd) -> Result<u64> {
        let mut size: u64 = 0;
        let ret = unsafe {
            libc::ioctl(fd, libc::BLKGETSIZE64, &mut size as *mut u64)
        };
        if ret != 0 {
            bail!("Failed to get device size: {}", std::io::Error::last_os_error());
        }
        Ok(size)
    }
    
    /// Get logical block size using BLKSSZGET ioctl (Linux)
    #[cfg(target_os = "linux")]
    fn get_block_size(fd: RawFd) -> Result<u32> {
        let mut block_size: i32 = 0;
        let ret = unsafe {
            libc::ioctl(fd, libc::BLKSSZGET, &mut block_size as *mut i32)
        };
        if ret != 0 {
            bail!("Failed to get block size: {}", std::io::Error::last_os_error());
        }
        Ok(block_size as u32)
    }
    
    /// Read aligned data from block device
    /// 
    /// # Requirements
    /// - `offset` must be block-aligned (multiple of block_size)
    /// - `length` must be block-aligned
    /// - Returns aligned buffer suitable for O_DIRECT
    pub async fn read_aligned(&self, offset: u64, length: usize) -> Result<Vec<u8>> {
        // Validate alignment
        self.validate_alignment(offset, length)?;
        
        // Allocate aligned buffer for O_DIRECT
        let buffer = self.allocate_aligned_buffer(length)?;
        
        // Perform pread in blocking pool (O_DIRECT is synchronous)
        let fd = self.file.as_raw_fd();
        let result = tokio::task::spawn_blocking(move || {
            unsafe {
                let ptr = buffer.as_ptr() as *mut libc::c_void;
                let ret = libc::pread(fd, ptr, length, offset as i64);
                if ret < 0 {
                    Err(std::io::Error::last_os_error())
                } else if ret as usize != length {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("Short read: expected {} bytes, got {}", length, ret)
                    ))
                } else {
                    Ok(buffer)
                }
            }
        }).await??;
        
        Ok(result)
    }
    
    /// Write aligned data to block device
    /// 
    /// # Requirements
    /// - `offset` must be block-aligned
    /// - `data.len()` must be block-aligned
    /// - Device must be opened with `allow_write: true`
    pub async fn write_aligned(&self, offset: u64, data: &[u8]) -> Result<()> {
        if self.read_only {
            bail!("Cannot write to read-only block device. Use --allow-write flag.");
        }
        
        // Validate alignment
        self.validate_alignment(offset, data.len())?;
        
        // Copy to aligned buffer if needed
        let aligned_data = if Self::is_aligned_ptr(data.as_ptr()) {
            data.to_vec()
        } else {
            let mut aligned = self.allocate_aligned_buffer(data.len())?;
            aligned.copy_from_slice(data);
            aligned
        };
        
        // Perform pwrite in blocking pool
        let fd = self.file.as_raw_fd();
        let length = data.len();
        tokio::task::spawn_blocking(move || {
            unsafe {
                let ptr = aligned_data.as_ptr() as *const libc::c_void;
                let ret = libc::pwrite(fd, ptr, length, offset as i64);
                if ret < 0 {
                    Err(std::io::Error::last_os_error())
                } else if ret as usize != length {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        format!("Short write: expected {} bytes, wrote {}", length, ret)
                    ))
                } else {
                    Ok(())
                }
            }
        }).await??;
        
        Ok(())
    }
    
    /// Validate offset and length are block-aligned
    fn validate_alignment(&self, offset: u64, length: usize) -> Result<()> {
        if offset % self.block_size as u64 != 0 {
            bail!("Offset {} not aligned to block size {}", offset, self.block_size);
        }
        if length % self.block_size as usize != 0 {
            bail!("Length {} not aligned to block size {}", length, self.block_size);
        }
        if offset + length as u64 > self.size_bytes {
            bail!("Operation exceeds device size: offset {} + length {} > device size {}", 
                  offset, length, self.size_bytes);
        }
        Ok(())
    }
    
    /// Allocate page-aligned buffer for O_DIRECT I/O
    fn allocate_aligned_buffer(&self, size: usize) -> Result<Vec<u8>> {
        let alignment = 4096; // Page size alignment (required for O_DIRECT)
        
        // Allocate with posix_memalign
        let mut ptr: *mut libc::c_void = std::ptr::null_mut();
        let ret = unsafe {
            libc::posix_memalign(&mut ptr, alignment, size)
        };
        
        if ret != 0 {
            bail!("Failed to allocate aligned buffer: errno {}", ret);
        }
        
        // Convert to Vec<u8> with proper drop semantics
        Ok(unsafe {
            Vec::from_raw_parts(ptr as *mut u8, size, size)
        })
    }
    
    /// Check if pointer is properly aligned for O_DIRECT
    fn is_aligned_ptr(ptr: *const u8) -> bool {
        (ptr as usize) % 4096 == 0
    }
    
    /// Get random aligned offset within device
    pub fn random_aligned_offset(&self, length: usize) -> u64 {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        
        let max_offset = self.size_bytes.saturating_sub(length as u64);
        let max_blocks = max_offset / self.block_size as u64;
        
        let random_block = rng.gen_range(0..=max_blocks);
        random_block * self.block_size as u64
    }
    
    /// Get device metadata
    pub fn metadata(&self) -> BlockDeviceMetadata {
        BlockDeviceMetadata {
            path: self.path.clone(),
            size_bytes: self.size_bytes,
            block_size: self.block_size,
            read_only: self.read_only,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BlockDeviceMetadata {
    pub path: String,
    pub size_bytes: u64,
    pub block_size: u32,
    pub read_only: bool,
}

// Proper cleanup
impl Drop for BlockDevice {
    fn drop(&mut self) {
        debug!("Closing block device: {}", self.path);
    }
}
```

### 3. Integration with ObjectStore Pattern

Option A: **Extend s3dlio library** (Recommended)
```rust
// In s3dlio repo: src/block_store.rs
pub struct BlockDeviceObjectStore {
    device: BlockDevice,
}

impl ObjectStore for BlockDeviceObjectStore {
    async fn get(&self, uri: &str) -> Result<Bytes> {
        // Parse URI: block:///dev/sdb?offset=0&length=4096
        // or block:///dev/sdb#random (random location)
        let (offset, length) = parse_block_uri(uri)?;
        let data = self.device.read_aligned(offset, length).await?;
        Ok(Bytes::from(data))
    }
    
    async fn put(&self, uri: &str, data: &[u8]) -> Result<()> {
        let (offset, _) = parse_block_uri(uri)?;
        self.device.write_aligned(offset, data).await
    }
    
    // list, delete, stat not applicable to block devices
    // Return NotSupported error
}
```

Option B: **Direct integration in sai3-bench** (Faster prototype)
```rust
// src/workload.rs - add block-specific operations
async fn get_object_block_device(uri: &str, device: &BlockDevice) -> Result<Vec<u8>> {
    // Handle block:// URIs directly
}

async fn put_object_block_device(uri: &str, data: &[u8], device: &BlockDevice) -> Result<()> {
    // Handle block:// URIs directly
}
```

### 4. Configuration Schema

```yaml
# Example: nvme_test.yaml
target: "block:///dev/nvme0n1"

# REQUIRED: Explicit write permission flag
allow_write: false  # Default: false (read-only for safety)

# Block-specific configuration
block_device:
  # Alignment enforcement (default: auto-detect from device)
  alignment: 4096  # bytes
  
  # I/O size constraints
  min_io_size: 512
  max_io_size: 1048576  # 1 MiB
  
  # Safety checks
  verify_device_type: true  # Ensure it's a block device
  require_confirmation: true  # Require --yes flag for writes

# Standard workload (same as other backends)
workload:
  - op: get
    path: "*"  # For block devices, "*" means random locations
    weight: 70
    
  - op: put
    path: "*"
    size_spec:
      type: fixed
      size: 4096  # Must be block-aligned
    weight: 30

duration: 60s
concurrency: 32
```

### 5. CLI Changes

```rust
// src/main.rs
#[derive(Parser)]
struct Cli {
    // ... existing ...
    
    /// Allow write operations on block devices (DANGEROUS - can destroy data)
    /// 
    /// This flag is REQUIRED for any PUT/DELETE operations on block:// URIs.
    /// Without this flag, block devices are opened read-only.
    /// 
    /// WARNING: Writing to a block device will OVERWRITE existing data!
    /// Make sure you are using the correct device path.
    #[arg(long = "allow-write", global = true)]
    allow_write: bool,
    
    /// Skip confirmation prompts (dangerous with --allow-write)
    #[arg(long = "yes", short = 'y', global = true)]
    yes: bool,
}

// In run command handler:
if cfg.target.as_ref().map(|t| t.starts_with("block://")).unwrap_or(false) {
    if has_write_operations(&cfg) && !cli.allow_write {
        bail!("Block device writes require --allow-write flag.\n\
               WARNING: This will OVERWRITE data on the device!");
    }
    
    if cli.allow_write && !cli.yes {
        // Interactive confirmation
        println!("⚠️  WARNING: You are about to perform write operations on a block device!");
        println!("   Device: {}", cfg.target.as_ref().unwrap());
        println!("   This will PERMANENTLY OVERWRITE existing data!");
        print!("   Type 'yes' to continue: ");
        // ... read confirmation ...
    }
}
```

---

## Implementation Phases

### Phase 1: Core Infrastructure (Week 1-2)

**Tasks**:
1. Create `src/block_device.rs` with BlockDevice struct
2. Implement `open()`, `get_device_size()`, `get_block_size()`
3. Implement `read_aligned()` with alignment validation
4. Add unit tests with loop devices
5. Add to `BackendType` enum

**Testing**:
```bash
# Create loop device for testing
sudo dd if=/dev/zero of=/tmp/test_block.img bs=1M count=100
sudo losetup /dev/loop20 /tmp/test_block.img

# Run read-only tests
cargo test test_block_device_read

# Cleanup
sudo losetup -d /dev/loop20
```

**Deliverable**: Read-only block device support

---

### Phase 2: Write Operations & Safety (Week 3)

**Tasks**:
1. Implement `write_aligned()` with safety checks
2. Add `--allow-write` CLI flag
3. Add confirmation prompts
4. Implement buffer alignment (`allocate_aligned_buffer()`)
5. Add comprehensive error messages

**Testing**:
```bash
# Test write operations (destructive - use loop device!)
sudo ./target/release/sai3-bench --allow-write --yes \
  run --config tests/configs/block_write_test.yaml
```

**Deliverable**: Write operations with safety guardrails

---

### Phase 3: ObjectStore Integration (Week 4)

**Tasks**:
1. Decide: Extend s3dlio vs direct integration
2. Implement URI parsing for block:// scheme
3. Add block-specific operations to workload.rs
4. Handle "*" pattern (random locations)
5. Add proper error handling for unsupported operations (list, delete)

**Testing**:
```bash
# Random read workload
./sai3-bench run --config tests/configs/block_random_read.yaml

# Mixed read/write workload
./sai3-bench --allow-write --yes \
  run --config tests/configs/block_mixed.yaml
```

**Deliverable**: Full integration with sai3-bench workload engine

---

### Phase 4: Advanced Features (Week 5-6)

**Tasks**:
1. Sequential access pattern support
2. Range specification (offset, size in URI or config)
3. Performance optimization (io_uring on Linux 5.1+)
4. Platform support (macOS, Windows)
5. Documentation and examples

**Testing**:
```bash
# Sequential read benchmark
./sai3-bench run --config tests/configs/block_sequential.yaml

# Compare with file:// backend
./sai3-bench run --config tests/configs/file_vs_block_comparison.yaml
```

**Deliverable**: Production-ready block device support

---

## Safety Considerations

### 1. **Data Loss Prevention**
- Read-only by default
- Explicit `--allow-write` flag required
- Interactive confirmation before writes
- Clear warning messages

### 2. **Path Validation**
```rust
// Prevent accidental writes to system disks
const PROTECTED_DEVICES: &[&str] = &[
    "/dev/sda",   // Common system disk
    "/dev/nvme0n1",  // Common system NVMe
    "/dev/vda",   // Common VM disk
];

fn validate_device_path(path: &str, allow_write: bool) -> Result<()> {
    if allow_write && PROTECTED_DEVICES.contains(&path) {
        bail!("Refusing to write to protected device: {}.\n\
               This appears to be a system disk. Use /dev/sdb, /dev/loop*, or similar.", 
              path);
    }
    Ok(())
}
```

### 3. **Permission Checks**
```rust
// Ensure user has appropriate permissions
fn check_permissions(path: &Path, allow_write: bool) -> Result<()> {
    let metadata = std::fs::metadata(path)?;
    let permissions = metadata.permissions();
    
    if allow_write && !permissions.readonly() {
        // Additional check: are we root?
        #[cfg(target_os = "linux")]
        {
            let euid = unsafe { libc::geteuid() };
            if euid != 0 {
                warn!("Not running as root. Write operations may fail.");
            }
        }
    }
    Ok(())
}
```

### 4. **Alignment Validation**
- All operations validated before execution
- Clear error messages for misaligned access
- Automatic rounding/padding (with warning) as option

---

## Documentation Requirements

### 1. User Guide: `docs/BLOCK_DEVICE_TESTING.md`

```markdown
# Block Device Testing with sai3-bench

## ⚠️  DANGER: DATA LOSS WARNING

Block device testing performs RAW I/O operations that will PERMANENTLY OVERWRITE data.

**ONLY use block devices that:**
- Contain NO important data
- Are dedicated test devices
- Are properly identified (double-check device paths!)

**Common safe devices for testing:**
- `/dev/loop*` - Loop devices (created from files)
- `/dev/ram*` - RAM disks
- `/dev/sdX` where X is a test disk (VERIFY BEFORE USE!)

## Quick Start

### Read-Only Testing (Safe)

Test device performance without modifying data:

```bash
./sai3-bench run --config nvme_read_test.yaml
```

```yaml
# nvme_read_test.yaml
target: "block:///dev/nvme1n1"  # Use non-system disk!
allow_write: false  # Explicit read-only (default)

workload:
  - op: get
    path: "*"
    weight: 100

duration: 60s
concurrency: 64
```

### Write Testing (DESTRUCTIVE)

Requires explicit flags:

```bash
./sai3-bench --allow-write --yes run --config nvme_write_test.yaml
```

## URI Patterns

```
block:///dev/sdb              # Entire device, random locations
block:///dev/sdb?offset=1GB   # Start at 1GB offset
block:///dev/sdb#sequential   # Sequential access pattern
```

## Best Practices

1. **Always use loop devices for development**
2. **Triple-check device paths before writing**
3. **Use read-only mode first to validate config**
4. **Monitor with iostat/iotop during tests**
5. **Backup any data before testing**
```

### 2. Technical Reference: `docs/BLOCK_IO_API.md`

API documentation for developers, error codes, platform differences, etc.

---

## Testing Strategy

### 1. Unit Tests (`tests/block_device_tests.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_alignment_validation() {
        // Test alignment checking
    }
    
    #[test]
    fn test_buffer_alignment() {
        // Test aligned buffer allocation
    }
    
    #[tokio::test]
    async fn test_read_only_operations() {
        // Test with loop device (non-destructive)
    }
    
    // More tests...
}
```

### 2. Integration Tests (`tests/block_integration_test.sh`)

```bash
#!/bin/bash
# Integration test with real loop device

set -e

# Create 100MB loop device
dd if=/dev/zero of=/tmp/test_block.img bs=1M count=100
LOOP=$(sudo losetup -f --show /tmp/test_block.img)

echo "Created loop device: $LOOP"

# Test read operations
echo "Testing read operations..."
./target/release/sai3-bench run --config tests/configs/block_read_test.yaml

# Test write operations
echo "Testing write operations..."
./target/release/sai3-bench --allow-write --yes \
  run --config tests/configs/block_write_test.yaml

# Cleanup
sudo losetup -d $LOOP
rm /tmp/test_block.img

echo "All tests passed!"
```

### 3. Performance Validation

Compare with known benchmarks:
```bash
# fio baseline
fio --name=random_read --ioengine=libaio --direct=1 \
    --bs=4k --rw=randread --filename=/dev/nvme1n1 --runtime=60

# sai3-bench equivalent
./sai3-bench run --config nvme_random_read.yaml
```

---

## Performance Considerations

### 1. **io_uring Support (Linux 5.1+)**

Future optimization using io_uring for true async block I/O:

```rust
#[cfg(all(target_os = "linux", feature = "io_uring"))]
mod io_uring_impl {
    use io_uring::{opcode, IoUring, types};
    
    pub struct IoUringBlockDevice {
        ring: IoUring,
        // ... 
    }
    
    // High-performance async I/O implementation
}
```

### 2. **Batching & Queueing**

Implement I/O batching for higher IOPS:
```rust
pub struct BatchedBlockIO {
    device: BlockDevice,
    queue_depth: usize,
    pending_ops: Vec<BlockOp>,
}
```

### 3. **NUMA Awareness**

For multi-socket systems, bind threads to NUMA nodes:
```rust
#[cfg(target_os = "linux")]
fn set_numa_affinity(node: usize) -> Result<()> {
    // Set CPU and memory affinity
}
```

---

## Platform Support

### Linux (Primary Target)
- ✅ Full support
- Uses: `O_DIRECT`, `pread`/`pwrite`, ioctl for device info
- Tested on: Ubuntu 22.04+, RHEL 8+, Arch Linux

### macOS (Secondary)
- ⚠️  Limited support
- Uses: `F_NOCACHE` (not as strict as O_DIRECT)
- Block devices: `/dev/diskN` (requires `diskutil`)

### Windows (Future)
- ❌ Not yet supported
- Would require: `CreateFile` with `FILE_FLAG_NO_BUFFERING`
- Device paths: `\\.\PhysicalDriveN`

---

## Migration Path from rdf-bench

For users migrating from rdf-bench:

```bash
# rdf-bench config:
sd=sd1,lun=/dev/sdb,size=100g
wd=wd1,sd=sd1,xfersize=4k,rdpct=70,seekpct=random
rd=run1,wd=wd1,iorate=1000,elapsed=60,interval=5
```

Equivalent sai3-bench config:
```yaml
target: "block:///dev/sdb"
allow_write: true

workload:
  - op: get
    path: "*"
    weight: 70
  - op: put
    path: "*"
    size_spec: 4096
    weight: 30

duration: 60s
concurrency: 32  # Equivalent to threads
```

---

## Success Criteria

- [ ] Read operations work on real block devices
- [ ] Write operations work with proper safety checks
- [ ] Performance matches or exceeds rdf-bench
- [ ] Clear error messages for all failure modes
- [ ] Comprehensive documentation
- [ ] Automated tests pass on loop devices
- [ ] Manual validation on NVMe device
- [ ] Zero data corruption in validation tests

---

## Next Steps

1. **Review this plan** with maintainers
2. **Prototype Phase 1** (read-only support)
3. **Validate design** with small test device
4. **Iterate on safety checks** (most critical aspect)
5. **Implement full feature set**
6. **Performance tuning**
7. **Production release**

**Target**: v0.7.0 release with block device support in ~6 weeks.
