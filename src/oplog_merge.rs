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
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::{info, warn};

/// Generate worker-specific op-log path
/// 
/// Given a base path like "/tmp/test.tsv", generates:
/// - Worker 0: "/tmp/test-worker-0.tsv"
/// - Worker 1: "/tmp/test-worker-1.tsv"
/// etc.
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

/// Entry from an op-log TSV file with parsed start timestamp
/// 
/// Op-log format (TSV columns):
/// - idx, thread, op, client_id, n_objects, bytes, endpoint, file, error, start, first_byte, end, duration_ns
/// 
/// We parse the 'start' column (index 9) as a SystemTime for correct chronological sorting.
#[derive(Debug, Clone)]
struct OpLogEntry {
    start_time: SystemTime,  // Parsed from RFC3339 timestamp for correct comparison
    line: String,            // Full TSV line
    worker_id: usize,        // Which worker file this came from
}

impl Eq for OpLogEntry {}

impl PartialEq for OpLogEntry {
    fn eq(&self, other: &Self) -> bool {
        self.start_time == other.start_time
    }
}

impl PartialOrd for OpLogEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// BinaryHeap is a max-heap, but we want min-heap (earliest timestamp first)
// So we reverse the comparison
impl Ord for OpLogEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        other.start_time.cmp(&self.start_time) // Reversed for min-heap
    }
}

/// Buffered reader for a worker op-log file
/// Decompresses zstd-compressed op-log on-the-fly
struct WorkerReader {
    lines: std::vec::IntoIter<String>,
    worker_id: usize,
}

impl WorkerReader {
    fn new(path: &Path, worker_id: usize) -> Result<Self> {
        // s3dlio compresses op-logs with zstd - decompress fully into memory
        // This is acceptable because each worker file should be reasonable size
        let input = File::open(path)
            .with_context(|| format!("Failed to open worker op-log: {}", path.display()))?;
        
        let mut decoder = zstd::Decoder::new(input)?;
        let mut decompressed = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut decompressed)?;
        
        // Split into lines and skip header
        let mut lines: Vec<String> = decompressed.lines().map(|s| s.to_string()).collect();
        if !lines.is_empty() {
            lines.remove(0); // Remove header
        }
        
        // CRITICAL: Sort lines by start_time (column 9) before merging!
        // Worker files are NOT pre-sorted because operations complete out of order
        lines.sort_by_cached_key(|line| {
            // Parse start timestamp from column 9
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 10 {
                // Parse RFC3339 timestamp
                humantime::parse_rfc3339(parts[9]).ok()
            } else {
                None
            }
        });
        
        Ok(WorkerReader { 
            lines: lines.into_iter(),
            worker_id,
        })
    }
    
    /// Read next entry from this worker file
    fn next_entry(&mut self) -> Result<Option<OpLogEntry>> {
        // Get next line from iterator
        let line = match self.lines.next() {
            Some(l) => l,
            None => return Ok(None), // EOF
        };
        
        if line.is_empty() {
            return Ok(None);
        }
        
        // Parse start_time from column 9 (0-indexed) - the "start" timestamp column
        // Header: idx thread op client_id n_objects bytes endpoint file error start first_byte end duration_ns
        //         0   1      2  3         4         5     6        7    8     9     10         11  12
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 10 {
            return Ok(None); // Invalid line
        }
        
        let start_str = parts[9]; // Column 9 (0-indexed) = "start" column
        
        // Parse RFC3339 timestamp to SystemTime for correct chronological comparison
        let start_time = humantime::parse_rfc3339(start_str)
            .with_context(|| format!("Failed to parse timestamp '{}' from worker {}", start_str, self.worker_id))?;
        
        Ok(Some(OpLogEntry {
            start_time,
            line,
            worker_id: self.worker_id,
        }))
    }
}

/// Merge multiple worker op-log files using streaming k-way merge
/// 
/// This uses a min-heap to efficiently merge sorted files without loading
/// everything into memory. Perfect for handling tens of millions of operations.
/// 
/// **Sorted by start_time** (first column, RFC3339 format) - this gives true
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
    let mut readers = Vec::new();
    let mut header: Option<String> = None;
    
    for worker_id in 0..num_workers {
        let worker_path = worker_oplog_path(base_path, worker_id);
        
        if !worker_path.exists() {
            warn!("Worker op-log file not found: {}", worker_path.display());
            continue;
        }
        
        // Read header from first worker file (decompress to read it)
        if header.is_none() {
            let file = File::open(&worker_path)?;
            let decoder = zstd::Decoder::new(file)?;
            let mut reader = BufReader::new(decoder);
            let mut first_line = String::new();
            reader.read_line(&mut first_line)?;
            header = Some(first_line.trim().to_string());
        }
        
        let reader = WorkerReader::new(&worker_path, worker_id)?;
        readers.push(reader);
    }
    
    if readers.is_empty() {
        bail!("No worker op-log files found");
    }
    
    info!("Opened {} worker files for streaming merge", readers.len());
    
    // Initialize min-heap with first entry from each worker
    let mut heap = BinaryHeap::new();
    
    for reader in readers.iter_mut() {
        if let Some(entry) = reader.next_entry()? {
            heap.push(entry);
        }
    }
    
    // Open output file (with zstd compression to match s3dlio format)
    let merged_path = base_path.to_path_buf();
    info!("Writing merged op-log to: {}", merged_path.display());
    
    let file = File::create(&merged_path)
        .with_context(|| format!("Failed to create merged op-log: {}", merged_path.display()))?;
    
    // Wrap in zstd encoder (compression level 3)
    let encoder = zstd::Encoder::new(file, 3)?;
    let mut writer = BufWriter::new(encoder.auto_finish());
    
    // Write header
    if let Some(ref hdr) = header {
        writeln!(writer, "{}", hdr)?;
    }
    
    // Streaming k-way merge
    let mut total_written = 0u64;
    
    while let Some(entry) = heap.pop() {
        // Write this entry
        writeln!(writer, "{}", entry.line)?;
        total_written += 1;
        
        // Read next entry from the same worker file
        if let Some(next_entry) = readers[entry.worker_id].next_entry()? {
            heap.push(next_entry);
        }
        
        // Progress logging every 1M operations
        if total_written % 1_000_000 == 0 {
            info!("Merged {} million operations...", total_written / 1_000_000);
        }
    }
    
    writer.flush()?;
    
    info!("Merged {} total operations from {} workers", total_written, readers.len());
    
    // Optionally delete worker files
    if !keep_worker_files {
        info!("Cleaning up worker op-log files...");
        for worker_id in 0..num_workers {
            let worker_path = worker_oplog_path(base_path, worker_id);
            if worker_path.exists() {
                std::fs::remove_file(&worker_path)
                    .with_context(|| format!("Failed to delete worker file: {}", worker_path.display()))?;
            }
        }
    }
    
    Ok(merged_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;
    
    #[test]
    fn test_worker_oplog_path() {
        let base = Path::new("/tmp/test.tsv");
        assert_eq!(
            worker_oplog_path(base, 0),
            PathBuf::from("/tmp/test-worker-0.tsv")
        );
        assert_eq!(
            worker_oplog_path(base, 3),
            PathBuf::from("/tmp/test-worker-3.tsv")
        );
        
        // Test with different extension
        let base = Path::new("/tmp/test.log");
        assert_eq!(
            worker_oplog_path(base, 1),
            PathBuf::from("/tmp/test-worker-1.log")
        );
    }
    
    #[test]
    fn test_merge_worker_oplogs_streaming() -> Result<()> {
        let tmpdir = TempDir::new()?;
        let base_path = tmpdir.path().join("test.tsv");
        
        // Create 3 worker files with RFC3339 timestamps
        // Each file has entries sorted by start time
        let worker0_path = worker_oplog_path(&base_path, 0);
        let mut f0 = File::create(&worker0_path)?;
        writeln!(f0, "start_time\tduration_ns\top\turi\tbytes\tstatus")?;
        writeln!(f0, "2025-11-06T10:00:00.000000000Z\t1000000\tPUT\ts3://bucket/obj1\t4096\t200")?;
        writeln!(f0, "2025-11-06T10:00:03.000000000Z\t500000\tGET\ts3://bucket/obj3\t4096\t200")?;
        
        let worker1_path = worker_oplog_path(&base_path, 1);
        let mut f1 = File::create(&worker1_path)?;
        writeln!(f1, "start_time\tduration_ns\top\turi\tbytes\tstatus")?;
        writeln!(f1, "2025-11-06T10:00:02.000000000Z\t2000000\tPUT\ts3://bucket/obj2\t8192\t200")?;
        writeln!(f1, "2025-11-06T10:00:04.000000000Z\t600000\tGET\ts3://bucket/obj4\t8192\t200")?;
        
        let worker2_path = worker_oplog_path(&base_path, 2);
        let mut f2 = File::create(&worker2_path)?;
        writeln!(f2, "start_time\tduration_ns\top\turi\tbytes\tstatus")?;
        writeln!(f2, "2025-11-06T10:00:01.500000000Z\t1500000\tPUT\ts3://bucket/obj5\t16384\t200")?;
        
        // Merge using streaming k-way merge
        let merged = merge_worker_oplogs(&base_path, 3, false)?;
        
        // Verify merged file exists
        assert!(merged.exists());
        
        // Verify worker files were deleted
        assert!(!worker0_path.exists());
        assert!(!worker1_path.exists());
        assert!(!worker2_path.exists());
        
        // Verify merged content is sorted by start_time
        let content = std::fs::read_to_string(&merged)?;
        let lines: Vec<&str> = content.lines().collect();
        
        assert_eq!(lines.len(), 6); // header + 5 operations
        assert!(lines[1].starts_with("2025-11-06T10:00:00")); // obj1 (earliest)
        assert!(lines[2].starts_with("2025-11-06T10:00:01.5")); // obj5
        assert!(lines[3].starts_with("2025-11-06T10:00:02")); // obj2
        assert!(lines[4].starts_with("2025-11-06T10:00:03")); // obj3
        assert!(lines[5].starts_with("2025-11-06T10:00:04")); // obj4 (latest)
        
        Ok(())
    }
    
    #[test]
    fn test_merge_keeps_worker_files() -> Result<()> {
        let tmpdir = TempDir::new()?;
        let base_path = tmpdir.path().join("test.tsv");
        
        // Create 2 worker files
        let worker0_path = worker_oplog_path(&base_path, 0);
        let mut f0 = File::create(&worker0_path)?;
        writeln!(f0, "start_time\tduration_ns\top\turi\tbytes\tstatus")?;
        writeln!(f0, "2025-11-06T10:00:00.000000000Z\t1000000\tPUT\ts3://bucket/obj1\t4096\t200")?;
        
        let worker1_path = worker_oplog_path(&base_path, 1);
        let mut f1 = File::create(&worker1_path)?;
        writeln!(f1, "start_time\tduration_ns\top\turi\tbytes\tstatus")?;
        writeln!(f1, "2025-11-06T10:00:01.000000000Z\t2000000\tPUT\ts3://bucket/obj2\t8192\t200")?;
        
        // Merge with keep_worker_files=true
        let merged = merge_worker_oplogs(&base_path, 2, true)?;
        
        // Verify merged file exists
        assert!(merged.exists());
        
        // Verify worker files still exist
        assert!(worker0_path.exists());
        assert!(worker1_path.exists());
        
        Ok(())
    }
}
