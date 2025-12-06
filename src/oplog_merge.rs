//! Op-log merging utilities for multi-worker execution
//! 
//! When running with multiple workers (MultiProcess or MultiRuntime modes),
//! each worker writes to a separate op-log file to avoid contention. This
//! module provides utilities to merge those separate files into a single
//! chronologically-ordered op-log using streaming k-way merge.
//! 
//! **Note**: s3dlio op-log files are zstd-compressed by default, so this module
//! handles decompression on read and compression on write.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

// Use s3dlio-oplog types instead of defining our own
pub use s3dlio_oplog::{OpLogEntry, OpLogStreamReader, OpType};

/// Generate worker-specific op-log path
/// 
/// Given a base path like "/tmp/test.tsv", generates:
/// - Worker 0: "/tmp/test-worker-0.tsv"
/// - Worker 1: "/tmp/test-worker-1.tsv"
/// - etc.
pub fn worker_oplog_path(base_path: &Path, worker_id: usize) -> PathBuf {
    let base_stem = base_path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("oplog");
    
    let extension = base_path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("tsv");
    
    let parent = base_path.parent().unwrap_or(Path::new("."));
    
    parent.join(format!("{}-worker-{}.{}", base_stem, worker_id, extension))
}

/// Wrapper for OpLogEntry that tracks which worker it came from
/// Used during k-way merge to maintain source information
#[derive(Debug, Clone)]
struct MergeEntry {
    entry: OpLogEntry,
    worker_id: usize,
}

impl Eq for MergeEntry {}

impl PartialEq for MergeEntry {
    fn eq(&self, other: &Self) -> bool {
        self.entry.start == other.entry.start
    }
}

impl PartialOrd for MergeEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// BinaryHeap is a max-heap, but we want min-heap (earliest timestamp first)
// So we reverse the comparison
impl Ord for MergeEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other.entry.start.cmp(&self.entry.start) // Reversed for min-heap
    }
}

/// Merge multiple worker op-log files using streaming k-way merge
/// 
/// This uses a min-heap to efficiently merge sorted files without loading
/// everything into memory. Perfect for handling tens of millions of operations.
/// 
/// **Sorted by start_time** (chronological ordering) - this gives true
/// chronological ordering of when operations began, not when they ended.
/// 
/// # Arguments
/// * `base_path` - Base path for the merged output file
/// * `num_workers` - Number of worker files to merge
/// * `keep_worker_files` - If true, keep individual worker files; if false, delete them
/// 
/// # Returns
/// Path to the merged op-log file
pub fn merge_worker_oplogs(
    base_path: &Path,
    num_workers: usize,
    keep_worker_files: bool,
) -> Result<PathBuf> {
    info!("Merging {} worker op-log files using streaming k-way merge...", num_workers);
    
    // Open all worker files
    let mut streams: Vec<(usize, OpLogStreamReader)> = Vec::new();
    for worker_id in 0..num_workers {
        let worker_path = worker_oplog_path(base_path, worker_id);
        if !worker_path.exists() {
            warn!("Worker file does not exist: {}", worker_path.display());
            continue;
        }
        
        let stream = OpLogStreamReader::from_file(&worker_path)
            .with_context(|| format!("Failed to open worker file: {}", worker_path.display()))?;
        streams.push((worker_id, stream));
    }
    
    if streams.is_empty() {
        bail!("No worker files found to merge");
    }
    
    info!("Opened {} worker files for merging", streams.len());
    
    // Create output file
    let output_path = base_path.to_path_buf();
    let output_file = File::create(&output_path)
        .with_context(|| format!("Failed to create merged op-log: {}", output_path.display()))?;
    let buf_writer = BufWriter::new(output_file);
    let mut encoder = zstd::stream::write::Encoder::new(buf_writer, 1)?
        .auto_finish();
    
    // Write header
    writeln!(encoder, "idx\tthread\top\tclient_id\tn_objects\tbytes\tendpoint\tfile\terror\tstart\tfirst_byte\tend\tduration_ns")?;
    
    // Initialize heap with first entry from each worker
    let mut heap: BinaryHeap<MergeEntry> = BinaryHeap::new();
    for (worker_id, stream) in streams.iter_mut() {
        if let Some(Ok(entry)) = stream.next() {
            heap.push(MergeEntry {
                entry,
                worker_id: *worker_id,
            });
        }
    }
    
    let mut output_count = 0u64;
    
    // Merge loop: take smallest timestamp, write it, read next from that worker
    while let Some(merge_entry) = heap.pop() {
        write_entry(&mut encoder, &merge_entry.entry, output_count)?;
        output_count += 1;
        
        // Read next entry from this worker's stream
        let worker_id = merge_entry.worker_id;
        if let Some(Ok(next_entry)) = streams.get_mut(worker_id).and_then(|(_, s)| s.next()) {
            heap.push(MergeEntry {
                entry: next_entry,
                worker_id,
            });
        }
    }
    
    drop(encoder); // Finish zstd encoding
    
    info!("Merged {} entries into {}", output_count, output_path.display());
    
    // Clean up worker files if requested
    if !keep_worker_files {
        for worker_id in 0..num_workers {
            let worker_path = worker_oplog_path(base_path, worker_id);
            if worker_path.exists() {
                std::fs::remove_file(&worker_path)
                    .with_context(|| format!("Failed to remove worker file: {}", worker_path.display()))?;
            }
        }
        info!("Removed {} worker files", num_workers);
    }
    
    Ok(output_path)
}

/// Helper function to write an entry to the encoder
fn write_entry<W: Write>(
    encoder: &mut W,
    entry: &OpLogEntry,
    idx: u64,
) -> Result<()> {
    // Format: idx thread op client_id n_objects bytes endpoint file error start first_byte end duration_ns
    // We'll use simplified format since some fields may not be in OpLogEntry
    writeln!(
        encoder,
        "{}\t{}\t{}\t\t{}\t{}\t{}\t{}\t{}\t{}\t\t\t{}",
        idx,
        0, // thread (not tracked in OpLogEntry)
        entry.op, // client_id (not tracked)
        1, // n_objects (assume 1)
        entry.bytes,
        entry.endpoint,
        entry.file,
        entry.error.as_deref().unwrap_or(""),
        entry.start.to_rfc3339(), // end (not tracked)
        entry.duration_ns.unwrap_or(0)
    )?;
    Ok(())
}

/// Check if an op-log file is sorted by start timestamp
/// 
/// Reads up to `max_lines` from the file and checks if timestamps are in order.
/// Returns (is_sorted, lines_checked, first_out_of_order_line) where:
/// - is_sorted: true if all checked lines are in chronological order
/// - lines_checked: number of data lines checked (excluding header)
/// - first_out_of_order_line: line number of first out-of-order timestamp, or None if sorted
/// 
/// # Arguments
/// * `path` - Path to op-log file (supports .zst compression)
/// * `max_lines` - Maximum number of lines to check (None = check all)
pub fn check_oplog_sorted(path: &Path, max_lines: Option<usize>) -> Result<(bool, usize, Option<usize>)> {
    let stream = OpLogStreamReader::from_file(path)
        .with_context(|| format!("Failed to open op-log file: {}", path.display()))?;
    
    let mut prev_time: Option<DateTime<Utc>> = None;
    let mut line_num = 0;
    let max = max_lines.unwrap_or(usize::MAX);
    
    for entry_result in stream {
        let entry = entry_result?;
        line_num += 1;
        
        if let Some(prev) = prev_time {
            if entry.start < prev {
                // Found out-of-order timestamp
                return Ok((false, line_num, Some(line_num)));
            }
        }
        
        prev_time = Some(entry.start);
        
        if line_num >= max {
            break;
        }
    }
    
    Ok((true, line_num, None))
}

/// Sort an op-log file by start timestamp using streaming window-based algorithm
/// 
/// This function uses a sliding window approach to sort with constant memory usage.
/// The window size should be larger than the maximum out-of-order distance.
/// 
/// # Arguments
/// * `input_path` - Path to unsorted op-log file (supports .zst compression)
/// * `output_path` - Path for sorted output file (will be zstd-compressed)
/// * `window_size` - Number of entries to buffer (default: 10000)
/// 
/// # Algorithm
/// 1. Read entries into a sorted window buffer (Vec<OpLogEntry>)
/// 2. When window is full, output the earliest entry
/// 3. At EOF, output all remaining entries in sorted order
pub fn sort_oplog_file(input_path: &Path, output_path: &Path, window_size: usize) -> Result<()> {
    info!("Sorting op-log file: {} -> {} (window_size={})", 
          input_path.display(), output_path.display(), window_size);
    
    let stream = OpLogStreamReader::from_file(input_path)
        .with_context(|| format!("Failed to open input file: {}", input_path.display()))?;
    
    // Create output file with zstd compression
    let output_file = File::create(output_path)
        .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;
    let buf_writer = BufWriter::new(output_file);
    let mut encoder = zstd::stream::write::Encoder::new(buf_writer, 1)?
        .auto_finish();
    
    // Write header
    writeln!(encoder, "idx\tthread\top\tclient_id\tn_objects\tbytes\tendpoint\tfile\terror\tstart\tfirst_byte\tend\tduration_ns")?;
    
    // Sliding window buffer (kept sorted)
    let mut window: Vec<OpLogEntry> = Vec::with_capacity(window_size);
    let mut output_count: u64 = 0;
    
    for entry_result in stream {
        let entry = entry_result?;
        
        // Insert into sorted position
        let insert_pos = window.binary_search_by(|e| e.start.cmp(&entry.start))
            .unwrap_or_else(|pos| pos);
        window.insert(insert_pos, entry);
        
        // If window is full, output the earliest entry
        while window.len() > window_size {
            let oldest = window.remove(0);
            write_entry(&mut encoder, &oldest, output_count)?;
            output_count += 1;
        }
    }
    
    // Output remaining entries in window (all sorted)
    for entry in window {
        write_entry(&mut encoder, &entry, output_count)?;
        output_count += 1;
    }
    
    drop(encoder); // Finish zstd stream
    
    info!("Sorted {} entries to {}", output_count, output_path.display());
    Ok(())
}
