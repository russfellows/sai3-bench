//
// Copyright, 2025: Suse io_bench::workload::{gnal65/Futurum
//

// -----------------------------------------------------------------------------
// warp‑test ‑ lightweight S3 performance tester & utility CLI built on s3dlio
// -----------------------------------------------------------------------------

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use futures::{stream::FuturesUnordered, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::{Regex, escape};
use io_bench::config::Config;
use io_bench::metrics::{OpHists, bucket_index};
use io_bench::workload;
use serde_yaml;
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Builder as RtBuilder;
use tokio::sync::Semaphore;
use tracing::info;
use url::Url;

// Multi-backend ObjectStore operations
use io_bench::workload::{
    get_object_multi_backend, put_object_multi_backend, list_objects_multi_backend, 
    stat_object_multi_backend, delete_object_multi_backend,
};

// -----------------------------------------------------------------------------
// CLI definition
// -----------------------------------------------------------------------------
#[derive(Parser)]
#[command(name = "io-bench", version, about = "An io-bench tool that leverages s3dlio library")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    
    /// Verbose output (-v for info, -vv for debug, -vvv for trace with s3dlio debug)
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbose: u8,
    
    /// Enable operation logging to specified file (always zstd compressed, use .tsv.zst extension)
    /// Records all storage operations across all backends for performance analysis and replay
    #[arg(long = "op-log", value_name = "PATH")]
    op_log: Option<std::path::PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Verify storage backend reachability across all supported backends
    /// 
    /// Examples:
    ///   io-bench health --uri "file:///tmp/test/"
    ///   io-bench health --uri "s3://bucket/prefix/"
    ///   io-bench health --uri "direct:///mnt/fast/"
    ///   io-bench health --uri "az://storageaccount/container/"
    Health {
        #[arg(long)]
        uri: String,
    },
    /// List objects (supports basename glob across all backends)
    /// 
    /// Examples:
    ///   io-bench list --uri "file:///tmp/data/"
    ///   io-bench list --uri "s3://bucket/prefix/"
    ///   io-bench list --uri "direct:///mnt/data/*.txt"
    List {
        #[arg(long)]
        uri: String,
    },
    /// Stat (HEAD) one object across all backends
    /// 
    /// Examples:
    ///   io-bench stat --uri "file:///tmp/data/file.txt"
    ///   io-bench stat --uri "s3://bucket/object.txt"
    Stat {
        #[arg(long)]
        uri: String,
    },
    /// Get objects (prefix, glob, or single) from any backend
    /// 
    /// Examples:
    ///   io-bench get --uri "file:///tmp/data/*" --jobs 8
    ///   io-bench get --uri "s3://bucket/prefix/" --jobs 4
    Get {
        #[arg(long)]
        uri: String,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Delete objects (prefix, glob, or single) from any backend
    /// 
    /// Examples:
    ///   io-bench delete --uri "file:///tmp/old/*" --jobs 8
    ///   io-bench delete --uri "s3://bucket/prefix/"
    Delete {
        #[arg(long)]
        uri: String,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Put random-data objects to any backend
    /// 
    /// Examples:
    ///   io-bench put --uri "file:///tmp/data/test*.dat" --objects 100
    ///   io-bench put --uri "s3://bucket/prefix/file*.dat" --object-size 1048576
    Put {
        #[arg(long)]
        uri: String,
        #[arg(long, default_value_t = 1024)]
        object_size: usize,
        #[arg(long, default_value_t = 1)]
        objects: usize,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },
    /// Run workload from config file
    /// 
    /// Examples:
    ///   io-bench run --config mixed.yaml
    ///   io-bench run --config mixed.yaml --prepare-only
    ///   io-bench run --config mixed.yaml --no-cleanup
    ///   io-bench run --config mixed.yaml --results-tsv /tmp/benchmark
    Run {
        #[arg(long)]
        config: String,
        
        /// Only execute prepare step, then exit (for pre-populating test data)
        #[arg(long)]
        prepare_only: bool,
        
        /// Skip cleanup of prepared objects (keep them for repeated runs)
        #[arg(long)]
        no_cleanup: bool,
        
        /// Export machine-readable results to TSV file.
        /// 
        /// Output file will be <path>-results.tsv with 13 columns:
        /// operation, size_bucket, bucket_idx, mean_us, p50_us, p90_us, p95_us,
        /// p99_us, max_us, avg_bytes, ops_per_sec, throughput_mibps, count
        /// 
        /// Size buckets: zero, 1B-8KiB, 8KiB-64KiB, 64KiB-512KiB, 512KiB-4MiB,
        /// 4MiB-32MiB, 32MiB-256MiB, 256MiB-2GiB, >2GiB
        /// 
        /// Example: --results-tsv /tmp/test creates /tmp/test-results.tsv
        #[arg(long, value_name = "PATH")]
        results_tsv: Option<String>,
    },
    /// Replay workload from op-log file with timing-faithful execution
    /// 
    /// Examples:
    ///   io-bench replay --op-log /tmp/ops.tsv.zst
    ///   io-bench replay --op-log /tmp/ops.tsv --target "s3://newbucket/"
    ///   io-bench replay --op-log /tmp/ops.tsv.zst --speed 2.0 --target "file:///tmp/replay/"
    ///   io-bench replay --op-log /tmp/ops.tsv.zst --remap remap-config.yaml
    Replay {
        /// Path to op-log file (TSV, optionally zstd-compressed with .zst extension)
        #[arg(long)]
        op_log: std::path::PathBuf,
        
        /// Optional target URI to retarget all operations (simple 1:1 remapping)
        #[arg(long)]
        target: Option<String>,
        
        /// Advanced remap configuration file (YAML) - takes priority over --target
        #[arg(long)]
        remap: Option<std::path::PathBuf>,
        
        /// Speed multiplier (e.g., 2.0 = 2x faster, 0.5 = half speed)
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        
        /// Continue on errors instead of stopping
        #[arg(long)]
        continue_on_error: bool,
    },
}

// -----------------------------------------------------------------------------
// main
// -----------------------------------------------------------------------------
fn main() -> Result<()> {
    let cli = Cli::parse();
    
    // Initialize logging based on verbosity level
    // Map io-bench verbosity to appropriate levels for both io_bench and s3dlio:
    // -v (1): io_bench=info, s3dlio=warn (default passthrough)
    // -vv (2): io_bench=debug, s3dlio=info (detailed io_bench, operational s3dlio)
    // -vvv (3+): io_bench=trace, s3dlio=debug (full debugging both crates)
    let (io_bench_level, s3dlio_level) = match cli.verbose {
        0 => ("warn", "warn"),   // Default: only warnings and errors
        1 => ("info", "warn"),   // -v: info level for io_bench, minimal s3dlio
        2 => ("debug", "info"),  // -vv: debug io_bench, info s3dlio
        _ => ("trace", "debug"), // -vvv+: trace io_bench, debug s3dlio
    };
    
    // Initialize tracing subscriber with levels for both crates
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::new(format!("io_bench={},s3dlio={}", io_bench_level, s3dlio_level));
    fmt()
        .with_env_filter(filter)
        .init();
    
    // Initialize operation logger if requested
    if let Some(ref op_log_path) = cli.op_log {
        info!("Initializing operation logger: {}", op_log_path.display());
        workload::init_operation_logger(op_log_path)
            .context("Failed to initialize operation logger")?;
    }
    
    // Execute command
    match cli.command {
        Commands::Health { uri } => health_cmd(&uri)?,
        Commands::List { uri } => list_cmd(&uri)?,
        Commands::Stat { uri } => stat_cmd(&uri)?,
        Commands::Get { uri, jobs } => get_cmd(&uri, jobs)?,
        Commands::Delete { uri, jobs } => delete_cmd(&uri, jobs)?,
        Commands::Put { uri, object_size, objects, concurrency } => {
            put_cmd(&uri, object_size, objects, concurrency)?
        }
        Commands::Run { config, prepare_only, no_cleanup, results_tsv } => {
            run_workload(&config, prepare_only, no_cleanup, results_tsv.as_deref())?
        }
        Commands::Replay { op_log, target, remap, speed, continue_on_error } => {
            replay_cmd(op_log, target, remap, speed, continue_on_error)?
        }
    }
    
    // Finalize operation logger if enabled
    if cli.op_log.is_some() {
        info!("Finalizing operation logger");
        workload::finalize_operation_logger()
            .context("Failed to finalize operation logger")?;
    }
    
    Ok(())
}

// -----------------------------------------------------------------------------
// Helper: validate and normalize URI for multi-backend support
// -----------------------------------------------------------------------------
fn validate_uri(uri: &str) -> Result<String> {
    let parsed = Url::parse(uri).context("Invalid URI format")?;
    
    match parsed.scheme() {
        "file" | "direct" | "s3" | "az" | "gs" | "gcs" => {
            // URI is valid for supported backends
            Ok(uri.to_string())
        }
        scheme => {
            bail!("Unsupported backend scheme '{}'. Supported schemes: file://, direct://, s3://, az://, gs://", scheme)
        }
    }
}

// -----------------------------------------------------------------------------
// Commands implementations - Multi-backend support
// -----------------------------------------------------------------------------
fn health_cmd(uri: &str) -> Result<()> {
    let validated_uri = validate_uri(uri)?;
    
    // Create async runtime for the health check
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    
    rt.block_on(async {
        let h = OpHists::new();
        let t0 = Instant::now();
        
        // Use list operation to test backend connectivity
        let objects = list_objects_multi_backend(&validated_uri).await
            .context("Backend health check failed - could not list objects")?;
        
        h.record(0, t0.elapsed());
        
        println!("OK – found {} objects at {}", objects.len(), validated_uri);
        h.print_summary("HEALTH CHECK");
        Ok(())
    })
}

fn list_cmd(uri: &str) -> Result<()> {
    let validated_uri = validate_uri(uri)?;
    
    // Create async runtime for the list operation
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    
    rt.block_on(async {
        let h = OpHists::new();
        let t0 = Instant::now();
        
        let mut objects = if validated_uri.contains('*') {
            // Extract the directory part (everything before the last '/')
            let dir_uri = if let Some(pos) = validated_uri.rfind('/') {
                &validated_uri[..=pos]  // Include the trailing '/'
            } else {
                return Err(anyhow!("Invalid URI pattern: {}", validated_uri));
            };
            
            // List the directory
            list_objects_multi_backend(dir_uri).await
                .context("Failed to list objects for pattern matching")?
        } else {
            // Direct listing (no pattern)
            list_objects_multi_backend(&validated_uri).await
                .context("Failed to list objects")?
        };
        
        h.record(0, t0.elapsed());
        
        // Apply glob pattern filtering if needed
        if validated_uri.contains('*') {
            let pattern_part = validated_uri.split('/').last().unwrap_or("*");
            if pattern_part.contains('*') {
                let pattern = format!("^{}$", escape(pattern_part).replace(r"\*", ".*"));
                let re = Regex::new(&pattern).context("Invalid glob pattern")?;
                objects.retain(|obj| {
                    let basename = obj.split('/').last().unwrap_or(obj);
                    re.is_match(basename)
                });
            }
        }
        
        // Print results
        for obj in &objects {
            println!("{}", obj);
        }
        println!("\nTotal objects: {}", objects.len());
        h.print_summary("LIST");
        Ok(())
    })
}

fn stat_cmd(uri: &str) -> Result<()> {
    let validated_uri = validate_uri(uri)?;
    
    // Create async runtime for the stat operation  
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    
    rt.block_on(async {
        let h = OpHists::new();
        let t0 = Instant::now();
        
        let size = stat_object_multi_backend(&validated_uri).await
            .context("Failed to stat object")?;
        
        h.record(0, t0.elapsed());
        
        println!("URI             : {}", validated_uri);
        println!("Size            : {} bytes", size);
        // Note: Only size is available from ObjectStore stat interface
        // Backend-specific metadata would require specialized calls
        
        h.print_summary("STAT");
        Ok(())
    })
}

fn get_cmd(uri: &str, jobs: usize) -> Result<()> {
    let validated_uri = validate_uri(uri)?;
    
    // Create async runtime for the get operation
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    
    rt.block_on(async {
        // Determine if this is a pattern, directory, or single object
        let objects = if validated_uri.contains('*') {
            // Handle glob patterns - extract directory and list it
            let dir_uri = if let Some(pos) = validated_uri.rfind('/') {
                &validated_uri[..=pos]  // Include the trailing '/'
            } else {
                return Err(anyhow!("Invalid URI pattern: {}", validated_uri));
            };
            
            let pattern_part = validated_uri.split('/').last().unwrap_or("*");
            
            let mut found_objects = list_objects_multi_backend(dir_uri).await
                .context("Failed to list objects for pattern matching")?;
                
            if pattern_part.contains('*') {
                let pattern = format!("^{}$", escape(pattern_part).replace(r"\*", ".*"));
                let re = Regex::new(&pattern).context("Invalid glob pattern")?;
                found_objects.retain(|obj| {
                    let basename = obj.split('/').last().unwrap_or(obj);
                    re.is_match(basename)
                });
            }
            found_objects
        } else if validated_uri.ends_with('/') {
            // Directory listing
            list_objects_multi_backend(&validated_uri).await
                .context("Failed to list directory contents")?
        } else {
            // Single object
            vec![validated_uri.clone()]
        };
        
        if objects.is_empty() {
            bail!("No objects match the given URI: {}", validated_uri);
        }
        
        eprintln!("Fetching {} objects with {} jobs…", objects.len(), jobs);
        
        // Create progress bar for operations
        let pb = ProgressBar::new(objects.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects ({eta_precise}) {msg}"
            )?
            .progress_chars("#>-")
        );
        pb.set_message(format!("downloading with {} workers", jobs));
        
        let hist = OpHists::new();
        let hist2 = hist.clone();
        let t0 = Instant::now();
        
        let total_bytes = {
            let sem = Arc::new(Semaphore::new(jobs));
            let mut futs = FuturesUnordered::new();
            
            for obj_uri in objects {
                let sem2 = sem.clone();
                let hist2 = hist2.clone();
                let pb_clone = pb.clone();
                
                futs.push(tokio::spawn(async move {
                    let _permit = sem2.acquire_owned().await.unwrap();
                    let t1 = Instant::now();
                    
                    let bytes = get_object_multi_backend(&obj_uri).await?;
                    let idx = bucket_index(bytes.len());
                    hist2.record(idx, t1.elapsed());
                    
                    pb_clone.inc(1);
                    
                    Ok::<usize, anyhow::Error>(bytes.len())
                }));
            }
            
            let mut total = 0usize;
            while let Some(result) = futs.next().await {
                let byte_count = result.context("Task join error")?;
                total += byte_count?;
            }
            total
        };
        
        pb.finish_with_message(format!("downloaded {:.2} MB", total_bytes as f64 / 1_048_576.0));
        
        let dt = t0.elapsed();
        println!(
            "Downloaded {:.2} MB in {:?} ({:.2} MB/s)",
            total_bytes as f64 / 1_048_576.0,
            dt,
            total_bytes as f64 / 1_048_576.0 / dt.as_secs_f64(),
        );
        hist.print_summary("GET");
        Ok(())
    })
}

fn delete_cmd(uri: &str, jobs: usize) -> Result<()> {
    let validated_uri = validate_uri(uri)?;
    
    // Create async runtime for the delete operation
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    
    rt.block_on(async {
        // Determine what objects to delete
        let objects = if validated_uri.contains('*') {
            // Handle glob patterns - extract directory and list it
            let dir_uri = if let Some(pos) = validated_uri.rfind('/') {
                &validated_uri[..=pos]  // Include the trailing '/'
            } else {
                return Err(anyhow!("Invalid URI pattern: {}", validated_uri));
            };
            
            let pattern_part = validated_uri.split('/').last().unwrap_or("*");
            
            let mut found_objects = list_objects_multi_backend(dir_uri).await
                .context("Failed to list objects for pattern matching")?;
                
            if pattern_part.contains('*') {
                let pattern = format!("^{}$", escape(pattern_part).replace(r"\*", ".*"));
                let re = Regex::new(&pattern).context("Invalid glob pattern")?;
                found_objects.retain(|obj| {
                    let basename = obj.split('/').last().unwrap_or(obj);
                    re.is_match(basename)
                });
            }
            found_objects
        } else if validated_uri.ends_with('/') {
            // Directory deletion - list all objects
            list_objects_multi_backend(&validated_uri).await
                .context("Failed to list directory contents for deletion")?
        } else {
            // Single object
            vec![validated_uri.clone()]
        };
        
        if objects.is_empty() {
            bail!("No objects to delete under the specified URI: {}", validated_uri);
        }
        
        eprintln!("Deleting {} objects with {} jobs…", objects.len(), jobs);
        
        // Create progress bar for delete operations
        let pb = ProgressBar::new(objects.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects ({eta_precise}) {msg}"
            )?
            .progress_chars("#>-")
        );
        pb.set_message(format!("deleting with {} workers", jobs));
        
        let hist = OpHists::new();
        let hist2 = hist.clone();
        let t0 = Instant::now();
        
        {
            let sem = Arc::new(Semaphore::new(jobs));
            let mut futs = FuturesUnordered::new();
            
            for obj_uri in objects.iter() {
                let sem2 = sem.clone();
                let hist2 = hist2.clone();
                let obj_uri = obj_uri.clone();
                let pb_clone = pb.clone();
                
                futs.push(tokio::spawn(async move {
                    let _permit = sem2.acquire_owned().await.unwrap();
                    let t1 = Instant::now();
                    
                    delete_object_multi_backend(&obj_uri).await?;
                    hist2.record(0, t1.elapsed());
                    
                    pb_clone.inc(1);
                    
                    Ok::<(), anyhow::Error>(())
                }));
            }
            
            while let Some(result) = futs.next().await {
                result.context("Task join error")??;
            }
        };
        
        pb.finish_with_message(format!("deleted {} objects", objects.len()));
        
        let dt = t0.elapsed();
        println!("Deleted {} objects in {:?}", objects.len(), dt);
        hist.print_summary("DELETE");
        Ok(())
    })
}

fn put_cmd(uri: &str, object_size: usize, objects: usize, concurrency: usize) -> Result<()> {
    let validated_uri = validate_uri(uri)?;
    
    // Create async runtime for the put operation
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    
    rt.block_on(async {
        // Generate object URIs
        let object_uris: Vec<String> = if validated_uri.contains('*') {
            // Replace * with numbered objects
            (0..objects).map(|i| {
                validated_uri.replace('*', &format!("{:06}", i))
            }).collect()
        } else if validated_uri.ends_with('/') {
            // Append numbered objects to directory
            (0..objects).map(|i| {
                format!("{}obj_{:06}.dat", validated_uri, i)
            }).collect()
        } else {
            // Single object URI - only create one regardless of objects count
            if objects > 1 {
                eprintln!("Warning: URI specifies single object but {} objects requested. Creating numbered variants.", objects);
                (0..objects).map(|i| {
                    if i == 0 {
                        validated_uri.clone()
                    } else {
                        format!("{}.{}", validated_uri, i)
                    }
                }).collect()
            } else {
                vec![validated_uri]
            }
        };
        
        // Generate random data for all objects
        let data = vec![0u8; object_size];
        // TODO: Consider using random data generation for more realistic testing
        
        eprintln!("Uploading {} objects ({} bytes each) with {} jobs…", 
                 object_uris.len(), object_size, concurrency);
        
        // Create progress bar for upload operations
        let pb = ProgressBar::new(object_uris.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} objects ({eta_precise}) {msg}"
            )?
            .progress_chars("#>-")
        );
        let size_mb = (object_size as f64 * object_uris.len() as f64) / 1_048_576.0;
        pb.set_message(format!("uploading {:.1}MB with {} workers", size_mb, concurrency));
        
        let hist = OpHists::new();
        let hist2 = hist.clone();
        let t0 = Instant::now();
        
        {
            let sem = Arc::new(Semaphore::new(concurrency));
            let mut futs = FuturesUnordered::new();
            
            for obj_uri in object_uris.iter() {
                let sem2 = sem.clone();
                let hist2 = hist2.clone();
                let obj_uri = obj_uri.clone();
                let data = data.clone();
                let pb_clone = pb.clone();
                
                futs.push(tokio::spawn(async move {
                    let _permit = sem2.acquire_owned().await.unwrap();
                    let t1 = Instant::now();
                    
                    put_object_multi_backend(&obj_uri, &data).await?;
                    let idx = bucket_index(data.len());
                    hist2.record(idx, t1.elapsed());
                    
                    pb_clone.inc(1);
                    
                    Ok::<(), anyhow::Error>(())
                }));
            }
            
            while let Some(result) = futs.next().await {
                result.context("Task join error")??;
            }
        };
        
        let total_mb = (object_size as f64 * object_uris.len() as f64) / 1_048_576.0;
        pb.finish_with_message(format!("uploaded {:.2} MB", total_mb));
        
        let dt = t0.elapsed();
        let total_mb = (object_uris.len() * object_size) as f64 / (1024.0 * 1024.0);
        println!(
            "Uploaded {} objects ({:.2} MB) in {:?} ({:.2} MB/s)",
            object_uris.len(),
            total_mb,
            dt,
            total_mb / dt.as_secs_f64(),
        );
        hist.print_summary("PUT");
        Ok(())
    })
}

// -----------------------------------------------------------------------------
// Workload execution
// -----------------------------------------------------------------------------
fn run_workload(config_path: &str, prepare_only: bool, no_cleanup: bool, results_tsv: Option<&str>) -> Result<()> {
    info!("Loading workload configuration from: {}", config_path);
    let config_content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path))?;
    
    let mut config: Config = serde_yaml::from_str(&config_content)
        .with_context(|| format!("Failed to parse config file: {}", config_path))?;
    
    // Override cleanup from CLI flag
    if no_cleanup {
        if let Some(ref mut prepare) = config.prepare {
            prepare.cleanup = false;
        }
    }
    
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
    
    // Execute prepare step if configured
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    let prepared_objects = if let Some(ref prepare_config) = config.prepare {
        println!("\n=== Prepare Phase ===");
        info!("Executing prepare step");
        let prepared = rt.block_on(workload::prepare_objects(prepare_config))?;
        println!("Prepared {} objects", prepared.len());
        prepared
    } else {
        Vec::new()
    };
    
    // If prepare-only mode, exit after preparation
    if prepare_only {
        if config.prepare.is_none() {
            bail!("--prepare-only requires 'prepare' section in config");
        }
        info!("Prepare-only mode: objects created, exiting");
        println!("\nPrepare-only mode: {} objects created, exiting", prepared_objects.len());
        return Ok(());
    }
    
    // Always show preparation status
    println!("\n=== Test Phase ===");
    println!("Preparing workload...");
    
    // Run the workload
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
        println!("  Latency p50: {}µs, p95: {}µs, p99: {}µs", summary.get.p50_us, summary.get.p95_us, summary.get.p99_us);
    }
    
    if summary.put.ops > 0 {
        println!("\nPUT operations:");
        println!("  Ops: {}", summary.put.ops);
        println!("  Bytes: {} ({:.2} MB)", summary.put.bytes, summary.put.bytes as f64 / (1024.0 * 1024.0));
        println!("  Latency p50: {}µs, p95: {}µs, p99: {}µs", summary.put.p50_us, summary.put.p95_us, summary.put.p99_us);
    }
    
    if summary.meta.ops > 0 {
        println!("\nMETA-DATA operations:");
        println!("  Ops: {}", summary.meta.ops);
        println!("  Bytes: {} ({:.2} MB)", summary.meta.bytes, summary.meta.bytes as f64 / (1024.0 * 1024.0));
        println!("  Latency p50: {}µs, p95: {}µs, p99: {}µs", summary.meta.p50_us, summary.meta.p95_us, summary.meta.p99_us);
    }
    
    // Export TSV results if requested
    if let Some(tsv_path) = results_tsv {
        use io_bench::tsv_export::TsvExporter;
        let exporter = TsvExporter::new(tsv_path);
        exporter.export_results(
            &summary.get_hists,
            &summary.put_hists,
            &summary.meta_hists,
            &summary.get_bins,
            &summary.put_bins,
            &summary.meta_bins,
            summary.wall_seconds,
        )?;
    }
    
    // Cleanup prepared objects if configured
    if let Some(ref prepare_config) = config.prepare {
        if prepare_config.cleanup && !prepared_objects.is_empty() {
            println!("\n=== Cleanup Phase ===");
            info!("Cleaning up prepared objects");
            rt.block_on(workload::cleanup_prepared_objects(&prepared_objects))?;
            println!("Cleanup complete");
        }
    }
    
    Ok(())
}

// -----------------------------------------------------------------------------
// replay_cmd: Replay workload from op-log file (STREAMING v0.5.0+)
// -----------------------------------------------------------------------------
fn replay_cmd(
    op_log: std::path::PathBuf,
    target: Option<String>,
    remap: Option<std::path::PathBuf>,
    speed: f64,
    continue_on_error: bool,
) -> Result<()> {
    use io_bench::replay_streaming::{replay_workload_streaming, ReplayConfig};
    use io_bench::remap::RemapConfig;
    
    // Validate target URI if provided
    if let Some(ref uri) = target {
        validate_uri(uri)?;
    }
    
    // Load remap configuration if provided
    let remap_config = if let Some(remap_path) = remap {
        println!("Loading remap configuration from: {}", remap_path.display());
        let file = std::fs::File::open(&remap_path)
            .with_context(|| format!("Failed to open remap config: {}", remap_path.display()))?;
        let config: RemapConfig = serde_yaml::from_reader(file)
            .with_context(|| format!("Failed to parse remap config: {}", remap_path.display()))?;
        info!("Loaded remap config with {} rules", config.rules.len());
        println!("  Loaded {} remap rules", config.rules.len());
        Some(config)
    } else {
        None
    };
    
    // Warn if both target and remap are provided
    if target.is_some() && remap_config.is_some() {
        println!("WARNING: Both --target and --remap provided. Using --remap (--target ignored).");
    }
    
    let config = ReplayConfig {
        op_log_path: op_log,
        target_uri: target,
        speed,
        continue_on_error,
        max_concurrent: Some(1000), // Default max concurrent operations
        remap_config,
    };
    
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    let stats = rt.block_on(replay_workload_streaming(config))?;
    
    // Print summary statistics
    println!("\nReplay Summary:");
    println!("  Total operations: {}", stats.total_operations);
    println!("  Completed: {}", stats.completed_operations);
    println!("  Failed: {}", stats.failed_operations);
    println!("  Skipped: {}", stats.skipped_operations);
    
    Ok(())
}

