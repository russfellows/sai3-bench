// src/multiruntime.rs
// Multi-runtime execution mode: spawns N separate tokio runtimes in threads
// Uses native Rust channels for zero-copy communication (no serialization)

use crate::config::Config;
use crate::workload::Summary;
use crate::directory_tree::TreeManifest;
use anyhow::{Context, Result};
use tracing::{info, error};
use std::sync::mpsc;
use std::thread;
use tokio::runtime::Runtime;

/// Run workload using multiple tokio runtimes (one per core/worker)
/// 
/// This mode creates N separate OS threads, each with its own tokio runtime.
/// Each runtime executes the workload with concurrency/N threads.
/// Results are collected via native Rust channels (zero-copy, no serialization).
/// 
/// **Note on op_log**: Per-worker op-logs are not supported in MultiRuntime mode
/// because all workers share the same process and s3dlio uses a global logger.
/// The global op_logger initialized by main.rs will capture all operations.
/// Per-worker op-logs only work with MultiProcess mode (separate processes).
/// 
/// Benefits:
/// - No IPC overhead (in-process communication)
/// - No serialization (direct Summary passing)
/// - Easier debugging (single process)
/// - Better resource sharing (single ObjectStore cache)
/// 
/// Tradeoffs vs MultiProcess:
/// - Less isolation (shared address space)
/// - No fork() benefits for initialization
/// - Potential GIL-like contention on shared resources
/// - Single op-log for all workers (can't split by worker)
pub fn run_multiruntime(
    cfg: &Config,
    num_workers: usize,
    tree_manifest: Option<TreeManifest>,
) -> Result<Summary> {
    info!("Starting multi-runtime execution with {} workers", num_workers);
    
    // Validate worker count
    if num_workers == 0 {
        anyhow::bail!("Number of workers must be at least 1");
    }
    
    if num_workers == 1 {
        info!("Single worker mode - running directly in current runtime");
        // Optimization: skip the complexity for single worker
        let runtime = Runtime::new()
            .context("Failed to create tokio runtime")?;
        return runtime.block_on(async {
            crate::workload::run(cfg, tree_manifest).await
        });
    }
    
    // Create channel for collecting results from workers
    let (tx, rx) = mpsc::channel::<Summary>();
    
    // Calculate concurrency per worker (divide total concurrency across workers)
    let base_concurrency = cfg.concurrency / num_workers;
    let remainder = cfg.concurrency % num_workers;
    
    info!("Total concurrency: {}, base per worker: {}, remainder: {}", 
          cfg.concurrency, base_concurrency, remainder);
    
    // Spawn worker threads
    let mut handles = Vec::new();
    
    for worker_id in 0..num_workers {
        // Clone config for this worker
        let mut worker_cfg = cfg.clone();
        
        // Clone tree_manifest for this worker
        let worker_tree = tree_manifest.clone();
        
        // Assign concurrency for this worker (some get +1 for remainder)
        worker_cfg.concurrency = if worker_id < remainder {
            base_concurrency + 1
        } else {
            base_concurrency
        };
        
        if worker_cfg.concurrency == 0 {
            info!("Worker {} skipped (concurrency would be 0)", worker_id);
            continue;
        }
        
        let tx = tx.clone();
        
        info!("Spawning worker {} with concurrency {}", worker_id, worker_cfg.concurrency);
        
        // Spawn OS thread with its own tokio runtime
        let handle = thread::spawn(move || {
            // Create dedicated tokio runtime for this worker
            let runtime = Runtime::new()
                .expect("Failed to create tokio runtime");
            
            info!("Worker {} runtime created, starting workload", worker_id);
            
            // Run workload in this runtime
            let result = runtime.block_on(async {
                crate::workload::run(&worker_cfg, worker_tree).await
            });
            
            match result {
                Ok(summary) => {
                    info!("Worker {} completed: {} ops in {:.2}s", 
                          worker_id, summary.total_ops, summary.wall_seconds);
                    
                    // Send result back to coordinator
                    if let Err(e) = tx.send(summary) {
                        error!("Worker {} failed to send result: {}", worker_id, e);
                    }
                }
                Err(e) => {
                    error!("Worker {} failed: {}", worker_id, e);
                    // Don't send anything on failure - coordinator will detect missing results
                }
            }
        });
        
        handles.push(handle);
    }
    
    // Drop the original sender so the channel closes when all workers finish
    drop(tx);
    
    info!("All {} workers spawned, waiting for results", handles.len());
    
    // Collect results from workers
    let mut summaries = Vec::new();
    for summary in rx {
        summaries.push(summary);
    }
    
    info!("Collected {} summaries from workers", summaries.len());
    
    // Get expected worker count before consuming the handles vector
    let expected_workers = handles.len();
    
    // Wait for all threads to complete
    for (i, handle) in handles.into_iter().enumerate() {
        if let Err(e) = handle.join() {
            error!("Worker {} thread panicked: {:?}", i, e);
        }
    }
    
    // Check that we got results from all workers
    if summaries.len() != expected_workers {
        anyhow::bail!(
            "Expected {} worker results, but only received {}",
            expected_workers,
            summaries.len()
        );
    }
    
    info!("All workers completed successfully, merging results");
    
    // Merge all summaries
    merge_summaries(summaries)
}

/// Merge multiple Summary results into one aggregate Summary
/// 
/// This performs lossless histogram merging using HDR histogram's add() method.
/// All percentiles in the merged result are mathematically correct.
fn merge_summaries(summaries: Vec<Summary>) -> Result<Summary> {
    if summaries.is_empty() {
        anyhow::bail!("No summaries to merge");
    }
    
    if summaries.len() == 1 {
        // Single summary - just return it
        return Ok(summaries.into_iter().next().unwrap());
    }
    
    info!("Merging {} summaries", summaries.len());
    
    // Start with first summary as base
    let mut merged = summaries[0].clone();
    
    // Merge remaining summaries
    for (i, summary) in summaries.iter().skip(1).enumerate() {
        info!("Merging summary {} of {}", i + 2, summaries.len());
        
        // Accumulate simple metrics
        merged.total_ops += summary.total_ops;
        merged.total_bytes += summary.total_bytes;
        
        // Take maximum wall time (workers run in parallel)
        merged.wall_seconds = merged.wall_seconds.max(summary.wall_seconds);
        
        // Merge GET histograms
        for (bucket_idx, hist) in summary.get_hists.buckets.iter().enumerate() {
            let source_hist = hist.lock().unwrap();
            let mut target_hist = merged.get_hists.buckets[bucket_idx].lock().unwrap();
            target_hist.add(&*source_hist)
                .with_context(|| format!("Failed to merge GET histogram for bucket {}", bucket_idx))?;
        }
        
        // Merge PUT histograms
        for (bucket_idx, hist) in summary.put_hists.buckets.iter().enumerate() {
            let source_hist = hist.lock().unwrap();
            let mut target_hist = merged.put_hists.buckets[bucket_idx].lock().unwrap();
            target_hist.add(&*source_hist)
                .with_context(|| format!("Failed to merge PUT histogram for bucket {}", bucket_idx))?;
        }
        
        // Merge META histograms
        for (bucket_idx, hist) in summary.meta_hists.buckets.iter().enumerate() {
            let source_hist = hist.lock().unwrap();
            let mut target_hist = merged.meta_hists.buckets[bucket_idx].lock().unwrap();
            target_hist.add(&*source_hist)
                .with_context(|| format!("Failed to merge META histogram for bucket {}", bucket_idx))?;
        }
        
        // Merge operation counts and sizes
        merged.get.ops += summary.get.ops;
        merged.get.bytes += summary.get.bytes;
        merged.put.ops += summary.put.ops;
        merged.put.bytes += summary.put.bytes;
        merged.meta.ops += summary.meta.ops;
        
        // Merge size bins
        merged.get_bins.merge_from(&summary.get_bins);
        merged.put_bins.merge_from(&summary.put_bins);
        merged.meta_bins.merge_from(&summary.meta_bins);
    }
    
    // Recalculate percentiles from merged histograms
    recalculate_percentiles(&mut merged)?;
    
    info!("Merge complete: {} total ops in {:.2}s ({:.0} ops/s)",
          merged.total_ops, merged.wall_seconds,
          merged.total_ops as f64 / merged.wall_seconds);
    
    Ok(merged)
}

/// Recalculate all percentiles in a Summary from its merged histograms
fn recalculate_percentiles(summary: &mut Summary) -> Result<()> {
    // Recalculate overall percentiles across all operation types
    let mut all_hists = Vec::new();
    
    // Collect all non-empty histograms
    for hist_mutex in summary.get_hists.buckets.iter() {
        let hist = hist_mutex.lock().unwrap();
        if hist.len() > 0 {
            all_hists.push(hist.clone());
        }
    }
    for hist_mutex in summary.put_hists.buckets.iter() {
        let hist = hist_mutex.lock().unwrap();
        if hist.len() > 0 {
            all_hists.push(hist.clone());
        }
    }
    for hist_mutex in summary.meta_hists.buckets.iter() {
        let hist = hist_mutex.lock().unwrap();
        if hist.len() > 0 {
            all_hists.push(hist.clone());
        }
    }
    
    if !all_hists.is_empty() {
        // Create temporary histogram to merge all operations
        let mut combined = hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)
            .context("Failed to create combined histogram")?;
        for hist in &all_hists {
            combined.add(hist)
                .context("Failed to merge histograms for overall percentiles")?;
        }
        
        summary.p50_us = combined.value_at_quantile(0.5);
        summary.p95_us = combined.value_at_quantile(0.95);
        summary.p99_us = combined.value_at_quantile(0.99);
    }
    
    // Recalculate GET percentiles
    if summary.get.ops > 0 {
        let mut get_combined = hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)
            .context("Failed to create GET combined histogram")?;
        for hist_mutex in summary.get_hists.buckets.iter() {
            let hist = hist_mutex.lock().unwrap();
            if hist.len() > 0 {
                get_combined.add(&*hist)
                    .context("Failed to merge GET histograms")?;
            }
        }
        summary.get.mean_us = get_combined.mean() as u64;
        summary.get.p50_us = get_combined.value_at_quantile(0.5);
        summary.get.p95_us = get_combined.value_at_quantile(0.95);
        summary.get.p99_us = get_combined.value_at_quantile(0.99);
    }
    
    // Recalculate PUT percentiles
    if summary.put.ops > 0 {
        let mut put_combined = hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)
            .context("Failed to create PUT combined histogram")?;
        for hist_mutex in summary.put_hists.buckets.iter() {
            let hist = hist_mutex.lock().unwrap();
            if hist.len() > 0 {
                put_combined.add(&*hist)
                    .context("Failed to merge PUT histograms")?;
            }
        }
        summary.put.mean_us = put_combined.mean() as u64;
        summary.put.p50_us = put_combined.value_at_quantile(0.5);
        summary.put.p95_us = put_combined.value_at_quantile(0.95);
        summary.put.p99_us = put_combined.value_at_quantile(0.99);
    }
    
    // Recalculate META percentiles
    if summary.meta.ops > 0 {
        let mut meta_combined = hdrhistogram::Histogram::<u64>::new_with_bounds(1, 60_000_000, 3)
            .context("Failed to create META combined histogram")?;
        for hist_mutex in summary.meta_hists.buckets.iter() {
            let hist = hist_mutex.lock().unwrap();
            if hist.len() > 0 {
                meta_combined.add(&*hist)
                    .context("Failed to merge META histograms")?;
            }
        }
        summary.meta.mean_us = meta_combined.mean() as u64;
        summary.meta.p50_us = meta_combined.value_at_quantile(0.5);
        summary.meta.p95_us = meta_combined.value_at_quantile(0.95);
        summary.meta.p99_us = meta_combined.value_at_quantile(0.99);
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::OpHists;
    
    #[test]
    fn test_summary_merge_simple() {
        // Create two simple summaries
        let sum1 = Summary {
            wall_seconds: 1.0,
            total_bytes: 1000,
            total_ops: 100,
            p50_us: 100,
            p95_us: 200,
            p99_us: 300,
            get: Default::default(),
            put: Default::default(),
            meta: Default::default(),
            get_bins: Default::default(),
            put_bins: Default::default(),
            meta_bins: Default::default(),
            get_hists: OpHists::new(),
            put_hists: OpHists::new(),
            meta_hists: OpHists::new(),
            total_errors: 0,
            error_rate: 0.0,
        };
        
        let sum2 = Summary {
            wall_seconds: 1.5,
            total_bytes: 2000,
            total_ops: 200,
            p50_us: 150,
            p95_us: 250,
            p99_us: 350,
            get: Default::default(),
            put: Default::default(),
            meta: Default::default(),
            get_bins: Default::default(),
            put_bins: Default::default(),
            meta_bins: Default::default(),
            get_hists: OpHists::new(),
            put_hists: OpHists::new(),
            meta_hists: OpHists::new(),
            total_errors: 0,
            error_rate: 0.0,
        };
        
        // Add some data to histograms (bucket 0)
        {
            let mut hist = sum1.get_hists.buckets[0].lock().unwrap();
            hist.record(100).unwrap();
        }
        {
            let mut hist = sum2.get_hists.buckets[0].lock().unwrap();
            hist.record(200).unwrap();
        }
        
        let merged = merge_summaries(vec![sum1, sum2]).unwrap();
        
        // Verify merged values
        assert_eq!(merged.total_ops, 300); // 100 + 200
        assert_eq!(merged.total_bytes, 3000); // 1000 + 2000
        assert_eq!(merged.wall_seconds, 1.5); // max(1.0, 1.5)
    }
}
