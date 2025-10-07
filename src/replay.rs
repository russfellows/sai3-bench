//! Op-log replay functionality for sai3-bench v0.4.0 (LEGACY)
//!
//! **DEPRECATED**: This module uses in-memory Vec-based replay.
//! Use `replay_streaming` module for memory-efficient streaming replay.
//!
//! This implementation is kept for backward compatibility and as a fallback.
//! It loads the entire op-log into memory, which can consume significant RAM
//! for large workloads (e.g., 1M operations ≈ 100 MB).
//!
//! For new code, use: `crate::replay_streaming::replay_workload_streaming()`

#![allow(dead_code)] // Keep legacy code available but unused

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::workload;

/// Replay configuration
#[derive(Debug, Clone)]
pub struct ReplayConfig {
    pub op_log_path: PathBuf,
    pub target_uri: Option<String>,
    pub speed: f64,
    pub continue_on_error: bool,
}

/// Operation type from op-log
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpType {
    GET,
    PUT,
    DELETE,
    LIST,
    STAT,
}

impl std::str::FromStr for OpType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "GET" => Ok(OpType::GET),
            "PUT" => Ok(OpType::PUT),
            "DELETE" => Ok(OpType::DELETE),
            "LIST" => Ok(OpType::LIST),
            "STAT" | "HEAD" => Ok(OpType::STAT),
            _ => bail!("Unknown operation type: {}", s),
        }
    }
}

/// Single operation from op-log
#[derive(Debug, Clone)]
pub struct OpLogEntry {
    pub idx: u64,
    pub op: OpType,
    pub bytes: u64,
    pub endpoint: String,
    pub file: String,
    pub start: DateTime<Utc>,
}

/// Parse op-log file (auto-detects zstd compression)
pub fn parse_oplog(path: &Path) -> Result<Vec<OpLogEntry>> {
    use csv::ReaderBuilder;
    use std::fs::File;
    use std::io::Read;

    info!("Parsing op-log: {}", path.display());

    let file = File::open(path).context("Failed to open op-log file")?;

    // Auto-detect zstd compression
    let reader: Box<dyn Read> = if path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s == "zst")
        .unwrap_or(false)
    {
        info!("Detected zstd compression");
        Box::new(zstd::stream::read::Decoder::new(file)?)
    } else {
        Box::new(file)
    };

    let mut csv_reader = ReaderBuilder::new()
        .delimiter(b'\t')
        .has_headers(true)
        .from_reader(reader);

    let mut entries = Vec::new();
    let mut skipped = 0;

    for (line_num, result) in csv_reader.records().enumerate() {
        let record = match result {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to parse line {}: {}", line_num + 2, e);
                skipped += 1;
                continue;
            }
        };

        // Expected columns: idx, thread, op, client_id, n_objects, bytes, endpoint, file, error, start, first_byte, end, duration_ns
        if record.len() < 13 {
            debug!("Skipping line {} (insufficient columns)", line_num + 2);
            skipped += 1;
            continue;
        }

        // Parse operation type
        let op = match record[2].parse::<OpType>() {
            Ok(o) => o,
            Err(_) => {
                debug!("Skipping line {} (unknown op: {})", line_num + 2, &record[2]);
                skipped += 1;
                continue;
            }
        };

        // Parse other fields
        let idx = record[0].parse().context("Failed to parse idx")?;
        let bytes = record[5].parse().context("Failed to parse bytes")?;
        let endpoint = record[6].to_string();
        let file = record[7].to_string();
        let start = DateTime::parse_from_rfc3339(&record[9])
            .context("Failed to parse start timestamp")?
            .into();

        entries.push(OpLogEntry {
            idx,
            op,
            bytes,
            endpoint,
            file,
            start,
        });
    }

    if skipped > 0 {
        info!("Skipped {} invalid/unknown operations", skipped);
    }

    // Sort by start time for absolute timeline scheduling
    entries.sort_by_key(|e| e.start);

    info!("Loaded {} operations", entries.len());
    Ok(entries)
}

/// Main replay orchestrator with absolute timeline scheduling
pub async fn replay_workload(config: ReplayConfig) -> Result<()> {
    info!("Starting replay with config: {:?}", config);

    let operations = parse_oplog(&config.op_log_path)?;

    if operations.is_empty() {
        bail!("No operations in op-log");
    }

    let first_time = operations[0].start;
    let last_time = operations[operations.len() - 1].start;
    let duration = last_time.signed_duration_since(first_time);

    info!(
        "Replaying {} operations over {} seconds (speed: {:.1}x)",
        operations.len(),
        duration.num_seconds(),
        config.speed
    );

    let replay_epoch = Instant::now();

    // Spawn all operations with absolute timing
    let mut tasks = FuturesUnordered::new();

    for op in operations {
        let target = config.target_uri.clone();
        let speed = config.speed;
        let _continue_on_error = config.continue_on_error;

        let task = tokio::spawn(async move {
            // Calculate absolute time offset from first operation
            let elapsed = op.start.signed_duration_since(first_time);
            let delay = elapsed.to_std()? / speed as u32; // Adjusted for speed multiplier

            schedule_and_execute(op, replay_epoch, delay, target.as_deref()).await
        });

        tasks.push(task);
    }

    // Collect results
    let mut success = 0;
    let mut failed = 0;

    while let Some(result) = tasks.next().await {
        match result {
            Ok(Ok(())) => success += 1,
            Ok(Err(e)) => {
                failed += 1;
                if config.continue_on_error {
                    warn!("Operation failed (continuing): {}", e);
                } else {
                    return Err(e);
                }
            }
            Err(e) => {
                failed += 1;
                if config.continue_on_error {
                    warn!("Task panicked (continuing): {}", e);
                } else {
                    return Err(e.into());
                }
            }
        }
    }

    info!(
        "Replay complete: {} successful, {} failed",
        success, failed
    );
    Ok(())
}

/// Schedule operation at absolute time and execute with microsecond precision
async fn schedule_and_execute(
    op: OpLogEntry,
    replay_epoch: Instant,
    delay: Duration,
    target: Option<&str>,
) -> Result<()> {
    let target_time = replay_epoch + delay;
    let now = Instant::now();

    // Sleep until target time (microsecond precision with std::thread::sleep)
    if target_time > now {
        let sleep_dur = target_time - now;
        tokio::task::spawn_blocking(move || {
            std::thread::sleep(sleep_dur);
        })
        .await?;
    }

    // Translate URI if target provided
    let uri = if let Some(tgt) = target {
        translate_uri(&op.file, &op.endpoint, tgt)?
    } else {
        format!("{}{}", op.endpoint, op.file)
    };

    debug!("Executing {:?} on {}", op.op, uri);

    execute_operation(&op, &uri).await
}

/// Execute a single operation
async fn execute_operation(op: &OpLogEntry, uri: &str) -> Result<()> {
    match op.op {
        OpType::GET => {
            workload::get_object_multi_backend(uri).await?;
        }
        OpType::PUT => {
            // Generate data with s3dlio (dedup=1, compress=1 for random)
            let data = s3dlio::data_gen::generate_controlled_data(op.bytes as usize, 1, 1);
            workload::put_object_multi_backend(uri, &data).await?;
        }
        OpType::DELETE => {
            workload::delete_object_multi_backend(uri).await?;
        }
        OpType::LIST => {
            workload::list_objects_multi_backend(uri).await?;
        }
        OpType::STAT => {
            workload::stat_object_multi_backend(uri).await?;
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
    fn test_op_type_parse() {
        assert_eq!("GET".parse::<OpType>().unwrap(), OpType::GET);
        assert_eq!("PUT".parse::<OpType>().unwrap(), OpType::PUT);
        assert_eq!("DELETE".parse::<OpType>().unwrap(), OpType::DELETE);
        assert_eq!("LIST".parse::<OpType>().unwrap(), OpType::LIST);
        assert_eq!("STAT".parse::<OpType>().unwrap(), OpType::STAT);
        assert_eq!("HEAD".parse::<OpType>().unwrap(), OpType::STAT);
        assert!("UNKNOWN".parse::<OpType>().is_err());
    }
}
