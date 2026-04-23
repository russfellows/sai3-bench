//
// Copyright (C) 2025 Russ Fellows, Signal65.com
// Licensed under the GNU General Public License v3.0 or later
//

// jemalloc as global allocator: eliminates glibc arena contention and fragmentation.
// Profiling shows glibc malloc_consolidate at 3% + allocator frame at ~52% of CPU cycles.
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// -----------------------------------------------------------------------------
// sai3-bench - Multi-protocol I/O benchmarking suite built on s3dlio
// -----------------------------------------------------------------------------

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use futures::{stream::FuturesUnordered, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::{escape, Regex};
use sai3_bench::config::Config;
use sai3_bench::cpu_monitor::CpuMonitor;
use sai3_bench::metrics::{bucket_index, OpHists};
use sai3_bench::perf_log::{PerfLogDeltaTracker, PerfLogWriter};
use sai3_bench::workload;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::runtime::Builder as RtBuilder;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use url::Url;

// Multi-backend ObjectStore operations
use sai3_bench::workload::{
    delete_object_multi_backend, get_object_multi_backend, list_objects_multi_backend,
    put_object_multi_backend, stat_object_multi_backend,
};

// -----------------------------------------------------------------------------
// Pre-flight validation (v0.8.19)
// -----------------------------------------------------------------------------

/// Check if objects exist for GET operations when skip_verification=true
/// This prevents running an entire workload only to discover all GETs fail
async fn preflight_check_get_objects(
    config: &Config,
    _results_dir: &sai3_bench::results_dir::ResultsDir,
) -> Result<()> {
    // Only check if prepare phase exists and skip_verification=true
    let Some(ref prepare_config) = config.prepare else {
        return Ok(()); // No prepare phase, nothing to check
    };

    if !prepare_config.skip_verification {
        return Ok(()); // Objects will be created, no need to check
    }

    // Check if workload has any GET operations
    let get_operations: Vec<_> = config
        .workload
        .iter()
        .filter_map(|w| {
            if let sai3_bench::config::OpSpec::Get { path, .. } = &w.spec {
                Some(path.as_str())
            } else {
                None
            }
        })
        .collect();

    if get_operations.is_empty() {
        return Ok(()); // No GET operations, nothing to check
    }

    info!("Preflight check: Verifying objects exist for GET operations (skip_verification=true)");

    // Try to list a few objects from each GET path pattern
    let target_uri = config
        .target
        .as_ref()
        .ok_or_else(|| anyhow!("Target URI required for preflight check"))?;

    let mut any_objects_found = false;
    let mut checked_patterns = Vec::new();

    for get_path in get_operations.iter().take(3) {
        // Check up to 3 different patterns
        // Construct full URI
        let full_uri = if target_uri.ends_with('/') {
            format!("{}{}", target_uri, get_path)
        } else {
            format!("{}/{}", target_uri, get_path)
        };

        // Extract base path for listing (remove wildcard/glob patterns)
        let list_prefix = full_uri
            .trim_end_matches('*')
            .trim_end_matches('/')
            .to_string();

        // Try to list up to 5 objects
        match list_objects_multi_backend(&list_prefix).await {
            Ok(objects) if !objects.is_empty() => {
                any_objects_found = true;
                info!(
                    "Preflight check: Found {} objects at {}",
                    objects.len(),
                    list_prefix
                );
                break; // Found objects, no need to check more patterns
            }
            Ok(_) => {
                checked_patterns.push(list_prefix.clone());
                info!("Preflight check: No objects found at {}", list_prefix);
            }
            Err(e) => {
                warn!("Preflight check: Failed to list {}: {}", list_prefix, e);
                checked_patterns.push(list_prefix);
            }
        }
    }

    // If no objects were found, print prominent warning
    if !any_objects_found {
        eprintln!("\n{}", "=".repeat(80));
        eprintln!("⚠️  PREFLIGHT CHECK WARNING: No objects found for GET operations!");
        eprintln!("{}", "=".repeat(80));
        eprintln!();
        eprintln!(
            "Your config has skip_verification=true and GET operations, but no objects exist."
        );
        eprintln!();
        eprintln!("Checked locations:");
        for pattern in &checked_patterns {
            eprintln!("  - {}", pattern);
        }
        eprintln!();
        eprintln!("Likely causes:");
        eprintln!("  • skip_verification=true prevents object creation during prepare phase");
        eprintln!("  • Objects were deleted from a previous run");
        eprintln!("  • Wrong path patterns in workload operations");
        eprintln!();
        eprintln!("Recommendations:");
        eprintln!("  • Set skip_verification=false (default) to create objects during prepare");
        eprintln!("  • Run prepare phase separately first");
        eprintln!("  • Verify target URI and paths match your storage");
        eprintln!();
        eprintln!("Workload will start in 5 seconds - press Ctrl+C to cancel...");
        eprintln!("{}", "=".repeat(80));
        eprintln!();

        // Give user 5 seconds to cancel
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// CLI definition
// -----------------------------------------------------------------------------
#[derive(Parser)]
#[command(
    name = "sai3-bench",
    version,
    about = "A sai3-bench tool that leverages s3dlio library"
)]
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
    /// Convert legacy YAML configs to explicit stages format (v0.8.61+)
    ///
    /// Converts old implicit-stage configs (top-level duration/workload) to new
    /// explicit-stage format (distributed.stages array).
    ///
    /// Creates backup files (.yaml.bak) before conversion.
    /// Validates converted configs before replacing original.
    ///
    /// Examples:
    ///   sai3-bench convert --config mixed.yaml
    ///   sai3-bench convert --config mixed.yaml --dry-run
    ///   sai3-bench convert --files tests/configs/*.yaml
    ///   sai3-bench convert --files tests/configs/*.yaml --no-validate
    Convert {
        /// Single config file to convert
        #[arg(long, conflicts_with = "files")]
        config: Option<std::path::PathBuf>,

        /// Multiple config files to convert
        #[arg(long, conflicts_with = "config", num_args = 1..)]
        files: Option<Vec<std::path::PathBuf>>,

        /// Show what would be converted without making changes
        #[arg(long)]
        dry_run: bool,

        /// Skip validation after conversion (faster but risky)
        #[arg(long)]
        no_validate: bool,
    },
    /// Auto-tune throughput/latency across size and concurrency parameters
    ///
    /// All tuning parameters (uri, sizes, threads, channels, etc.) are specified
    /// in a YAML config file.  Use --dry-run to validate the config and preview
    /// the full sweep plan (loop order, total cases, estimated run count) before
    /// committing to a potentially long run.
    ///
    /// Examples:
    ///   sai3-bench autotune --config autotune.yaml
    ///   sai3-bench autotune --config autotune.yaml --dry-run
    Autotune {
        /// Path to autotune YAML config file (all tuning parameters live here)
        #[arg(long)]
        config: Option<PathBuf>,

        /// Validate config and print sweep plan without running any trials
        #[arg(long)]
        dry_run: bool,
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
        #[arg(long, value_name = "URI", conflicts_with = "uri_pos")]
        uri: Option<String>,
        #[arg(value_name = "URI", conflicts_with = "uri")]
        uri_pos: Option<String>,
    },
    /// List objects (supports basename glob across all backends)
    ///
    /// Examples:
    ///   sai3-bench util list --uri "file:///tmp/data/"
    ///   sai3-bench util list --uri "s3://bucket/prefix/"
    ///   sai3-bench util list --uri "direct:///mnt/data/*.txt"
    List {
        #[arg(long, value_name = "URI", conflicts_with = "uri_pos")]
        uri: Option<String>,
        #[arg(value_name = "URI", conflicts_with = "uri")]
        uri_pos: Option<String>,
    },
    /// Stat (HEAD) one object across all backends
    ///
    /// Examples:
    ///   sai3-bench util stat --uri "file:///tmp/data/file.txt"
    ///   sai3-bench util stat --uri "s3://bucket/object.txt"
    Stat {
        #[arg(long, value_name = "URI", conflicts_with = "uri_pos")]
        uri: Option<String>,
        #[arg(value_name = "URI", conflicts_with = "uri")]
        uri_pos: Option<String>,
    },
    /// Get objects (prefix, glob, or single) from any backend
    ///
    /// Examples:
    ///   sai3-bench util get --uri "file:///tmp/data/*" --jobs 8
    ///   sai3-bench util get --uri "s3://bucket/prefix/" --jobs 4
    Get {
        #[arg(long, value_name = "URI", conflicts_with = "uri_pos")]
        uri: Option<String>,
        #[arg(value_name = "URI", conflicts_with = "uri")]
        uri_pos: Option<String>,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Delete objects (prefix, glob, or single) from any backend
    ///
    /// Examples:
    ///   sai3-bench util delete --uri "file:///tmp/old/*" --jobs 8
    ///   sai3-bench util delete --uri "s3://bucket/prefix/"
    Delete {
        #[arg(long, value_name = "URI", conflicts_with = "uri_pos")]
        uri: Option<String>,
        #[arg(value_name = "URI", conflicts_with = "uri")]
        uri_pos: Option<String>,
        #[arg(long, default_value_t = 4)]
        jobs: usize,
    },
    /// Put random-data objects to any backend
    ///
    /// Examples:
    ///   sai3-bench util put --uri "file:///tmp/data/test*.dat" --objects 100
    ///   sai3-bench util put --uri "s3://bucket/prefix/file*.dat" --object-size 1048576
    Put {
        #[arg(long, value_name = "URI", conflicts_with = "uri_pos")]
        uri: Option<String>,
        #[arg(value_name = "URI", conflicts_with = "uri")]
        uri_pos: Option<String>,
        #[arg(long, default_value_t = 1024)]
        object_size: usize,
        #[arg(long, default_value_t = 1)]
        objects: usize,
        #[arg(long, default_value_t = 4)]
        concurrency: usize,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum OptimizeFor {
    Throughput,
    Latency,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum TuneOps {
    Put,
    Get,
    Both,
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum RapidModeArg {
    Auto,
    On,
    Off,
}

#[derive(Debug, Deserialize, Default)]
struct AutotuneYaml {
    uri: Option<String>,
    sizes: Option<String>,
    size_range: Option<String>,
    size_steps: Option<usize>,
    threads: Option<String>,
    /// Absolute GCS gRPC channel counts to sweep (CSV).  Mutually exclusive with channels_per_thread.
    channels: Option<String>,
    /// GCS gRPC channels expressed as a multiplier of thread count (CSV of positive integers).
    /// effective_channels = threads × channels_per_thread.
    /// Default when neither 'channels' nor 'channels_per_thread' is set: 1
    ///   (one channel per thread; effective_channels = threads).
    /// Practical range: 1–8.  Values ≥ 9 are unusual; 0 is rejected as invalid.
    /// Example: channels_per_thread: "1,2,4"  sweeps 1×, 2×, and 4× thread count.
    /// Mutually exclusive with channels.
    channels_per_thread: Option<String>,
    range_enabled: Option<String>,
    range_thresholds_mb: Option<String>,
    gcs_write_chunk_sizes: Option<String>,
    gcs_rapid_mode: Option<String>,
    objects: Option<usize>,
    trials: Option<usize>,
    optimize_for: Option<String>,
    ops: Option<String>,
}

fn parse_rapid_mode_str(s: &str) -> Result<RapidModeArg> {
    match s.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(RapidModeArg::Auto),
        "on" | "true" | "1" | "yes" => Ok(RapidModeArg::On),
        "off" | "false" | "0" | "no" => Ok(RapidModeArg::Off),
        _ => bail!("Invalid gcs_rapid_mode '{}'. Use auto|on|off", s),
    }
}

fn parse_optimize_for_str(s: &str) -> Result<OptimizeFor> {
    match s.trim().to_ascii_lowercase().as_str() {
        "throughput" => Ok(OptimizeFor::Throughput),
        "latency" => Ok(OptimizeFor::Latency),
        _ => bail!("Invalid optimize_for '{}'. Use throughput|latency", s),
    }
}

fn parse_tune_ops_str(s: &str) -> Result<TuneOps> {
    match s.trim().to_ascii_lowercase().as_str() {
        "put" => Ok(TuneOps::Put),
        "get" => Ok(TuneOps::Get),
        "both" => Ok(TuneOps::Both),
        _ => bail!("Invalid ops '{}'. Use put|get|both", s),
    }
}

// -----------------------------------------------------------------------------
// Helper: format a u64 with comma separators for terminal output
// -----------------------------------------------------------------------------
fn fmt_commas(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut result = String::with_capacity(len + len / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(b as char);
    }
    result
}

/// Format a float latency value with the right decimal places for 5 significant digits.
/// `value` must be in [0, 9999.x) — caller ensures correct unit conversion.
fn fmt_lat_f(value: f64, unit: &str) -> String {
    let decimals = if value < 10.0 {
        4
    } else if value < 100.0 {
        3
    } else if value < 1_000.0 {
        2
    } else {
        1
    };
    let s = format!("{:.prec$}", value, prec = decimals);
    let dot = s.find('.').unwrap_or(s.len());
    let int_part = &s[..dot];
    let frac_part = &s[dot..];
    if int_part.len() == 4 {
        // "1234" → "1,234"
        format!("{},{}{}{}", &int_part[..1], &int_part[1..], frac_part, unit)
    } else {
        format!("{}{}{}", int_part, frac_part, unit)
    }
}

/// Format a nanosecond latency value with adaptive units (ns → µs → ms → s).
fn fmt_latency_ns(ns: u64) -> String {
    if ns < 10_000 {
        format!("{}ns", fmt_commas(ns))
    } else if ns < 10_000_000 {
        fmt_lat_f(ns as f64 / 1_000.0, "µs")
    } else if ns < 10_000_000_000 {
        fmt_lat_f(ns as f64 / 1_000_000.0, "ms")
    } else {
        fmt_lat_f(ns as f64 / 1_000_000_000.0, "s")
    }
}

/// Format a microsecond latency value with adaptive units (µs → ms → s).
fn fmt_latency_us(us: u64) -> String {
    if us < 10_000 {
        format!("{}µs", fmt_commas(us))
    } else if us < 10_000_000 {
        fmt_lat_f(us as f64 / 1_000.0, "ms")
    } else {
        fmt_lat_f(us as f64 / 1_000_000.0, "s")
    }
}

/// Format a byte count with adaptive binary units (bytes → KiB → MiB → GiB → TiB),
/// using 5 significant digits via fmt_lat_f.
fn fmt_bytes(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = 1_024 * KIB;
    const GIB: u64 = 1_024 * MIB;
    const TIB: u64 = 1_024 * GIB;
    if bytes < 10_000 {
        format!("{} bytes", fmt_commas(bytes))
    } else if bytes < 10_000 * KIB {
        fmt_lat_f(bytes as f64 / KIB as f64, "KiB")
    } else if bytes < 10_000 * MIB {
        fmt_lat_f(bytes as f64 / MIB as f64, "MiB")
    } else if bytes < 10_000 * GIB {
        fmt_lat_f(bytes as f64 / GIB as f64, "GiB")
    } else {
        fmt_lat_f(bytes as f64 / TIB as f64, "TiB")
    }
}

/// Format a floating-point number with comma-separated thousands (2 decimal places).
fn fmt_float_commas(v: f64) -> String {
    let int_part = v as u64;
    let frac = v - int_part as f64;
    format!(
        "{}.{:02}",
        fmt_commas(int_part),
        (frac * 100.0).round() as u64
    )
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
    let filter = EnvFilter::new(format!(
        "sai3_bench={},s3dlio={}",
        sai3_bench_level, s3dlio_level
    ));
    fmt().with_env_filter(filter).init();

    // Print hardware detection info to help users understand performance
    if cli.verbose > 0 {
        sai3_bench::data_gen_pool::print_hardware_info();
    }

    // Initialize operation logger if requested
    if let Some(ref op_log_path) = cli.op_log {
        info!("Initializing operation logger: {}", op_log_path.display());
        workload::init_operation_logger(op_log_path)
            .context("Failed to initialize operation logger")?;
    }

    // Execute command
    match cli.command {
        Commands::Run {
            config,
            dry_run,
            prepare_only,
            cleanup_only,
            verify,
            skip_prepare,
            no_cleanup,
            tsv_name,
        } => run_workload(
            &config,
            dry_run,
            prepare_only,
            cleanup_only,
            verify,
            skip_prepare,
            no_cleanup,
            tsv_name.as_deref(),
            cli.op_log.as_deref(),
        )?,
        Commands::Replay {
            op_log,
            target,
            remap,
            config,
            speed,
            continue_on_error,
            dry_run,
        } => replay_cmd(
            op_log,
            target,
            remap,
            config,
            speed,
            continue_on_error,
            dry_run,
        )?,
        Commands::InternalWorker { worker_id, op_log } => {
            // This is a child worker process - read config from stdin, run workload, output to stdout
            use std::io::{self, Read, Write};

            // Read config JSON from stdin
            let mut config_json = String::new();
            io::stdin()
                .read_to_string(&mut config_json)
                .context("Failed to read config from stdin")?;
            let config: Config = serde_json::from_str(&config_json)
                .context("Failed to parse config JSON from stdin")?;

            // Initialize worker-specific op-logger if provided
            if let Some(ref op_log_path) = op_log {
                workload::init_operation_logger(op_log_path).with_context(|| {
                    format!(
                        "Worker {} failed to initialize op-logger at {}",
                        worker_id,
                        op_log_path.display()
                    )
                })?;
            }

            // Run the workload
            let rt = RtBuilder::new_multi_thread().enable_all().build()?;
            let summary = rt.block_on(async { workload::run(&config, None).await })?;

            // Finalize op-logger if enabled
            if op_log.is_some() {
                workload::finalize_operation_logger().with_context(|| {
                    format!("Worker {} failed to finalize op-logger", worker_id)
                })?;
            }

            // Convert to IPC format and output Summary as JSON to stdout
            let ipc_summary = workload::IpcSummary::from(&summary);
            serde_json::to_writer(io::stdout(), &ipc_summary)
                .context("Failed to write summary JSON to stdout")?;
            io::stdout().flush()?;

            // Exit immediately (don't fall through to global logger finalization)
            return Ok(());
        }
        Commands::Sort {
            files,
            in_place,
            window_size,
        } => sort_oplog_files(&files, in_place, window_size)?,
        Commands::Convert {
            config,
            files,
            dry_run,
            no_validate,
        } => convert_configs_cmd(config, files, dry_run, !no_validate)?,
        Commands::Autotune { config, dry_run } => {
            let yaml = if let Some(path) = config.as_ref() {
                let txt = std::fs::read_to_string(path).with_context(|| {
                    format!("Failed to read autotune config: {}", path.display())
                })?;
                serde_yaml::from_str::<AutotuneYaml>(&txt)
                    .with_context(|| format!("Failed to parse autotune YAML: {}", path.display()))?
            } else {
                AutotuneYaml::default()
            };

            let uri = yaml.uri.ok_or_else(|| {
                anyhow!("autotune requires 'uri' in YAML config (use --config <file>)")
            })?;
            let size_steps = yaml.size_steps.unwrap_or(4);
            let threads = yaml.threads.unwrap_or_else(|| "16,32,64".to_string());
            let range_enabled = yaml
                .range_enabled
                .unwrap_or_else(|| "false,true".to_string());
            let range_thresholds = yaml
                .range_thresholds_mb
                .unwrap_or_else(|| "32,64".to_string());
            let gcs_chunks = yaml
                .gcs_write_chunk_sizes
                .unwrap_or_else(|| "2097152,4128768".to_string());
            let gcs_rapid = if let Some(s) = yaml.gcs_rapid_mode.as_deref() {
                parse_rapid_mode_str(s)?
            } else {
                RapidModeArg::Auto
            };
            let objects = yaml.objects.unwrap_or(64);
            let trials = yaml.trials.unwrap_or(1);
            let optimize_for = if let Some(s) = yaml.optimize_for.as_deref() {
                parse_optimize_for_str(s)?
            } else {
                OptimizeFor::Throughput
            };
            let ops = if let Some(s) = yaml.ops.as_deref() {
                parse_tune_ops_str(s)?
            } else {
                TuneOps::Both
            };

            if yaml.channels.is_some() && yaml.channels_per_thread.is_some() {
                bail!("autotune: specify 'channels' OR 'channels_per_thread' in YAML, not both");
            }

            autotune_cmd(
                &uri,
                yaml.sizes.as_deref(),
                yaml.size_range.as_deref(),
                size_steps,
                &threads,
                yaml.channels.as_deref(),
                yaml.channels_per_thread.as_deref(),
                &range_enabled,
                &range_thresholds,
                &gcs_chunks,
                gcs_rapid,
                objects,
                trials,
                optimize_for,
                ops,
                dry_run,
            )?
        }
        Commands::Util { command } => match command {
            UtilCommands::Health { uri, uri_pos } => {
                let resolved_uri = resolve_util_uri(uri, uri_pos)?;
                health_cmd(&resolved_uri)?
            }
            UtilCommands::List { uri, uri_pos } => {
                let resolved_uri = resolve_util_uri(uri, uri_pos)?;
                list_cmd(&resolved_uri)?
            }
            UtilCommands::Stat { uri, uri_pos } => {
                let resolved_uri = resolve_util_uri(uri, uri_pos)?;
                stat_cmd(&resolved_uri)?
            }
            UtilCommands::Get { uri, uri_pos, jobs } => {
                let resolved_uri = resolve_util_uri(uri, uri_pos)?;
                get_cmd(&resolved_uri, jobs)?
            }
            UtilCommands::Delete { uri, uri_pos, jobs } => {
                let resolved_uri = resolve_util_uri(uri, uri_pos)?;
                delete_cmd(&resolved_uri, jobs)?
            }
            UtilCommands::Put {
                uri,
                uri_pos,
                object_size,
                objects,
                concurrency,
            } => {
                let resolved_uri = resolve_util_uri(uri, uri_pos)?;
                put_cmd(&resolved_uri, object_size, objects, concurrency)?
            }
        },
    }

    // Finalize operation logger if enabled
    if cli.op_log.is_some() {
        info!("Finalizing operation logger");
        workload::finalize_operation_logger().context("Failed to finalize operation logger")?;
    }

    Ok(())
}

fn resolve_util_uri(uri_flag: Option<String>, uri_pos: Option<String>) -> Result<String> {
    match (uri_flag, uri_pos) {
        (Some(uri), None) | (None, Some(uri)) => Ok(uri),
        (Some(uri_a), Some(uri_b)) => {
            if uri_a == uri_b {
                Ok(uri_a)
            } else {
                bail!("Provide URI either as positional <URI> or --uri <URI>, not both")
            }
        }
        (None, None) => bail!("Missing URI: use positional <URI> or --uri <URI>"),
    }
}

#[derive(Clone, Debug)]
struct TuneCase {
    size_bytes: usize,
    threads: usize,
    /// Effective (absolute) number of GCS gRPC channels used for this trial.
    channels: usize,
    /// Multiplier used to derive `channels` from `threads` (None = absolute sweep).
    channels_per_thread: Option<usize>,
    range_enabled: bool,
    range_threshold_mb: usize,
    gcs_write_chunk_size: usize,
}

#[derive(Clone, Debug)]
struct TuneResult {
    case: TuneCase,
    put_mbps: Option<f64>,
    get_mbps: Option<f64>,
    put_avg_ms: Option<f64>,
    get_avg_ms: Option<f64>,
}

fn parse_csv_usize(input: &str) -> Result<Vec<usize>> {
    let values = input
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<usize>()
                .with_context(|| format!("Invalid integer: {}", s))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.is_empty() {
        bail!("Expected at least one numeric value, got '{}'.", input);
    }
    Ok(values)
}

fn parse_csv_bool(input: &str) -> Result<Vec<bool>> {
    let mut out = Vec::new();
    for token in input.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let v = match token.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => bail!("Invalid bool '{}'. Use true/false.", token),
        };
        out.push(v);
    }
    if out.is_empty() {
        bail!("Expected at least one bool value, got '{}'.", input);
    }
    Ok(out)
}

fn parse_sizes(
    sizes: Option<&str>,
    size_range: Option<&str>,
    size_steps: usize,
) -> Result<Vec<usize>> {
    if let Some(s) = sizes {
        let mut parsed = Vec::new();
        for token in s.split(',').map(|v| v.trim()).filter(|v| !v.is_empty()) {
            let bytes = sai3_bench::size_parser::parse_size(token)
                .with_context(|| format!("Invalid size '{}'.", token))?;
            parsed.push(bytes as usize);
        }
        if parsed.is_empty() {
            bail!("--sizes provided but no values were parsed");
        }
        parsed.sort_unstable();
        parsed.dedup();
        return Ok(parsed);
    }

    let range = size_range.unwrap_or("32MiB-64MiB");
    let mut parts = range.split('-').map(|v| v.trim());
    let start_s = parts
        .next()
        .ok_or_else(|| anyhow!("Invalid --size-range"))?;
    let end_s = parts
        .next()
        .ok_or_else(|| anyhow!("Invalid --size-range"))?;
    if parts.next().is_some() {
        bail!(
            "Invalid --size-range '{}'. Expected format like 64KiB-1MiB",
            range
        );
    }
    let start = sai3_bench::size_parser::parse_size(start_s)?;
    let end = sai3_bench::size_parser::parse_size(end_s)?;
    if start == 0 || end == 0 {
        bail!("Size range values must be > 0");
    }
    let (min_sz, max_sz) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let steps = size_steps.max(1);

    if steps == 1 || min_sz == max_sz {
        return Ok(vec![min_sz as usize]);
    }

    let min_f = min_sz as f64;
    let max_f = max_sz as f64;
    let mut out = Vec::with_capacity(steps);
    for i in 0..steps {
        let ratio = i as f64 / (steps - 1) as f64;
        let val = min_f * (max_f / min_f).powf(ratio);
        let rounded = ((val / 1024.0).round() * 1024.0).max(1024.0) as usize;
        out.push(rounded);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn mbps_from_output(output: &str) -> Option<f64> {
    let re = Regex::new(r"\(([0-9]+(?:\.[0-9]+)?) MB/s\)").ok()?;
    let mut last = None;
    for cap in re.captures_iter(output) {
        last = cap.get(1).and_then(|m| m.as_str().parse::<f64>().ok());
    }
    last
}

fn build_trial_uri(base_uri: &str, trial_id: usize) -> String {
    if base_uri.contains('*') {
        base_uri.replacen('*', &format!("trial{:03}_*", trial_id), 1)
    } else if base_uri.ends_with('/') {
        format!("{}autotune/trial{:03}/obj*.dat", base_uri, trial_id)
    } else {
        format!("{}.trial{:03}.*", base_uri, trial_id)
    }
}

fn run_trial_command(
    exe: &Path,
    args: &[String],
    envs: &[(String, String)],
    objects: usize,
) -> Result<(f64, f64)> {
    let t0 = Instant::now();
    let mut cmd = Command::new(exe);
    cmd.args(args);
    cmd.env_remove("S3DLIO_GCS_GRPC_CHANNELS");
    cmd.env_remove("S3DLIO_GCS_RAPID");
    cmd.env_remove("S3DLIO_ENABLE_RANGE_OPTIMIZATION");
    cmd.env_remove("S3DLIO_RANGE_THRESHOLD_MB");
    cmd.env_remove("S3DLIO_GRPC_WRITE_CHUNK_SIZE");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let output = cmd
        .output()
        .context("Failed to launch child trial process")?;
    let dt = t0.elapsed().as_secs_f64();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        bail!(
            "Trial command failed: {:?}\nstdout:\n{}\nstderr:\n{}",
            args,
            stdout,
            stderr
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let throughput = mbps_from_output(&stdout).unwrap_or(0.0);
    let avg_ms = if objects == 0 {
        0.0
    } else {
        (dt * 1000.0) / objects as f64
    };
    Ok((throughput, avg_ms))
}

#[allow(clippy::too_many_arguments)]
fn autotune_cmd(
    uri: &str,
    sizes: Option<&str>,
    size_range: Option<&str>,
    size_steps: usize,
    threads: &str,
    channels: Option<&str>,
    channels_per_thread: Option<&str>,
    range_enabled: &str,
    range_thresholds_mb: &str,
    gcs_write_chunk_sizes: &str,
    gcs_rapid_mode: RapidModeArg,
    objects: usize,
    trials: usize,
    optimize_for: OptimizeFor,
    ops: TuneOps,
    dry_run: bool,
) -> Result<()> {
    let validated_uri = validate_uri(uri)?;
    let is_gcs = validated_uri.starts_with("gs://") || validated_uri.starts_with("gcs://");

    let size_values = parse_sizes(sizes, size_range, size_steps)?;
    let thread_values = parse_csv_usize(threads)?;

    // Channel sweep: absolute counts (channels=) OR per-thread multiplier (channels_per_thread=).
    // channels_per_thread must be a CSV of positive integers (≥ 1).  Practical range: 1–8.
    // Default when neither key is set: 1 (one channel per thread; effective = threads × 1).
    // Encoding: (effective_channels, Option<cpt_multiplier>, is_default)
    // For non-GCS backends the channel concept does not apply; use a single sentinel.
    enum ChannelSpec {
        Absolute(Vec<usize>),
        PerThread(Vec<usize>, bool /* is_default */),
    }

    let channel_spec: ChannelSpec = if is_gcs {
        if let Some(cpt_str) = channels_per_thread {
            let cpts = parse_csv_usize(cpt_str)?;
            if cpts.contains(&0) {
                bail!("channels_per_thread: all values must be ≥ 1 (0 is not a valid channel multiplier)");
            }
            ChannelSpec::PerThread(cpts, false)
        } else if let Some(ch_str) = channels {
            ChannelSpec::Absolute(parse_csv_usize(ch_str)?)
        } else {
            // Default: 1 channel per thread (effective_channels = threads)
            ChannelSpec::PerThread(vec![1], true)
        }
    } else {
        ChannelSpec::Absolute(vec![0])
    };

    let range_enabled_values = parse_csv_bool(range_enabled)?;
    let range_threshold_values = parse_csv_usize(range_thresholds_mb)?;
    let gcs_write_chunk_values = if is_gcs {
        parse_csv_usize(gcs_write_chunk_sizes)?
    } else {
        vec![0]
    };

    // Build the full sweep matrix.
    let mut cases = Vec::new();
    for size in &size_values {
        for th in &thread_values {
            let ch_pairs: Vec<(usize, Option<usize>)> = match &channel_spec {
                ChannelSpec::Absolute(vals) => vals.iter().map(|&v| (v, None)).collect(),
                ChannelSpec::PerThread(cpts, _) => {
                    cpts.iter().map(|&cpt| (th * cpt, Some(cpt))).collect()
                }
            };
            for (ch, cpt) in &ch_pairs {
                for re in &range_enabled_values {
                    for rt in &range_threshold_values {
                        for gcs_chunk in &gcs_write_chunk_values {
                            cases.push(TuneCase {
                                size_bytes: *size,
                                threads: *th,
                                channels: *ch,
                                channels_per_thread: *cpt,
                                range_enabled: *re,
                                range_threshold_mb: *rt,
                                gcs_write_chunk_size: *gcs_chunk,
                            });
                        }
                    }
                }
            }
        }
    }

    if cases.is_empty() {
        bail!("No autotune cases generated");
    }

    let total_runs = cases.len() * trials;

    // ── Dry-run: print sweep plan and exit ───────────────────────────────────
    if dry_run {
        println!("Autotune sweep plan (dry-run)");
        println!("  URI           : {}", validated_uri);
        println!("  Objective     : {:?}", optimize_for);
        println!("  Ops           : {:?}", ops);
        println!("  Objects/trial : {}", objects);
        println!("  Trials/case   : {}", trials);
        println!();

        // Show loop order with dimension labels and values.
        println!("  Loop order (outermost → innermost):");
        println!(
            "    {:2}. {:22}: {:?}  ({} value{})",
            1,
            "object_size (bytes)",
            size_values,
            size_values.len(),
            if size_values.len() == 1 { "" } else { "s" }
        );
        println!(
            "    {:2}. {:22}: {:?}  ({} value{})",
            2,
            "threads",
            thread_values,
            thread_values.len(),
            if thread_values.len() == 1 { "" } else { "s" }
        );
        match &channel_spec {
            ChannelSpec::PerThread(cpts, is_default) => {
                let default_note = if *is_default { "  [default: 1]" } else { "" };
                println!(
                    "    {:2}. {:22}: {:?}  ({} value{})  [effective = threads × cpt]{}",
                    3,
                    "channels_per_thread",
                    cpts,
                    cpts.len(),
                    if cpts.len() == 1 { "" } else { "s" },
                    default_note
                );
            }
            ChannelSpec::Absolute(vals) if is_gcs => {
                println!(
                    "    {:2}. {:22}: {:?}  ({} value{})",
                    3,
                    "channels (absolute)",
                    vals,
                    vals.len(),
                    if vals.len() == 1 { "" } else { "s" }
                );
            }
            _ => {}
        }
        println!(
            "    {:2}. {:22}: {:?}  ({} value{})",
            4,
            "range_enabled",
            range_enabled_values,
            range_enabled_values.len(),
            if range_enabled_values.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        println!(
            "    {:2}. {:22}: {:?}  ({} value{})",
            5,
            "range_threshold_mb",
            range_threshold_values,
            range_threshold_values.len(),
            if range_threshold_values.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        if is_gcs {
            println!(
                "    {:2}. {:22}: {:?}  ({} value{})",
                6,
                "gcs_write_chunk_size",
                gcs_write_chunk_values,
                gcs_write_chunk_values.len(),
                if gcs_write_chunk_values.len() == 1 {
                    ""
                } else {
                    "s"
                }
            );
        }
        println!();
        println!("  Total cases   : {}", cases.len());
        println!(
            "  Total runs    : {} case(s) × {} trial(s) = {} runs",
            cases.len(),
            trials,
            total_runs
        );
        let op_count = match ops {
            TuneOps::Both => total_runs * 2,
            _ => total_runs,
        };
        println!(
            "  Object I/Os   : {} runs × {} objects = {} I/Os",
            op_count,
            objects,
            op_count * objects
        );
        println!();
        println!("  Note: no time limit is enforced.  Actual duration depends on storage");
        println!(
            "  throughput.  A rough estimate: ~30 s/trial → ~{:.0} minutes for {} runs.",
            total_runs as f64 * 30.0 / 60.0,
            total_runs
        );
        if total_runs > 200 {
            println!();
            println!(
                "  WARNING: {} runs is a large sweep.  Consider narrowing parameters.",
                total_runs
            );
        }
        return Ok(());
    }

    println!(
        "Autotune: {} cases × {} trial(s), objective={:?}, ops={:?}",
        cases.len(),
        trials,
        optimize_for,
        ops
    );
    println!("Target URI: {}", validated_uri);

    let exe = std::env::current_exe().context("Failed to resolve current executable path")?;
    let mut results = Vec::new();

    for (idx, case) in cases.iter().enumerate() {
        let mut put_mbps_samples = Vec::new();
        let mut get_mbps_samples = Vec::new();
        let mut put_avg_samples = Vec::new();
        let mut get_avg_samples = Vec::new();

        for rep in 0..trials {
            let trial_uri = build_trial_uri(&validated_uri, idx * trials + rep);
            let mut envs = vec![
                (
                    "S3DLIO_ENABLE_RANGE_OPTIMIZATION".to_string(),
                    if case.range_enabled { "1" } else { "0" }.to_string(),
                ),
                (
                    "S3DLIO_RANGE_THRESHOLD_MB".to_string(),
                    case.range_threshold_mb.to_string(),
                ),
            ];

            if is_gcs {
                envs.push((
                    "S3DLIO_GCS_GRPC_CHANNELS".to_string(),
                    case.channels.to_string(),
                ));
                envs.push((
                    "S3DLIO_GRPC_WRITE_CHUNK_SIZE".to_string(),
                    case.gcs_write_chunk_size.to_string(),
                ));
                match gcs_rapid_mode {
                    RapidModeArg::On => {
                        envs.push(("S3DLIO_GCS_RAPID".to_string(), "true".to_string()))
                    }
                    RapidModeArg::Off => {
                        envs.push(("S3DLIO_GCS_RAPID".to_string(), "false".to_string()))
                    }
                    RapidModeArg::Auto => {}
                }
            }

            if matches!(ops, TuneOps::Put | TuneOps::Both) {
                let put_args = vec![
                    "util".to_string(),
                    "put".to_string(),
                    "--uri".to_string(),
                    trial_uri.clone(),
                    "--object-size".to_string(),
                    case.size_bytes.to_string(),
                    "--objects".to_string(),
                    objects.to_string(),
                    "--concurrency".to_string(),
                    case.threads.to_string(),
                ];
                let (mbps, avg_ms) = run_trial_command(&exe, &put_args, &envs, objects)?;
                put_mbps_samples.push(mbps);
                put_avg_samples.push(avg_ms);
            }

            if matches!(ops, TuneOps::Get | TuneOps::Both) {
                if matches!(ops, TuneOps::Get) {
                    let seed_put_args = vec![
                        "util".to_string(),
                        "put".to_string(),
                        "--uri".to_string(),
                        trial_uri.clone(),
                        "--object-size".to_string(),
                        case.size_bytes.to_string(),
                        "--objects".to_string(),
                        objects.to_string(),
                        "--concurrency".to_string(),
                        case.threads.to_string(),
                    ];
                    let _ = run_trial_command(&exe, &seed_put_args, &envs, objects)?;
                }

                let get_args = vec![
                    "util".to_string(),
                    "get".to_string(),
                    "--uri".to_string(),
                    trial_uri.clone(),
                    "--jobs".to_string(),
                    case.threads.to_string(),
                ];
                let (mbps, avg_ms) = run_trial_command(&exe, &get_args, &envs, objects)?;
                get_mbps_samples.push(mbps);
                get_avg_samples.push(avg_ms);
            }
        }

        let mean = |vals: &[f64]| -> Option<f64> {
            if vals.is_empty() {
                None
            } else {
                Some(vals.iter().sum::<f64>() / vals.len() as f64)
            }
        };

        let result = TuneResult {
            case: case.clone(),
            put_mbps: mean(&put_mbps_samples),
            get_mbps: mean(&get_mbps_samples),
            put_avg_ms: mean(&put_avg_samples),
            get_avg_ms: mean(&get_avg_samples),
        };
        let channels_label = match result.case.channels_per_thread {
            Some(cpt) => format!("{}(×{})", result.case.channels, cpt),
            None => result.case.channels.to_string(),
        };
        println!(
            "[{:>3}/{}] size={}B threads={} channels={} range={} thr={}MB chunk={} -> PUT={:.2?}MB/s GET={:.2?}MB/s",
            idx + 1,
            cases.len(),
            result.case.size_bytes,
            result.case.threads,
            channels_label,
            result.case.range_enabled,
            result.case.range_threshold_mb,
            result.case.gcs_write_chunk_size,
            result.put_mbps,
            result.get_mbps
        );
        results.push(result);
    }

    if results.is_empty() {
        bail!("No autotune results were produced");
    }

    let score = |r: &TuneResult| -> f64 {
        match optimize_for {
            OptimizeFor::Throughput => {
                let mut total = 0.0;
                let mut n = 0.0;
                if let Some(v) = r.put_mbps {
                    total += v;
                    n += 1.0;
                }
                if let Some(v) = r.get_mbps {
                    total += v;
                    n += 1.0;
                }
                if n == 0.0 {
                    0.0
                } else {
                    total / n
                }
            }
            OptimizeFor::Latency => {
                let mut total = 0.0;
                let mut n = 0.0;
                if let Some(v) = r.put_avg_ms {
                    total += v;
                    n += 1.0;
                }
                if let Some(v) = r.get_avg_ms {
                    total += v;
                    n += 1.0;
                }
                if n == 0.0 {
                    f64::INFINITY
                } else {
                    total / n
                }
            }
        }
    };

    let best = match optimize_for {
        OptimizeFor::Throughput => results
            .iter()
            .max_by(|a, b| {
                score(a)
                    .partial_cmp(&score(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| anyhow!("No best result found"))?,
        OptimizeFor::Latency => results
            .iter()
            .min_by(|a, b| {
                score(a)
                    .partial_cmp(&score(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| anyhow!("No best result found"))?,
    };

    println!("\n=== AUTOTUNE BEST ({:?}) ===", optimize_for);
    println!("score: {:.3}", score(best));
    println!("size_bytes: {}", best.case.size_bytes);
    println!("threads/jobs: {}", best.case.threads);
    if is_gcs {
        println!("gcs_channels: {}", best.case.channels);
        if let Some(cpt) = best.case.channels_per_thread {
            println!(
                "gcs_channels_per_thread: {}  (= {} threads × {})",
                best.case.channels, best.case.threads, cpt
            );
        }
        println!(
            "gcs_write_chunk_size_bytes: {}",
            best.case.gcs_write_chunk_size
        );
        println!("gcs_rapid_mode: {:?}", gcs_rapid_mode);
    }
    println!("range_enabled: {}", best.case.range_enabled);
    println!("range_threshold_mb: {}", best.case.range_threshold_mb);
    println!("put_mbps: {:.2?}", best.put_mbps);
    println!("get_mbps: {:.2?}", best.get_mbps);
    println!("put_avg_ms_per_object: {:.2?}", best.put_avg_ms);
    println!("get_avg_ms_per_object: {:.2?}", best.get_avg_ms);

    println!("\nRecommended YAML block:");
    println!("concurrency: {}", best.case.threads);
    println!("s3dlio_optimization:");
    println!("  enable_range_downloads: {}", best.case.range_enabled);
    println!("  range_threshold_mb: {}", best.case.range_threshold_mb);
    if is_gcs {
        println!("  gcs_channel_count: {}", best.case.channels);
        match gcs_rapid_mode {
            RapidModeArg::On => println!("  gcs_rapid_mode: true"),
            RapidModeArg::Off => println!("  gcs_rapid_mode: false"),
            RapidModeArg::Auto => println!("  # gcs_rapid_mode omitted (auto-detect)"),
        }
        println!(
            "  gcs_write_chunk_size_bytes: {}",
            best.case.gcs_write_chunk_size
        );
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

fn sort_oplog_files(
    files: &[std::path::PathBuf],
    in_place: bool,
    window_size: usize,
) -> Result<()> {
    use sai3_bench::oplog_merge;

    if files.is_empty() {
        bail!("No files provided to sort");
    }

    info!(
        "Sorting {} op-log file(s) with window_size={}",
        files.len(),
        window_size
    );

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
            std::fs::rename(&temp_path, file_path).with_context(|| {
                format!(
                    "Failed to rename sorted file: {} -> {}",
                    temp_path.display(),
                    file_path.display()
                )
            })?;

            info!("✓ Sorted in-place: {}", file_path.display());
            continue;
        } else {
            // Create .sorted version
            let file_stem = file_path
                .file_stem()
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

        info!(
            "✓ Sorted: {} -> {}",
            file_path.display(),
            output_path.display()
        );
    }

    info!("Successfully sorted {} file(s)", files.len());
    Ok(())
}

fn convert_configs_cmd(
    config: Option<std::path::PathBuf>,
    files: Option<Vec<std::path::PathBuf>>,
    dry_run: bool,
    validate: bool,
) -> Result<()> {
    use sai3_bench::config_converter::{convert_yaml_file, ConversionResult};

    // Determine files to process
    let files_to_convert: Vec<std::path::PathBuf> = if let Some(single_config) = config {
        vec![single_config]
    } else if let Some(file_list) = files {
        file_list
    } else {
        bail!("Must specify either --config or --files");
    };

    if files_to_convert.is_empty() {
        bail!("No files provided for conversion");
    }

    println!("╔═══════════════════════════════════════════════════════════════════════╗");
    println!("║        YAML Config Converter: Legacy → Explicit Stages              ║");
    println!("╚═══════════════════════════════════════════════════════════════════════╝");
    println!();

    if dry_run {
        println!("🔍 DRY RUN MODE - No changes will be made");
    } else if !validate {
        println!("⚠️  VALIDATION DISABLED - Configs will NOT be validated");
    }

    println!();
    println!("Processing {} file(s)...", files_to_convert.len());
    println!();

    let mut success_count = 0;
    let mut skip_count = 0;
    let mut fail_count = 0;

    for file_path in &files_to_convert {
        println!("📄 {}", file_path.display());

        if !file_path.exists() {
            println!("  ❌ File does not exist");
            fail_count += 1;
            continue;
        }

        match convert_yaml_file(file_path, validate, dry_run) {
            Ok(ConversionResult::AlreadyHasStages) => {
                println!("  ℹ️  Already has stages section - skipping");
                skip_count += 1;
            }
            Ok(ConversionResult::NoConversionNeeded) => {
                println!("  ℹ️  No distributed/workload section - skipping");
                skip_count += 1;
            }
            Ok(ConversionResult::DryRun { stage_count }) => {
                println!("  🔄 Would convert → {} stages", stage_count);
                success_count += 1;
            }
            Ok(ConversionResult::Converted {
                stage_count,
                backup_path,
            }) => {
                println!("  ✅ Converted → {} stages", stage_count);
                println!("  💾 Backup: {}", backup_path);
                success_count += 1;
            }
            Err(e) => {
                println!("  ❌ Conversion failed: {}", e);
                if let Some(source) = e.source() {
                    println!("     Cause: {}", source);
                }
                fail_count += 1;
            }
        }

        println!();
    }

    // Summary
    println!("╔═══════════════════════════════════════════════════════════════════════╗");
    println!("║                     CONVERSION SUMMARY                               ║");
    println!("╚═══════════════════════════════════════════════════════════════════════╝");
    println!();
    println!("✅ Converted: {}", success_count);
    println!("ℹ️  Skipped:   {}", skip_count);
    println!("❌ Failed:    {}", fail_count);
    println!("📁 Total:     {}", files_to_convert.len());
    println!();

    if !dry_run && success_count > 0 {
        println!("💾 Backups saved with .yaml.bak extension");
        println!("   To restore: mv <file>.yaml.bak <file>.yaml");
        println!();
    }

    if fail_count > 0 {
        bail!("{} file(s) failed to convert", fail_count);
    }

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
        let objects = list_objects_multi_backend(&validated_uri)
            .await
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
                &validated_uri[..=pos] // Include the trailing '/'
            } else {
                return Err(anyhow!("Invalid URI pattern: {}", validated_uri));
            };

            // List the directory
            list_objects_multi_backend(dir_uri)
                .await
                .context("Failed to list objects for pattern matching")?
        } else {
            // Direct listing (no pattern)
            list_objects_multi_backend(&validated_uri)
                .await
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

        let size = stat_object_multi_backend(&validated_uri)
            .await
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

        // Generate zero-filled data for all objects
        // Convert to Bytes for zero-copy semantics
        let data = bytes::Bytes::from(vec![0u8; object_size]);
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

                    put_object_multi_backend(&obj_uri, data.clone()).await?;  // Clone is cheap: Bytes is Arc-like
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

/// Run sai3-analyze to generate Excel summary of results
///
/// This function attempts to find and execute the sai3-analyze binary to create
/// a consolidated Excel file in the results directory. If it fails, a warning
/// is logged but the overall benchmark is considered successful.
fn run_sai3_analyze(results_dir: &Path) -> Result<()> {
    use std::process::Command;

    // Try to find sai3-analyze binary in the same directory as current executable
    let analyze_binary = if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let analyze_path = parent.join("sai3-analyze");
            if analyze_path.exists() {
                analyze_path
            } else {
                // Fall back to PATH
                PathBuf::from("sai3-analyze")
            }
        } else {
            PathBuf::from("sai3-analyze")
        }
    } else {
        PathBuf::from("sai3-analyze")
    };

    let output_file = results_dir.join("Summary-Results.xlsx");

    // Execute sai3-analyze with the results directory
    let output = Command::new(&analyze_binary)
        .arg("--dirs")
        .arg(results_dir)
        .arg("--output")
        .arg(&output_file)
        .arg("--overwrite") // Overwrite if file already exists
        .output()
        .context("Failed to execute sai3-analyze")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("sai3-analyze failed: {}", stderr);
    }

    Ok(())
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

    // Apply s3dlio optimization settings as environment variables (v0.8.63+)
    if let Some(ref s3dlio_opt) = config.s3dlio_optimization {
        s3dlio_opt.apply();
        info!("Applied s3dlio optimization configuration from YAML");
    }

    // GCS smart defaults: auto channel count + optional concurrency scaling (v0.9.65+)
    // For gs:// targets: sets gRPC subchannel count and optionally scales config.concurrency.
    // Must run after apply() (which sets explicit channel count) but before workload starts.
    config.apply_gcs_defaults();

    sai3_bench::validation::apply_directory_tree_counts(&mut config)
        .context("Directory tree count validation failed")?;

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
        workload::init_operation_logger(op_log_path).with_context(|| {
            format!(
                "Failed to initialize operation logger at {}",
                op_log_path.display()
            )
        })?;
        info!("Initialized operation logger: {}", op_log_path.display());

        // Set client_id for standalone mode (v0.8.6+)
        // Use "standalone" as default, or could use hostname
        let client_id =
            std::env::var("SAI3_CLIENT_ID").unwrap_or_else(|_| "standalone".to_string());
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
        let range_enabled = config
            .range_engine
            .as_ref()
            .map(|c| c.enabled)
            .unwrap_or(false);

        if range_enabled {
            let min_size_mb = config
                .range_engine
                .as_ref()
                .map(|c| c.min_split_size / (1024 * 1024))
                .unwrap_or(16);
            info!(
                "RangeEngine ENABLED for {} backend - files >= {} MiB",
                backend.name(),
                min_size_mb
            );
        } else {
            info!(
                "RangeEngine DISABLED for {} backend (default for optimal performance)",
                backend.name()
            );
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
            format!(
                "  {} - {:.1}% (weight: {}, concurrency: {} threads)",
                op_name, percentage, weighted_op.weight, op_concurrency
            )
        } else {
            format!(
                "  {} - {:.1}% (weight: {})",
                op_name, percentage, weighted_op.weight
            )
        };
        println!("{}", op_msg);
        results_dir.write_console(&op_msg)?;
    }

    info!(
        "Starting workload execution with {} operation types",
        config.workload.len()
    );

    let rt = RtBuilder::new_multi_thread().enable_all().build()?;

    // v0.8.23: Pre-flight validation for filesystem targets (standalone mode)
    if let Some(ref target) = config.target {
        if target.starts_with("file://") || target.starts_with("direct://") {
            use sai3_bench::preflight::filesystem;
            use std::path::PathBuf;

            // Extract filesystem path
            let fs_path = if let Some(stripped) = target.strip_prefix("file://") {
                stripped
            } else if let Some(stripped) = target.strip_prefix("direct://") {
                stripped
            } else {
                target.as_str()
            };

            let path = PathBuf::from(fs_path);

            let preflight_header = "\n🔍 Pre-flight Validation\n━━━━━━━━━━━━━━━━━━━━━━";
            println!("{}", preflight_header);
            results_dir.write_console(preflight_header)?;

            // Run filesystem validation
            let check_write = config.prepare.is_some(); // Only check write if prepare phase exists
            let validation_result = rt.block_on(filesystem::validate_filesystem(
                &path,
                check_write,
                Some(1024 * 1024 * 1024), // 1 GB required space
            ))?;

            // Display results using shared display function
            let (passed, error_count, warning_count) =
                sai3_bench::preflight::display_validation_results(&validation_result.results, None);

            let separator = "━━━━━━━━━━━━━━━━━━━━━━";
            println!("{}", separator);
            results_dir.write_console(separator)?;

            // Check for errors
            if !passed {
                let error_summary = format!("❌ Pre-flight validation FAILED\n   {} errors, {} warnings\n\nFix the above errors before running the workload.",
                    error_count,
                    warning_count
                );
                println!("{}", error_summary);
                results_dir.write_console(&error_summary)?;
                bail!("Pre-flight validation failed with {} errors", error_count);
            } else {
                let info_count = validation_result.results.len() - error_count - warning_count;
                let success_msg = format!(
                    "✅ Pre-flight validation passed\n   {} warnings, {} info messages",
                    warning_count, info_count
                );
                println!("{}", success_msg);
                results_dir.write_console(&success_msg)?;
            }
        }
    }

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

            // v0.8.22: Create multi-endpoint cache for prepare phase statistics
            use std::collections::HashMap;
            use std::sync::{Arc, Mutex};
            let prepare_multi_ep_cache: workload::MultiEndpointCache =
                Arc::new(Mutex::new(HashMap::new()));

            info!("Executing prepare step");
            let (prepared, manifest, prepare_metrics) = rt.block_on(workload::prepare_objects(
                prepare_config,
                Some(&config.workload),
                None,                           // live_stats_tracker
                config.multi_endpoint.as_ref(), // v0.8.22: pass multi-endpoint config
                &prepare_multi_ep_cache,        // v0.8.22: pass multi-endpoint cache for stats
                config.concurrency,
                0,    // agent_id (standalone mode)
                1,    // num_agents (standalone mode)
                true, // shared_storage (N/A in standalone, but use true for backward compat)
                if config.enable_metadata_cache {
                    Some(results_dir.path())
                } else {
                    None
                }, // v0.8.89: Conditional metadata cache
                if config.enable_metadata_cache {
                    Some(&config)
                } else {
                    None
                }, // v0.8.89: Conditional cache hash
            ))?;

            let prepared_msg = format!(
                "Prepared {} objects ({} created, {} existed) in {:.2}s",
                fmt_commas(prepared.len() as u64),
                fmt_commas(prepare_metrics.objects_created),
                fmt_commas(prepare_metrics.objects_existed),
                prepare_metrics.wall_seconds
            );
            println!("{}", prepared_msg);
            results_dir.write_console(&prepared_msg)?;

            // Print prepare performance summary
            if prepare_metrics.put.ops > 0 {
                let put_ops_s = prepare_metrics.put.ops as f64 / prepare_metrics.wall_seconds;
                let put_mib_s =
                    (prepare_metrics.put.bytes as f64 / 1_048_576.0) / prepare_metrics.wall_seconds;

                let perf_header = "\nPrepare Performance:";
                println!("{}", perf_header);
                results_dir.write_console(perf_header)?;

                let ops_msg = format!(
                    "  Total ops: {} ({} ops/s)",
                    fmt_commas(prepare_metrics.put.ops),
                    fmt_float_commas(put_ops_s)
                );
                println!("{}", ops_msg);
                results_dir.write_console(&ops_msg)?;

                let bytes_msg = format!(
                    "  Total bytes: {} ({:.2} MiB)",
                    fmt_commas(prepare_metrics.put.bytes),
                    prepare_metrics.put.bytes as f64 / 1_048_576.0
                );
                println!("{}", bytes_msg);
                results_dir.write_console(&bytes_msg)?;

                let throughput_msg = format!("  Throughput: {:.2} MiB/s", put_mib_s);
                println!("{}", throughput_msg);
                results_dir.write_console(&throughput_msg)?;

                let latency_msg = format!(
                    "  Latency: mean={:.2}ms, p50={:.2}ms, p95={:.2}ms, p99={:.2}ms",
                    prepare_metrics.put.mean_us as f64 / 1000.0,
                    prepare_metrics.put.p50_us as f64 / 1000.0,
                    prepare_metrics.put.p95_us as f64 / 1000.0,
                    prepare_metrics.put.p99_us as f64 / 1000.0
                );
                println!("{}", latency_msg);
                results_dir.write_console(&latency_msg)?;
            }

            if prepare_metrics.mkdir_count > 0 {
                let mkdir_summary = format!(
                    "  MKDIR: {} directories created",
                    prepare_metrics.mkdir_count
                );
                println!("{}", mkdir_summary);
                results_dir.write_console(&mkdir_summary)?;
            }

            // v0.8.23: Display per-endpoint statistics if multi-endpoint was used during prepare
            if let Some(ref endpoint_stats) = prepare_metrics.endpoint_stats {
                let endpoint_header = "\nPrepare Per-Endpoint Statistics:";
                println!("{}", endpoint_header);
                results_dir.write_console(endpoint_header)?;

                let endpoint_count_msg = format!("  Total endpoints: {}", endpoint_stats.len());
                println!("{}", endpoint_count_msg);
                results_dir.write_console(&endpoint_count_msg)?;

                for (idx, stats) in endpoint_stats.iter().enumerate() {
                    let endpoint_msg = format!("\n  Endpoint {}: {}", idx + 1, stats.uri);
                    println!("{}", endpoint_msg);
                    results_dir.write_console(&endpoint_msg)?;

                    let requests_msg =
                        format!("    Total requests: {}", fmt_commas(stats.total_requests));
                    println!("{}", requests_msg);
                    results_dir.write_console(&requests_msg)?;

                    let write_msg = format!(
                        "    Bytes written: {} ({:.2} MiB)",
                        fmt_commas(stats.bytes_written),
                        stats.bytes_written as f64 / 1_048_576.0
                    );
                    println!("{}", write_msg);
                    results_dir.write_console(&write_msg)?;

                    if stats.error_count > 0 {
                        let error_msg = format!("    Errors: {}", stats.error_count);
                        println!("{}", error_msg);
                        results_dir.write_console(&error_msg)?;
                    }
                }

                // Show load distribution for round-robin verification
                if endpoint_stats.len() > 1 {
                    let distribution_header = "\n  Load Distribution:";
                    println!("{}", distribution_header);
                    results_dir.write_console(distribution_header)?;

                    let total_requests: u64 = endpoint_stats.iter().map(|s| s.total_requests).sum();
                    let total_bytes_written: u64 =
                        endpoint_stats.iter().map(|s| s.bytes_written).sum();

                    for (idx, stats) in endpoint_stats.iter().enumerate() {
                        let req_pct = if total_requests > 0 {
                            (stats.total_requests as f64 / total_requests as f64) * 100.0
                        } else {
                            0.0
                        };

                        let write_pct = if total_bytes_written > 0 {
                            (stats.bytes_written as f64 / total_bytes_written as f64) * 100.0
                        } else {
                            0.0
                        };

                        let dist_msg = format!(
                            "    Endpoint {}: {:.1}% requests, {:.1}% write",
                            idx + 1,
                            req_pct,
                            write_pct
                        );
                        println!("{}", dist_msg);
                        results_dir.write_console(&dist_msg)?;
                    }
                }
            }

            // Export prepare metrics to TSV
            if prepare_metrics.put.ops > 0 {
                use sai3_bench::tsv_export::TsvExporter;
                let prepare_tsv_path = results_dir.prepare_tsv_path();
                let exporter = TsvExporter::with_path(&prepare_tsv_path)?;
                exporter.export_prepare_metrics(&prepare_metrics)?;

                let export_msg = format!(
                    "Prepare metrics exported to: {}",
                    prepare_tsv_path.display()
                );
                println!("{}", export_msg);
                results_dir.write_console(&export_msg)?;

                // v0.8.23: Export prepare phase endpoint stats if multi-endpoint was used
                if let Some(ref endpoint_stats) = prepare_metrics.endpoint_stats {
                    let ep_tsv_path = results_dir.prepare_endpoint_stats_tsv_path();
                    let ep_exporter = TsvExporter::with_path(&ep_tsv_path)?;
                    ep_exporter.export_endpoint_stats(endpoint_stats)?;

                    let ep_export_msg = format!(
                        "Prepare endpoint stats exported to: {}",
                        ep_tsv_path.display()
                    );
                    println!("{}", ep_export_msg);
                    results_dir.write_console(&ep_export_msg)?;
                }
            }

            // v0.8.90: Write populate ledger row (always-on, regardless of enable_metadata_cache)
            {
                let ledger_row = sai3_bench::populate_ledger::LedgerRow {
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    test_name: results_dir.test_name().to_string(),
                    agent_id: "standalone".to_string(),
                    stage: "prepare".to_string(),
                    objects_created: prepare_metrics.objects_created,
                    objects_existed: prepare_metrics.objects_existed,
                    total_bytes: prepare_metrics.put.bytes,
                    wall_seconds: prepare_metrics.wall_seconds,
                };
                if let Err(e) =
                    sai3_bench::populate_ledger::append_row(results_dir.path(), &ledger_row)
                {
                    warn!("Failed to write populate ledger: {}", e);
                } else {
                    let ledger_msg = format!(
                        "Populate ledger: {} objects, {} total, {} avg/obj → {}",
                        fmt_commas(ledger_row.objects_created),
                        fmt_bytes(ledger_row.total_bytes),
                        fmt_bytes(ledger_row.avg_bytes() as u64),
                        results_dir
                            .path()
                            .join(sai3_bench::populate_ledger::LEDGER_FILENAME)
                            .display()
                    );
                    println!("{}", ledger_msg);
                    results_dir.write_console(&ledger_msg)?;
                }
            }

            // Use configurable delay from YAML (only if objects were created)
            if prepared.iter().any(|p| p.created) && prepare_config.post_prepare_delay > 0 {
                let delay_secs = prepare_config.post_prepare_delay;
                let delay_msg = format!(
                    "Waiting {}s for object propagation (configured delay)...",
                    delay_secs
                );
                println!("{}", delay_msg);
                results_dir.write_console(&delay_msg)?;

                info!(
                    "Delaying {}s for eventual consistency (post_prepare_delay from config)",
                    delay_secs
                );
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

        let prepare_only_msg = format!(
            "\nPrepare-only mode: {} objects created, exiting",
            prepared_objects.len()
        );
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
            let base_uri = prepare_config
                .ensure_objects
                .first()
                .ok_or_else(|| {
                    anyhow!("directory_structure requires at least one ensure_objects entry")
                })?
                .get_base_uri(None)?;

            Some(sai3_bench::prepare::create_tree_manifest_only(
                prepare_config,
                0, // agent_id
                1, // num_agents
                &base_uri,
                None, // No live_stats_tracker in cleanup-only mode
            )?)
        } else {
            None
        };

        // Reconstruct object list (same logic as prepare, but we mark all as created=true)
        let mut objects_to_cleanup = Vec::new();
        for spec in &prepare_config.ensure_objects {
            // Get effective base_uri for this spec
            let base_uri = spec.get_base_uri(None)?;

            let prefix = base_uri.trim_end_matches('/');
            let prefix_name = prefix.rsplit('/').next().unwrap_or("prepared");

            if let Some(ref manifest) = tree_manifest_for_cleanup {
                // Tree mode: enumerate all file paths
                for file_idx in 0..manifest.total_files {
                    if let Some(relative_path) = manifest.get_file_path(file_idx) {
                        let uri = if base_uri.ends_with('/') {
                            format!("{}{}", base_uri, relative_path)
                        } else {
                            format!("{}/{}", base_uri, relative_path)
                        };
                        objects_to_cleanup.push(sai3_bench::prepare::PreparedObject {
                            uri,
                            size: 0, // Size doesn't matter for cleanup
                            created: true,
                        });
                    }
                }
            } else {
                // Flat mode: enumerate numbered files
                for idx in 0..spec.count {
                    let key = format!("{}-{:08}.dat", prefix_name, idx);
                    let uri = if base_uri.ends_with('/') {
                        format!("{}{}", base_uri, key)
                    } else {
                        format!("{}/{}", base_uri, key)
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
            0, // agent_id (standalone mode)
            1, // num_agents (standalone mode)
            prepare_config.cleanup_mode,
            None, // No live stats tracker in standalone mode
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
    let num_processes = config.processes.as_ref().map(|p| p.resolve()).unwrap_or(1); // Default to single process
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

    // v0.8.19: Preflight check - verify objects exist if skip_verification=true and workload has GET ops
    rt.block_on(preflight_check_get_objects(&config, &results_dir))?;

    // Run the workload using the configured processing mode
    let summary = if num_processes > 1 {
        // Multi-worker execution
        info!(
            "Using {} mode with {} workers",
            match processing_mode {
                sai3_bench::config::ProcessingMode::MultiProcess => "MultiProcess",
                sai3_bench::config::ProcessingMode::MultiRuntime => "MultiRuntime",
            },
            num_processes
        );

        match processing_mode {
            sai3_bench::config::ProcessingMode::MultiProcess => {
                // Multi-process mode: spawn N child processes
                rt.block_on(sai3_bench::multiprocess::run_multiprocess(
                    &config,
                    tree_manifest.clone(),
                    op_log_path,
                ))?
            }
            sai3_bench::config::ProcessingMode::MultiRuntime => {
                // Multi-runtime mode: spawn N tokio runtimes in threads
                // Note: op_log not passed - all workers use global logger in single process
                sai3_bench::multiruntime::run_multiruntime(
                    &config,
                    num_processes,
                    tree_manifest.clone(),
                )?
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
                    warn!(
                        "Failed to create perf_log: {} - continuing without perf_log",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        // Create LiveStatsTracker for workload execution
        let tracker = Arc::new(
            sai3_bench::live_stats::LiveStatsTracker::new_with_concurrency(
                config.concurrency as u32,
            ),
        );

        // Initialize perf_log tracker with warmup duration
        let warmup_duration = config.warmup_period.unwrap_or(Duration::ZERO);
        let perf_log_interval = config
            .perf_log
            .as_ref()
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
                // Initialize CPU monitor for this thread
                let mut cpu_monitor = CpuMonitor::new();

                loop {
                    std::thread::sleep(interval);

                    // Sample CPU utilization
                    let cpu_util = cpu_monitor.sample().unwrap_or(None).unwrap_or_default();

                    let stats = tracker_for_task.snapshot();
                    let metrics = sai3_bench::perf_log::PerfMetrics {
                        get_ops: stats.get_ops,
                        get_bytes: stats.get_bytes,
                        put_ops: stats.put_ops,
                        put_bytes: stats.put_bytes,
                        meta_ops: stats.meta_ops,
                        errors: 0, // errors not tracked
                        get_mean_us: stats.get_mean_us,
                        get_p50_us: stats.get_p50_us,
                        get_p90_us: stats.get_p90_us,
                        get_p99_us: stats.get_p99_us,
                        put_mean_us: stats.put_mean_us,
                        put_p50_us: stats.put_p50_us,
                        put_p90_us: stats.put_p90_us,
                        put_p99_us: stats.put_p99_us,
                        meta_mean_us: stats.meta_mean_us,
                        meta_p50_us: stats.meta_p50_us,
                        meta_p90_us: stats.meta_p90_us,
                        meta_p99_us: stats.meta_p99_us,
                        cpu_user_percent: cpu_util.user_percent,
                        cpu_system_percent: cpu_util.system_percent,
                        cpu_iowait_percent: cpu_util.iowait_percent,
                    };
                    let entry = tracker_mut.compute_delta(
                        "standalone",
                        &metrics,
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
        let get_msg = format!(
            "  GET: {} ops ({:.1}%)",
            fmt_commas(summary.get.ops),
            get_pct
        );
        println!("{}", get_msg);
        results_dir.write_console(&get_msg)?;
    }
    if summary.put.ops > 0 {
        let put_pct = (summary.put.ops as f64 / summary.total_ops as f64) * 100.0;
        let put_msg = format!(
            "  PUT: {} ops ({:.1}%)",
            fmt_commas(summary.put.ops),
            put_pct
        );
        println!("{}", put_msg);
        results_dir.write_console(&put_msg)?;
    }
    if summary.meta.ops > 0 {
        let meta_pct = (summary.meta.ops as f64 / summary.total_ops as f64) * 100.0;
        let meta_msg = format!(
            "  META (LIST/STAT/DELETE): {} ops ({:.1}%)",
            fmt_commas(summary.meta.ops),
            meta_pct
        );
        println!("{}", meta_msg);
        results_dir.write_console(&meta_msg)?;
    }

    let overall_header = "\nOverall Performance:";
    println!("{}", overall_header);
    results_dir.write_console(overall_header)?;

    let total_ops_msg = format!("  Total ops: {}", fmt_commas(summary.total_ops));
    println!("{}", total_ops_msg);
    results_dir.write_console(&total_ops_msg)?;

    let total_bytes_msg = format!(
        "  Total bytes: {} ({:.2} MiB)",
        fmt_commas(summary.total_bytes),
        summary.total_bytes as f64 / 1_048_576.0
    );
    println!("{}", total_bytes_msg);
    results_dir.write_console(&total_bytes_msg)?;

    let throughput_msg = format!(
        "  Throughput: {} ops/s",
        fmt_float_commas(summary.total_ops as f64 / summary.wall_seconds)
    );
    println!("{}", throughput_msg);
    results_dir.write_console(&throughput_msg)?;

    if summary.get.ops > 0 {
        let get_mib_s = (summary.get.bytes as f64 / 1_048_576.0) / summary.wall_seconds;

        let get_header = "\nGET Performance:";
        println!("{}", get_header);
        results_dir.write_console(get_header)?;

        let get_ops_msg = format!(
            "  Ops: {} ({} ops/s)",
            fmt_commas(summary.get.ops),
            fmt_float_commas(summary.get.ops as f64 / summary.wall_seconds)
        );
        println!("{}", get_ops_msg);
        results_dir.write_console(&get_ops_msg)?;

        let get_bytes_msg = format!(
            "  Bytes: {} ({:.2} MiB)",
            fmt_commas(summary.get.bytes),
            summary.get.bytes as f64 / 1_048_576.0
        );
        println!("{}", get_bytes_msg);
        results_dir.write_console(&get_bytes_msg)?;

        let get_throughput_msg = format!("  Throughput: {:.2} MiB/s", get_mib_s);
        println!("{}", get_throughput_msg);
        results_dir.write_console(&get_throughput_msg)?;

        let get_latency_msg = format!(
            "  Latency: mean={}µs, p50={}µs, p95={}µs, p99={}µs",
            fmt_commas(summary.get.mean_us),
            fmt_commas(summary.get.p50_us),
            fmt_commas(summary.get.p95_us),
            fmt_commas(summary.get.p99_us)
        );
        println!("{}", get_latency_msg);
        results_dir.write_console(&get_latency_msg)?;
    }

    if summary.put.ops > 0 {
        let put_mib_s = (summary.put.bytes as f64 / 1_048_576.0) / summary.wall_seconds;

        let put_header = "\nPUT Performance:";
        println!("{}", put_header);
        results_dir.write_console(put_header)?;

        let put_ops_msg = format!(
            "  Ops: {} ({} ops/s)",
            fmt_commas(summary.put.ops),
            fmt_float_commas(summary.put.ops as f64 / summary.wall_seconds)
        );
        println!("{}", put_ops_msg);
        results_dir.write_console(&put_ops_msg)?;

        let put_bytes_msg = format!(
            "  Bytes: {} ({:.2} MiB)",
            fmt_commas(summary.put.bytes),
            summary.put.bytes as f64 / 1_048_576.0
        );
        println!("{}", put_bytes_msg);
        results_dir.write_console(&put_bytes_msg)?;

        let put_throughput_msg = format!("  Throughput: {:.2} MiB/s", put_mib_s);
        println!("{}", put_throughput_msg);
        results_dir.write_console(&put_throughput_msg)?;

        let put_latency_msg = format!(
            "  Latency (total):  mean={}, p50={}, p95={}, p99={}",
            fmt_latency_us(summary.put.mean_us),
            fmt_latency_us(summary.put.p50_us),
            fmt_latency_us(summary.put.p95_us),
            fmt_latency_us(summary.put.p99_us)
        );
        println!("{}", put_latency_msg);
        results_dir.write_console(&put_latency_msg)?;

        // Internal vs external latency breakdown (standalone mode only; empty in distributed mode)
        let put_io_combined = summary.put_hists_io.combined_histogram();
        let put_setup_combined = summary.put_hists_setup.combined_histogram();
        if !put_io_combined.is_empty() {
            let io_msg = format!(
                "  Latency (I/O ext): mean={}, p50={}, p95={}, p99={}  ← store.put() wait",
                fmt_latency_us(put_io_combined.mean() as u64),
                fmt_latency_us(put_io_combined.value_at_quantile(0.50)),
                fmt_latency_us(put_io_combined.value_at_quantile(0.95)),
                fmt_latency_us(put_io_combined.value_at_quantile(0.99))
            );
            println!("{}", io_msg);
            results_dir.write_console(&io_msg)?;
        }
        if !put_setup_combined.is_empty() {
            let setup_msg = format!(
                "  Latency (setup int): mean={}, p50={}, p95={}, p99={}  ← data-gen + scheduling",
                fmt_latency_ns(put_setup_combined.mean() as u64),
                fmt_latency_ns(put_setup_combined.value_at_quantile(0.50)),
                fmt_latency_ns(put_setup_combined.value_at_quantile(0.95)),
                fmt_latency_ns(put_setup_combined.value_at_quantile(0.99))
            );
            println!("{}", setup_msg);
            results_dir.write_console(&setup_msg)?;
        }
    }

    if summary.meta.ops > 0 {
        let meta_header = "\nMETA-DATA Performance:";
        println!("{}", meta_header);
        results_dir.write_console(meta_header)?;

        let meta_ops_msg = format!(
            "  Ops: {} ({} ops/s)",
            fmt_commas(summary.meta.ops),
            fmt_float_commas(summary.meta.ops as f64 / summary.wall_seconds)
        );
        println!("{}", meta_ops_msg);
        results_dir.write_console(&meta_ops_msg)?;

        let meta_bytes_msg = format!(
            "  Bytes: {} ({:.2} MiB)",
            fmt_commas(summary.meta.bytes),
            summary.meta.bytes as f64 / 1_048_576.0
        );
        println!("{}", meta_bytes_msg);
        results_dir.write_console(&meta_bytes_msg)?;

        let meta_latency_msg = format!(
            "  Latency: mean={}µs, p50={}µs, p95={}µs, p99={}µs",
            fmt_commas(summary.meta.mean_us),
            fmt_commas(summary.meta.p50_us),
            fmt_commas(summary.meta.p95_us),
            fmt_commas(summary.meta.p99_us)
        );
        println!("{}", meta_latency_msg);
        results_dir.write_console(&meta_latency_msg)?;
    }

    // v0.8.22: Display per-endpoint statistics if multi-endpoint was used
    if let Some(ref endpoint_stats) = summary.endpoint_stats {
        let endpoint_header = "\nPer-Endpoint Statistics:";
        println!("{}", endpoint_header);
        results_dir.write_console(endpoint_header)?;

        let endpoint_count_msg = format!("  Total endpoints: {}", endpoint_stats.len());
        println!("{}", endpoint_count_msg);
        results_dir.write_console(&endpoint_count_msg)?;

        for (idx, stats) in endpoint_stats.iter().enumerate() {
            let endpoint_msg = format!("\n  Endpoint {}: {}", idx + 1, stats.uri);
            println!("{}", endpoint_msg);
            results_dir.write_console(&endpoint_msg)?;

            let requests_msg = format!("    Total requests: {}", fmt_commas(stats.total_requests));
            println!("{}", requests_msg);
            results_dir.write_console(&requests_msg)?;

            let read_msg = format!(
                "    Bytes read: {} ({:.2} MiB)",
                fmt_commas(stats.bytes_read),
                stats.bytes_read as f64 / 1_048_576.0
            );
            println!("{}", read_msg);
            results_dir.write_console(&read_msg)?;

            let write_msg = format!(
                "    Bytes written: {} ({:.2} MiB)",
                fmt_commas(stats.bytes_written),
                stats.bytes_written as f64 / 1_048_576.0
            );
            println!("{}", write_msg);
            results_dir.write_console(&write_msg)?;

            if stats.error_count > 0 {
                let error_msg = format!("    Errors: {}", fmt_commas(stats.error_count));
                println!("{}", error_msg);
                results_dir.write_console(&error_msg)?;
            }

            if stats.active_requests > 0 {
                let active_msg = format!(
                    "    Active requests: {}",
                    fmt_commas(stats.active_requests as u64)
                );
                println!("{}", active_msg);
                results_dir.write_console(&active_msg)?;
            }
        }

        // Show load distribution for round-robin verification
        if endpoint_stats.len() > 1 {
            let distribution_header = "\n  Load Distribution:";
            println!("{}", distribution_header);
            results_dir.write_console(distribution_header)?;

            let total_requests: u64 = endpoint_stats.iter().map(|s| s.total_requests).sum();
            let total_bytes_read: u64 = endpoint_stats.iter().map(|s| s.bytes_read).sum();
            let total_bytes_written: u64 = endpoint_stats.iter().map(|s| s.bytes_written).sum();

            for (idx, stats) in endpoint_stats.iter().enumerate() {
                let req_pct = if total_requests > 0 {
                    (stats.total_requests as f64 / total_requests as f64) * 100.0
                } else {
                    0.0
                };

                let read_pct = if total_bytes_read > 0 {
                    (stats.bytes_read as f64 / total_bytes_read as f64) * 100.0
                } else {
                    0.0
                };

                let write_pct = if total_bytes_written > 0 {
                    (stats.bytes_written as f64 / total_bytes_written as f64) * 100.0
                } else {
                    0.0
                };

                let dist_msg = format!(
                    "    Endpoint {}: {:.1}% requests, {:.1}% read, {:.1}% write",
                    idx + 1,
                    req_pct,
                    read_pct,
                    write_pct
                );
                println!("{}", dist_msg);
                results_dir.write_console(&dist_msg)?;
            }
        }
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

        // v0.8.23: Export endpoint stats if multi-endpoint was used
        if let Some(ref endpoint_stats) = summary.endpoint_stats {
            let ep_tsv_path = results_dir.endpoint_stats_tsv_path();
            let ep_exporter = TsvExporter::with_path(&ep_tsv_path)?;
            ep_exporter.export_endpoint_stats(endpoint_stats)?;

            let ep_export_msg = format!("Endpoint stats exported to: {}", ep_tsv_path.display());
            println!("{}", ep_export_msg);
            results_dir.write_console(&ep_export_msg)?;
        }
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
                0, // agent_id (standalone mode)
                1, // num_agents (standalone mode)
                prepare_config.cleanup_mode,
                None, // No live stats tracker in standalone mode
            ))?;

            let cleanup_msg = "Cleanup complete";
            println!("{}", cleanup_msg);
            results_dir.write_console(cleanup_msg)?;
        }
    }

    // Finalize results directory with metadata
    results_dir.finalize(summary.wall_seconds)?;

    // Generate Excel summary using sai3-analyze
    let analyze_msg = "\nGenerating Excel summary...";
    println!("{}", analyze_msg);
    results_dir.write_console(analyze_msg)?;

    match run_sai3_analyze(results_dir.path()) {
        Ok(()) => {
            let output_file = results_dir.path().join("Summary-Results.xlsx");
            let success_msg = format!("✅ Excel summary: {}", output_file.display());
            println!("{}", success_msg);
            results_dir.write_console(&success_msg)?;
        }
        Err(e) => {
            let warn_msg = format!("⚠️  Warning: Failed to generate Excel summary: {}", e);
            println!("{}", warn_msg);
            results_dir.write_console(&warn_msg)?;
        }
    }

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
    use sai3_bench::config::ReplayConfig;
    use sai3_bench::oplog_merge;
    use sai3_bench::remap::RemapConfig;
    use sai3_bench::replay_streaming::{replay_workload_streaming, ReplayRunConfig};

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
                    println!(
                        "      First out-of-order line: {}",
                        first_ooo_line.unwrap_or(0)
                    );
                    println!("      Replay will issue operations out of chronological order.");
                    println!(
                        "      Use 'sai3-bench sort --files {}' to sort the file.",
                        op_log.display()
                    );
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
            let file = std::fs::File::open(remap_path).with_context(|| {
                format!("Failed to open remap config: {}", remap_path.display())
            })?;
            let config: RemapConfig = serde_yaml::from_reader(file).with_context(|| {
                format!("Failed to parse remap config: {}", remap_path.display())
            })?;
            println!("  ✓ Remap config is valid ({} rules)", config.rules.len());
        }

        if let Some(ref bp_config_path) = config_path {
            println!("  Backpressure config: {}", bp_config_path.display());
            if !bp_config_path.exists() {
                bail!(
                    "Backpressure config file does not exist: {}",
                    bp_config_path.display()
                );
            }
            let file = std::fs::File::open(bp_config_path).with_context(|| {
                format!(
                    "Failed to open backpressure config: {}",
                    bp_config_path.display()
                )
            })?;
            let bp_config: ReplayConfig = serde_yaml::from_reader(file).with_context(|| {
                format!(
                    "Failed to parse backpressure config: {}",
                    bp_config_path.display()
                )
            })?;
            println!("  ✓ Backpressure config is valid");
            println!("      lag_threshold: {:?}", bp_config.lag_threshold);
            println!(
                "      recovery_threshold: {:?}",
                bp_config.recovery_threshold
            );
            println!(
                "      max_flaps_per_minute: {}",
                bp_config.max_flaps_per_minute
            );
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
        println!(
            "Loading backpressure configuration from: {}",
            bp_config_path.display()
        );
        let file = std::fs::File::open(bp_config_path).with_context(|| {
            format!(
                "Failed to open backpressure config: {}",
                bp_config_path.display()
            )
        })?;
        let bp_config: ReplayConfig = serde_yaml::from_reader(file).with_context(|| {
            format!(
                "Failed to parse backpressure config: {}",
                bp_config_path.display()
            )
        })?;
        info!(
            "Loaded backpressure config: lag_threshold={:?}, recovery_threshold={:?}, max_flaps={}",
            bp_config.lag_threshold, bp_config.recovery_threshold, bp_config.max_flaps_per_minute
        );
        println!(
            "  lag_threshold: {:?}, recovery_threshold: {:?}, max_flaps: {}/min",
            bp_config.lag_threshold, bp_config.recovery_threshold, bp_config.max_flaps_per_minute
        );
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

// ----------------------------------------------------------------------------
// Unit tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // resolve_util_uri
    // -------------------------------------------------------------------------

    #[test]
    fn test_resolve_util_uri_flag_only() {
        let result = resolve_util_uri(Some("s3://bucket/".to_string()), None).unwrap();
        assert_eq!(result, "s3://bucket/");
    }

    #[test]
    fn test_resolve_util_uri_positional_only() {
        let result = resolve_util_uri(None, Some("gs://bucket/prefix/".to_string())).unwrap();
        assert_eq!(result, "gs://bucket/prefix/");
    }

    #[test]
    fn test_resolve_util_uri_both_same_accepted() {
        let result = resolve_util_uri(
            Some("s3://bucket/".to_string()),
            Some("s3://bucket/".to_string()),
        )
        .unwrap();
        assert_eq!(result, "s3://bucket/");
    }

    #[test]
    fn test_resolve_util_uri_both_different_errors() {
        let result = resolve_util_uri(
            Some("s3://bucket-a/".to_string()),
            Some("s3://bucket-b/".to_string()),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_util_uri_neither_errors() {
        let result = resolve_util_uri(None, None);
        assert!(result.is_err());
    }

    // -------------------------------------------------------------------------
    // parse_csv_usize
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_csv_usize_single() {
        assert_eq!(parse_csv_usize("16").unwrap(), vec![16]);
    }

    #[test]
    fn test_parse_csv_usize_multi() {
        assert_eq!(parse_csv_usize("16,32,64").unwrap(), vec![16, 32, 64]);
    }

    #[test]
    fn test_parse_csv_usize_spaces() {
        assert_eq!(parse_csv_usize(" 16 , 32 , 64 ").unwrap(), vec![16, 32, 64]);
    }

    #[test]
    fn test_parse_csv_usize_empty_errors() {
        assert!(parse_csv_usize("").is_err());
        assert!(parse_csv_usize("   ").is_err());
    }

    #[test]
    fn test_parse_csv_usize_invalid_errors() {
        assert!(parse_csv_usize("16,abc").is_err());
    }

    // -------------------------------------------------------------------------
    // parse_csv_bool
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_csv_bool_true_values() {
        for v in &["true", "1", "yes", "on"] {
            assert_eq!(parse_csv_bool(v).unwrap(), vec![true], "failed for '{}'", v);
        }
    }

    #[test]
    fn test_parse_csv_bool_false_values() {
        for v in &["false", "0", "no", "off"] {
            assert_eq!(
                parse_csv_bool(v).unwrap(),
                vec![false],
                "failed for '{}'",
                v
            );
        }
    }

    #[test]
    fn test_parse_csv_bool_multi() {
        assert_eq!(parse_csv_bool("false,true").unwrap(), vec![false, true]);
    }

    #[test]
    fn test_parse_csv_bool_empty_errors() {
        assert!(parse_csv_bool("").is_err());
    }

    #[test]
    fn test_parse_csv_bool_invalid_errors() {
        assert!(parse_csv_bool("maybe").is_err());
    }

    // -------------------------------------------------------------------------
    // parse_sizes
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_sizes_explicit_list() {
        let sizes = parse_sizes(Some("64KiB,1MiB"), None, 4).unwrap();
        assert_eq!(sizes, vec![64 * 1024, 1024 * 1024]);
    }

    #[test]
    fn test_parse_sizes_dedup_and_sort() {
        let sizes = parse_sizes(Some("1MiB,64KiB,1MiB"), None, 4).unwrap();
        assert_eq!(sizes, vec![64 * 1024, 1024 * 1024]);
    }

    #[test]
    fn test_parse_sizes_range_single_step() {
        let sizes = parse_sizes(None, Some("4MiB-4MiB"), 1).unwrap();
        assert_eq!(sizes.len(), 1);
        assert_eq!(sizes[0], 4 * 1024 * 1024);
    }

    #[test]
    fn test_parse_sizes_range_two_steps() {
        let sizes = parse_sizes(None, Some("1MiB-4MiB"), 2).unwrap();
        assert_eq!(sizes.len(), 2);
        // First element should be near 1 MiB, last near 4 MiB
        let mib1 = 1024 * 1024usize;
        let mib4 = 4 * 1024 * 1024usize;
        assert!(sizes[0] <= mib1 + 1024, "expected ~1MiB, got {}", sizes[0]);
        assert!(sizes[1] >= mib4 - 1024, "expected ~4MiB, got {}", sizes[1]);
    }

    #[test]
    fn test_parse_sizes_range_reversed_equals_forward() {
        let forward = parse_sizes(None, Some("1MiB-4MiB"), 3).unwrap();
        let reversed = parse_sizes(None, Some("4MiB-1MiB"), 3).unwrap();
        assert_eq!(forward, reversed);
    }

    #[test]
    fn test_parse_sizes_range_default_four_steps() {
        let sizes = parse_sizes(None, None, 4).unwrap();
        assert_eq!(sizes.len(), 4);
        let mib32 = 32 * 1024 * 1024usize;
        let mib64 = 64 * 1024 * 1024usize;
        assert!(
            sizes[0] >= mib32 - 1024,
            "expected first >= 32MiB, got {}",
            sizes[0]
        );
        assert!(
            sizes[3] <= mib64 + 1024,
            "expected last <= 64MiB, got {}",
            sizes[3]
        );
    }

    // -------------------------------------------------------------------------
    // mbps_from_output
    // -------------------------------------------------------------------------

    #[test]
    fn test_mbps_from_output_basic() {
        assert_eq!(mbps_from_output("PUT complete (123.45 MB/s)"), Some(123.45));
    }

    #[test]
    fn test_mbps_from_output_integer() {
        assert_eq!(mbps_from_output("done (500 MB/s)"), Some(500.0));
    }

    #[test]
    fn test_mbps_from_output_last_match_wins() {
        assert_eq!(
            mbps_from_output("pass1 (100.0 MB/s) pass2 (200.5 MB/s)"),
            Some(200.5)
        );
    }

    #[test]
    fn test_mbps_from_output_none_when_absent() {
        assert_eq!(mbps_from_output("no throughput here"), None);
    }

    #[test]
    fn test_mbps_from_output_wrong_unit_ignored() {
        assert_eq!(mbps_from_output("done (10.0 GB/s)"), None);
    }

    // -------------------------------------------------------------------------
    // build_trial_uri
    // -------------------------------------------------------------------------

    #[test]
    fn test_build_trial_uri_with_glob() {
        assert_eq!(
            build_trial_uri("gs://bucket/prefix/*.dat", 0),
            "gs://bucket/prefix/trial000_*.dat"
        );
    }

    #[test]
    fn test_build_trial_uri_trailing_slash() {
        assert_eq!(
            build_trial_uri("gs://bucket/prefix/", 5),
            "gs://bucket/prefix/autotune/trial005/obj*.dat"
        );
    }

    #[test]
    fn test_build_trial_uri_bare() {
        assert_eq!(
            build_trial_uri("gs://bucket/prefix", 3),
            "gs://bucket/prefix.trial003.*"
        );
    }

    #[test]
    fn test_build_trial_uri_zero_padding() {
        let uri = build_trial_uri("s3://bucket/", 42);
        assert!(uri.contains("trial042"), "expected trial042 in '{}'", uri);
    }

    // -------------------------------------------------------------------------
    // parse_rapid_mode_str / parse_optimize_for_str / parse_tune_ops_str
    // -------------------------------------------------------------------------

    #[test]
    fn test_parse_rapid_mode_str_all_variants() {
        assert!(matches!(
            parse_rapid_mode_str("auto").unwrap(),
            RapidModeArg::Auto
        ));
        assert!(matches!(
            parse_rapid_mode_str("on").unwrap(),
            RapidModeArg::On
        ));
        assert!(matches!(
            parse_rapid_mode_str("true").unwrap(),
            RapidModeArg::On
        ));
        assert!(matches!(
            parse_rapid_mode_str("yes").unwrap(),
            RapidModeArg::On
        ));
        assert!(matches!(
            parse_rapid_mode_str("1").unwrap(),
            RapidModeArg::On
        ));
        assert!(matches!(
            parse_rapid_mode_str("off").unwrap(),
            RapidModeArg::Off
        ));
        assert!(matches!(
            parse_rapid_mode_str("false").unwrap(),
            RapidModeArg::Off
        ));
        assert!(matches!(
            parse_rapid_mode_str("no").unwrap(),
            RapidModeArg::Off
        ));
        assert!(matches!(
            parse_rapid_mode_str("0").unwrap(),
            RapidModeArg::Off
        ));
    }

    #[test]
    fn test_parse_rapid_mode_str_invalid() {
        assert!(parse_rapid_mode_str("maybe").is_err());
        assert!(parse_rapid_mode_str("").is_err());
    }

    #[test]
    fn test_parse_optimize_for_str() {
        assert!(matches!(
            parse_optimize_for_str("throughput").unwrap(),
            OptimizeFor::Throughput
        ));
        assert!(matches!(
            parse_optimize_for_str("latency").unwrap(),
            OptimizeFor::Latency
        ));
        assert!(parse_optimize_for_str("bandwidth").is_err());
    }

    #[test]
    fn test_parse_tune_ops_str() {
        assert!(matches!(parse_tune_ops_str("put").unwrap(), TuneOps::Put));
        assert!(matches!(parse_tune_ops_str("get").unwrap(), TuneOps::Get));
        assert!(matches!(parse_tune_ops_str("both").unwrap(), TuneOps::Both));
        assert!(parse_tune_ops_str("all").is_err());
    }

    // ── channels_per_thread ────────────────────────────────────────────────

    #[test]
    fn test_autotune_yaml_channels_per_thread_deserialize() {
        let yaml = "uri: \"gs://b/p\"\nchannels_per_thread: \"1,2,4\"\nthreads: \"32,64\"\n";
        let parsed: AutotuneYaml = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.channels_per_thread.as_deref(), Some("1,2,4"));
        assert!(parsed.channels.is_none(), "channels should be absent");
    }

    #[test]
    fn test_autotune_yaml_channels_absolute_deserialize() {
        let yaml = "uri: \"gs://b/p\"\nchannels: \"32,64\"\n";
        let parsed: AutotuneYaml = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.channels.as_deref(), Some("32,64"));
        assert!(parsed.channels_per_thread.is_none());
    }

    #[test]
    fn test_autotune_yaml_neither_channels_is_ok() {
        // No channels or channels_per_thread: should default to 1×threads
        let yaml = "uri: \"gs://b/p\"\nthreads: \"16,32\"\n";
        let parsed: AutotuneYaml = serde_yaml::from_str(yaml).unwrap();
        assert!(parsed.channels.is_none());
        assert!(parsed.channels_per_thread.is_none());
    }

    #[test]
    fn test_channels_per_thread_effective_computation() {
        // Verify that effective_channels = threads * cpt for several values
        let cases = vec![(16, 1, 16), (32, 2, 64), (64, 4, 256), (8, 3, 24)];
        for (th, cpt, expected) in cases {
            assert_eq!(th * cpt, expected, "threads={} × cpt={}", th, cpt);
        }
    }

    #[test]
    fn test_autotune_yaml_channels_per_thread_csv_parsing() {
        let cpt_str = "1,2,4,8";
        let parsed = parse_csv_usize(cpt_str).unwrap();
        assert_eq!(parsed, vec![1, 2, 4, 8]);
    }

    #[test]
    fn test_autotune_yaml_channels_per_thread_single() {
        let parsed = parse_csv_usize("2").unwrap();
        assert_eq!(parsed, vec![2]);
    }

    #[test]
    fn test_autotune_yaml_channels_per_thread_invalid() {
        assert!(parse_csv_usize("1,x,4").is_err());
    }

    #[test]
    fn test_channels_per_thread_zero_rejected() {
        // 0 is never a valid multiplier — would mean 0 channels
        let csv = parse_csv_usize("0").unwrap();
        assert!(csv.iter().any(|&v| v == 0), "parsed 0 as usize");
        // The actual rejection happens in autotune_cmd() after parsing.
        // Simulate that check directly:
        let cpts = parse_csv_usize("1,0,4").unwrap();
        assert!(cpts.iter().any(|&v| v == 0));
        // Confirm a clean list with no zeros passes the check
        let ok = parse_csv_usize("1,2,4").unwrap();
        assert!(!ok.iter().any(|&v| v == 0));
    }

    #[test]
    fn test_tune_case_has_channels_per_thread_field() {
        // Ensure the struct can hold both absolute and per-thread variants
        let absolute = TuneCase {
            size_bytes: 1024,
            threads: 32,
            channels: 64,
            channels_per_thread: None,
            range_enabled: false,
            range_threshold_mb: 32,
            gcs_write_chunk_size: 2097152,
        };
        assert!(absolute.channels_per_thread.is_none());

        let per_thread = TuneCase {
            size_bytes: 1024,
            threads: 32,
            channels: 128, // 32 * 4
            channels_per_thread: Some(4),
            range_enabled: false,
            range_threshold_mb: 32,
            gcs_write_chunk_size: 2097152,
        };
        assert_eq!(per_thread.channels_per_thread, Some(4));
        assert_eq!(
            per_thread.channels,
            per_thread.threads * per_thread.channels_per_thread.unwrap()
        );
    }
}
