// src/multiprocess.rs
//
// Multi-process execution for sai3-bench (v0.7.3+)
// Spawns multiple child processes to scale parallel I/O operations

use anyhow::{Context, Result, bail};
use hdrhistogram::Histogram;
use hdrhistogram::serialization::Deserializer;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use tracing::info;

use crate::config::Config;
use crate::workload::{IpcSummary, Summary, OpAgg, SizeBins};
use crate::directory_tree::TreeManifest;

/// Execute workload with multi-process scaling
/// Spawns N child processes, each running workload with adjusted concurrency
/// Returns merged results from all processes
pub async fn run_multiprocess(
    cfg: &Config,
    tree_manifest: Option<TreeManifest>,
    op_log_base_path: Option<&std::path::Path>,
) -> Result<Summary> {
    let process_count = cfg.processes
        .as_ref()
        .map(|p| p.resolve())
        .unwrap_or(1);
    
    if process_count == 1 {
        // Single process mode - use existing workload::run directly
        return crate::workload::run(cfg, tree_manifest).await;
    }
    
    info!("Starting multi-process execution: {} processes", process_count);
    
    // Adjust concurrency per process
    let threads_per_process = (cfg.concurrency + process_count - 1) / process_count;
    info!("Threads per process: {} (total: {})", 
        threads_per_process, threads_per_process * process_count);
    
    // Spawn child processes
    let mut children = Vec::with_capacity(process_count);
    
    for proc_id in 0..process_count {
        info!("Spawning child process {}/{}", proc_id + 1, process_count);
        
        // Each child gets adjusted config via stdin
        let mut child_cfg = cfg.clone();
        child_cfg.concurrency = threads_per_process;
        child_cfg.processes = None; // Prevent recursive spawning
        
        let child_config_json = serde_json::to_string(&child_cfg)
            .context("Failed to serialize child config")?;
        
        // Build command for child process
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("internal-worker")
            .arg("--worker-id")
            .arg(proc_id.to_string());
        
        // Add op-log argument if enabled
        if let Some(base_path) = op_log_base_path {
            let worker_op_log = crate::oplog_merge::worker_oplog_path(base_path, proc_id);
            cmd.arg("--op-log").arg(worker_op_log);
        }
        
        cmd.stdin(Stdio::piped())   // Config JSON goes here
            .stdout(Stdio::piped())  // Summary JSON comes back
            .stderr(Stdio::inherit()); // Child logs go to parent stderr
        
        let mut child = cmd.spawn()
            .with_context(|| format!("Failed to spawn child process {}", proc_id))?;
        
        // Write config JSON to child's stdin
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(child_config_json.as_bytes())
                .context("Failed to write config to child stdin")?;
            stdin.flush()?;
        }
        
        children.push((proc_id, child));
    }
    
    // Collect results from all children
    let mut summaries = Vec::with_capacity(process_count);
    
    for (proc_id, mut child) in children {
        info!("Waiting for child process {}", proc_id);
        
        // Read JSON from child's stdout
        let stdout = child.stdout.take()
            .context("Failed to capture child stdout")?;
        
        let reader = BufReader::new(stdout);
        let mut json_line = String::new();
        
        for line in reader.lines() {
            let line = line.context("Failed to read from child stdout")?;
            if line.starts_with("{") {
                json_line = line;
                break;
            }
        }
        
        if json_line.is_empty() {
            bail!("Child process {} produced no JSON output", proc_id);
        }
        
        let ipc_summary: IpcSummary = serde_json::from_str(&json_line)
            .with_context(|| format!("Failed to parse JSON from child {}", proc_id))?;
        
        summaries.push(ipc_summary);
        
        // Wait for child to exit
        let status = child.wait()
            .with_context(|| format!("Failed to wait for child {}", proc_id))?;
        
        if !status.success() {
            bail!("Child process {} exited with status: {:?}", proc_id, status);
        }
        
        info!("Child process {} completed successfully", proc_id);
    }
    
    info!("All child processes completed, merging results");
    
    // Merge summaries from all processes
    merge_summaries(summaries)
}

/// Merge multiple IpcSummary results into a single Summary
/// Deserializes and merges HDR histograms for accurate percentiles
fn merge_summaries(summaries: Vec<IpcSummary>) -> Result<Summary> {
    if summaries.is_empty() {
        bail!("No summaries to merge");
    }
    
    let mut merged_get = OpAgg::default();
    let mut merged_put = OpAgg::default();
    let mut merged_meta = OpAgg::default();
    let mut merged_get_bins = SizeBins::default();
    let mut merged_put_bins = SizeBins::default();
    let mut merged_meta_bins = SizeBins::default();
    
    let mut total_wall_seconds: f64 = 0.0;
    let mut total_bytes = 0u64;
    let mut total_ops = 0u64;
    
    // Deserialize and merge histograms
    let num_buckets = summaries[0].get_hists_serialized.len();
    let mut merged_get_hists = create_empty_ophists(num_buckets);
    let mut merged_put_hists = create_empty_ophists(num_buckets);
    let mut merged_meta_hists = create_empty_ophists(num_buckets);
    
    for summary in &summaries {
        total_wall_seconds = total_wall_seconds.max(summary.wall_seconds);
        total_bytes += summary.total_bytes;
        total_ops += summary.total_ops;
        
        // GET
        merged_get.ops += summary.get.ops;
        merged_get.bytes += summary.get.bytes;
        
        // PUT
        merged_put.ops += summary.put.ops;
        merged_put.bytes += summary.put.bytes;
        
        // META
        merged_meta.ops += summary.meta.ops;
        merged_meta.bytes += summary.meta.bytes;
        
        // Merge size bins
        merged_get_bins.merge_from(&summary.get_bins);
        merged_put_bins.merge_from(&summary.put_bins);
        merged_meta_bins.merge_from(&summary.meta_bins);
        
        // Deserialize and merge histograms
        merge_hist_vec(&mut merged_get_hists, &summary.get_hists_serialized)?;
        merge_hist_vec(&mut merged_put_hists, &summary.put_hists_serialized)?;
        merge_hist_vec(&mut merged_meta_hists, &summary.meta_hists_serialized)?;
    }
    
    // Calculate percentiles from merged histograms
    merged_get.p50_us = calculate_percentile(&merged_get_hists, 50.0);
    merged_get.p95_us = calculate_percentile(&merged_get_hists, 95.0);
    merged_get.p99_us = calculate_percentile(&merged_get_hists, 99.0);
    merged_get.mean_us = calculate_mean(&merged_get_hists);
    
    merged_put.p50_us = calculate_percentile(&merged_put_hists, 50.0);
    merged_put.p95_us = calculate_percentile(&merged_put_hists, 95.0);
    merged_put.p99_us = calculate_percentile(&merged_put_hists, 99.0);
    merged_put.mean_us = calculate_mean(&merged_put_hists);
    
    merged_meta.p50_us = calculate_percentile(&merged_meta_hists, 50.0);
    merged_meta.p95_us = calculate_percentile(&merged_meta_hists, 95.0);
    merged_meta.p99_us = calculate_percentile(&merged_meta_hists, 99.0);
    merged_meta.mean_us = calculate_mean(&merged_meta_hists);
    
    // Overall percentiles (use GET as primary, as before)
    let overall_p50 = merged_get.p50_us;
    let overall_p95 = merged_get.p95_us;
    let overall_p99 = merged_get.p99_us;
    
    // v0.7.13: Aggregate error statistics across all processes
    let total_errors: u64 = summaries.iter().map(|s| s.total_errors).sum();
    let avg_error_rate: f64 = if !summaries.is_empty() {
        summaries.iter().map(|s| s.error_rate).sum::<f64>() / summaries.len() as f64
    } else {
        0.0
    };
    
    Ok(Summary {
        wall_seconds: total_wall_seconds,
        total_bytes,
        total_ops,
        p50_us: overall_p50,
        p95_us: overall_p95,
        p99_us: overall_p99,
        get: merged_get,
        put: merged_put,
        meta: merged_meta,
        get_bins: merged_get_bins,
        put_bins: merged_put_bins,
        meta_bins: merged_meta_bins,
        get_hists: merged_get_hists,
        put_hists: merged_put_hists,
        meta_hists: merged_meta_hists,
        total_errors,
        error_rate: avg_error_rate,
    })
}

/// Create empty OpHists with specified number of buckets
fn create_empty_ophists(num_buckets: usize) -> crate::metrics::OpHists {
    use std::sync::{Arc, Mutex};
    let mut buckets = Vec::with_capacity(num_buckets);
    for _ in 0..num_buckets {
        buckets.push(Mutex::new(
            Histogram::<u64>::new_with_bounds(1, 3_600_000_000, 3)
                .expect("Failed to allocate histogram"),
        ));
    }
    crate::metrics::OpHists {
        buckets: Arc::new(buckets),
    }
}

/// Deserialize and merge histogram vector
fn merge_hist_vec(
    target: &mut crate::metrics::OpHists,
    serialized: &[String],
) -> Result<()> {
    use base64::Engine;
    
    for (i, encoded) in serialized.iter().enumerate() {
        if i >= target.buckets.len() {
            bail!("Histogram bucket index {} out of range", i);
        }
        
        // Decode base64
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("Failed to decode base64 histogram")?;
        
        // Deserialize histogram
        let mut cursor = std::io::Cursor::new(&bytes);
        let deserialized: Histogram<u64> = Deserializer::new()
            .deserialize(&mut cursor)
            .context("Failed to deserialize histogram")?;
        
        // Merge into target
        let mut target_hist = target.buckets[i].lock().unwrap();
        target_hist.add(&deserialized)
            .context("Failed to merge histograms")?;
    }
    
    Ok(())
}

/// Calculate percentile across all buckets in OpHists
fn calculate_percentile(ophists: &crate::metrics::OpHists, percentile: f64) -> u64 {
    // Find first non-empty histogram
    for bucket in ophists.buckets.iter() {
        let hist = bucket.lock().unwrap();
        if hist.len() > 0 {
            return hist.value_at_quantile(percentile / 100.0);
        }
    }
    0
}

/// Calculate mean across all buckets in OpHists
fn calculate_mean(ophists: &crate::metrics::OpHists) -> u64 {
    let mut total_count = 0u64;
    let mut weighted_sum = 0u64;
    
    for bucket in ophists.buckets.iter() {
        let hist = bucket.lock().unwrap();
        if hist.len() > 0 {
            total_count += hist.len();
            weighted_sum += (hist.mean() * hist.len() as f64) as u64;
        }
    }
    
    if total_count > 0 {
        weighted_sum / total_count
    } else {
        0
    }
}

/// Internal worker mode - run workload and output JSON to stdout
pub async fn run_internal_worker(config_json: &str) -> Result<()> {
    let cfg: Config = serde_json::from_str(config_json)
        .context("Failed to parse worker config")?;
    
    // Run workload
    let summary = crate::workload::run(&cfg, None).await?;
    
    // Convert to IPC format and output as JSON
    let ipc_summary = IpcSummary::from(&summary);
    let json = serde_json::to_string(&ipc_summary)
        .context("Failed to serialize summary")?;
    
    // Output JSON to stdout (parent reads this)
    println!("{}", json);
    
    Ok(())
}
