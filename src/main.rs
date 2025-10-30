//
// Copyright, 2025: Suse sai3_bench::workload::{gnal65/Futurum
//

// -----------------------------------------------------------------------------
// warp‑test ‑ lightweight S3 performance tester & utility CLI built on s3dlio
// -----------------------------------------------------------------------------

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use futures::{stream::FuturesUnordered, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::{Regex, escape};
use sai3_bench::config::Config;
use sai3_bench::metrics::{OpHists, bucket_index};
use sai3_bench::workload;
use serde_yaml;
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Builder as RtBuilder;
use tokio::sync::Semaphore;
use tracing::info;
use url::Url;

// Multi-backend ObjectStore operations
use sai3_bench::workload::{
    get_object_multi_backend, put_object_multi_backend, list_objects_multi_backend, 
    stat_object_multi_backend, delete_object_multi_backend,
};

// -----------------------------------------------------------------------------
// CLI definition
// -----------------------------------------------------------------------------
#[derive(Parser)]
#[command(name = "sai3-bench", version, about = "A sai3-bench tool that leverages s3dlio library")]
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
    /// Run workload from config file (use --dry-run to validate config first)
    /// 
    /// Results are automatically exported to TSV file. Default filename:
    ///   sai3bench-YYYY-MM-DD-HHMMSS-<config_basename>-results.tsv
    /// 
    /// Examples:
    ///   sai3-bench run --config mixed.yaml
    ///   sai3-bench run --config mixed.yaml --dry-run
    ///   sai3-bench run --config mixed.yaml --prepare-only
    ///   sai3-bench run --config mixed.yaml --verify
    ///   sai3-bench run --config mixed.yaml --skip-prepare
    ///   sai3-bench run --config mixed.yaml --no-cleanup
    ///   sai3-bench run --config mixed.yaml --tsv-name my-benchmark
    Run {
        #[arg(long)]
        config: String,
        
        /// Parse and validate config, display test summary, then exit (no execution)
        #[arg(long)]
        dry_run: bool,
        
        /// Only execute prepare step, then exit (for pre-populating test data)
        #[arg(long)]
        prepare_only: bool,
        
        /// Verify that prepared objects exist and are accessible, then exit
        #[arg(long)]
        verify: bool,
        
        /// Skip prepare phase (assume objects already exist from previous run)
        #[arg(long)]
        skip_prepare: bool,
        
        /// Skip cleanup of prepared objects (keep them for repeated runs)
        #[arg(long)]
        no_cleanup: bool,
        
        /// Custom basename for TSV results file (default: auto-generated with timestamp)
        /// 
        /// The TsvExporter will append "-results.tsv" to this name.
        /// Example: --tsv-name my-test creates my-test-results.tsv
        #[arg(long, value_name = "BASENAME")]
        tsv_name: Option<String>,
    },
    /// Replay workload from op-log file with timing-faithful execution
    /// 
    /// Examples:
    ///   sai3-bench replay --op-log /tmp/ops.tsv.zst
    ///   sai3-bench replay --op-log /tmp/ops.tsv --target "s3://newbucket/"
    ///   sai3-bench replay --op-log /tmp/ops.tsv.zst --speed 2.0 --target "file:///tmp/replay/"
    ///   sai3-bench replay --op-log /tmp/ops.tsv.zst --remap remap-config.yaml
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
    /// Storage utility operations (for quick testing and validation)
    /// 
    /// These are helper commands for basic storage operations.
    /// For comprehensive CLI operations, consider using s3-cli from the s3dlio package.
    /// 
    /// Examples:
    ///   sai3-bench util health --uri "s3://bucket/"
    ///   sai3-bench util list --uri "file:///tmp/data/"
    ///   sai3-bench util get --uri "s3://bucket/prefix/*" --jobs 8
    Util {
        #[command(subcommand)]
        command: UtilCommands,
    },
}

#[derive(Subcommand)]
enum UtilCommands {
    /// Verify storage backend reachability across all supported backends
    /// 
    /// Examples:
    ///   sai3-bench util health --uri "file:///tmp/test/"
    ///   sai3-bench util health --uri "s3://bucket/prefix/"
    ///   sai3-bench util health --uri "direct:///mnt/fast/"
    ///   sai3-bench util health --uri "az://storageaccount/container/"
    Health {
        #[arg(long)]
        uri: String,
    },
    /// List objects (supports basename glob across all backends)
    /// 
    /// Examples:
    ///   sai3-bench util list --uri "file:///tmp/data/"
    ///   sai3-bench util list --uri "s3://bucket/prefix/"
    ///   sai3-bench util list --uri "direct:///mnt/data/*.txt"
    List {
        #[arg(long)]
        uri: String,
    },
    /// Stat (HEAD) one object across all backends
    /// 
    /// Examples:
    ///   sai3-bench util stat --uri "file:///tmp/data/file.txt"
    ///   sai3-bench util stat --uri "s3://bucket/object.txt"
    Stat {
        #[arg(long)]
        uri: String,
    },
    /// Get objects (prefix, glob, or single) from any backend
    /// 
    /// Examples:
    ///   sai3-bench util get --uri "file:///tmp/data/*" --jobs 8
    ///   sai3-bench util get --uri "s3://bucket/prefix/" --jobs 4
    Get {
        #[arg(long)]
        uri: String,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Delete objects (prefix, glob, or single) from any backend
    /// 
    /// Examples:
    ///   sai3-bench util delete --uri "file:///tmp/old/*" --jobs 8
    ///   sai3-bench util delete --uri "s3://bucket/prefix/"
    Delete {
        #[arg(long)]
        uri: String,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Put random-data objects to any backend
    /// 
    /// Examples:
    ///   sai3-bench util put --uri "file:///tmp/data/test*.dat" --objects 100
    ///   sai3-bench util put --uri "s3://bucket/prefix/file*.dat" --object-size 1048576
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
}

// -----------------------------------------------------------------------------
// main
// -----------------------------------------------------------------------------
fn main() -> Result<()> {
    let cli = Cli::parse();
    
    // Initialize logging based on verbosity level
    // Map sai3-bench verbosity to appropriate levels for both sai3_bench and s3dlio:
    // -v (1): sai3_bench=info, s3dlio=warn (default passthrough)
    // -vv (2): sai3_bench=debug, s3dlio=info (detailed sai3_bench, operational s3dlio)
    // -vvv (3+): sai3_bench=trace, s3dlio=debug (full debugging both crates)
    let (sai3_bench_level, s3dlio_level) = match cli.verbose {
        0 => ("warn", "warn"),   // Default: only warnings and errors
        1 => ("info", "warn"),   // -v: info level for sai3_bench, minimal s3dlio
        2 => ("debug", "info"),  // -vv: debug sai3_bench, info s3dlio
        _ => ("trace", "debug"), // -vvv+: trace sai3_bench, debug s3dlio
    };
    
    // Initialize tracing subscriber with levels for both crates
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::new(format!("sai3_bench={},s3dlio={}", sai3_bench_level, s3dlio_level));
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
        Commands::Run { config, dry_run, prepare_only, verify, skip_prepare, no_cleanup, tsv_name } => {
            run_workload(&config, dry_run, prepare_only, verify, skip_prepare, no_cleanup, tsv_name.as_deref())?
        }
        Commands::Replay { op_log, target, remap, speed, continue_on_error } => {
            replay_cmd(op_log, target, remap, speed, continue_on_error)?
        }
        Commands::Util { command } => {
            match command {
                UtilCommands::Health { uri } => health_cmd(&uri)?,
                UtilCommands::List { uri } => list_cmd(&uri)?,
                UtilCommands::Stat { uri } => stat_cmd(&uri)?,
                UtilCommands::Get { uri, jobs } => get_cmd(&uri, jobs)?,
                UtilCommands::Delete { uri, jobs } => delete_cmd(&uri, jobs)?,
                UtilCommands::Put { uri, object_size, objects, concurrency } => {
                    put_cmd(&uri, object_size, objects, concurrency)?
                }
            }
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
// Config validation and summary display (--dry-run)
// -----------------------------------------------------------------------------
fn display_config_summary(config: &Config, config_path: &str) -> Result<()> {
    use sai3_bench::size_generator::SizeGenerator;
    
    println!("╔═══════════════════════════════════════════════════════════════════════╗");
    println!("║           CONFIGURATION VALIDATION & TEST SUMMARY                    ║");
    println!("╚═══════════════════════════════════════════════════════════════════════╝");
    println!();
    println!("✅ Config file parsed successfully: {}", config_path);
    println!();
    
    // Basic configuration
    println!("┌─ Test Configuration ─────────────────────────────────────────────────┐");
    println!("│ Duration:     {:?}", config.duration);
    println!("│ Concurrency:  {} threads", config.concurrency);
    if let Some(ref target) = config.target {
        let backend = sai3_bench::workload::BackendType::from_uri(target);
        println!("│ Target URI:   {}", target);
        println!("│ Backend:      {}", backend.name());
    } else {
        println!("│ Target URI:   (not set - using absolute URIs in operations)");
    }
    println!("└──────────────────────────────────────────────────────────────────────┘");
    println!();
    
    // RangeEngine configuration
    if let Some(ref range_config) = config.range_engine {
        println!("┌─ RangeEngine Configuration ──────────────────────────────────────────┐");
        println!("│ Enabled:      {}", if range_config.enabled { "✅ YES" } else { "❌ NO" });
        if range_config.enabled {
            let min_mb = range_config.min_split_size / (1024 * 1024);
            let chunk_mb = range_config.chunk_size / (1024 * 1024);
            println!("│ Min Size:     {} MiB (files >= this use concurrent range downloads)", min_mb);
            println!("│ Chunk Size:   {} MiB per range request", chunk_mb);
            println!("│ Max Ranges:   {} concurrent ranges per file", range_config.max_concurrent_ranges);
        }
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
    }
    
    // PageCache configuration
    if let Some(page_cache_mode) = config.page_cache_mode {
        println!("┌─ Page Cache Configuration (file:// and direct:// only) ─────────────┐");
        let mode_str = match page_cache_mode {
            sai3_bench::config::PageCacheMode::Auto => "Auto (Sequential for large files, Random for small)",
            sai3_bench::config::PageCacheMode::Sequential => "Sequential (streaming workloads)",
            sai3_bench::config::PageCacheMode::Random => "Random (random access patterns)",
            sai3_bench::config::PageCacheMode::DontNeed => "DontNeed (read-once data, free immediately)",
            sai3_bench::config::PageCacheMode::Normal => "Normal (default kernel heuristics)",
        };
        println!("│ Mode:         {:?} - {}", page_cache_mode, mode_str);
        println!("│ Note:         Linux/Unix only, uses posix_fadvise() hints");
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
    }
    
    // Prepare configuration
    if let Some(ref prepare) = config.prepare {
        println!("┌─ Prepare Phase ──────────────────────────────────────────────────────┐");
        println!("│ Objects will be created BEFORE test execution");
        println!("│");
        
        for (idx, spec) in prepare.ensure_objects.iter().enumerate() {
            println!("│ Section {}:", idx + 1);
            println!("│   URI:              {}", spec.base_uri);
            println!("│   Count:            {} objects", spec.count);
            
            // Display size information
            if let Some(ref size_spec) = spec.size_spec {
                let generator = SizeGenerator::new(size_spec)?;
                println!("│   Size:             {}", generator.description());
            } else if let (Some(min), Some(max)) = (spec.min_size, spec.max_size) {
                if min == max {
                    println!("│   Size:             {} bytes (fixed)", min);
                } else {
                    println!("│   Size:             {} - {} bytes (uniform)", min, max);
                }
            }
            
            // Display fill pattern
            println!("│   Fill Pattern:     {:?}", spec.fill);
            if matches!(spec.fill, sai3_bench::config::FillPattern::Random) {
                println!("│   Dedup Factor:     {} ({})", spec.dedup_factor, 
                    if spec.dedup_factor == 1 { "all unique" } else { &format!("{:.1}% dedup", (spec.dedup_factor - 1) as f64 / spec.dedup_factor as f64 * 100.0) });
                println!("│   Compress Factor:  {} ({})", spec.compress_factor,
                    if spec.compress_factor == 1 { "uncompressible" } else { &format!("{:.1}% compressible", (spec.compress_factor - 1) as f64 / spec.compress_factor as f64 * 100.0) });
            }
            
            if idx < prepare.ensure_objects.len() - 1 {
                println!("│");
            }
        }
        
        println!("│");
        println!("│ Cleanup:            {}", if prepare.cleanup { "✅ YES (delete after test)" } else { "❌ NO (keep objects)" });
        if prepare.post_prepare_delay > 0 {
            println!("│ Post-Prepare Delay: {}s (eventual consistency wait)", prepare.post_prepare_delay);
        }
        println!("└──────────────────────────────────────────────────────────────────────┘");
        println!();
    }
    
    // Workload operations
    println!("┌─ Workload Operations ────────────────────────────────────────────────┐");
    let total_weight: u32 = config.workload.iter().map(|w| w.weight).sum();
    println!("│ {} operation types, total weight: {}", config.workload.len(), total_weight);
    println!("│");
    
    for (idx, weighted_op) in config.workload.iter().enumerate() {
        let percentage = (weighted_op.weight as f64 / total_weight as f64) * 100.0;
        
        let (op_name, details) = match &weighted_op.spec {
            sai3_bench::config::OpSpec::Get { path } => {
                ("GET", format!("path: {}", path))
            },
            sai3_bench::config::OpSpec::Put { path, object_size, size_spec, dedup_factor, compress_factor } => {
                let mut details = format!("path: {}", path);
                if let Some(ref spec) = size_spec {
                    let generator = SizeGenerator::new(spec)?;
                    details.push_str(&format!(", size: {}", generator.description()));
                } else if let Some(size) = object_size {
                    details.push_str(&format!(", size: {} bytes", size));
                }
                details.push_str(&format!(", dedup: {}, compress: {}", dedup_factor, compress_factor));
                ("PUT", details)
            },
            sai3_bench::config::OpSpec::List { path } => {
                ("LIST", format!("path: {}", path))
            },
            sai3_bench::config::OpSpec::Stat { path } => {
                ("STAT", format!("path: {}", path))
            },
            sai3_bench::config::OpSpec::Delete { path } => {
                ("DELETE", format!("path: {}", path))
            },
            sai3_bench::config::OpSpec::Mkdir { path } => {
                ("MKDIR", format!("path: {}", path))
            },
            sai3_bench::config::OpSpec::Rmdir { path, recursive } => {
                let rec = if *recursive { " (recursive)" } else { "" };
                ("RMDIR", format!("path: {}{}", path, rec))
            },
        };
        
        println!("│ Op {}: {} - {:.1}% (weight: {})", idx + 1, op_name, percentage, weighted_op.weight);
        println!("│       {}", details);
        
        if let Some(concurrency) = weighted_op.concurrency {
            println!("│       concurrency override: {} threads", concurrency);
        }
        
        if idx < config.workload.len() - 1 {
            println!("│");
        }
    }
    
    println!("└──────────────────────────────────────────────────────────────────────┘");
    println!();
    
    // Summary
    println!("✅ Configuration is valid and ready to run");
    println!();
    println!("To execute this test, run:");
    println!("  sai3-bench run --config {}", config_path);
    println!();
    
    Ok(())
}

fn run_workload(config_path: &str, dry_run: bool, prepare_only: bool, verify: bool, skip_prepare: bool, no_cleanup: bool, tsv_name: Option<&str>) -> Result<()> {
    info!("Loading workload configuration from: {}", config_path);
    let config_content = std::fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read config file: {}", config_path))?;
    
    let mut config: Config = serde_yaml::from_str(&config_content)
        .with_context(|| format!("Failed to parse config file: {}", config_path))?;
    
    // Dry-run mode: display config summary and exit
    if dry_run {
        display_config_summary(&config, config_path)?;
        return Ok(());
    }
    
    // Validate flag combinations
    if prepare_only && verify {
        bail!("Cannot use both --prepare-only and --verify");
    }
    if prepare_only && skip_prepare {
        bail!("Cannot use both --prepare-only and --skip-prepare");
    }
    if verify && skip_prepare {
        bail!("Cannot use both --verify and --skip-prepare");
    }
    
    // Override cleanup from CLI flag
    if no_cleanup {
        if let Some(ref mut prepare) = config.prepare {
            prepare.cleanup = false;
        }
    }
    
    info!("Configuration loaded successfully");
    
    // Create results directory (v0.6.4+)
    use sai3_bench::results_dir::ResultsDir;
    let config_path_buf = std::path::PathBuf::from(config_path);
    let mut results_dir = ResultsDir::create(&config_path_buf, tsv_name, None)
        .context("Failed to create results directory")?;
    
    println!("Running workload from: {}", config_path);
    results_dir.write_console(&format!("Running workload from: {}", config_path))?;
    
    if let Some(target) = &config.target {
        let target_msg = format!("Target: {}", target);
        println!("{}", target_msg);
        results_dir.write_console(&target_msg)?;
        info!("Target backend: {}", target);
        
        // Log RangeEngine status for all backends
        let backend = sai3_bench::workload::BackendType::from_uri(target);
        let range_enabled = config.range_engine.as_ref().map(|c| c.enabled).unwrap_or(false);
        
        if range_enabled {
            let min_size_mb = config.range_engine.as_ref()
                .map(|c| c.min_split_size / (1024 * 1024))
                .unwrap_or(16);
            info!("RangeEngine ENABLED for {} backend - files >= {} MiB", backend.name(), min_size_mb);
        } else {
            info!("RangeEngine DISABLED for {} backend (default for optimal performance)", backend.name());
        }
    }
    let duration_msg = format!("Duration: {:?}", config.duration);
    println!("{}", duration_msg);
    results_dir.write_console(&duration_msg)?;
    
    let concurrency_msg = format!("Concurrency: {} threads", config.concurrency);
    println!("{}", concurrency_msg);
    results_dir.write_console(&concurrency_msg)?;
    
    // Calculate and display operation mix with percentages
    let total_weight: u32 = config.workload.iter().map(|w| w.weight).sum();
    let mix_header = format!("\nOperation Mix ({} types):", config.workload.len());
    println!("{}", mix_header);
    results_dir.write_console(&mix_header)?;
    
    for weighted_op in &config.workload {
        let percentage = (weighted_op.weight as f64 / total_weight as f64) * 100.0;
        let op_name = match &weighted_op.spec {
            sai3_bench::config::OpSpec::Get { .. } => "GET",
            sai3_bench::config::OpSpec::Put { .. } => "PUT",
            sai3_bench::config::OpSpec::List { .. } => "LIST",
            sai3_bench::config::OpSpec::Stat { .. } => "STAT",
            sai3_bench::config::OpSpec::Delete { .. } => "DELETE",
            sai3_bench::config::OpSpec::Mkdir { .. } => "MKDIR",
            sai3_bench::config::OpSpec::Rmdir { .. } => "RMDIR",
        };
        
        // Show per-operation concurrency override if specified
        let op_msg = if let Some(op_concurrency) = weighted_op.concurrency {
            format!("  {} - {:.1}% (weight: {}, concurrency: {} threads)", 
                op_name, percentage, weighted_op.weight, op_concurrency)
        } else {
            format!("  {} - {:.1}% (weight: {})", 
                op_name, percentage, weighted_op.weight)
        };
        println!("{}", op_msg);
        results_dir.write_console(&op_msg)?;
    }
    
    info!("Starting workload execution with {} operation types", config.workload.len());
    
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    
    // Verify-only mode: check that prepared objects exist and are accessible
    if verify {
        if let Some(ref prepare_config) = config.prepare {
            let verify_header = "\n=== Verification Phase ===";
            println!("{}", verify_header);
            results_dir.write_console(verify_header)?;
            
            info!("Verifying prepared objects");
            rt.block_on(workload::verify_prepared_objects(prepare_config))?;
            
            let verify_complete = "\nVerification complete: all prepared objects are accessible";
            println!("{}", verify_complete);
            results_dir.write_console(verify_complete)?;
            
            results_dir.finalize(0.0)?; // No wall time for verify-only
            return Ok(());
        } else {
            bail!("--verify requires 'prepare' section in config");
        }
    }
    
    // Execute prepare step if configured and not skipped
    let (prepared_objects, tree_manifest) = if !skip_prepare {
        if let Some(ref prepare_config) = config.prepare {
            let prepare_header = "\n=== Prepare Phase ===";
            println!("{}", prepare_header);
            results_dir.write_console(prepare_header)?;
            
            info!("Executing prepare step");
            let (prepared, manifest) = rt.block_on(workload::prepare_objects(prepare_config, Some(&config.workload)))?;
            
            let prepared_msg = format!("Prepared {} objects", prepared.len());
            println!("{}", prepared_msg);
            results_dir.write_console(&prepared_msg)?;
            
            // Use configurable delay from YAML (only if objects were created)
            if prepared.iter().any(|p| p.created) && prepare_config.post_prepare_delay > 0 {
                let delay_secs = prepare_config.post_prepare_delay;
                let delay_msg = format!("Waiting {}s for object propagation (configured delay)...", delay_secs);
                println!("{}", delay_msg);
                results_dir.write_console(&delay_msg)?;
                
                info!("Delaying {}s for eventual consistency (post_prepare_delay from config)", delay_secs);
                std::thread::sleep(std::time::Duration::from_secs(delay_secs));
            }
            
            (prepared, manifest)
        } else {
            (Vec::new(), None)
        }
    } else {
        info!("Skipping prepare phase (--skip-prepare flag)");
        (Vec::new(), None)
    };
    
    // If prepare-only mode, exit after preparation
    if prepare_only {
        if config.prepare.is_none() {
            bail!("--prepare-only requires 'prepare' section in config");
        }
        info!("Prepare-only mode: objects created, exiting");
        
        let prepare_only_msg = format!("\nPrepare-only mode: {} objects created, exiting", prepared_objects.len());
        println!("{}", prepare_only_msg);
        results_dir.write_console(&prepare_only_msg)?;
        
        results_dir.finalize(0.0)?; // No wall time for prepare-only
        return Ok(());
    }
    
    // Always show preparation status
    let test_header = "\n=== Test Phase ===";
    println!("{}", test_header);
    results_dir.write_console(test_header)?;
    
    let workload_msg = "Preparing workload...";
    println!("{}", workload_msg);
    results_dir.write_console(workload_msg)?;
    
    // Run the workload
    let summary = rt.block_on(workload::run(&config, tree_manifest))?;
    
    // Print results
    let results_header = "\n=== Results ===";
    println!("{}", results_header);
    results_dir.write_console(results_header)?;
    
    let config_header = "Configuration:";
    println!("{}", config_header);
    results_dir.write_console(config_header)?;
    
    let duration_msg = format!("  Duration: {:.2}s", summary.wall_seconds);
    println!("{}", duration_msg);
    results_dir.write_console(&duration_msg)?;
    
    let concurrency_msg = format!("  Concurrency: {} threads", config.concurrency);
    println!("{}", concurrency_msg);
    results_dir.write_console(&concurrency_msg)?;
    
    // Show actual operation distribution
    let dist_header = "\nActual Operation Distribution:";
    println!("{}", dist_header);
    results_dir.write_console(dist_header)?;
    
    if summary.get.ops > 0 {
        let get_pct = (summary.get.ops as f64 / summary.total_ops as f64) * 100.0;
        let get_msg = format!("  GET: {} ops ({:.1}%)", summary.get.ops, get_pct);
        println!("{}", get_msg);
        results_dir.write_console(&get_msg)?;
    }
    if summary.put.ops > 0 {
        let put_pct = (summary.put.ops as f64 / summary.total_ops as f64) * 100.0;
        let put_msg = format!("  PUT: {} ops ({:.1}%)", summary.put.ops, put_pct);
        println!("{}", put_msg);
        results_dir.write_console(&put_msg)?;
    }
    if summary.meta.ops > 0 {
        let meta_pct = (summary.meta.ops as f64 / summary.total_ops as f64) * 100.0;
        let meta_msg = format!("  META (LIST/STAT/DELETE): {} ops ({:.1}%)", summary.meta.ops, meta_pct);
        println!("{}", meta_msg);
        results_dir.write_console(&meta_msg)?;
    }
    
    let overall_header = "\nOverall Performance:";
    println!("{}", overall_header);
    results_dir.write_console(overall_header)?;
    
    let total_ops_msg = format!("  Total ops: {}", summary.total_ops);
    println!("{}", total_ops_msg);
    results_dir.write_console(&total_ops_msg)?;
    
    let total_bytes_msg = format!("  Total bytes: {} ({:.2} MiB)", summary.total_bytes, summary.total_bytes as f64 / 1_048_576.0);
    println!("{}", total_bytes_msg);
    results_dir.write_console(&total_bytes_msg)?;
    
    let throughput_msg = format!("  Throughput: {:.2} ops/s", summary.total_ops as f64 / summary.wall_seconds);
    println!("{}", throughput_msg);
    results_dir.write_console(&throughput_msg)?;
    
    if summary.get.ops > 0 {
        let get_mib_s = (summary.get.bytes as f64 / 1_048_576.0) / summary.wall_seconds;
        
        let get_header = "\nGET Performance:";
        println!("{}", get_header);
        results_dir.write_console(get_header)?;
        
        let get_ops_msg = format!("  Ops: {} ({:.2} ops/s)", summary.get.ops, summary.get.ops as f64 / summary.wall_seconds);
        println!("{}", get_ops_msg);
        results_dir.write_console(&get_ops_msg)?;
        
        let get_bytes_msg = format!("  Bytes: {} ({:.2} MiB)", summary.get.bytes, summary.get.bytes as f64 / 1_048_576.0);
        println!("{}", get_bytes_msg);
        results_dir.write_console(&get_bytes_msg)?;
        
        let get_throughput_msg = format!("  Throughput: {:.2} MiB/s", get_mib_s);
        println!("{}", get_throughput_msg);
        results_dir.write_console(&get_throughput_msg)?;
        
        let get_latency_msg = format!("  Latency: mean={}µs, p50={}µs, p95={}µs, p99={}µs", 
            summary.get.mean_us, summary.get.p50_us, summary.get.p95_us, summary.get.p99_us);
        println!("{}", get_latency_msg);
        results_dir.write_console(&get_latency_msg)?;
    }
    
    if summary.put.ops > 0 {
        let put_mib_s = (summary.put.bytes as f64 / 1_048_576.0) / summary.wall_seconds;
        
        let put_header = "\nPUT Performance:";
        println!("{}", put_header);
        results_dir.write_console(put_header)?;
        
        let put_ops_msg = format!("  Ops: {} ({:.2} ops/s)", summary.put.ops, summary.put.ops as f64 / summary.wall_seconds);
        println!("{}", put_ops_msg);
        results_dir.write_console(&put_ops_msg)?;
        
        let put_bytes_msg = format!("  Bytes: {} ({:.2} MiB)", summary.put.bytes, summary.put.bytes as f64 / 1_048_576.0);
        println!("{}", put_bytes_msg);
        results_dir.write_console(&put_bytes_msg)?;
        
        let put_throughput_msg = format!("  Throughput: {:.2} MiB/s", put_mib_s);
        println!("{}", put_throughput_msg);
        results_dir.write_console(&put_throughput_msg)?;
        
        let put_latency_msg = format!("  Latency: mean={}µs, p50={}µs, p95={}µs, p99={}µs", 
            summary.put.mean_us, summary.put.p50_us, summary.put.p95_us, summary.put.p99_us);
        println!("{}", put_latency_msg);
        results_dir.write_console(&put_latency_msg)?;
    }
    
    if summary.meta.ops > 0 {
        let meta_header = "\nMETA-DATA Performance:";
        println!("{}", meta_header);
        results_dir.write_console(meta_header)?;
        
        let meta_ops_msg = format!("  Ops: {} ({:.2} ops/s)", summary.meta.ops, summary.meta.ops as f64 / summary.wall_seconds);
        println!("{}", meta_ops_msg);
        results_dir.write_console(&meta_ops_msg)?;
        
        let meta_bytes_msg = format!("  Bytes: {} ({:.2} MiB)", summary.meta.bytes, summary.meta.bytes as f64 / 1_048_576.0);
        println!("{}", meta_bytes_msg);
        results_dir.write_console(&meta_bytes_msg)?;
        
        let meta_latency_msg = format!("  Latency: mean={}µs, p50={}µs, p95={}µs, p99={}µs", 
            summary.meta.mean_us, summary.meta.p50_us, summary.meta.p95_us, summary.meta.p99_us);
        println!("{}", meta_latency_msg);
        results_dir.write_console(&meta_latency_msg)?;
    }
    
    // Export TSV results to the results directory
    {
        use sai3_bench::tsv_export::TsvExporter;
        let tsv_path = results_dir.tsv_path();
        let tsv_msg = format!("\nExporting results to: {}", tsv_path.display());
        println!("{}", tsv_msg);
        results_dir.write_console(&tsv_msg)?;
        
        let exporter = TsvExporter::with_path(&tsv_path)?;
        exporter.export_results(
            &summary.get_hists,
            &summary.put_hists,
            &summary.meta_hists,
            &summary.get_bins,
            &summary.put_bins,
            &summary.meta_bins,
            summary.wall_seconds,
        )?;
        
        let export_complete_msg = format!("TSV results exported to: {}", tsv_path.display());
        println!("{}", export_complete_msg);
        results_dir.write_console(&export_complete_msg)?;
    }
    
    // Cleanup prepared objects if configured
    if let Some(ref prepare_config) = config.prepare {
        if prepare_config.cleanup && !prepared_objects.is_empty() {
            let cleanup_header = "\n=== Cleanup Phase ===";
            println!("{}", cleanup_header);
            results_dir.write_console(cleanup_header)?;
            
            info!("Cleaning up prepared objects");
            rt.block_on(workload::cleanup_prepared_objects(&prepared_objects))?;
            
            let cleanup_msg = "Cleanup complete";
            println!("{}", cleanup_msg);
            results_dir.write_console(cleanup_msg)?;
        }
    }
    
    // Finalize results directory with metadata
    results_dir.finalize(summary.wall_seconds)?;
    
    let final_msg = format!("\nResults saved to: {}", results_dir.path().display());
    println!("{}", final_msg);
    
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
    use sai3_bench::replay_streaming::{replay_workload_streaming, ReplayConfig};
    use sai3_bench::remap::RemapConfig;
    
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

