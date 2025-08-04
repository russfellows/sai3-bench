// src/bin/run.rs
use anyhow::{Context, Result};
use dotenvy::dotenv;
use clap::Parser;
use std::fs;

#[derive(Parser, Debug)]
#[command(name = "s3bench-run", version)]
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
    let cfg: s3_bench::config::Config =
        serde_yaml::from_slice(&buf).with_context(|| format!("parse {}", cli.config))?;

    let summary = s3_bench::workload::run(&cfg).await?;

    // ---- Overall combined line (back-compat) ----
    let mb = summary.total_bytes as f64 / (1024.0 * 1024.0);
    let mbps = mb / summary.wall_seconds.max(1e-9);
    println!(
        "WALL {:>6.2}s  OPS {:>10}  BYTES {:>12} ({:.2} MB)  THROUGHPUT {:.2} MB/s  p50={}ms p95={}ms p99={}ms",
        summary.wall_seconds,
        summary.total_ops,
        summary.total_bytes,
        mb,
        mbps,
        summary.p50_ms, summary.p95_ms, summary.p99_ms
    );

    // ---- Per-op latency/throughput summaries ----
    if summary.get.ops > 0 {
        let mb = summary.get.bytes as f64 / (1024.0 * 1024.0);
        let mbps = mb / summary.wall_seconds.max(1e-9);
        println!(
            "GET   ops={:>10}  bytes={:>12} ({:>8.2} MB)  {:.2} MB/s  p50={}ms p95={}ms p99={}ms",
            summary.get.ops,
            summary.get.bytes,
            mb,
            mbps,
            summary.get.p50_ms, summary.get.p95_ms, summary.get.p99_ms
        );
    }
    if summary.put.ops > 0 {
        let mb = summary.put.bytes as f64 / (1024.0 * 1024.0);
        let mbps = mb / summary.wall_seconds.max(1e-9);
        println!(
            "PUT   ops={:>10}  bytes={:>12} ({:>8.2} MB)  {:.2} MB/s  p50={}ms p95={}ms p99={}ms",
            summary.put.ops,
            summary.put.bytes,
            mb,
            mbps,
            summary.put.p50_ms, summary.put.p95_ms, summary.put.p99_ms
        );
    }

    // ---- Size bins (simple display by bin index) ----
    // We print separate tables for GET and PUT if present.
    if !summary.get_bins.by_bucket.is_empty() {
        println!("Size bins (GET):");
        print_bins(&summary.get_bins);
    }
    if !summary.put_bins.by_bucket.is_empty() {
        println!("Size bins (PUT):");
        print_bins(&summary.put_bins);
    }

    Ok(())
}

/// Helper: print bins sorted by bucket index.
/// (Labels are shown as `bin #N`; if you later add a helper to map indices
/// to human-readable ranges, swap it in here.)
fn print_bins(bins: &s3_bench::workload::SizeBins) {
    let mut items: Vec<(usize, (u64, u64))> = bins.by_bucket.iter().map(|(k, v)| (*k, *v)).collect();
    items.sort_by_key(|(k, _)| *k);
    for (idx, (ops, bytes)) in items {
        let mb = bytes as f64 / (1024.0 * 1024.0);
        println!("  bin {:>2}: ops={:>10}  bytes={:>12} ({:>8.2} MB)", idx, ops, bytes, mb);
    }
}
