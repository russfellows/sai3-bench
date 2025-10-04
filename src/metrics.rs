//! Shared metrics collection infrastructure
//!
//! Size-bucketed HDR histograms for detailed latency analysis.

use hdrhistogram::Histogram;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Number of size buckets for histogram collection
pub const NUM_BUCKETS: usize = 9;

/// Labels for each size bucket
pub const BUCKET_LABELS: [&str; NUM_BUCKETS] = [
    "zero",
    "1B-8KiB",
    "8KiB-64KiB",
    "64KiB-512KiB",
    "512KiB-4MiB",
    "4MiB-32MiB",
    "32MiB-256MiB",
    "256MiB-2GiB",
    ">2GiB",
];

/// Determine which size bucket a given byte count belongs to
pub fn bucket_index(nbytes: usize) -> usize {
    if nbytes == 0 {
        0
    } else if nbytes <= 8 * 1024 {
        1
    } else if nbytes <= 64 * 1024 {
        2
    } else if nbytes <= 512 * 1024 {
        3
    } else if nbytes <= 4 * 1024 * 1024 {
        4
    } else if nbytes <= 32 * 1024 * 1024 {
        5
    } else if nbytes <= 256 * 1024 * 1024 {
        6
    } else if nbytes <= 2 * 1024 * 1024 * 1024 {
        7
    } else {
        8
    }
}

/// Size-bucketed histograms for one operation type
#[derive(Debug, Clone)]
pub struct OpHists {
    pub buckets: Arc<Vec<Mutex<Histogram<u64>>>>,
}

impl OpHists {
    /// Create a new set of histograms (one per size bucket)
    pub fn new() -> Self {
        let mut v = Vec::with_capacity(NUM_BUCKETS);
        for _ in 0..NUM_BUCKETS {
            v.push(Mutex::new(
                Histogram::<u64>::new_with_bounds(1, 3_600_000_000, 3)
                    .expect("failed to allocate histogram"),
            ));
        }
        OpHists {
            buckets: Arc::new(v),
        }
    }

    /// Record a latency measurement in the appropriate size bucket
    pub fn record(&self, bucket: usize, duration: Duration) {
        let micros = duration.as_micros() as u64;
        let mut hist = self.buckets[bucket].lock().unwrap();
        let _ = hist.record(micros);
    }

    /// Print a summary of all buckets for this operation
    pub fn print_summary(&self, op: &str) {
        println!("\n{} latency (µs):", op);
        for (i, m) in self.buckets.iter().enumerate() {
            let hist = m.lock().unwrap();
            let count = hist.len();
            if count == 0 {
                continue;
            }
            let mean = hist.mean();
            let p50 = hist.value_at_quantile(0.50);
            let p95 = hist.value_at_quantile(0.95);
            let p99 = hist.value_at_quantile(0.99);
            let max = hist.max();
            println!(
                "  [{:>13}] count={:<8} mean={:<8.0} p50={:<8} p95={:<8} p99={:<8} max={:<8}",
                BUCKET_LABELS[i], count, mean, p50, p95, p99, max
            );
        }
    }

    /// Merge another OpHists into this one (for combining worker results)
    pub fn merge(&mut self, other: &OpHists) {
        for i in 0..NUM_BUCKETS {
            let mut my_hist = self.buckets[i].lock().unwrap();
            let other_hist = other.buckets[i].lock().unwrap();
            my_hist.add(&*other_hist).ok();
        }
    }

    /// Get a combined histogram across all size buckets (for aggregated percentiles)
    pub fn combined_histogram(&self) -> Histogram<u64> {
        let mut combined = Histogram::<u64>::new_with_bounds(1, 3_600_000_000, 3)
            .expect("failed to allocate combined histogram");
        
        for bucket_hist in self.buckets.iter() {
            let hist = bucket_hist.lock().unwrap();
            combined.add(&*hist).ok();
        }
        
        combined
    }
}

impl Default for OpHists {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_index() {
        assert_eq!(bucket_index(0), 0); // zero
        assert_eq!(bucket_index(1), 1); // 1B-8KiB
        assert_eq!(bucket_index(8 * 1024), 1); // 1B-8KiB
        assert_eq!(bucket_index(8 * 1024 + 1), 2); // 8KiB-64KiB
        assert_eq!(bucket_index(64 * 1024), 2); // 8KiB-64KiB
        assert_eq!(bucket_index(64 * 1024 + 1), 3); // 64KiB-512KiB
        assert_eq!(bucket_index(4 * 1024 * 1024), 4); // 512KiB-4MiB
        assert_eq!(bucket_index(32 * 1024 * 1024), 5); // 4MiB-32MiB
        assert_eq!(bucket_index(256 * 1024 * 1024), 6); // 32MiB-256MiB
        assert_eq!(bucket_index(2 * 1024 * 1024 * 1024), 7); // 256MiB-2GiB
        assert_eq!(bucket_index(3 * 1024 * 1024 * 1024), 8); // >2GiB
    }

    #[test]
    fn test_ophists_record() {
        let hists = OpHists::new();
        hists.record(1, Duration::from_micros(100));
        hists.record(1, Duration::from_micros(200));

        let hist = hists.buckets[1].lock().unwrap();
        assert_eq!(hist.len(), 2);
    }

    #[test]
    fn test_ophists_merge() {
        let mut hists1 = OpHists::new();
        let hists2 = OpHists::new();

        hists1.record(1, Duration::from_micros(100));
        hists2.record(1, Duration::from_micros(200));

        hists1.merge(&hists2);

        let hist = hists1.buckets[1].lock().unwrap();
        assert_eq!(hist.len(), 2);
    }
}
