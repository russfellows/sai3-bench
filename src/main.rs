//
// Copyright (C) 2025 Russ Fellows, Signal65.com
// Licensed under the GNU General Public License v3.0 or later
//

// -----------------------------------------------------------------------------
// sai3-bench - Multi-protocol I/O benchmarking suite built on s3dlio
// -----------------------------------------------------------------------------

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use futures::{stream::FuturesUnordered, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::{Regex, escape};
use sai3_bench::config::Config;
use sai3_bench::metrics::{OpHists, bucket_index};
use sai3_bench::perf_log::{PerfLogWriter, PerfLogDeltaTracker};
use sai3_bench::workload;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Builder as RtBuilder;
use tokio::sync::Semaphore;
use tracing::{info, warn};
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
        
        /// Only execute cleanup step, then exit (for cleaning up after previous runs)
        /// Requires 'prepare' section in config. Uses cleanup_mode from config (default: tolerant).
        #[arg(long)]
        cleanup_only: bool,
        
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
    ///   sai3-bench replay --op-log /tmp/ops.tsv.zst --config backpressure.yaml
    ///   sai3-bench replay --op-log /tmp/ops.tsv.zst --dry-run
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
        
        /// Backpressure configuration file (YAML) for controlling lag thresholds and flap detection
        /// See docs/REPLAY_BACKPRESSURE.md for configuration options
        #[arg(long)]
        config: Option<std::path::PathBuf>,
        
        /// Speed multiplier (e.g., 2.0 = 2x faster, 0.5 = half speed)
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        
        /// Continue on errors instead of stopping
        #[arg(long)]
        continue_on_error: bool,
        
        /// Parse and validate op-log, check sort order, then exit (no execution)
        #[arg(long)]
        dry_run: bool,
    },
    /// Internal: Child worker process (multi-process mode)
    /// 
    /// This command is used internally by multi-process mode to spawn child worker processes.
    /// It reads config from stdin (JSON), runs the workload, and outputs Summary to stdout (JSON).
    /// 
    /// DO NOT INVOKE MANUALLY - this is for internal use only.
    #[command(hide = true)]
    InternalWorker {
        /// Worker ID (0-based index)
        #[arg(long)]
        worker_id: usize,
        
        /// Optional op-log file path for this worker
        #[arg(long)]
        op_log: Option<std::path::PathBuf>,
    },
    /// Sort op-log file(s) by start timestamp (offline operation)
    /// 
    /// Sorts one or more op-log files in chronological order by start timestamp.
    /// Creates new sorted files with ".sorted" suffix before extension.
    /// Original files are not modified.
    /// 
    /// Examples:
    ///   sai3-bench sort --files /tmp/ops.tsv.zst
    ///   sai3-bench sort --files worker0.tsv.zst worker1.tsv.zst worker2.tsv.zst
    ///   sai3-bench sort --files /tmp/*.tsv.zst --in-place
    Sort {
        /// Op-log file(s) to sort (supports multiple files)
        #[arg(long, required = true, num_args = 1..)]
        files: Vec<std::path::PathBuf>,
        
        /// Sort in-place (overwrite original files instead of creating .sorted versions)
        #[arg(long)]
        in_place: bool,
        
        /// Window size for streaming sort (default: 10000 lines)
        #[arg(long, default_value_t = 10000)]
        window_size: usize,
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
        Commands::Run { config, dry_run, prepare_only, cleanup_only, verify, skip_prepare, no_cleanup, tsv_name } => {
            run_workload(&config, dry_run, prepare_only, cleanup_only, verify, skip_prepare, no_cleanup, tsv_name.as_deref(), cli.op_log.as_deref())?
        }
        Commands::Replay { op_log, target, remap, config, speed, continue_on_error, dry_run } => {
            replay_cmd(op_log, target, remap, config, speed, continue_on_error, dry_run)?
        }
        Commands::InternalWorker { worker_id, op_log } => {
            // This is a child worker process - read config from stdin, run workload, output to stdout
            use std::io::{self, Read, Write};
            
            // Read config JSON from stdin
            let mut config_json = String::new();
            io::stdin().read_to_string(&mut config_json)
                .context("Failed to read config from stdin")?;
            let config: Config = serde_json::from_str(&config_json)
                .context("Failed to parse config JSON from stdin")?;
            
            // Initialize worker-specific op-logger if provided
            if let Some(ref op_log_path) = op_log {
                workload::init_operation_logger(op_log_path)
                    .with_context(|| format!("Worker {} failed to initialize op-logger at {}", worker_id, op_log_path.display()))?;
            }
            
            // Run the workload
            let rt = RtBuilder::new_multi_thread().enable_all().build()?;
            let summary = rt.block_on(async {
                workload::run(&config, None).await
            })?;
            
            // Finalize op-logger if enabled
            if op_log.is_some() {
                workload::finalize_operation_logger()
                    .with_context(|| format!("Worker {} failed to finalize op-logger", worker_id))?;
            }
            
            // Convert to IPC format and output Summary as JSON to stdout
            let ipc_summary = workload::IpcSummary::from(&summary);
            serde_json::to_writer(io::stdout(), &ipc_summary)
                .context("Failed to write summary JSON to stdout")?;
            io::stdout().flush()?;
            
            // Exit immediately (don't fall through to global logger finalization)
            return Ok(());
        }
        Commands::Sort { files, in_place, window_size } => {
            sort_oplog_files(&files, in_place, window_size)?
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

fn sort_oplog_files(files: &[std::path::PathBuf], in_place: bool, window_size: usize) -> Result<()> {
    use sai3_bench::oplog_merge;
    
    if files.is_empty() {
        bail!("No files provided to sort");
    }
    
    info!("Sorting {} op-log file(s) with window_size={}", files.len(), window_size);
    
    for file_path in files {
        if !file_path.exists() {
            bail!("File does not exist: {}", file_path.display());
        }
        
        // Determine output path
        let output_path = if in_place {
            // Create temp file, then rename
            let temp_path = file_path.with_extension("tmp.zst");
            oplog_merge::sort_oplog_file(file_path, &temp_path, window_size)
                .with_context(|| format!("Failed to sort file: {}", file_path.display()))?;
            
            // Replace original with sorted
            std::fs::rename(&temp_path, file_path)
                .with_context(|| format!("Failed to rename sorted file: {} -> {}", 
                                        temp_path.display(), file_path.display()))?;
            
            info!("✓ Sorted in-place: {}", file_path.display());
            continue;
        } else {
            // Create .sorted version
            let file_stem = file_path.file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("Invalid filename: {}", file_path.display()))?;
            
            // Handle .zst extension
            let sorted_name = if file_path.extension().and_then(|e| e.to_str()) == Some("zst") {
                // Remove .zst, add .sorted, add .zst back
                let base_stem = std::path::Path::new(file_stem)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(file_stem);
                format!("{}.sorted.tsv.zst", base_stem)
            } else {
                format!("{}.sorted.zst", file_stem)
            };
            
            file_path.with_file_name(sorted_name)
        };
        
        oplog_merge::sort_oplog_file(file_path, &output_path, window_size)
            .with_context(|| format!("Failed to sort file: {}", file_path.display()))?;
        
        info!("✓ Sorted: {} -> {}", file_path.display(), output_path.display());
    }
    
    info!("Successfully sorted {} file(s)", files.len());
    Ok(())
}

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
            let pattern_part = validated_uri.split('/').next_back().unwrap_or("*");
            if pattern_part.contains('*') {
                let pattern = format!("^{}$", escape(pattern_part).replace(r"\*", ".*"));
                let re = Regex::new(&pattern).context("Invalid glob pattern")?;
                objects.retain(|obj| {
                    let basename = obj.split('/').next_back().unwrap_or(obj);
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
            
            let pattern_part = validated_uri.split('/').next_back().unwrap_or("*");
            
            let mut found_objects = list_objects_multi_backend(dir_uri).await
                .context("Failed to list objects for pattern matching")?;
                
            if pattern_part.contains('*') {
                let pattern = format!("^{}$", escape(pattern_part).replace(r"\*", ".*"));
                let re = Regex::new(&pattern).context("Invalid glob pattern")?;
                found_objects.retain(|obj| {
                    let basename = obj.split('/').next_back().unwrap_or(obj);
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
            
            let pattern_part = validated_uri.split('/').next_back().unwrap_or("*");
            
            let mut found_objects = list_objects_multi_backend(dir_uri).await
                .context("Failed to list objects for pattern matching")?;
                
            if pattern_part.contains('*') {
                let pattern = format!("^{}$", escape(pattern_part).replace(r"\*", ".*"));
                let re = Regex::new(&pattern).context("Invalid glob pattern")?;
                found_objects.retain(|obj| {
                    let basename = obj.split('/').next_back().unwrap_or(obj);
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
// Note: This function is now in src/validation.rs and shared with controller
// Keeping a small wrapper here for backward compatibility
// -----------------------------------------------------------------------------
fn display_config_summary(config: &Config, config_path: &str) -> Result<()> {
    sai3_bench::validation::display_config_summary(config, config_path)
}

#[allow(clippy::too_many_arguments)]
fn run_workload(
    config_path: &str, 
    dry_run: bool, 
    prepare_only: bool,
    cleanup_only: bool,
    verify: bool, 
    skip_prepare: bool, 
    no_cleanup: bool, 
    tsv_name: Option<&str>,
    op_log_path: Option<&Path>,
) -> Result<()> {
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
    if prepare_only && cleanup_only {
        bail!("Cannot use both --prepare-only and --cleanup-only");
    }
    if cleanup_only && skip_prepare {
        bail!("Cannot use both --cleanup-only and --skip-prepare (cleanup requires knowing what objects were prepared)");
    }
    if cleanup_only && verify {
        bail!("Cannot use both --cleanup-only and --verify");
    }
    if cleanup_only && no_cleanup {
        bail!("Cannot use --cleanup-only with --no-cleanup");
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
    
    // Initialize operation logger if --op-log flag provided (v0.8.6+)
    if let Some(op_log_path) = op_log_path {
        workload::init_operation_logger(op_log_path)
            .with_context(|| format!("Failed to initialize operation logger at {}", op_log_path.display()))?;
        info!("Initialized operation logger: {}", op_log_path.display());
        
        // Set client_id for standalone mode (v0.8.6+)
        // Use "standalone" as default, or could use hostname
        let client_id = std::env::var("SAI3_CLIENT_ID").unwrap_or_else(|_| "standalone".to_string());
        s3dlio::set_client_id(&client_id)
            .context("Failed to set client_id for operation logger")?;
        info!("Set operation logger client_id: {}", client_id);
        
        // For standalone client, no clock offset needed (local time is reference)
        // s3dlio oplog will use local timestamps
    }
    
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
            let (prepared, manifest, prepare_metrics) = rt.block_on(workload::prepare_objects(
                prepare_config, 
                Some(&config.workload), 
                None, 
                config.concurrency,
                0,  // agent_id (standalone mode)
                1,  // num_agents (standalone mode)
            ))?;
            
            let prepared_msg = format!("Prepared {} objects ({} created, {} existed) in {:.2}s", 
                prepared.len(), prepare_metrics.objects_created, prepare_metrics.objects_existed, prepare_metrics.wall_seconds);
            println!("{}", prepared_msg);
            results_dir.write_console(&prepared_msg)?;
            
            // Print prepare performance summary
            if prepare_metrics.put.ops > 0 {
                let put_ops_s = prepare_metrics.put.ops as f64 / prepare_metrics.wall_seconds;
                let put_mib_s = (prepare_metrics.put.bytes as f64 / 1_048_576.0) / prepare_metrics.wall_seconds;
                
                let perf_header = "\nPrepare Performance:";
                println!("{}", perf_header);
                results_dir.write_console(perf_header)?;
                
                let ops_msg = format!("  Total ops: {} ({:.2} ops/s)", prepare_metrics.put.ops, put_ops_s);
                println!("{}", ops_msg);
                results_dir.write_console(&ops_msg)?;
                
                let bytes_msg = format!("  Total bytes: {} ({:.2} MiB)", prepare_metrics.put.bytes, prepare_metrics.put.bytes as f64 / 1_048_576.0);
                println!("{}", bytes_msg);
                results_dir.write_console(&bytes_msg)?;
                
                let throughput_msg = format!("  Throughput: {:.2} MiB/s", put_mib_s);
                println!("{}", throughput_msg);
                results_dir.write_console(&throughput_msg)?;
                
                let latency_msg = format!("  Latency: mean={:.2}ms, p50={:.2}ms, p95={:.2}ms, p99={:.2}ms",
                    prepare_metrics.put.mean_us as f64 / 1000.0,
                    prepare_metrics.put.p50_us as f64 / 1000.0,
                    prepare_metrics.put.p95_us as f64 / 1000.0,
                    prepare_metrics.put.p99_us as f64 / 1000.0);
                println!("{}", latency_msg);
                results_dir.write_console(&latency_msg)?;
            }
            
            if prepare_metrics.mkdir_count > 0 {
                let mkdir_summary = format!("  MKDIR: {} directories created", prepare_metrics.mkdir_count);
                println!("{}", mkdir_summary);
                results_dir.write_console(&mkdir_summary)?;
            }
            
            // Export prepare metrics to TSV
            if prepare_metrics.put.ops > 0 {
                use sai3_bench::tsv_export::TsvExporter;
                let prepare_tsv_path = results_dir.prepare_tsv_path();
                let exporter = TsvExporter::with_path(&prepare_tsv_path)?;
                exporter.export_prepare_metrics(&prepare_metrics)?;
                
                let export_msg = format!("Prepare metrics exported to: {}", prepare_tsv_path.display());
                println!("{}", export_msg);
                results_dir.write_console(&export_msg)?;
            }
            
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
    
    // If cleanup-only mode, skip to cleanup phase
    if cleanup_only {
        if config.prepare.is_none() {
            bail!("--cleanup-only requires 'prepare' section in config");
        }
        
        let cleanup_header = "\n=== Cleanup-Only Mode ===";
        println!("{}", cleanup_header);
        results_dir.write_console(cleanup_header)?;
        
        info!("Cleanup-only mode: cleaning up prepared objects");
        
        // For cleanup-only, we need to reconstruct the list of objects to delete
        // This uses the same logic as prepare to determine what objects should exist
        let prepare_config = config.prepare.as_ref().unwrap();
        
        // Recreate the tree manifest if directory_structure is configured
        let tree_manifest_for_cleanup = if prepare_config.directory_structure.is_some() {
            info!("Recreating directory tree structure for cleanup...");
            let base_uri = prepare_config.ensure_objects.first()
                .ok_or_else(|| anyhow!("directory_structure requires at least one ensure_objects entry"))?
                .base_uri.as_str();
            
            Some(sai3_bench::prepare::create_tree_manifest_only(
                prepare_config, 
                0,  // agent_id
                1,  // num_agents
                base_uri
            )?)
        } else {
            None
        };
        
        // Reconstruct object list (same logic as prepare, but we mark all as created=true)
        let mut objects_to_cleanup = Vec::new();
        for spec in &prepare_config.ensure_objects {
            let prefix = spec.base_uri.trim_end_matches('/');
            let prefix_name = prefix.rsplit('/').next().unwrap_or("prepared");
            
            if let Some(ref manifest) = tree_manifest_for_cleanup {
                // Tree mode: enumerate all file paths
                for file_idx in 0..manifest.total_files {
                    if let Some(relative_path) = manifest.get_file_path(file_idx) {
                        let uri = if spec.base_uri.ends_with('/') {
                            format!("{}{}", spec.base_uri, relative_path)
                        } else {
                            format!("{}/{}", spec.base_uri, relative_path)
                        };
                        objects_to_cleanup.push(sai3_bench::prepare::PreparedObject {
                            uri,
                            size: 0,  // Size doesn't matter for cleanup
                            created: true,
                        });
                    }
                }
            } else {
                // Flat mode: enumerate numbered files
                for idx in 0..spec.count {
                    let key = format!("{}-{:08}.dat", prefix_name, idx);
                    let uri = if spec.base_uri.ends_with('/') {
                        format!("{}{}", spec.base_uri, key)
                    } else {
                        format!("{}/{}", spec.base_uri, key)
                    };
                    objects_to_cleanup.push(sai3_bench::prepare::PreparedObject {
                        uri,
                        size: 0,
                        created: true,
                    });
                }
            }
        }
        
        let cleanup_msg = format!("Cleaning up {} objects...", objects_to_cleanup.len());
        println!("{}", cleanup_msg);
        results_dir.write_console(&cleanup_msg)?;
        
        rt.block_on(workload::cleanup_prepared_objects(
            &objects_to_cleanup,
            tree_manifest_for_cleanup.as_ref(),
            0,  // agent_id (standalone mode)
            1,  // num_agents (standalone mode)
            prepare_config.cleanup_mode,
            None,  // No live stats tracker in standalone mode
        ))?;
        
        let done_msg = "Cleanup complete";
        println!("{}", done_msg);
        results_dir.write_console(done_msg)?;
        
        results_dir.finalize(0.0)?; // No wall time for cleanup-only
        return Ok(());
    }
    
    // Always show preparation status
    let test_header = "\n=== Test Phase ===";
    println!("{}", test_header);
    results_dir.write_console(test_header)?;
    
    let workload_msg = "Preparing workload...";
    println!("{}", workload_msg);
    results_dir.write_console(workload_msg)?;
    
    // Determine process scaling configuration
    let num_processes = config.processes
        .as_ref()
        .map(|p| p.resolve())
        .unwrap_or(1); // Default to single process
    let processing_mode = config.processing_mode;
    
    // Handle op_log for multi-process execution
    // Only MultiProcess mode supports per-worker op-logs (separate processes).
    // MultiRuntime mode uses the global op-logger since all workers share one process.
    let needs_oplog_merge = num_processes > 1 
        && op_log_path.is_some() 
        && processing_mode == sai3_bench::config::ProcessingMode::MultiProcess;
    
    if needs_oplog_merge {
        info!("MultiProcess mode with op_log enabled - workers will write separate files");
        // Finalize the global op_logger before spawning worker processes
        workload::finalize_operation_logger()
            .context("Failed to finalize global operation logger")?;
    }
    
    // Run the workload using the configured processing mode
    let summary = if num_processes > 1 {
        // Multi-worker execution
        info!("Using {} mode with {} workers", 
              match processing_mode {
                  sai3_bench::config::ProcessingMode::MultiProcess => "MultiProcess",
                  sai3_bench::config::ProcessingMode::MultiRuntime => "MultiRuntime",
              },
              num_processes);
        
        match processing_mode {
            sai3_bench::config::ProcessingMode::MultiProcess => {
                // Multi-process mode: spawn N child processes
                rt.block_on(sai3_bench::multiprocess::run_multiprocess(&config, tree_manifest.clone(), op_log_path))?
            }
            sai3_bench::config::ProcessingMode::MultiRuntime => {
                // Multi-runtime mode: spawn N tokio runtimes in threads
                // Note: op_log not passed - all workers use global logger in single process
                sai3_bench::multiruntime::run_multiruntime(&config, num_processes, tree_manifest.clone())?
            }
        }
    } else {
        // Single worker - use traditional execution
        info!("Single worker mode (processes={})", num_processes);
        
        // v0.8.17: Setup performance logging for standalone mode (same as distributed)
        let perf_log_enabled = config.perf_log.is_some();
        let perf_log_path = results_dir.path().join("perf_log.tsv");
        let perf_log_writer_opt: Option<PerfLogWriter> = if perf_log_enabled {
            match PerfLogWriter::new(&perf_log_path) {
                Ok(writer) => {
                    info!("Created perf_log at: {}", perf_log_path.display());
                    Some(writer)
                }
                Err(e) => {
                    warn!("Failed to create perf_log: {} - continuing without perf_log", e);
                    None
                }
            }
        } else {
            None
        };
        
        // Create LiveStatsTracker for workload execution
        let tracker = Arc::new(sai3_bench::live_stats::LiveStatsTracker::new_with_concurrency(
            config.concurrency as u32
        ));
        
        // Initialize perf_log tracker with warmup duration
        let warmup_duration = config.warmup_period.unwrap_or(Duration::ZERO);
        let perf_log_interval = config.perf_log.as_ref()
            .map(|p| p.interval)
            .unwrap_or(Duration::from_secs(1));
        
        // Clone config and wire in LiveStatsTracker
        let mut config_with_tracker = config.clone();
        config_with_tracker.live_stats_tracker = Some(tracker.clone());
        
        // Spawn perf_log writer task if enabled
        let (perf_stop_tx, perf_stop_rx) = std::sync::mpsc::channel::<()>();
        let perf_log_task = if let Some(mut writer) = perf_log_writer_opt {
            let tracker_for_task = tracker.clone();
            let interval = perf_log_interval;
            let mut tracker_mut = PerfLogDeltaTracker::new();
            let warmup_opt = if warmup_duration > Duration::ZERO {
                Some(warmup_duration)
            } else {
                None
            };
            tracker_mut.start(warmup_opt);
            
            Some(std::thread::spawn(move || {
                loop {
                    std::thread::sleep(interval);
                    let stats = tracker_for_task.snapshot();
                    let entry = tracker_mut.compute_delta(
                        "standalone",
                        stats.get_ops,
                        stats.get_bytes,
                        stats.put_ops,
                        stats.put_bytes,
                        stats.meta_ops,
                        0,  // errors not tracked
                        stats.get_p50_us,
                        stats.get_p95_us,
                        stats.get_p99_us,
                        stats.put_p50_us,
                        stats.put_p95_us,
                        stats.put_p99_us,
                        stats.meta_mean_us,
                        stats.meta_mean_us,
                        stats.meta_mean_us,
                        0.0,  // cpu_user_percent not available
                        0.0,  // cpu_system_percent not available
                        0.0,  // cpu_iowait_percent not available
                        sai3_bench::live_stats::WorkloadStage::Workload,
                        String::new(),
                    );
                    if let Err(e) = writer.write_entry(&entry) {
                        warn!("Failed to write perf_log entry: {}", e);
                    }
                    
                    // Check if we should stop
                    if perf_stop_rx.try_recv().is_ok() {
                        break;
                    }
                }
            }))
        } else {
            None
        };
        
        // Run workload with LiveStatsTracker
        let summary = rt.block_on(workload::run(&config_with_tracker, tree_manifest.clone()))?;
        
        // Stop perf_log writer and wait for completion
        if let Some(handle) = perf_log_task {
            let _ = perf_stop_tx.send(());
            let _ = handle.join();
        }
        
        summary
    };
    
    // Merge worker op-log files if multi-worker mode was used with op_log enabled
    if needs_oplog_merge {
        if let Some(op_log_base) = op_log_path {
            info!("Merging worker op-log files...");
            let merged_path = sai3_bench::oplog_merge::merge_worker_oplogs(
                op_log_base,
                num_processes,
                false, // Delete worker files after merge
            )?;
            
            let merge_msg = format!("\nOp-log merged: {}", merged_path.display());
            println!("{}", merge_msg);
            info!("Op-log merge complete: {}", merged_path.display());
        }
    }
    
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
            rt.block_on(workload::cleanup_prepared_objects(
                &prepared_objects,
                tree_manifest.as_ref(),
                0,  // agent_id (standalone mode)
                1,  // num_agents (standalone mode)
                prepare_config.cleanup_mode,
                None,  // No live stats tracker in standalone mode
            ))?;
            
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
    config_path: Option<std::path::PathBuf>,
    speed: f64,
    continue_on_error: bool,
    dry_run: bool,
) -> Result<()> {
    use sai3_bench::replay_streaming::{replay_workload_streaming, ReplayRunConfig};
    use sai3_bench::remap::RemapConfig;
    use sai3_bench::config::ReplayConfig;
    use sai3_bench::oplog_merge;
    
    // Dry-run mode: validate op-log file and check sort order
    if dry_run {
        println!("Dry-run mode: Validating replay op-log file...");
        println!("  File: {}", op_log.display());
        
        if !op_log.exists() {
            bail!("Op-log file does not exist: {}", op_log.display());
        }
        
        println!("  Checking sort order (first 10,000 lines)...");
        match oplog_merge::check_oplog_sorted(&op_log, Some(10000)) {
            Ok((is_sorted, lines_checked, first_ooo_line)) => {
                if is_sorted {
                    println!("  ✓ Op-log is sorted ({} lines checked)", lines_checked);
                } else {
                    println!("  ⚠️  WARNING: Op-log is NOT sorted!");
                    println!("      First out-of-order line: {}", first_ooo_line.unwrap_or(0));
                    println!("      Replay will issue operations out of chronological order.");
                    println!("      Use 'sai3-bench sort --files {}' to sort the file.", op_log.display());
                }
            }
            Err(e) => {
                println!("  ⚠️  WARNING: Failed to check sort order: {}", e);
            }
        }
        
        // Count total operations
        match s3dlio_oplog::OpLogStreamReader::from_file(&op_log) {
            Ok(mut reader) => {
                let mut count = 0;
                while let Some(Ok(_)) = reader.next() {
                    count += 1;
                }
                println!("  Total operations: {}", count);
            }
            Err(e) => {
                println!("  ⚠️  WARNING: Failed to count operations: {}", e);
            }
        }
        
        if let Some(ref uri) = target {
            println!("  Target URI: {}", uri);
            validate_uri(uri)?;
            println!("  ✓ Target URI is valid");
        }
        
        if let Some(ref remap_path) = remap {
            println!("  Remap config: {}", remap_path.display());
            if !remap_path.exists() {
                bail!("Remap config file does not exist: {}", remap_path.display());
            }
            // Try to parse it
            let file = std::fs::File::open(remap_path)
                .with_context(|| format!("Failed to open remap config: {}", remap_path.display()))?;
            let config: RemapConfig = serde_yaml::from_reader(file)
                .with_context(|| format!("Failed to parse remap config: {}", remap_path.display()))?;
            println!("  ✓ Remap config is valid ({} rules)", config.rules.len());
        }
        
        if let Some(ref bp_config_path) = config_path {
            println!("  Backpressure config: {}", bp_config_path.display());
            if !bp_config_path.exists() {
                bail!("Backpressure config file does not exist: {}", bp_config_path.display());
            }
            let file = std::fs::File::open(bp_config_path)
                .with_context(|| format!("Failed to open backpressure config: {}", bp_config_path.display()))?;
            let bp_config: ReplayConfig = serde_yaml::from_reader(file)
                .with_context(|| format!("Failed to parse backpressure config: {}", bp_config_path.display()))?;
            println!("  ✓ Backpressure config is valid");
            println!("      lag_threshold: {:?}", bp_config.lag_threshold);
            println!("      recovery_threshold: {:?}", bp_config.recovery_threshold);
            println!("      max_flaps_per_minute: {}", bp_config.max_flaps_per_minute);
            println!("      drain_timeout: {:?}", bp_config.drain_timeout);
            println!("      max_concurrent: {}", bp_config.max_concurrent);
        }
        
        println!("\n✓ Dry-run validation complete");
        return Ok(());
    }
    
    // Normal execution mode
    // Validate target URI if provided
    if let Some(ref uri) = target {
        validate_uri(uri)?;
    }
    
    // Load backpressure configuration if provided
    let backpressure_config = if let Some(ref bp_config_path) = config_path {
        println!("Loading backpressure configuration from: {}", bp_config_path.display());
        let file = std::fs::File::open(bp_config_path)
            .with_context(|| format!("Failed to open backpressure config: {}", bp_config_path.display()))?;
        let bp_config: ReplayConfig = serde_yaml::from_reader(file)
            .with_context(|| format!("Failed to parse backpressure config: {}", bp_config_path.display()))?;
        info!("Loaded backpressure config: lag_threshold={:?}, recovery_threshold={:?}, max_flaps={}",
              bp_config.lag_threshold, bp_config.recovery_threshold, bp_config.max_flaps_per_minute);
        println!("  lag_threshold: {:?}, recovery_threshold: {:?}, max_flaps: {}/min",
                 bp_config.lag_threshold, bp_config.recovery_threshold, bp_config.max_flaps_per_minute);
        Some(bp_config)
    } else {
        None
    };
    
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
    
    // Use max_concurrent from backpressure config if provided, otherwise default
    let max_concurrent = backpressure_config
        .as_ref()
        .map(|c| c.max_concurrent)
        .unwrap_or(1000);
    
    let config = ReplayRunConfig {
        op_log_path: op_log,
        target_uri: target,
        speed,
        continue_on_error,
        max_concurrent: Some(max_concurrent),
        remap_config,
        backpressure: backpressure_config,
    };
    
    let rt = RtBuilder::new_multi_thread().enable_all().build()?;
    let stats = rt.block_on(replay_workload_streaming(config))?;
    
    // Print summary statistics
    println!("\nReplay Summary:");
    println!("  Total operations: {}", stats.total_operations);
    println!("  Completed: {}", stats.completed_operations);
    println!("  Failed: {}", stats.failed_operations);
    println!("  Skipped: {}", stats.skipped_operations);
    
    // Print backpressure statistics if any mode transitions occurred
    if stats.mode_transitions > 0 || stats.peak_lag.as_millis() > 0 {
        println!("\nBackpressure Statistics:");
        println!("  Mode transitions: {}", stats.mode_transitions);
        println!("  Peak lag: {:?}", stats.peak_lag);
        println!("  Time in best-effort mode: {:?}", stats.best_effort_time);
        if stats.flap_exit {
            println!("  ⚠️  Exited due to flap limit (mode oscillation detected)");
        }
    }
    
    Ok(())
}

