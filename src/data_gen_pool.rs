// src/data_gen_pool.rs
//
// High-performance data generation pool for sai3-bench
//
// ROLLING-POINTER DESIGN (v0.8.91):
// Generates a single POOL_BLOCK_SIZE (1 MB) buffer via dgen-data, then hands out
// zero-copy Bytes::slice() windows as successive PUT operations consume it.  Only
// when the remaining bytes in the pool are insufficient for the next request (or the
// dedup/compress/seed config changes) is a new buffer generated.
//
// This means:
//  - Generator setup cost paid once per 1 MB of data produced (not once per PUT)
//  - No data copying for any object size ≤ POOL_BLOCK_SIZE
//  - Each returned Bytes holds its own Arc reference into the 1 MB allocation; the
//    allocation stays alive until the last in-flight PUT drops its Bytes handle
//  - Objects larger than POOL_BLOCK_SIZE are generated individually (no pool; the
//    PUT itself dominates, so generation overhead is negligible)

use bytes::Bytes;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

// Hardware detection from s3dlio (always available at runtime)
use s3dlio::hardware;

// dgen-data: high-performance data generation
pub use dgen_data::constants::BLOCK_SIZE as POOL_BLOCK_SIZE;
use dgen_data::generate_data_simple;
use dgen_data::RollingPool;

// ============================================================================
// Global RNG Seed (Per-Agent, Per-Run Uniqueness)
// ============================================================================

static GLOBAL_RNG_SEED: AtomicU64 = AtomicU64::new(0);

/// Set the global RNG seed for this process.
///
/// Combines process ID + optional agent ID + nanosecond timestamp so that each
/// agent instance and each successive run generates distinct data patterns,
/// preventing accidental storage-level deduplication.
pub fn set_global_rng_seed(agent_id: Option<&str>) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let mut hasher = DefaultHasher::new();
    std::process::id().hash(&mut hasher);
    if let Some(id) = agent_id {
        id.hash(&mut hasher);
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    timestamp.hash(&mut hasher);

    let seed = hasher.finish();
    GLOBAL_RNG_SEED.store(seed, Ordering::Relaxed);

    tracing::info!(
        "Data generation RNG seed: {} (agent_id: {:?}, pid: {}, timestamp: {}ns)",
        seed,
        agent_id,
        std::process::id(),
        timestamp
    );
}

fn get_rng_seed() -> u64 {
    let seed = GLOBAL_RNG_SEED.load(Ordering::Relaxed);
    if seed == 0 {
        set_global_rng_seed(None);
        GLOBAL_RNG_SEED.load(Ordering::Relaxed)
    } else {
        seed
    }
}

// ============================================================================
// Rolling Pool — Thread-Local (backed by dgen_data::RollingPool)
// ============================================================================
//
// One pool per OS thread.  The pool tuple holds (RollingPool, last_seed) so
// that a change in the global RNG seed (new agent run) forces a full refill,
// preventing successive runs from reusing the same data pattern.

thread_local! {
    static ROLLING_POOL: RefCell<Option<(RollingPool, u64)>> = const { RefCell::new(None) };
}

/// Generate exactly `size` bytes of synthetic data (the main entry point).
///
/// For `size` ≤ POOL_BLOCK_SIZE (1 MB):
///   Returns a zero-copy `Bytes::slice()` window into the shared 1 MB pool.
///   The backing 1 MB allocation is Arc-shared; each returned slice holds its
///   own Arc reference and keeps the allocation alive until dropped.
///
/// For `size` > POOL_BLOCK_SIZE:
///   Generates a fresh buffer of exactly `size` bytes.  No pool caching (the
///   PUT itself far dominates the generation cost for large objects).
pub fn generate_data_cached(size: usize, dedup: usize, compress: usize) -> Bytes {
    let seed = get_rng_seed();

    // ── Large object fast path ────────────────────────────────────────────
    if size > POOL_BLOCK_SIZE {
        let mut buf = generate_data_simple(size, dedup, compress);
        buf.truncate(size);
        return buf.into_bytes();
    }

    // ── Small/medium object rolling pool path ─────────────────────────────
    ROLLING_POOL.with(|cell| {
        let mut pool_opt = cell.borrow_mut();

        // Replace the pool entirely when the global seed changes (new agent run).
        let seed_changed = pool_opt.as_ref().is_none_or(|(_, s)| *s != seed);
        if seed_changed {
            *pool_opt = Some((RollingPool::new(dedup, compress), seed));
        }

        let (pool, _) = pool_opt.as_mut().unwrap();
        // reconfigure() is a no-op if dedup/compress are unchanged; otherwise
        // it regenerates the 1 MB block before serving the next slice.
        pool.reconfigure(dedup, compress);
        pool.next_slice(size)
    })
}

/// Generate data with optimal settings.
///
/// This is the public API called by workload.rs, prepare/sequential.rs,
/// prepare/parallel.rs, and replay.rs.  Signature is unchanged from v0.8.90;
/// all callers continue to work without modification.
pub fn generate_data_optimized(size: usize, dedup: usize, compress: usize) -> Bytes {
    generate_data_cached(size, dedup, compress)
}

/// Print hardware detection info at startup.
pub fn print_hardware_info() {
    let affinity_cpus = hardware::get_affinity_cpu_count();
    let total_cpus = hardware::total_cpus();
    let numa_available = hardware::is_numa_available();

    tracing::info!("=== Hardware Detection ===");
    tracing::info!("  CPUs available to this process: {}", affinity_cpus);
    tracing::info!("  Total system CPUs: {}", total_cpus);
    if numa_available {
        tracing::info!("  NUMA: Detected (multi-socket system)");
    } else {
        tracing::info!("  NUMA: Not available (UMA/single-socket system)");
    }
    tracing::info!(
        "  Data generation: {} threads (dgen-data rolling pool, {} MB blocks)",
        hardware::recommended_data_gen_threads(None, None),
        POOL_BLOCK_SIZE / (1024 * 1024)
    );
    tracing::info!("========================");
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    // Helper: reset thread-local pool so each test starts clean.
    fn reset_pool() {
        ROLLING_POOL.with(|cell| *cell.borrow_mut() = None);
        // Non-deterministic seed ensures successive tests don't share pools
        // across the reset boundary.
        set_global_rng_seed(None);
    }

    // ── Exact-size correctness ────────────────────────────────────────────

    #[test]
    fn test_small_object_1_byte() {
        reset_pool();
        let data = generate_data_optimized(1, 1, 1);
        assert_eq!(data.len(), 1, "1-byte object must be exactly 1 byte");
    }

    #[test]
    fn test_small_object_64_bytes() {
        reset_pool();
        let data = generate_data_optimized(64, 1, 1);
        assert_eq!(data.len(), 64, "64-byte object must be exactly 64 bytes");
        // Verify the buffer isn't all zeros (data was actually generated)
        assert!(
            data.iter().any(|&b| b != 0),
            "Generated data should not be all zeros"
        );
    }

    #[test]
    fn test_small_object_512_bytes() {
        reset_pool();
        let data = generate_data_optimized(512, 1, 1);
        assert_eq!(data.len(), 512);
    }

    #[test]
    fn test_small_object_4096_bytes() {
        reset_pool();
        let data = generate_data_optimized(4096, 1, 1);
        assert_eq!(data.len(), 4096);
    }

    // ── Zero-copy: pool slices share backing allocation ───────────────────

    #[test]
    fn test_rolling_pool_advances_without_copy() {
        use dgen_data::RollingPool;
        // Test RollingPool directly rather than through generate_data_optimized().
        //
        // Reason: cargo test runs tests on multiple OS threads in parallel.
        // reset_pool() mutates the global GLOBAL_RNG_SEED atomic.  If a parallel
        // test calls reset_pool() between our two generate_data_optimized() calls,
        // the stored seed changes and generate_data_cached() rebuilds the pool,
        // serving slices from a completely different 1 MB allocation.  That breaks
        // the pointer-arithmetic assertion even though the pool itself is correct.
        //
        // Testing RollingPool directly is the right level: we're verifying that
        // dgen-data's zero-copy slicing contract holds, which is the invariant the
        // generate_data_cached() wrapper relies on.
        let mut pool = RollingPool::new(1, 1);
        let a = pool.next_slice(64);
        let b = pool.next_slice(64);

        assert_eq!(a.len(), 64);
        assert_eq!(b.len(), 64);

        // b starts immediately after a in the pool → ptr_b == ptr_a + 64
        let ptr_a = a.as_ptr() as usize;
        let ptr_b = b.as_ptr() as usize;
        assert_eq!(
            ptr_b,
            ptr_a + 64,
            "Second slice must start exactly 64 bytes after the first (zero-copy rolling pointer)"
        );

        // Both slices must lie within one POOL_BLOCK_SIZE window
        assert!(
            ptr_b + 64 <= ptr_a + POOL_BLOCK_SIZE,
            "Both slices must be within the same 1 MB pool block"
        );
    }

    #[test]
    fn test_slices_are_distinct_data() {
        // After rolling, the two 64-byte windows must contain different bytes
        // (because dgen-data generates unique content per block position).
        reset_pool();
        let a = generate_data_optimized(64, 1, 1);
        let b = generate_data_optimized(64, 1, 1);
        assert_ne!(
            a, b,
            "Consecutive pool slices should contain different data"
        );
    }

    // ── Pool exhaustion + refill ──────────────────────────────────────────

    #[test]
    fn test_pool_refill_on_exhaustion() {
        reset_pool();
        // 1 KB chunks, 2 × POOL_BLOCK_SIZE total → forces at least one refill
        let chunk = 1024;
        let count = (2 * POOL_BLOCK_SIZE) / chunk;
        for i in 0..count {
            let data = generate_data_optimized(chunk, 1, 1);
            assert_eq!(
                data.len(),
                chunk,
                "Chunk {} must be exactly {} bytes after pool refill",
                i,
                chunk
            );
        }
    }

    #[test]
    fn test_pool_refill_boundary_alignment() {
        // Request sizes that don't evenly divide POOL_BLOCK_SIZE to exercise
        // the boundary condition where the pool runs out mid-way.
        reset_pool();
        let chunk = 65537; // 64 KiB + 1; doesn't divide 1 MiB evenly
        let count = (3 * POOL_BLOCK_SIZE) / chunk + 1;
        for i in 0..count {
            let data = generate_data_optimized(chunk, 1, 1);
            assert_eq!(
                data.len(),
                chunk,
                "Chunk {} must be exact size at refill boundary",
                i
            );
        }
    }

    // ── Large objects (> POOL_BLOCK_SIZE) ────────────────────────────────

    #[test]
    fn test_large_object_exact_size() {
        reset_pool();
        let size = POOL_BLOCK_SIZE + 1; // 1 byte over the pool threshold
        let data = generate_data_optimized(size, 1, 1);
        assert_eq!(
            data.len(),
            size,
            "Large object must be exactly the requested size (truncate path)"
        );
    }

    #[test]
    fn test_large_object_multi_mb() {
        reset_pool();
        let size = 8 * POOL_BLOCK_SIZE; // 8 MB
        let data = generate_data_optimized(size, 1, 1);
        assert_eq!(data.len(), size);
    }

    // ── Performance smoke tests (run with --release --ignored) ───────────

    #[test]
    #[ignore] // cargo test --release test_small_object_throughput -- --ignored --nocapture
    fn test_small_object_throughput() {
        println!("\n=== Small Object (64-byte) Rolling Pool Throughput ===\n");
        reset_pool();

        let size = 64;
        let count = 10_000_000usize; // 10 million × 64 bytes = ~640 MB
        let total_bytes = count * size;

        let start = Instant::now();
        for _ in 0..count {
            let _data = generate_data_optimized(size, 1, 1);
        }
        let elapsed = start.elapsed();

        println!("  Objects:    {}", count);
        println!("  Total data: {} MB", total_bytes / (1024 * 1024));
        println!("  Time:       {:.3}s", elapsed.as_secs_f64());
        println!(
            "  Rate:       {:.0} ops/s",
            count as f64 / elapsed.as_secs_f64()
        );
        println!(
            "  Throughput: {:.2} GB/s (data volume)",
            total_bytes as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0 * 1024.0)
        );
        println!("  Note: Most ops are Arc clone + slice (no generation)");
    }

    #[test]
    #[ignore] // cargo test --release test_large_object_throughput -- --ignored --nocapture
    fn test_large_object_throughput() {
        println!("\n=== Large Object (100 MB) Throughput ===\n");
        reset_pool();

        let size = 100 * 1024 * 1024;
        let count = 10usize;

        let start = Instant::now();
        for _ in 0..count {
            let _data = generate_data_optimized(size, 1, 1);
        }
        let elapsed = start.elapsed();
        let total_bytes = count * size;

        println!("  Objects:    {}", count);
        println!("  Total data: {} MB", total_bytes / (1024 * 1024));
        println!("  Time:       {:.3}s", elapsed.as_secs_f64());
        println!(
            "  Throughput: {:.2} GB/s",
            total_bytes as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0 * 1024.0)
        );
    }
}
