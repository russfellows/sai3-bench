//! Shared metrics collection infrastructure
//!
//! Size-bucketed HDR histograms for detailed latency analysis.

use hdrhistogram::Histogram;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// Import bucket constants from constants module
use crate::constants::{NUM_BUCKETS, BUCKET_LABELS};

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
        assert_eq!(bucket_index(512 * 1024), 3); // 64KiB-512KiB
        assert_eq!(bucket_index(512 * 1024 + 1), 4); // 512KiB-4MiB
        assert_eq!(bucket_index(4 * 1024 * 1024), 4); // 512KiB-4MiB
        assert_eq!(bucket_index(4 * 1024 * 1024 + 1), 5); // 4MiB-32MiB
        assert_eq!(bucket_index(32 * 1024 * 1024), 5); // 4MiB-32MiB
        assert_eq!(bucket_index(32 * 1024 * 1024 + 1), 6); // 32MiB-256MiB
        assert_eq!(bucket_index(256 * 1024 * 1024), 6); // 32MiB-256MiB
        assert_eq!(bucket_index(256 * 1024 * 1024 + 1), 7); // 256MiB-2GiB
        assert_eq!(bucket_index(2 * 1024 * 1024 * 1024), 7); // 256MiB-2GiB
        assert_eq!(bucket_index(2 * 1024 * 1024 * 1024 + 1), 8); // >2GiB
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

    #[test]
    fn test_bucket_labels_count() {
        // Ensure BUCKET_LABELS array matches NUM_BUCKETS
        assert_eq!(BUCKET_LABELS.len(), NUM_BUCKETS);
        assert_eq!(BUCKET_LABELS.len(), 9);
    }

    #[test]
    fn test_all_bucket_boundaries_comprehensive() {
        // This test systematically verifies ALL bucket boundaries to ensure:
        // 1. No gaps between buckets
        // 2. No overlaps between buckets
        // 3. Exact boundary values go to the correct bucket
        // 4. Boundary+1 values transition to the next bucket
        
        // Define all bucket boundaries (inclusive upper bounds)
        let boundaries: [(usize, usize, &str); 9] = [
            (0, 0, "zero"),                           // Bucket 0: exactly 0
            (1, 8 * 1024, "1B-8KiB"),                 // Bucket 1: 1 to 8 KiB
            (8 * 1024 + 1, 64 * 1024, "8KiB-64KiB"),  // Bucket 2: 8 KiB + 1 to 64 KiB
            (64 * 1024 + 1, 512 * 1024, "64KiB-512KiB"),  // Bucket 3: 64 KiB + 1 to 512 KiB
            (512 * 1024 + 1, 4 * 1024 * 1024, "512KiB-4MiB"),  // Bucket 4: 512 KiB + 1 to 4 MiB
            (4 * 1024 * 1024 + 1, 32 * 1024 * 1024, "4MiB-32MiB"),  // Bucket 5: 4 MiB + 1 to 32 MiB
            (32 * 1024 * 1024 + 1, 256 * 1024 * 1024, "32MiB-256MiB"),  // Bucket 6: 32 MiB + 1 to 256 MiB
            (256 * 1024 * 1024 + 1, 2 * 1024 * 1024 * 1024, "256MiB-2GiB"),  // Bucket 7: 256 MiB + 1 to 2 GiB
            (2 * 1024 * 1024 * 1024 + 1, usize::MAX, ">2GiB"),  // Bucket 8: > 2 GiB
        ];
        
        // Test that each boundary definition matches BUCKET_LABELS
        for (expected_bucket, (_, _, label)) in boundaries.iter().enumerate() {
            assert_eq!(
                BUCKET_LABELS[expected_bucket], 
                *label,
                "Bucket {} label mismatch", 
                expected_bucket
            );
        }
        
        // Test lower bound of each bucket (except bucket 0 which is special)
        for (expected_bucket, (lower, _, _)) in boundaries.iter().enumerate() {
            if expected_bucket == 0 {
                // Bucket 0 is special: only exactly 0
                assert_eq!(bucket_index(0), 0, "Zero should be in bucket 0");
            } else {
                assert_eq!(
                    bucket_index(*lower), 
                    expected_bucket,
                    "Lower bound {} should be in bucket {}", 
                    lower, 
                    expected_bucket
                );
            }
        }
        
        // Test upper bound of each bucket (except bucket 8 which is unbounded)
        for (expected_bucket, (_, upper, _)) in boundaries.iter().enumerate() {
            if expected_bucket == 8 {
                // Bucket 8 is unbounded, test a very large value
                assert_eq!(
                    bucket_index(10 * 1024 * 1024 * 1024), 
                    8,
                    "Very large values should be in bucket 8"
                );
            } else if expected_bucket == 0 {
                // Bucket 0 only contains 0, skip upper bound test
                continue;
            } else {
                assert_eq!(
                    bucket_index(*upper), 
                    expected_bucket,
                    "Upper bound {} should be in bucket {}", 
                    upper, 
                    expected_bucket
                );
            }
        }
        
        // Test that upper_bound + 1 transitions to next bucket
        // (except for bucket 0 and bucket 8)
        for (expected_bucket, (_, upper, _)) in boundaries.iter().enumerate() {
            if expected_bucket == 0 {
                // 0 + 1 = 1 should go to bucket 1
                assert_eq!(bucket_index(1), 1, "1 byte should be in bucket 1");
            } else if expected_bucket < 8 {
                // upper + 1 should be in next bucket
                let next_bucket = expected_bucket + 1;
                assert_eq!(
                    bucket_index(*upper + 1), 
                    next_bucket,
                    "Upper bound + 1 ({}) should transition from bucket {} to bucket {}", 
                    upper + 1, 
                    expected_bucket, 
                    next_bucket
                );
            }
        }
        
        // Additional edge case tests
        assert_eq!(bucket_index(usize::MAX), 8, "usize::MAX should be in bucket 8");
        
        // Test mid-range values for each bucket
        assert_eq!(bucket_index(4 * 1024), 1, "4 KiB (mid-range) should be in bucket 1");
        assert_eq!(bucket_index(32 * 1024), 2, "32 KiB (mid-range) should be in bucket 2");
        assert_eq!(bucket_index(256 * 1024), 3, "256 KiB (mid-range) should be in bucket 3");
        assert_eq!(bucket_index(2 * 1024 * 1024), 4, "2 MiB (mid-range) should be in bucket 4");
        assert_eq!(bucket_index(16 * 1024 * 1024), 5, "16 MiB (mid-range) should be in bucket 5");
        assert_eq!(bucket_index(128 * 1024 * 1024), 6, "128 MiB (mid-range) should be in bucket 6");
        assert_eq!(bucket_index(1024 * 1024 * 1024), 7, "1 GiB (mid-range) should be in bucket 7");
        assert_eq!(bucket_index(5 * 1024 * 1024 * 1024), 8, "5 GiB (mid-range) should be in bucket 8");
    }
}
