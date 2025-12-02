//! Op-log replay functionality for sai3-bench v0.5.0
//!
//! Implements timing-faithful workload replay using s3dlio-oplog streaming reader.
//! This version uses constant memory (~1.5 MB) regardless of op-log size.

use anyhow::{Context, Result};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// Use s3dlio-oplog types instead of our own
pub use s3dlio_oplog::{OpLogEntry, OpType, OpLogStreamReader};

use crate::workload::{
    StoreCache,  // v0.8.9: Use shared cache type from workload
    get_object_cached_simple, 
    put_object_cached_simple, 
    delete_object_cached_simple,
    list_objects_cached_simple,
    stat_object_cached_simple,
};
use crate::remap::{RemapConfig, RemapEngine};

/// Replay configuration
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    pub op_log_path: PathBuf,
    pub target_uri: Option<String>,
    pub speed: f64,
    pub continue_on_error: bool,
    /// Maximum concurrent operations (to prevent unbounded task growth)
    pub max_concurrent: Option<usize>,
    /// Optional remap configuration for advanced URI transformations
    pub remap_config: Option<RemapConfig>,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            op_log_path: PathBuf::new(),
            target_uri: None,
            speed: 1.0,
            continue_on_error: false,
            max_concurrent: Some(1000), // Reasonable default to prevent memory issues
            remap_config: None,
        }
    }
}

/// Statistics from replay execution
#[derive(Debug, Default)]
pub struct ReplayStats {
    pub total_operations: u64,
    pub completed_operations: u64,
    pub failed_operations: u64,
    pub skipped_operations: u64,
}

/// Main replay orchestrator with streaming op-log processing
///
/// This version uses OpLogStreamReader for constant memory usage (~1.5 MB)
/// regardless of op-log file size, supporting multi-GB operation logs.
///
/// # Memory Efficiency
///
/// - **Old (v0.4.0)**: Loads entire op-log into Vec (e.g., 1M ops = ~100 MB)
/// - **New (v0.5.0)**: Streams operations (constant ~1.5 MB via s3dlio-oplog)
///
/// # Timing Model
///
/// Uses on-demand spawning with absolute timeline scheduling:
/// 1. First operation becomes time=0 (epoch)
/// 2. Each subsequent operation scheduled at: epoch + (op.start - first.start) / speed
/// 3. Tasks spawned as we stream, with max_concurrent limit to prevent unbounded growth
///
/// # Example
///
/// ```no_run
/// use sai3_bench::replay_streaming::{ReplayConfig, replay_workload_streaming};
/// use std::path::PathBuf;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), anyhow::Error> {
/// let config = ReplayConfig {
///     op_log_path: PathBuf::from("large_workload.tsv.zst"),
///     target_uri: Some("s3://test-bucket/replay/".to_string()),
///     speed: 2.0,  // 2x faster
///     continue_on_error: true,
///     max_concurrent: Some(500),
///     remap_config: None,
/// };
///
/// let stats = replay_workload_streaming(config).await?;
/// println!("Completed: {}/{}", stats.completed_operations, stats.total_operations);
/// # Ok(())
/// # }
/// ```
pub async fn replay_workload_streaming(config: ReplayConfig) -> Result<ReplayStats> {
    info!("Starting streaming replay with config: {:?}", config);

    // Create streaming reader (constant memory)
    let stream = OpLogStreamReader::from_file(&config.op_log_path)
        .context("Failed to open streaming op-log reader")?;

    let mut stats = ReplayStats::default();
    let mut tasks = FuturesUnordered::new();
    let max_concurrent = config.max_concurrent.unwrap_or(1000);
    
    // v0.8.9: Create shared store cache to avoid per-operation store creation
    let store_cache: StoreCache = Arc::new(std::sync::Mutex::new(HashMap::new()));

    // First pass: Get the first operation to establish epoch
    let mut stream_iter = stream;
    let first_entry = match stream_iter.next() {
        Some(Ok(entry)) => entry,
        Some(Err(e)) => return Err(e).context("Failed to parse first op-log entry"),
        None => {
            info!("Empty op-log file");
            return Ok(stats);
        }
    };

    let first_time = first_entry.start;
    let replay_epoch = Instant::now();
    
    info!(
        "Replay epoch established. First operation: {:?} at {}",
        first_entry.op, first_entry.start
    );

    // Process first entry
    stats.total_operations += 1;
    let task = spawn_operation(
        first_entry,
        replay_epoch,
        Duration::ZERO, // First operation has no delay
        config.clone(),
        store_cache.clone(),
    );
    tasks.push(task);

    // Track previous timestamp for order validation
    let mut prev_time = first_time;

    // Stream remaining operations
    for entry_result in stream_iter {
        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                warn!("Failed to parse op-log entry: {}", e);
                stats.skipped_operations += 1;
                if !config.continue_on_error {
                    return Err(e).context("Failed to parse op-log entry");
                }
                continue;
            }
        };

        stats.total_operations += 1;

        // Validate chronological order
        if entry.start < prev_time {
            let time_delta = prev_time.signed_duration_since(entry.start);
            warn!(
                "Out-of-order op-log entry detected during replay: operation #{} has start={}, previous={} (went back by {:?})",
                stats.total_operations,
                entry.start,
                prev_time,
                time_delta
            );
            // Continue anyway - we'll execute immediately (delay will be negative/zero)
        }
        prev_time = entry.start;

        // Calculate absolute delay from first operation
        let elapsed = entry.start.signed_duration_since(first_time);
        let delay = match elapsed.to_std() {
            Ok(d) => Duration::from_secs_f64(d.as_secs_f64() / config.speed),
            Err(_) => {
                warn!("Negative timestamp offset detected, using zero delay");
                Duration::ZERO
            }
        };

        // Spawn operation
        let task = spawn_operation(entry, replay_epoch, delay, config.clone(), store_cache.clone());
        tasks.push(task);

        // Poll completed tasks to prevent unbounded growth
        while tasks.len() >= max_concurrent {
            if let Some(result) = tasks.next().await {
                handle_task_result(result, &mut stats, config.continue_on_error)?;
            }
        }

        // Log progress periodically
        if stats.total_operations % 1000 == 0 {
            debug!(
                "Progress: {} operations processed, {} tasks active",
                stats.total_operations,
                tasks.len()
            );
        }
    }

    info!(
        "Finished streaming {} operations, waiting for {} remaining tasks",
        stats.total_operations,
        tasks.len()
    );

    // Wait for all remaining tasks to complete
    while let Some(result) = tasks.next().await {
        handle_task_result(result, &mut stats, config.continue_on_error)?;
    }

    info!(
        "Replay complete: {} total, {} completed, {} failed, {} skipped",
        stats.total_operations,
        stats.completed_operations,
        stats.failed_operations,
        stats.skipped_operations
    );

    Ok(stats)
}

/// Spawn a single operation at its scheduled time
fn spawn_operation(
    entry: OpLogEntry,
    replay_epoch: Instant,
    delay: Duration,
    config: ReplayConfig,
    store_cache: StoreCache,
) -> tokio::task::JoinHandle<Result<()>> {
    tokio::spawn(async move {
        // Calculate absolute target time
        let target_time = replay_epoch + delay;
        let now = Instant::now();

        // Sleep until target time (microsecond precision via std::thread::sleep)
        if target_time > now {
            let sleep_dur = target_time - now;
            tokio::task::spawn_blocking(move || {
                std::thread::sleep(sleep_dur);
            })
            .await?;
        }

        // Apply URI transformation (priority: remap > simple retargeting)
        let uri = if let Some(ref remap_config) = config.remap_config {
            // Advanced remapping with rules
            let engine = RemapEngine::new(remap_config.clone());
            let original_uri = format!("{}{}", entry.endpoint, entry.file);
            engine.remap(&original_uri)
                .context("Failed to apply remap rules")?
        } else if let Some(ref target) = config.target_uri {
            // Simple 1→1 retargeting (legacy behavior)
            translate_uri(&entry.file, &entry.endpoint, target)?
        } else {
            // No transformation
            format!("{}{}", entry.endpoint, entry.file)
        };

        debug!("Executing {:?} on {}", entry.op, uri);

        // Execute operation with cached store (v0.8.9: efficient store reuse)
        execute_operation(&entry, &uri, &store_cache).await
    })
}

/// Handle task result and update statistics
fn handle_task_result(
    result: Result<Result<()>, tokio::task::JoinError>,
    stats: &mut ReplayStats,
    continue_on_error: bool,
) -> Result<()> {
    match result {
        Ok(Ok(())) => {
            stats.completed_operations += 1;
        }
        Ok(Err(e)) => {
            stats.failed_operations += 1;
            if continue_on_error {
                warn!("Operation failed (continuing): {}", e);
            } else {
                return Err(e).context("Operation failed");
            }
        }
        Err(e) => {
            stats.failed_operations += 1;
            if continue_on_error {
                warn!("Task panicked (continuing): {}", e);
            } else {
                return Err(e.into());
            }
        }
    }
    Ok(())
}

/// Execute a single operation using cached store (v0.8.9: efficient store reuse)
async fn execute_operation(entry: &OpLogEntry, uri: &str, cache: &StoreCache) -> Result<()> {
    match entry.op {
        OpType::GET => {
            get_object_cached_simple(uri, cache).await?;
        }
        OpType::PUT => {
            // Generate data with s3dlio (dedup=1, compress=1 for random)
            let data = s3dlio::data_gen::generate_controlled_data(entry.bytes as usize, 1, 1);
            put_object_cached_simple(uri, &data, cache).await?;
        }
        OpType::DELETE => {
            delete_object_cached_simple(uri, cache).await?;
        }
        OpType::LIST => {
            list_objects_cached_simple(uri, cache).await?;
        }
        OpType::STAT => {
            stat_object_cached_simple(uri, cache).await?;
        }
    }
    Ok(())
}

/// Simple 1:1 URI translation from original endpoint to target
fn translate_uri(file: &str, endpoint: &str, target: &str) -> Result<String> {
    // Remove endpoint prefix from file path
    let relative = file.strip_prefix(endpoint).unwrap_or(file);
    let clean = relative.trim_start_matches('/');

    // Construct new URI with target
    let target_clean = target.trim_end_matches('/');
    Ok(format!("{}/{}", target_clean, clean))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_translate_uri() {
        let file = "/bucket/data/file.bin";
        let endpoint = "/bucket/";
        let target = "s3://newbucket";

        let result = translate_uri(file, endpoint, target).unwrap();
        assert_eq!(result, "s3://newbucket/data/file.bin");
    }

    #[test]
    fn test_translate_uri_with_scheme() {
        let file = "file:///tmp/test/data.bin";
        let endpoint = "file://";
        let target = "s3://bucket/prefix";

        let result = translate_uri(file, endpoint, target).unwrap();
        assert_eq!(result, "s3://bucket/prefix/tmp/test/data.bin");
    }
}
