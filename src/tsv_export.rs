//! TSV export for machine-readable benchmark results

use anyhow::{Context, Result};
use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::metrics::{OpHists, BUCKET_LABELS};
use crate::workload::SizeBins;

/// TSV exporter for benchmark results
pub struct TsvExporter {
    basename: String,
}

impl TsvExporter {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            basename: path.as_ref().to_string_lossy().to_string(),
        }
    }

    /// Export complete results to TSV file
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
        let path = format!("{}-results.tsv", self.basename);
        let mut f = File::create(&path)
            .with_context(|| format!("Failed to create {}", path))?;

        // Write header
        writeln!(
            f,
            "operation\tsize_bucket\tbucket_idx\tmean_us\tp50_us\tp90_us\tp95_us\tp99_us\tmax_us\tavg_bytes\tops_per_sec\tthroughput_mibps\tcount"
        )?;

        // Collect all rows first
        let mut rows = Vec::new();
        self.collect_op_buckets(&mut rows, "GET", get_hists, get_bins, wall_seconds)?;
        self.collect_op_buckets(&mut rows, "PUT", put_hists, put_bins, wall_seconds)?;
        self.collect_op_buckets(&mut rows, "META", meta_hists, meta_bins, wall_seconds)?;

        // Sort by bucket_idx (field 2)
        rows.sort_by_key(|(bucket_idx, _)| *bucket_idx);

        // Write sorted rows
        for (_, row) in rows {
            writeln!(f, "{}", row)?;
        }

        println!("\n✅ TSV results exported to: {}", path);
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
}
