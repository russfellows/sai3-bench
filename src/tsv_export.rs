//! TSV export for machine-readable benchmark results

use anyhow::{Context, Result};
use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::metrics::OpHists;
use crate::constants::BUCKET_LABELS;
use crate::prepare::PrepareMetrics;
use crate::workload::SizeBins;

/// TSV exporter for benchmark results
pub struct TsvExporter {
    output_path: std::path::PathBuf,
}

impl TsvExporter {
    /// Create exporter with basename (will append -results.tsv)
    pub fn new<P: AsRef<Path>>(basename: P) -> Self {
        let path = format!("{}-results.tsv", basename.as_ref().to_string_lossy());
        Self {
            output_path: std::path::PathBuf::from(path),
        }
    }
    
    /// Create exporter with explicit output path
    pub fn with_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        Ok(Self {
            output_path: path.as_ref().to_path_buf(),
        })
    }

    /// Export complete results to TSV file
    #[allow(clippy::too_many_arguments)]
    pub fn export_results(
        &self,
        get_hists: &OpHists,
        put_hists: &OpHists,
        meta_hists: &OpHists,
        get_bins: &SizeBins,
        put_bins: &SizeBins,
        meta_bins: &SizeBins,
        wall_seconds: f64,
    ) -> Result<()> {
        let mut f = File::create(&self.output_path)
            .with_context(|| format!("Failed to create {}", self.output_path.display()))?;

        // Write header
        writeln!(
            f,
            "operation\tsize_bucket\tbucket_idx\tmean_us\tp50_us\tp90_us\tp95_us\tp99_us\tmax_us\tavg_bytes\tops_per_sec\tthroughput_mibps\tcount"
        )?;

        // Collect all rows first (including per-bucket and aggregate rows)
        let mut rows = Vec::new();
        self.collect_op_buckets(&mut rows, "GET", get_hists, get_bins, wall_seconds)?;
        self.collect_op_buckets(&mut rows, "PUT", put_hists, put_bins, wall_seconds)?;
        self.collect_op_buckets(&mut rows, "META", meta_hists, meta_bins, wall_seconds)?;

        // Add aggregate summary rows with operation-specific bucket indices
        // META=97, GET=98, PUT=99 ensures correct sort order after per-bucket rows (0-8)
        self.collect_aggregate_row(&mut rows, "META", 97, meta_hists, meta_bins, wall_seconds)?;
        self.collect_aggregate_row(&mut rows, "GET", 98, get_hists, get_bins, wall_seconds)?;
        self.collect_aggregate_row(&mut rows, "PUT", 99, put_hists, put_bins, wall_seconds)?;

        // Sort by bucket_idx (field 0) - this naturally groups operations and puts aggregates at end
        rows.sort_by_key(|(bucket_idx, _)| *bucket_idx);

        // Write sorted rows
        for (_, row) in rows {
            writeln!(f, "{}", row)?;
        }

        // Don't print here - caller will handle logging
        Ok(())
    }

    fn collect_op_buckets(
        &self,
        rows: &mut Vec<(usize, String)>,
        op: &str,
        hists: &OpHists,
        bins: &SizeBins,
        wall_seconds: f64,
    ) -> Result<()> {
        for (i, bucket_label) in BUCKET_LABELS.iter().enumerate() {
            let hist = hists.buckets[i].lock().unwrap();
            let count = hist.len();

            if count == 0 {
                continue;
            }

            // Get actual bytes from SizeBins (not estimated!)
            let (bucket_ops, bucket_bytes) = bins.by_bucket.get(&i).copied().unwrap_or((0, 0));

            let avg_bytes = if bucket_ops > 0 {
                bucket_bytes as f64 / bucket_ops as f64
            } else {
                0.0
            };

            let ops_per_sec = count as f64 / wall_seconds;
            let throughput_mibps = (bucket_bytes as f64 / 1_048_576.0) / wall_seconds;

            let row = format!(
                "{}\t{}\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.0}\t{:.2}\t{:.2}\t{}",
                op,
                bucket_label,
                i,
                hist.mean(),
                hist.value_at_quantile(0.50) as f64,
                hist.value_at_quantile(0.90) as f64,
                hist.value_at_quantile(0.95) as f64,
                hist.value_at_quantile(0.99) as f64,
                hist.max() as f64,
                avg_bytes,
                ops_per_sec,
                throughput_mibps,
                count
            );

            rows.push((i, row));
        }

        Ok(())
    }

    /// Collect an aggregate row combining all size buckets for one operation type
    fn collect_aggregate_row(
        &self,
        rows: &mut Vec<(usize, String)>,
        op: &str,
        bucket_idx: usize,
        hists: &OpHists,
        bins: &SizeBins,
        wall_seconds: f64,
    ) -> Result<()> {
        // Get combined histogram across all size buckets
        let combined_hist = hists.combined_histogram();
        let count = combined_hist.len();

        // Skip if no operations
        if count == 0 {
            return Ok(());
        }

        // Sum up total operations and bytes across all buckets
        let (total_ops, total_bytes): (u64, u64) = bins.by_bucket
            .values()
            .fold((0, 0), |(ops_acc, bytes_acc), (ops, bytes)| {
                (ops_acc + ops, bytes_acc + bytes)
            });

        let avg_bytes = if total_ops > 0 {
            total_bytes as f64 / total_ops as f64
        } else {
            0.0
        };

        let ops_per_sec = count as f64 / wall_seconds;
        let throughput_mibps = (total_bytes as f64 / 1_048_576.0) / wall_seconds;

        // Create aggregate row with "ALL" as bucket label
        // bucket_idx is passed in: META=97, GET=98, PUT=99 for proper sorting
        let row = format!(
            "{}\tALL\t{}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.2}\t{:.0}\t{:.2}\t{:.2}\t{}",
            op,
            bucket_idx,
            combined_hist.mean(),
            combined_hist.value_at_quantile(0.50) as f64,
            combined_hist.value_at_quantile(0.90) as f64,
            combined_hist.value_at_quantile(0.95) as f64,
            combined_hist.value_at_quantile(0.99) as f64,
            combined_hist.max() as f64,
            avg_bytes,
            ops_per_sec,
            throughput_mibps,
            count
        );

        rows.push((bucket_idx, row));

        Ok(())
    }

    /// Export prepare phase metrics to TSV file
    pub fn export_prepare_metrics(&self, metrics: &PrepareMetrics) -> Result<()> {
        let mut f = File::create(&self.output_path)
            .with_context(|| format!("Failed to create {}", self.output_path.display()))?;

        // Write header
        writeln!(
            f,
            "operation\tsize_bucket\tbucket_idx\tmean_us\tp50_us\tp90_us\tp95_us\tp99_us\tmax_us\tavg_bytes\tops_per_sec\tthroughput_mibps\tcount"
        )?;

        // Collect all rows
        let mut rows = Vec::new();
        
        // PUT operations (per-bucket)
        self.collect_op_buckets(
            &mut rows,
            "PUT",
            &metrics.put_hists,
            &metrics.put_bins,
            metrics.wall_seconds,
        )?;

        // PUT aggregate (all buckets combined)
        self.collect_aggregate_row(
            &mut rows,
            "PUT",
            99,  // bucket_idx for sorting
            &metrics.put_hists,
            &metrics.put_bins,
            metrics.wall_seconds,
        )?;

        // Sort by bucket_idx
        rows.sort_by_key(|(bucket_idx, _)| *bucket_idx);

        // Write sorted rows
        for (_, row) in rows {
            writeln!(f, "{}", row)?;
        }

        Ok(())
    }
}
