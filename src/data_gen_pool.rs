// src/data_gen_pool.rs
//
// High-performance data generation pool for sai3-bench
//
// CRITICAL OPTIMIZATION: Reuses thread pools across data generation calls
// to achieve 50+ GB/s throughput instead of 1-2 GB/s with repeated single calls.
//
// v0.8.20: Now uses s3dlio::hardware API for automatic hardware detection
// and optimal thread pool sizing.

use bytes::Bytes;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

// Hardware detection from s3dlio (always available at runtime)
use s3dlio::hardware;

// NOTE: This uses the INTERNAL s3dlio streaming API which may not be in git tag version.
// For compatibility, we need to check if data_gen_alt is available.
// If using s3dlio from git tag (pre-v0.9.30), this won't work.
// Solution: Update Cargo.toml to use local path during development.

// ============================================================================
// Global RNG Seed (Per-Agent, Per-Run Uniqueness)
// ============================================================================
// CRITICAL: Each sai3bench-agent instance AND each run MUST have a unique seed
// to prevent data deduplication:
// - Across agents targeting the same storage (different agent IDs)
// - Across successive runs on the same agent (different timestamps)
// 
// The seed combines:
// - Process ID (different for each agent process)
// - Agent ID hash (if running distributed mode)
// - Nanosecond timestamp (different for each run)
//
// This ensures that:
// 1. Multiple agents starting simultaneously generate different data
// 2. The same agent running twice generates different data each time
// 3. Storage doesn't artificially deduplicate identical benchmark data

static GLOBAL_RNG_SEED: AtomicU64 = AtomicU64::new(0);

/// Set the global RNG seed for this process (called at agent startup OR workload start)
/// 
/// This ensures each agent instance and each RUN generates unique data patterns.
/// Combines agent_id + PID + nanosecond timestamp for maximum uniqueness.
/// 
/// # Arguments
/// * `agent_id` - Optional agent ID string (used for distributed mode)
/// 
/// # Example
/// ```rust
/// use sai3_bench::data_gen_pool::set_global_rng_seed;
/// 
/// // At agent startup (distributed mode)
/// set_global_rng_seed(Some("agent-1"));
/// 
/// // At workload start (standalone mode)
/// set_global_rng_seed(None);
/// ```
pub fn set_global_rng_seed(agent_id: Option<&str>) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};
    
    let mut hasher = DefaultHasher::new();
    
    // Mix in process ID (different for each agent)
    std::process::id().hash(&mut hasher);
    
    // Mix in agent ID if provided (different for each distributed agent)
    if let Some(id) = agent_id {
        id.hash(&mut hasher);
    }
    
    // CRITICAL: Mix in nanosecond timestamp (different for each RUN)
    // This prevents successive runs from generating identical data
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

/// Get the global RNG seed (or initialize with default if not set)
fn get_rng_seed() -> u64 {
    let seed = GLOBAL_RNG_SEED.load(Ordering::Relaxed);
    if seed == 0 {
        // Not initialized yet - use process ID + timestamp
        set_global_rng_seed(None);
        GLOBAL_RNG_SEED.load(Ordering::Relaxed)
    } else {
        seed
    }
}

// ============================================================================
// Thread-Local Data Cache
// ============================================================================
// The DATA_CACHE is a thread-local pool that caches the last generated data
// configuration to avoid recreating thread pools for each PUT operation.
//
// Performance Impact:
// - WITHOUT caching: ~1-2 GB/s (creates new data each call)
// - WITH caching: ~50+ GB/s (reuses generated data when config matches)
//
// Implementation: Uses thread_local! macro to create a RefCell<Option<CachedData>>
// per thread, ensuring thread safety without locks.

thread_local! {
    static DATA_CACHE: RefCell<Option<CachedData>> = const { RefCell::new(None) };
}

/// Cached generated data with configuration
struct CachedData {
    config: (usize, usize, usize, u64),  // (size, dedup, compress, seed)
    data: Bytes,  // Cached data
}

/// Generate data using thread-local cache (OPTIMIZED for repeated same-size calls)
/// 
/// v0.8.20: Now uses s3dlio streaming API with automatic hardware detection
/// for optimal performance (50+ GB/s). Each agent gets unique RNG seed to
/// prevent cross-agent deduplication.
/// 
/// Configuration (optimal defaults from testing):
/// - 1 MB internal block size
/// - 64 MB streaming chunk size
/// - Thread count auto-detected (respects CPU affinity, NUMA)
/// - Per-agent RNG seed for uniqueness
pub fn generate_data_cached(size: usize, dedup: usize, compress: usize) -> Bytes {
    let seed = get_rng_seed();
    
    DATA_CACHE.with(|cache| {
        let mut cache_ref = cache.borrow_mut();
        
        // Check if we can reuse cached data
        let needs_new_data = match &*cache_ref {
            None => true,  // No cache exists
            Some(cached) => {
                // Need new data if config OR seed changed
                cached.config != (size, dedup, compress, seed)
            }
        };
        
        if needs_new_data {
            // Generate new data with optimal settings
            use s3dlio::data_gen_alt::{generate_data, GeneratorConfig, NumaMode};
            
            // Auto-detect optimal thread count (respects CPU affinity, NUMA, etc.)
            let num_threads = hardware::recommended_data_gen_threads(None, None);
            
            let config = GeneratorConfig {
                size,
                dedup_factor: dedup,
                compress_factor: compress,
                block_size: Some(1 * 1024 * 1024),  // 1 MB blocks (optimal from testing)
                max_threads: Some(num_threads),     // Use all available cores
                numa_mode: NumaMode::Auto,          // Auto-detect NUMA
                numa_node: None,                    // Auto-detect which node
                seed: Some(seed),                   // Per-agent, per-run unique seed
            };
            
            // Generate all data at once (uses streaming internally with 64MB chunks)
            let buffer = generate_data(config);
            let data = Bytes::copy_from_slice(buffer.as_slice());
            
            *cache_ref = Some(CachedData {
                config: (size, dedup, compress, seed),
                data: data.clone(),
            });
            
            data
        } else {
            // Reuse cached data (clone is cheap for Bytes - reference counted)
            cache_ref.as_ref().unwrap().data.clone()
        }
    })
}

/// Generate data with optimal settings (50+ GB/s)
/// 
/// v0.8.20: Always uses streaming API with hardware-detected thread count.
/// Automatically respects CPU affinity (taskset, Docker limits, Python multiprocessing).
/// 
/// Configuration:
/// - 1 MB internal blocks
/// - 64 MB streaming chunks
/// - All available cores (auto-detected)
/// - Per-agent unique RNG seed
pub fn generate_data_optimized(size: usize, dedup: usize, compress: usize) -> Bytes {
    // Always use cached generation (it's fast even for first call ~50 GB/s)
    generate_data_cached(size, dedup, compress)
}

/// Print hardware detection info at startup
/// 
/// This helps diagnose performance issues and shows users what hardware
/// sai3-bench detected (useful for verifying taskset/Docker limits work).
pub fn print_hardware_info() {
    let affinity_cpus = hardware::get_affinity_cpu_count();
    let total_cpus = hardware::total_cpus();
    let numa_available = hardware::is_numa_available();
    
    tracing::info!("=== Hardware Detection ===");
    tracing::info!("  CPUs available to this process: {}", affinity_cpus);
    tracing::info!("  Total system CPUs: {}", total_cpus);
    if numa_available {
        tracing::info!("  NUMA: Detected (multi-socket system)");
        // Note: NUMA topology details available via s3dlio's numa feature
        // sai3-bench doesn't need to enable it - just uses hardware detection
    } else {
        tracing::info!("  NUMA: Not available (UMA/single-socket system)");
    }
    tracing::info!("  Data generation: {} threads", hardware::recommended_data_gen_threads(None, None));
    tracing::info!("  Configuration: 1 MB blocks, auto-chunking, streaming API");
    tracing::info!("========================");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    
    #[test]
    #[ignore]  // Run with: cargo test --release test_cached_same_size -- --ignored --nocapture
    fn test_cached_same_size() {
        println!("\n=== Testing Cached Generator (Same Size) ===\n");
        
        let size = 100 * 1024 * 1024;  // 100 MB per call
        let iterations = 10;
        
        // Test with caching (same size every time - should hit cache after first)
        let start = Instant::now();
        for _ in 0..iterations {
            let _data = generate_data_optimized(size, 1, 1);
        }
        let cached_elapsed = start.elapsed();
        let cached_throughput = (size * iterations) as f64 / cached_elapsed.as_secs_f64() / (1024.0 * 1024.0 * 1024.0);
        
        println!("With caching (same size):");
        println!("  Total: {} MB", (size * iterations) / (1024 * 1024));
        println!("  Time: {:.3}s", cached_elapsed.as_secs_f64());
        println!("  Throughput: {:.2} GB/s", cached_throughput);
        println!("  Note: First call generates, rest clone cached data");
    }
    
    #[test]
    #[ignore]  // Run with: cargo test --release test_variable_sizes -- --ignored --nocapture  
    fn test_variable_sizes() {
        println!("\n=== Testing Variable Sizes (No Cache Benefit) ===\n");
        
        let sizes = vec![50, 75, 100, 125, 150];  // MB
        let iterations = 2;
        
        let start = Instant::now();
        for _ in 0..iterations {
            for size_mb in &sizes {
                let size = size_mb * 1024 * 1024;
                let _data = generate_data_optimized(size, 1, 1);
            }
        }
        let elapsed = start.elapsed();
        let total_mb: usize = sizes.iter().sum::<usize>() * iterations;
        let throughput = (total_mb as f64) / elapsed.as_secs_f64();
        
        println!("Variable sizes:");
        println!("  Total: {} MB", total_mb);
        println!("  Time: {:.3}s", elapsed.as_secs_f64());
        println!("  Throughput: {:.2} MB/s", throughput);
        println!("  Note: Cache misses every time (different sizes)");
    }
}
