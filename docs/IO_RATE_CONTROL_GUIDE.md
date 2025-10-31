# I/O Rate Control Guide (v0.7.1)

## Overview

The I/O rate control feature allows you to throttle the rate at which operations are **started** (not completed) to simulate realistic workload patterns and avoid overwhelming storage systems. This feature is inspired by rdf-bench's `iorate=` parameter and provides equivalent functionality with additional distribution options.

**Key Concept**: Rate control throttles operation **starts**, not completions. This means:
- Operations are delayed before they begin
- Actual IOPS achieved depends on operation latency
- High-latency operations may result in lower achieved IOPS than target

## Configuration

Add the `io_rate` section to your YAML configuration:

```yaml
# Basic rate control
io_rate:
  iops: 1000                    # Target IOPS (operations per second)
  distribution: exponential     # Distribution type

# Other valid IOPS values
io_rate:
  iops: max                     # Unlimited throughput (no throttling)
  iops: 0                       # Same as "max"
  iops: 5000                    # Fixed numeric target
```

### Distribution Types

#### 1. Exponential Distribution (Recommended for Realistic Testing)

```yaml
io_rate:
  iops: 1000
  distribution: exponential
```

**Characteristics**:
- Simulates Poisson arrival process (realistic for many workloads)
- Inter-arrival times follow exponential distribution
- Average rate matches target IOPS, but with natural variability
- Models real-world scenarios with random client requests

**Use when**: Testing realistic production-like workload patterns

**Example**: Web servers, database query patterns, user-driven applications

#### 2. Uniform Distribution (Fixed Intervals)

```yaml
io_rate:
  iops: 5000
  distribution: uniform
```

**Characteristics**:
- Fixed inter-arrival time: `1,000,000 microseconds / (iops / workers)`
- Uses `tokio::time::Interval` for drift compensation
- Most predictable and consistent timing
- Minimal variance in operation starts

**Use when**: Testing steady-state performance, benchmarking with consistent load

**Example**: Synthetic benchmarks, capacity testing, baseline measurements

#### 3. Deterministic Distribution (Precise Timing)

```yaml
io_rate:
  iops: 1000
  distribution: deterministic
```

**Characteristics**:
- Calculates exact delay to maintain precise rate over time
- Compensates for timing drift by tracking elapsed time
- Most accurate for hitting exact IOPS targets
- Uses `tokio::sleep()` with drift calculation

**Use when**: Need exact IOPS target over the test duration

**Example**: Compliance testing, SLA validation, precise rate requirements

## Complete Example Configurations

### Example 1: Realistic Web Server Simulation

```yaml
# config: realistic_web_load.yaml
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

### Example 2: Steady-State Capacity Test

```yaml
# config: capacity_test.yaml
duration: 600                   # 10 minutes
concurrency: 50                 # 50 workers
target: "file:///mnt/storage"

io_rate:
  iops: 10000                   # 10K IOPS target
  distribution: uniform         # Consistent load

workload:
  - op: get
    weight: 70
    path: "testfiles/"
  - op: put
    weight: 30
    path: "testfiles/"
    size: "1MB"
```

### Example 3: Maximum Throughput (No Rate Limiting)

```yaml
# config: max_throughput.yaml
duration: 60                    # 1 minute burst test
concurrency: 100                # Maximum concurrency
target: "s3://benchmark-bucket/"

io_rate:
  iops: max                     # No throttling
  distribution: uniform         # (distribution ignored when iops=max)

workload:
  - op: get
    weight: 100
    path: "dataset/"
```

## Performance Characteristics

### Accuracy

- **Target Range**: Optimized for < 5,000 IOPS per workload
- **Typical Accuracy**: ±5% of target IOPS
- **Granularity**: ~1 millisecond (tokio timer resolution)

### Per-Worker IOPS Calculation

The target IOPS is divided equally among all workers:

```
per_worker_iops = target_iops / concurrency
inter_arrival_time = 1,000,000 microseconds / per_worker_iops
```

**Example**: 1000 IOPS with 10 workers
- Each worker: 100 ops/sec
- Inter-arrival: 10,000 microseconds (10ms) between operations

### Overhead

- **Disabled (iops: max)**: Zero overhead - direct passthrough
- **Enabled**: Minimal overhead from async wait
  - Exponential: ~1 RNG sample + sleep per operation
  - Uniform: Interval tick (drift-compensated)
  - Deterministic: Atomic counter + arithmetic + sleep

## Comparison with rdf-bench

| Feature | rdf-bench (`iorate=`) | sai3-bench (`io_rate`) |
|---------|----------------------|------------------------|
| **Basic rate control** | ✅ `iorate=1000` | ✅ `iops: 1000` |
| **Unlimited throughput** | ✅ `iorate=max` | ✅ `iops: max` |
| **Distribution types** | ❌ Fixed only | ✅ Exponential, Uniform, Deterministic |
| **Poisson arrivals** | ❌ | ✅ Exponential distribution |
| **Drift compensation** | ❌ | ✅ Uniform uses tokio::time::Interval |
| **Per-worker division** | ✅ | ✅ |
| **Async/await friendly** | N/A (Java) | ✅ Native Rust async |
| **Zero overhead when disabled** | ✅ | ✅ |

**Migration from rdf-bench**:
```
rdf-bench:  iorate=1000        →  sai3-bench:  iops: 1000, distribution: uniform
rdf-bench:  iorate=max         →  sai3-bench:  iops: max
rdf-bench:  (no equivalent)    →  sai3-bench:  iops: 1000, distribution: exponential
```

## Advanced Usage

### Combining with Per-Operation Concurrency

Rate control works with per-operation concurrency limits:

```yaml
concurrency: 20                 # Global worker count
io_rate:
  iops: 2000                    # Rate applies across ALL operations
  distribution: exponential

workload:
  - op: get
    weight: 80
    concurrency: 15             # Max 15 concurrent GETs
  - op: put
    weight: 20
    concurrency: 5              # Max 5 concurrent PUTs
```

**Execution flow**:
1. Worker wakes up
2. **Rate controller delays** (if needed)
3. Operation type selected (GET 80%, PUT 20%)
4. Operation-specific semaphore acquired
5. Operation executes

### High IOPS Scenarios (>5,000 IOPS)

For very high IOPS targets, consider:

1. **Increase worker count**: More workers = less per-worker load
   ```yaml
   concurrency: 100            # 10K IOPS / 100 = 100 IOPS per worker
   io_rate:
     iops: 10000
   ```

2. **Use uniform distribution**: Most efficient for high rates
   ```yaml
   io_rate:
     iops: 10000
     distribution: uniform     # Interval-based, minimal overhead
   ```

3. **Verify actual achieved rate**: Monitor metrics to ensure system can handle target

### Testing Rate Control Accuracy

Quick validation test:

```yaml
# test_rate_accuracy.yaml
duration: 60                    # 1 minute
concurrency: 10
target: "file:///tmp/test"

io_rate:
  iops: 1000                    # Should achieve ~1000 ops total
  distribution: uniform

workload:
  - op: get
    weight: 100
    path: "small_files/"        # Use small, fast files
```

Expected result: ~1000 operations in 60 seconds (check summary output)

## Troubleshooting

### Problem: Achieved IOPS Much Lower Than Target

**Symptoms**: 
```
Target: 5000 IOPS
Achieved: 500 IOPS
```

**Causes**:
1. **High operation latency**: Operations take longer than inter-arrival time
2. **Insufficient concurrency**: Not enough workers to sustain rate
3. **Storage system bottleneck**: Backend can't handle the load

**Solutions**:
- Increase `concurrency` (more parallel workers)
- Use faster storage or smaller objects
- Reduce target IOPS to match system capability
- Check if operation-specific `concurrency` limits are too low

### Problem: Timing Variance Too High

**Symptoms**: Large fluctuations in achieved rate

**Causes**:
1. Using exponential distribution (natural variance)
2. System scheduling jitter
3. Timer granularity (~1ms)

**Solutions**:
- Switch to `uniform` or `deterministic` distribution
- Increase target IOPS (higher rate = smaller variance percentage)
- Ensure system isn't overloaded (check CPU, I/O wait)

### Problem: No Rate Limiting Effect

**Symptoms**: Operations run at maximum speed despite `iops` setting

**Causes**:
1. `iops: max` or `iops: 0` configured
2. `io_rate` section missing or commented out
3. Target IOPS exceeds maximum achievable rate

**Solutions**:
- Verify YAML configuration is correct
- Check for typos in `io_rate` section
- Use lower target IOPS to see throttling effect
- Enable debug logging: `RUST_LOG=sai3_bench=debug`

### Problem: Workers Hanging or Slow Startup

**Symptoms**: Workers take long time to start or seem stuck

**Causes**:
1. Very low IOPS with few workers = very long inter-arrival time
2. Tokio runtime not initialized properly

**Solutions**:
- Check per-worker IOPS: `target / concurrency`
  - Example: 10 IOPS / 1 worker = 100ms between operations (expected)
- Increase worker count if inter-arrival time is too long
- Verify async runtime with simple test first

## Implementation Details

### Architecture

```
┌─────────────────────────────────────────┐
│         Config (io_rate)                │
│  ┌────────────────────────────────┐    │
│  │ iops: 1000                     │    │
│  │ distribution: exponential      │    │
│  └────────────────────────────────┘    │
└─────────────────┬───────────────────────┘
                  │
                  ▼
┌─────────────────────────────────────────┐
│    OptionalRateController (Arc)         │
│  ┌────────────────────────────────┐    │
│  │ is_enabled() → bool            │    │
│  │ wait() → async                 │    │
│  └────────────────────────────────┘    │
└─────────────────┬───────────────────────┘
                  │ (cloned to each worker)
                  │
    ┌─────────────┼─────────────┬─────────┐
    ▼             ▼             ▼         ▼
┌────────┐  ┌────────┐    ┌────────┐  ...
│Worker 1│  │Worker 2│    │Worker N│
└────────┘  └────────┘    └────────┘
    │             │             │
    │ Loop:       │             │
    │ 1. rate_controller.wait().await  (THROTTLE)
    │ 2. Select operation type
    │ 3. Acquire operation semaphore
    │ 4. Execute operation
    │ 5. Record metrics
    └─────────────┴─────────────┘
```

### Timing Mechanisms

| Distribution | Mechanism | Drift Compensation |
|-------------|-----------|-------------------|
| Exponential | `tokio::sleep()` + RNG | No (variance is feature) |
| Uniform | `tokio::time::Interval` | ✅ Yes (automatic) |
| Deterministic | `tokio::sleep()` + tracking | ✅ Yes (manual) |

### Thread Safety

- **RateController**: Uses `tokio::sync::Mutex` for async-safe interior mutability
- **RNG**: `StdRng` with `Send` trait (required for tokio::spawn)
- **Interval**: Per-controller instance, protected by Mutex
- **Arc sharing**: Safe to clone across workers (all fields are thread-safe)

## Examples Directory

Sample configurations are provided in `tests/configs/rate-control/`:

```
tests/configs/rate-control/
├── rate_1000_exponential.yaml    # Realistic 1K IOPS with Poisson arrivals
├── rate_5000_uniform.yaml        # Steady 5K IOPS with fixed intervals
├── rate_max.yaml                 # Maximum throughput (no limiting)
└── rate_deterministic.yaml       # Precise 1K IOPS with drift compensation
```

Run these examples:
```bash
# Realistic load test
sai3-bench run --config tests/configs/rate-control/rate_1000_exponential.yaml

# Steady-state test
sai3-bench run --config tests/configs/rate-control/rate_5000_uniform.yaml

# Maximum throughput baseline
sai3-bench run --config tests/configs/rate-control/rate_max.yaml
```

## Best Practices

1. **Start with uniform distribution** for initial testing and capacity planning
2. **Use exponential distribution** for realistic production simulation
3. **Match concurrency to target IOPS**: Aim for 10-100 ops/sec per worker
4. **Validate with small tests first**: 60s test before long runs
5. **Monitor actual achieved rate**: Check summary output vs target
6. **Consider operation latency**: Target IOPS must be achievable given latencies
7. **Use `iops: max` for baseline**: Measure maximum throughput without throttling

## Future Enhancements (Roadmap)

Potential improvements for future versions:

- **Per-operation rate control**: Different IOPS for GET vs PUT
- **Time-varying rates**: Ramp up/down during test
- **Adaptive rate control**: Adjust based on latency feedback
- **Microsecond precision**: spin_sleep option for >10K IOPS
- **Rate limit metrics**: Track throttling overhead in histograms

## References

- **rdf-bench iorate parameter**: Original inspiration for this feature
- **Poisson Process**: https://en.wikipedia.org/wiki/Poisson_point_process
- **tokio::time::Interval**: https://docs.rs/tokio/latest/tokio/time/struct.Interval.html
- **Exponential Distribution**: https://en.wikipedia.org/wiki/Exponential_distribution

## Support

For issues or questions:
1. Check this guide's Troubleshooting section
2. Review example configurations in `tests/configs/rate-control/`
3. Enable debug logging: `RUST_LOG=sai3_bench=debug sai3-bench run ...`
4. Check test suite: `cargo test --test rate_control_tests`

---

**Version**: sai3-bench v0.7.1  
**Last Updated**: October 31, 2025  
**Feature Status**: Stable
