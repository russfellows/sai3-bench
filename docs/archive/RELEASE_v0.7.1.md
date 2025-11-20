# sai3-bench v0.7.1 Release Notes

**Release Date**: October 31, 2025  
**Release Type**: Enhancement  
**Focus**: I/O Rate Control

## Overview

Version 0.7.1 adds comprehensive I/O rate control capabilities to sai3-bench, allowing users to throttle operation start rates with realistic arrival patterns. This feature closes a key gap in our parity analysis with rdf-bench (increasing coverage from 87% to 92%) and provides production-like workload simulation capabilities.

## What's New

### ⚡ I/O Rate Control

Control the rate at which operations are **started** (not completed) to simulate realistic workload patterns and avoid overwhelming storage systems.

**Quick Start**:
```yaml
io_rate:
  iops: 1000              # Target operations per second
  distribution: exponential  # Poisson arrivals (realistic)
```

**Key Features**:
- **Three distribution types** for different testing scenarios:
  - `exponential` - Realistic Poisson arrivals with natural variance
  - `uniform` - Fixed intervals with drift compensation
  - `deterministic` - Precise timing for exact IOPS targets
  
- **Flexible configuration**:
  - Numeric IOPS targets: `iops: 1000`
  - Unlimited throughput: `iops: max` or `iops: 0`
  - Automatic per-worker division
  
- **Zero overhead when disabled**: Optional wrapper provides passthrough
  
- **Production-grade implementation**:
  - Uses `tokio::time::Interval` for uniform distribution (better drift compensation)
  - Async-aware with proper thread safety
  - ~1ms timer granularity, ±5% accuracy for < 5,000 IOPS

## Usage Examples

### Example 1: Realistic Web Server Load

```yaml
# config: web_simulation.yaml
duration: 300                   # 5 minutes
concurrency: 20                 # 20 concurrent workers
target: "s3://my-bucket/"

io_rate:
  iops: 2000                    # 2000 requests/second
  distribution: exponential     # Realistic Poisson arrivals

workload:
  - op: get
    weight: 80                  # 80% reads
    path: "data/"
  - op: put
    weight: 20                  # 20% writes
    path: "uploads/"
    size: "64KB"
```

Run: `sai3-bench run --config web_simulation.yaml`

### Example 2: Steady-State Capacity Test

```yaml
# config: capacity_test.yaml
duration: 600                   # 10 minutes
concurrency: 50
target: "file:///mnt/storage"

io_rate:
  iops: 10000                   # 10K IOPS target
  distribution: uniform         # Consistent load

workload:
  - op: get
    weight: 70
    path: "testfiles/"
```

### Example 3: Maximum Throughput Baseline

```yaml
# config: max_throughput.yaml
duration: 60
concurrency: 100
target: "s3://benchmark-bucket/"

io_rate:
  iops: max                     # No throttling

workload:
  - op: get
    weight: 100
    path: "dataset/"
```

## Comparison with rdf-bench

For users migrating from rdf-bench, here's how the features compare:

| Feature | rdf-bench | sai3-bench v0.7.1 |
|---------|-----------|-------------------|
| Basic rate control | `iorate=1000` | `iops: 1000, distribution: uniform` |
| Unlimited throughput | `iorate=max` | `iops: max` |
| Poisson arrivals | ❌ Not supported | ✅ `distribution: exponential` |
| Drift compensation | ❌ | ✅ Interval-based (uniform) |
| Multiple distributions | ❌ | ✅ 3 types |

**Gap Closure**: This release increases our rdf-bench feature parity from **87% to 92%**.

## Technical Details

### Architecture

- **New module**: `src/rate_controller.rs` (355 lines)
- **Integration**: `src/workload.rs` - rate controller called before each operation
- **Configuration**: `src/config.rs` - new `IoRateConfig` struct

### Performance

- **Target Range**: Optimized for < 5,000 IOPS
- **Accuracy**: ±5% of target IOPS
- **Granularity**: ~1 millisecond (tokio timer resolution)
- **Overhead**: Zero when disabled, minimal (~μs) when enabled

### Thread Safety

- Uses `tokio::sync::Mutex` for async-aware interior mutability
- `StdRng` for Send-safe random number generation
- Safe Arc cloning across workers

## Documentation

### New Documentation
- **[I/O Rate Control Guide](IO_RATE_CONTROL_GUIDE.md)** - Comprehensive 500+ line guide
  - Usage examples for all distribution types
  - Performance characteristics
  - Troubleshooting section
  - Comparison with rdf-bench
  - Architecture details
  - Best practices

### Updated Documentation
- **README.md** - Added I/O Rate Control to Key Features
- **CHANGELOG.md** - Detailed v0.7.1 changes

## Testing

### New Tests
- **9 integration tests** in `tests/rate_control_tests.rs`:
  - Zero overhead verification
  - Timing accuracy tests (80-120ms for 100ms target)
  - Distribution behavior validation
  - Multi-worker coordination
  - Configuration parsing

- **4 example configs** in `tests/configs/rate-control/`:
  - Exponential, Uniform, Max, Deterministic examples

### Test Results
- **52 total tests passing** (9 new + 43 existing)
- **Zero compiler warnings** in sai3-bench code
- **Clean build** with no regressions

## Migration Guide

### From rdf-bench

If you're using rdf-bench's `iorate=` parameter:

```
# rdf-bench
iorate=1000         

# sai3-bench equivalent
io_rate:
  iops: 1000
  distribution: uniform  # Closest to rdf-bench behavior
```

```
# rdf-bench
iorate=max          

# sai3-bench equivalent
io_rate:
  iops: max
```

### From Previous sai3-bench Versions

No breaking changes - this is a pure enhancement. Existing configs without `io_rate` section continue to work with unlimited throughput (same as before).

## Upgrade Instructions

1. **Update binary**:
   ```bash
   cd sai3-bench
   git pull
   cargo build --release
   ```

2. **Verify installation**:
   ```bash
   ./target/release/sai3-bench --version
   # Should show: sai3-bench 0.7.1
   ```

3. **Run tests** (optional):
   ```bash
   cargo test --lib
   cargo test --test rate_control_tests
   ```

4. **Try example configs**:
   ```bash
   ./target/release/sai3-bench run --config tests/configs/rate-control/rate_1000_exponential.yaml
   ```

## Known Limitations

1. **High IOPS (>5,000)**: Accuracy may degrade due to ~1ms timer granularity
   - **Workaround**: Increase worker count to reduce per-worker IOPS
   
2. **Per-operation rate control**: Not yet supported (coming in future release)
   - Currently, rate applies to all operations combined
   
3. **Time-varying rates**: Static IOPS throughout test duration
   - Ramp-up/ramp-down not yet supported

## Future Enhancements (Roadmap)

Potential improvements for v0.7.2 or v0.8.0:

- Per-operation rate control (different IOPS for GET vs PUT)
- Time-varying rates (ramp up/down during test)
- Adaptive rate control (adjust based on latency)
- Microsecond precision option (spin_sleep for >10K IOPS)
- Rate limit metrics in histograms

## Breaking Changes

**None** - This is a backward-compatible enhancement release.

## Contributors

- Implementation, testing, and documentation by Russ Fellows

## Support

- **Documentation**: [I/O Rate Control Guide](IO_RATE_CONTROL_GUIDE.md)
- **Examples**: `tests/configs/rate-control/*.yaml`
- **Debug logging**: `RUST_LOG=sai3_bench=debug sai3-bench run ...`
- **Test suite**: `cargo test --test rate_control_tests`

## What's Next?

Version 0.7.2 will focus on **sequential access patterns** to further increase rdf-bench parity. This includes:
- Sequential read/write patterns
- Stride configuration
- File offset control
- Sequential vs random access distribution

Stay tuned for the next release!

---

**Full Changelog**: [CHANGELOG.md](CHANGELOG.md)  
**Previous Release**: [v0.7.0](RELEASE_v0.7.0.md)  
**Download**: See GitHub releases
