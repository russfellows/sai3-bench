//
// Copyright, 2025: Signal65/Futurum
//

// -----------------------------------------------------------------------------
// warp‑test ‑ lightweight S3 performance tester & utility CLI built on s3dlio
// -----------------------------------------------------------------------------

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use futures::{stream::FuturesUnordered, StreamExt};
use hdrhistogram::Histogram;
use regex::{Regex, escape};
use s3_bench::config::Config;
use s3_bench::workload;
use serde_yaml;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Builder as RtBuilder;
use tokio::sync::Semaphore;
use tracing::info;
use url::Url;

// TODO: Remove legacy s3_utils imports as we migrate operations
use s3dlio::s3_utils::{
    delete_objects, get_object, list_objects, parse_s3_uri, put_object_async, stat_object_uri,
};

// -----------------------------------------------------------------------------
// Histogram / metrics support
// -----------------------------------------------------------------------------
const NUM_BUCKETS: usize = 9;
const BUCKET_LABELS: [&str; NUM_BUCKETS] = [
    "zero", "1B-8KiB", "8KiB-64KiB", "64KiB-512KiB",
    "512KiB-4MiB", "4MiB-32MiB", "32MiB-256MiB", "256MiB-2GiB", ">2GiB",
];

fn bucket_index(nbytes: usize) -> usize {
    if nbytes == 0 {
        0
    } else if nbytes <= 8 * 1024 {
        1
    } else if nbytes <= 64 * 1024 {
        2
    } else if nbytes <= 512 * 1024 {
        3
    } else if nbytes <= 4 * 1024 * 1024 {
        4
    } else if nbytes <= 32 * 1024 * 1024 {
        5
    } else if nbytes <= 256 * 1024 * 1024 {
        6
    } else if nbytes <= 2 * 1024 * 1024 * 1024 {
        7
    } else {
        8
    }
}

#[derive(Clone)]
struct OpHists {
    buckets: Arc<Vec<Mutex<Histogram<u64>>>>,
}

impl OpHists {
    fn new() -> Self {
        let mut v = Vec::with_capacity(NUM_BUCKETS);
        for _ in 0..NUM_BUCKETS {
            v.push(Mutex::new(
                Histogram::<u64>::new_with_bounds(1, 3_600_000_000, 3)
                    .expect("failed to allocate histogram"),
            ));
        }
        OpHists { buckets: Arc::new(v) }
    }

    fn record(&self, bucket: usize, duration: Duration) {
        let micros = duration.as_micros() as u64;
        let mut hist = self.buckets[bucket].lock().unwrap();
        let _ = hist.record(micros);
    }

    fn print_summary(&self, op: &str) {
        println!("\n{} latency (µs):", op);
        for (i, m) in self.buckets.iter().enumerate() {
            let hist = m.lock().unwrap();
            let count = hist.len();
            if count == 0 {
                continue;
            }
            let p50 = hist.value_at_quantile(0.50);
            let p95 = hist.value_at_quantile(0.95);
            let p99 = hist.value_at_quantile(0.99);
            let max = hist.max();
            println!(
                "  [{:>10}] count={:<6} p50={:<8} p95={:<8} p99={:<8} max={:<8}",
                BUCKET_LABELS[i], count, p50, p95, p99, max
            );
        }
    }
}

// -----------------------------------------------------------------------------
// CLI definition
// -----------------------------------------------------------------------------
#[derive(Parser)]
#[command(name = "warp-test", version, about = "Light‑weight S3 tester & utility built on s3dlio")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    
    /// Verbose output (-v for info, -vv for debug)
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Commands {
    /// Verify bucket+prefix reachability
    Health {
        #[arg(long, conflicts_with = "bucket")]
        uri: Option<String>,
        #[arg(long, requires = "prefix")]
        bucket: Option<String>,
        #[arg(long, requires = "bucket", default_value = "")]
        prefix: String,
    },
    /// List objects (supports basename glob)
    List {
        #[arg(long)]
        uri: String,
    },
    /// Stat (HEAD) one object
    Stat {
        #[arg(long)]
        uri: String,
    },
    /// Get objects (prefix, glob, or single)
    Get {
        #[arg(long)]
        uri: String,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Delete objects (prefix, glob, or single)
    Delete {
        #[arg(long)]
        uri: String,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Put random-data objects
    Put {
        #[arg(long, conflicts_with = "bucket")]
        uri: Option<String>,
        #[arg(long, requires = "prefix")]
        bucket: Option<String>,
        #[arg(long, requires = "bucket", default_value = "bench/")]
        prefix: String,
        #[arg(long, default_value_t = 1024)]
        object_size: usize,
        #[arg(long, default_value_t = 1)]
        objects: usize,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },
    /// Run workload from config file
    Run {
        #[arg(long)]
        config: String,
    },
}

// -----------------------------------------------------------------------------
// main
// -----------------------------------------------------------------------------
fn main() -> Result<()> {
    let cli = Cli::parse();
    
    // Initialize logging based on verbosity level
    let log_level = match cli.verbose {
        0 => "warn",  // Default: only warnings and errors
        1 => "info",  // -v: info level and above
        _ => "debug", // -vv or more: debug level and above
    };
    
    // Initialize tracing subscriber with the appropriate level
    use tracing_subscriber::{fmt, EnvFilter};
    fmt()
        .with_env_filter(EnvFilter::new(format!("s3_bench={}", log_level)))
        .init();
    
    match cli.command {
        Commands::Health { uri, bucket, prefix } => {
            let (b, p) = parse_s3(&uri, bucket, prefix)?;
            health(&b, &p)?;
        }
        Commands::List { uri } => list_cmd(&uri)?,
        Commands::Stat { uri } => stat_cmd(&uri)?,
        Commands::Get { uri, jobs } => get_cmd(&uri, jobs)?,
        Commands::Delete { uri, jobs } => delete_cmd(&uri, jobs)?,
        Commands::Put { uri, bucket, prefix, object_size, objects, concurrency } => {
            let (b, p) = parse_s3(&uri, bucket, prefix)?;
            put_bench(object_size, objects, &b, &p, concurrency)?;
        }
        Commands::Run { config } => run_workload(&config)?,
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// helper: parse s3:// URI or separate bucket+prefix
// -----------------------------------------------------------------------------
fn parse_s3(uri: &Option<String>, bucket: Option<String>, prefix: String) -> Result<(String, String)> {
    if let Some(u) = uri {
        let parsed = Url::parse(u).context("Invalid S3 URI")?;
        if parsed.scheme() != "s3" {
            bail!("URI must begin with s3://");
        }
        let b = parsed.host_str().context("Missing bucket in URI")?.to_string();
        let mut p = parsed.path().trim_start_matches('/').to_string();
        if !p.is_empty() && !p.ends_with('/') {
            p.push('/');
        }
        return Ok((b, p));
    }
    let b = bucket.expect("--bucket is required if --uri is not set");
    Ok((b, prefix))
}

// -----------------------------------------------------------------------------
// Commands implementations
// -----------------------------------------------------------------------------
fn health(bucket: &str, prefix: &str) -> Result<()> {
    let h = OpHists::new();
    let t0 = Instant::now();
    let keys = list_objects(bucket, prefix, true).context("list_objects_v2 failed")?;
    h.record(0, t0.elapsed());
    println!("OK – found {} objects under s3://{}/{}", keys.len(), bucket, prefix);
    h.print_summary("LIST");
    Ok(())
}

fn list_cmd(uri: &str) -> Result<()> {
    let (bucket, key_pattern) = parse_s3_uri(uri)?;
    let (prefix, glob) = if let Some(pos) = key_pattern.rfind('/') {
        (&key_pattern[..=pos], &key_pattern[pos+1..])
    } else {
        ("", key_pattern.as_str())
    };
    let h = OpHists::new();
    let t0 = Instant::now();
    let mut keys = list_objects(&bucket, prefix, true)?;
    h.record(0, t0.elapsed());
    let pattern = format!("^{}$", escape(glob).replace(r"\*", ".*"));
    let re = Regex::new(&pattern).context("Invalid glob pattern")?;
    keys.retain(|k| re.is_match(k.rsplit('/').next().unwrap_or(k)));
    for k in &keys {
        println!("{}", k);
    }
    println!("\nTotal objects: {}", keys.len());
    h.print_summary("LIST");
    Ok(())
}

fn stat_cmd(uri: &str) -> Result<()> {
    let h = OpHists::new();
    let t0 = Instant::now();
    let os = stat_object_uri(uri)?;
    h.record(0, t0.elapsed());
    println!("Size            : {} bytes", os.size);
    println!("LastModified    : {:?}", os.last_modified);
    println!("ETag            : {:?}", os.e_tag);
    println!("Content-Type    : {:?}", os.content_type);
    println!("StorageClass    : {:?}", os.storage_class);
    println!("VersionId       : {:?}", os.version_id);
    h.print_summary("STAT");
    Ok(())
}

fn get_cmd(uri: &str, jobs: usize) -> Result<()> {
    let (bucket, pat) = parse_s3_uri(uri)?;
    let keys = if pat.contains('*') {
        let (prefix, glob) = if let Some(pos) = pat.rfind('/') {
            (&pat[..=pos], &pat[pos+1..])
        } else {
            ("", pat.as_str())
        };
        let mut ks = list_objects(&bucket, prefix, true)?;
        let pattern = format!("^{}$", escape(glob).replace(r"\*", ".*"));
        let re = Regex::new(&pattern)?;
        ks.retain(|k| re.is_match(k.rsplit('/').next().unwrap_or(k)));
        ks
    } else if pat.ends_with('/') || pat.is_empty() {
        list_objects(&bucket, &pat, true)?
    } else {
        vec![pat.to_string()]
    };
    if keys.is_empty() {
        bail!("No objects match given URI");
    }
    let uris: Vec<(String,String)> = keys.iter().map(|k| (bucket.clone(), k.clone())).collect();
    eprintln!("Fetching {} objects with {} jobs…", uris.len(), jobs);
    let hist = OpHists::new();
    let hist2 = hist.clone();
    let t0 = Instant::now();
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    let total_bytes = rt.block_on(async move {
        let hist = hist2;
        let sem = Arc::new(Semaphore::new(jobs));
        let mut futs = FuturesUnordered::new();
        for (b,k) in uris {
            let sem2 = sem.clone();
            let hist2 = hist.clone();
            futs.push(tokio::spawn(async move {
                let _permit = sem2.acquire_owned().await.unwrap();
                let t1 = Instant::now();
                let bytes = get_object(&b, &k).await?;
                let idx = bucket_index(bytes.len());
                hist2.record(idx, t1.elapsed());
                Ok::<usize, anyhow::Error>(bytes.len())
            }));
        }
        let mut total = 0usize;
        while let Some(r) = futs.next().await {
            let cnt = r.context("join error")?;
            total += cnt?;
        }
        Ok::<usize, anyhow::Error>(total)
    })?;
    let dt = t0.elapsed();
    println!(
        "downloaded {:.2} MB in {:?} ({:.2} MB/s)",
        total_bytes as f64 / 1_048_576.0,
        dt,
        total_bytes as f64 / 1_048_576.0 / dt.as_secs_f64(),
    );
    hist.print_summary("GET");
    Ok(())
}

fn delete_cmd(uri: &str, _jobs: usize) -> Result<()> {
    let (bucket, pat) = parse_s3_uri(uri)?;
    let keys = if pat.contains('*') {
        let (prefix, glob) = if let Some(pos) = pat.rfind('/') {
            (&pat[..=pos], &pat[pos+1..])
        } else {
            ("", pat.as_str())
        };
        let mut ks = list_objects(&bucket, prefix, true)?;
        let pattern = format!("^{}$", escape(glob).replace(r"\*", ".*"));
        let re = Regex::new(&pattern)?;
        ks.retain(|k| re.is_match(k.rsplit('/').next().unwrap_or(k)));
        ks
    } else if pat.ends_with('/') || pat.is_empty() {
        list_objects(&bucket, &pat, true)?
    } else {
        vec![pat.to_string()]
    };
    if keys.is_empty() {
        bail!("No objects to delete under the specified URI");
    }
    let h = OpHists::new();
    let t0 = Instant::now();
    delete_objects(&bucket, &keys)?;
    h.record(0, t0.elapsed());
    eprintln!("Deleted {} objects", keys.len());
    h.print_summary("DELETE");
    Ok(())
}

fn put_bench(
    size: usize,
    count: usize,
    bucket: &str,
    prefix: &str,
    concurrency: usize,
) -> Result<()> {
    let keys: Vec<String> = (0..count).map(|i| format!("{}obj_{}", prefix, i)).collect();
    let data = vec![0u8; size];
    let hist = OpHists::new();
    let t0 = Instant::now();
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        let sem = Arc::new(Semaphore::new(concurrency));
        let mut futs = FuturesUnordered::new();
        for key in keys {
            let sem2 = sem.clone();
            let hist2 = hist.clone();
            let b = bucket.to_string();
            let data2 = data.clone();
            futs.push(tokio::spawn(async move {
                let _permit = sem2.acquire_owned().await.unwrap();
                let t1 = Instant::now();
                put_object_async(&b, &key, &data2).await?;
                let idx = bucket_index(data2.len());
                hist2.record(idx, t1.elapsed());
                Ok::<(), anyhow::Error>(())
            }));
        }
        while let Some(r) = futs.next().await {
            (r.context("join error")?)?;
        }
        Ok::<(), anyhow::Error>(())
    })?;
    let dt = t0.elapsed();
    let mb = (count * size) as f64 / (1024.0 * 1024.0);
    println!(
        "Uploaded {} objects ({:.2} MB) in {:?} ({:.2} MB/s)",
        count,
        mb,
        dt,
        mb / dt.as_secs_f64(),
    );
    hist.print_summary("PUT");
    Ok(())
}

// -----------------------------------------------------------------------------
// Workload execution
// -----------------------------------------------------------------------------
fn run_workload(config_path: &str) -> Result<()> {
    info!("Loading workload configuration from: {}", config_path);
    let config_content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path))?;
    
    let config: Config = serde_yaml::from_str(&config_content)
        .with_context(|| format!("Failed to parse config file: {}", config_path))?;
    
    info!("Configuration loaded successfully");
    
    println!("Running workload from: {}", config_path);
    if let Some(target) = &config.target {
        println!("Target: {}", target);
        info!("Target backend: {}", target);
    }
    println!("Duration: {:?}", config.duration);
    println!("Concurrency: {}", config.concurrency);
    println!("Operations: {}", config.workload.len());
    
    info!("Starting workload execution with {} operation types", config.workload.len());
    
    // Run the workload
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    let summary = rt.block_on(workload::run(&config))?;
    
    // Print results
    println!("\n=== Results ===");
    println!("Wall time: {:.2}s", summary.wall_seconds);
    println!("Total ops: {}", summary.total_ops);
    println!("Total bytes: {} ({:.2} MB)", summary.total_bytes, summary.total_bytes as f64 / (1024.0 * 1024.0));
    println!("Throughput: {:.2} ops/s", summary.total_ops as f64 / summary.wall_seconds);
    
    if summary.get.ops > 0 {
        println!("\nGET operations:");
        println!("  Ops: {}", summary.get.ops);
        println!("  Bytes: {} ({:.2} MB)", summary.get.bytes, summary.get.bytes as f64 / (1024.0 * 1024.0));
        println!("  Latency p50: {}ms, p95: {}ms, p99: {}ms", summary.get.p50_ms, summary.get.p95_ms, summary.get.p99_ms);
    }
    
    if summary.put.ops > 0 {
        println!("\nPUT operations:");
        println!("  Ops: {}", summary.put.ops);
        println!("  Bytes: {} ({:.2} MB)", summary.put.bytes, summary.put.bytes as f64 / (1024.0 * 1024.0));
        println!("  Latency p50: {}ms, p95: {}ms, p99: {}ms", summary.put.p50_ms, summary.put.p95_ms, summary.put.p99_ms);
    }
    
    Ok(())
}

