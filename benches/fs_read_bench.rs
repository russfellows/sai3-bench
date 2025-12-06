// benches/fs_read_bench.rs
// Run with: cargo run --release --bin fs_read_bench -- <args>
use std::{fs, path::PathBuf, time::Instant};
use anyhow::{Result, Context};
use clap::Parser;

// Use sai3-bench's workload module for proper store creation
use sai3_bench::workload::create_store_for_uri_with_config;
use sai3_bench::config::PageCacheMode as ConfigPageCacheMode;

#[derive(Parser, Debug)]
#[command(name = "fs_read_bench")]
struct Args {
    /// Directory of files to read (will generate files if empty/non-existent)
    #[arg(long, default_value = "./_fsbench_data")]
    dir: PathBuf,

    /// Number of files to generate/read if dir was empty
    #[arg(long, default_value_t = 32)]
    num_files: usize,

    /// Size of each file to generate (MiB)
    #[arg(long, default_value_t = 8)]
    file_size_mib: usize,

    /// Fixed block size for reads (bytes); if 0, do whole-file reads
    #[arg(long, default_value_t = 262_144)] // 256 KiB
    block_size: usize,

    /// Use O_DIRECT (requires Direct I/O path to be wired)
    #[arg(long, default_value_t = false)]
    direct_io: bool,

    /// Page cache mode: auto|sequential|random|dontneed|normal
    #[arg(long, default_value = "auto")]
    page_cache: String,

    /// Number of total passes over the dataset
    #[arg(long, default_value_t = 3)]
    passes: usize,

    /// Shuffle file order each pass
    #[arg(long, default_value_t = true)]
    shuffle: bool,
}

fn parse_page_cache_mode(s: &str) -> ConfigPageCacheMode {
    match s.to_ascii_lowercase().as_str() {
        "sequential" => ConfigPageCacheMode::Sequential,
        "random"     => ConfigPageCacheMode::Random,
        "dontneed"   => ConfigPageCacheMode::DontNeed,
        "normal"     => ConfigPageCacheMode::Normal,
        _            => ConfigPageCacheMode::Auto,
    }
}

// Read minor/major page faults from /proc/self/stat (fields #10 and #12, 1-based)
fn read_faults() -> (u64, u64) {
    // ref: procfs(5): /proc/[pid]/stat
    if let Ok(s) = fs::read_to_string("/proc/self/stat") {
        let parts: Vec<&str> = s.split_whitespace().collect();
        if parts.len() > 12 {
            let minflt = parts[9].parse::<u64>().unwrap_or(0);
            let majflt = parts[11].parse::<u64>().unwrap_or(0);
            return (minflt, majflt);
        }
    }
    (0, 0)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Prepare dataset directory
    fs::create_dir_all(&args.dir).ok();
    let mut files: Vec<PathBuf> = fs::read_dir(&args.dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|d| d.path())).collect())
        .unwrap_or_default();

    // Generate files if dir empty
    if files.is_empty() {
        println!("Generating {} files × {} MiB in {}", args.num_files, args.file_size_mib, args.dir.display());
        let buf = vec![0u8; args.file_size_mib * 1024 * 1024];
        for i in 0..args.num_files {
            let p = args.dir.join(format!("f{:06}.bin", i));
            fs::write(&p, &buf)?;
            files.push(p);
        }
    }

    // Determine URI scheme and create appropriate store
    let scheme = if args.direct_io { "direct://" } else { "file://" };
    let page_cache_mode = parse_page_cache_mode(&args.page_cache);
    
    // For direct:// URIs, we use store_for_uri() instead of FileSystemObjectStore
    // to get proper O_DIRECT support. Page cache modes don't apply to O_DIRECT.
    let page_cache_config = if args.direct_io {
        None  // O_DIRECT bypasses page cache entirely
    } else {
        Some(page_cache_mode)
    };
    
    // Report configuration
    let cache_desc = if args.direct_io { 
        "N/A (O_DIRECT)".to_string()
    } else { 
        format!("{:?}", page_cache_mode)
    };
    println!("cfg: block_size={} bytes | scheme={} | page_cache={}",
        args.block_size,
        scheme,
        cache_desc,
    );

    let use_blocks = args.block_size > 0;
    let total_bytes: u64 = (args.file_size_mib as u64) * 1024 * 1024 * (files.len() as u64) * (args.passes as u64);
    let mut latencies: Vec<f64> = Vec::with_capacity(files.len() * args.passes);
    let (min0, maj0) = read_faults();

    let t0 = Instant::now();
    use rand::rng;
    let mut rng = rng();

    for pass in 0..args.passes {
        if args.shuffle {
            use rand::seq::SliceRandom;
            files.shuffle(&mut rng);
        }
        let pass_start = Instant::now();

        for p in &files {
            // Create URI with correct scheme (file:// or direct://)
            let uri = format!("{}{}", scheme, p.display());
            
            // Create store for this URI with page cache configuration
            // For direct:// URIs, page_cache_config is None (O_DIRECT bypasses cache)
            let store = create_store_for_uri_with_config(&uri, None, page_cache_config)?;

            let t_file = Instant::now();

            if use_blocks {
                // Fixed-size reads in a loop (vdbench-like)
                let meta = fs::metadata(p)?;
                let mut off: u64 = 0;
                while off < meta.len() {
                    let len = std::cmp::min(args.block_size as u64, meta.len() - off);
                    let _chunk = store.get_range(&uri, off, Some(len)).await
                        .with_context(|| format!("get_range({}, off={}, len={})", uri, off, len))?;
                    off += len;
                }
            } else {
                // Whole-file read
                let _data = store.get(&uri).await
                    .with_context(|| format!("get({})", uri))?;
            }

            let dt = t_file.elapsed().as_secs_f64();
            latencies.push(dt);
        }

        println!("pass {} time: {:.3}s", pass + 1, pass_start.elapsed().as_secs_f64());
    }

    let dt = t0.elapsed();

    // Fault deltas
    let (min1, maj1) = read_faults();
    let dmin = min1.saturating_sub(min0);
    let dmaj = maj1.saturating_sub(maj0);

    // Stats
    latencies.sort_by(|a, b| a.total_cmp(b));
    let p50 = latencies[latencies.len() * 50 / 100];
    let p95 = latencies[latencies.len() * 95 / 100];
    let p99 = latencies[latencies.len() * 99 / 100];

    let gib = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    let throughput_gib_s = gib / dt.as_secs_f64();

    println!("\n=== fs_read_bench results ===");
    println!("files: {} | passes: {}", files.len(), args.passes);
    println!("total: {:.3} GiB in {:.3} s -> {:.2} GiB/s", gib, dt.as_secs_f64(), throughput_gib_s);
    println!("latency per file: p50={:.4}s  p95={:.4}s  p99={:.4}s", p50, p95, p99);
    println!("page faults (delta): minor={}  major={}", dmin, dmaj);

    Ok(())
}
