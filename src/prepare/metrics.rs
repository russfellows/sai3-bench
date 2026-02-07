//! Prepare phase metrics and data structures

use crate::config::PrepareStrategy;

/// Information about a prepared object
#[derive(Debug, Clone)]
pub struct PreparedObject {
    pub uri: String,
    pub size: u64,
    pub created: bool,  // True if we created it, false if it already existed
}

/// Metrics collected during prepare phase
/// 
/// Tracks PUT operations and directory creation with full HDR histogram support.
/// Follows same structure as workload metrics (OpAgg + OpHists + SizeBins).
#[derive(Debug, Clone)]
pub struct PrepareMetrics {
    /// Wall clock time for entire prepare phase (seconds)
    pub wall_seconds: f64,
    
    /// PUT operation aggregate metrics
    pub put: crate::workload::OpAgg,
    
    /// PUT operation size bins
    pub put_bins: crate::workload::SizeBins,
    
    /// PUT operation HDR histograms (9 size buckets)
    pub put_hists: crate::metrics::OpHists,
    
    /// Directory operations (mkdir) - treated as metadata ops
    pub mkdir: crate::workload::OpAgg,
    
    /// Number of directories created (not tracked per-size, always zero-byte ops)
    pub mkdir_count: u64,
    
    /// Total objects created (excludes pre-existing objects)
    pub objects_created: u64,
    
    /// Total objects that already existed (skipped)
    pub objects_existed: u64,
    
    /// Prepare strategy used
    pub strategy: PrepareStrategy,
    
    /// v0.8.23: Per-endpoint statistics (if multi-endpoint was used)
    pub endpoint_stats: Option<Vec<crate::workload::EndpointStatsSnapshot>>,
}

impl Default for PrepareMetrics {
    fn default() -> Self {
        Self {
            wall_seconds: 0.0,
            put: crate::workload::OpAgg::default(),
            put_bins: crate::workload::SizeBins::default(),
            put_hists: crate::metrics::OpHists::new(),
            mkdir: crate::workload::OpAgg::default(),
            mkdir_count: 0,
            objects_created: 0,
            objects_existed: 0,
            strategy: PrepareStrategy::Sequential,
            endpoint_stats: None,
        }
    }
}

/// Helper to compute OpAgg from histogram data
pub(crate) fn compute_op_agg(hists: &crate::metrics::OpHists, total_bytes: u64, total_ops: u64) -> crate::workload::OpAgg {
    if total_ops == 0 {
        return crate::workload::OpAgg::default();
    }
    
    // Merge all size bucket histograms into one combined histogram
    let combined = hists.combined_histogram();
    
    crate::workload::OpAgg {
        bytes: total_bytes,
        ops: total_ops,
        mean_us: combined.mean() as u64,
        p50_us: combined.value_at_quantile(0.50),
        p95_us: combined.value_at_quantile(0.95),
        p99_us: combined.value_at_quantile(0.99),
    }
}
