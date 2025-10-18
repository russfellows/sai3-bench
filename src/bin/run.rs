// src/bin/run.rs
//
// ⚠️  DEPRECATED - This binary is no longer built by default (v0.6.9+)
// ⚠️  Use: sai3-bench run --config <CONFIG>
// ⚠️  
// ⚠️  This legacy standalone runner has been replaced by the "run" subcommand
// ⚠️  in the main sai3-bench binary, which provides the same functionality
// ⚠️  plus additional features like --dry-run, --prepare-only, --verify, etc.
// ⚠️  
// ⚠️  This file is kept for reference only.

use anyhow::{Context, Result};
use dotenvy::dotenv;
use clap::Parser;
use std::fs;

#[derive(Parser, Debug)]
#[command(name = "sai3bench-run", version)]
struct Cli {
    /// YAML config file path
    #[arg(short = 'c', long = "config")]
    config: String,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    // Load environment: AWS_REGION, AWS_ENDPOINT_URL, creds, etc.
    dotenv().ok();

    let cli = Cli::parse();
    let buf = fs::read(&cli.config).with_context(|| format!("read {}", cli.config))?;
    let cfg: sai3_bench::config::Config =
        serde_yaml::from_slice(&buf).with_context(|| format!("parse {}", cli.config))?;

    let summary = sai3_bench::workload::run(&cfg).await?;

    // ---- Overall combined line (back-compat) ----
    let mb = summary.total_bytes as f64 / (1024.0 * 1024.0);
    let mbps = mb / summary.wall_seconds.max(1e-9);
    println!(
        "WALL {:>6.2}s  OPS {:>10}  BYTES {:>12} ({:.2} MB)  THROUGHPUT {:.2} MB/s  p50={}µs p95={}µs p99={}µs",
        summary.wall_seconds,
        summary.total_ops,
        summary.total_bytes,
        mb,
        mbps,
        summary.p50_us, summary.p95_us, summary.p99_us
    );

    // ---- Per-op latency/throughput summaries ----
    if summary.get.ops > 0 {
        let mb = summary.get.bytes as f64 / (1024.0 * 1024.0);
        let mbps = mb / summary.wall_seconds.max(1e-9);
        println!(
            "GET   ops={:>10}  bytes={:>12} ({:>8.2} MB)  {:.2} MB/s  p50={}µs p95={}µs p99={}µs",
            summary.get.ops,
            summary.get.bytes,
            mb,
            mbps,
            summary.get.p50_us, summary.get.p95_us, summary.get.p99_us
        );
    }
    if summary.put.ops > 0 {
        let mb = summary.put.bytes as f64 / (1024.0 * 1024.0);
        let mbps = mb / summary.wall_seconds.max(1e-9);
        println!(
            "PUT   ops={:>10}  bytes={:>12} ({:>8.2} MB)  {:.2} MB/s  p50={}µs p95={}µs p99={}µs",
            summary.put.ops,
            summary.put.bytes,
            mb,
            mbps,
            summary.put.p50_us, summary.put.p95_us, summary.put.p99_us
        );
    }
    if summary.meta.ops > 0 {
        let mb = summary.meta.bytes as f64 / (1024.0 * 1024.0);
        let mbps = mb / summary.wall_seconds.max(1e-9);
        println!(
            "META  ops={:>10}  bytes={:>12} ({:>8.2} MB)  {:.2} MB/s  p50={}µs p95={}µs p99={}µs",
            summary.meta.ops,
            summary.meta.bytes,
            mb,
            mbps,
            summary.meta.p50_us, summary.meta.p95_us, summary.meta.p99_us
        );
    }

    // ---- Size bins (simple display by bin index) ----
    // We print separate tables for GET, PUT, and META if present.
    if !summary.get_bins.by_bucket.is_empty() {
        println!("Size bins (GET):");
        print_bins(&summary.get_bins);
    }
    if !summary.put_bins.by_bucket.is_empty() {
        println!("Size bins (PUT):");
        print_bins(&summary.put_bins);
    }
    if !summary.meta_bins.by_bucket.is_empty() {
        println!("Size bins (META-DATA):");
        print_bins(&summary.meta_bins);
    }

    Ok(())
}

/// Helper: print bins sorted by bucket index.
/// (Labels are shown as `bin #N`; if you later add a helper to map indices
/// to human-readable ranges, swap it in here.)
fn print_bins(bins: &sai3_bench::workload::SizeBins) {
    let mut items: Vec<(usize, (u64, u64))> = bins.by_bucket.iter().map(|(k, v)| (*k, *v)).collect();
    items.sort_by_key(|(k, _)| *k);
    for (idx, (ops, bytes)) in items {
        let mb = bytes as f64 / (1024.0 * 1024.0);
        println!("  bin {:>2}: ops={:>10}  bytes={:>12} ({:>8.2} MB)", idx, ops, bytes, mb);
    }
}
