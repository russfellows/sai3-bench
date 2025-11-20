// tests/multi_process_tests.rs
// Integration tests for multi-process scaling features (v0.7.3+)

use sai3_bench::config::{ProcessScaling, ProcessingMode};
use sai3_bench::workload::{IpcSummary, SizeBins, OpAgg};
use hdrhistogram::Histogram;
use hdrhistogram::serialization::Serializer;
use base64::Engine;
use std::collections::HashMap;
use std::sync::{Mutex};

#[test]
fn test_process_scaling_deserialize_single() {
    let yaml = r#"
        processes: single
    "#;
    
    let config: HashMap<String, ProcessScaling> = serde_yaml::from_str(yaml).unwrap();
    let scaling = config.get("processes").unwrap();
    
    assert_eq!(scaling.resolve(), 1);
}

#[test]
fn test_process_scaling_deserialize_auto() {
    let yaml = r#"
        processes: auto
    "#;
    
    let config: HashMap<String, ProcessScaling> = serde_yaml::from_str(yaml).unwrap();
    let scaling = config.get("processes").unwrap();
    
    // Should detect physical cores (at least 1, typically 2-64)
    let cores = scaling.resolve();
    assert!(cores >= 1, "Auto scaling should detect at least 1 core");
    assert!(cores <= 256, "Auto scaling should be reasonable (got {})", cores);
}

#[test]
fn test_process_scaling_deserialize_manual() {
    let yaml = r#"
        processes: 8
    "#;
    
    let config: HashMap<String, ProcessScaling> = serde_yaml::from_str(yaml).unwrap();
    let scaling = config.get("processes").unwrap();
    
    assert_eq!(scaling.resolve(), 8);
}

#[test]
fn test_process_scaling_default() {
    // Default should be Single
    let scaling = ProcessScaling::default();
    assert_eq!(scaling.resolve(), 1);
}

#[test]
fn test_processing_mode_deserialize() {
    let yaml_multi_process = r#"
        mode: multi_process
    "#;
    
    let config: HashMap<String, ProcessingMode> = serde_yaml::from_str(yaml_multi_process).unwrap();
    let mode = config.get("mode").unwrap();
    assert!(matches!(mode, ProcessingMode::MultiProcess));
    
    let yaml_multi_runtime = r#"
        mode: multi_runtime
    "#;
    
    let config: HashMap<String, ProcessingMode> = serde_yaml::from_str(yaml_multi_runtime).unwrap();
    let mode = config.get("mode").unwrap();
    assert!(matches!(mode, ProcessingMode::MultiRuntime));
}

#[test]
fn test_processing_mode_default() {
    // Default should be MultiRuntime (cleaner, easier to debug)
    let mode = ProcessingMode::default();
    assert!(matches!(mode, ProcessingMode::MultiRuntime));
}

#[test]
fn test_histogram_serialization_roundtrip() {
    // Create a histogram with some data
    let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    
    // Record some values
    hist.record(100).unwrap();
    hist.record(500).unwrap();
    hist.record(1000).unwrap();
    hist.record(5000).unwrap();
    hist.record(10_000).unwrap();
    
    let original_p50 = hist.value_at_quantile(0.5);
    let original_p95 = hist.value_at_quantile(0.95);
    let original_p99 = hist.value_at_quantile(0.99);
    let original_mean = hist.mean();
    
    // Serialize to base64
    let mut serialized = Vec::new();
    hdrhistogram::serialization::V2Serializer::new()
        .serialize(&hist, &mut serialized)
        .unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&serialized);
    
    // Deserialize back
    let decoded = base64::engine::general_purpose::STANDARD.decode(&encoded).unwrap();
    let mut cursor = std::io::Cursor::new(decoded);
    let deserialized: Histogram<u64> = 
        hdrhistogram::serialization::Deserializer::new()
            .deserialize(&mut cursor)
            .unwrap();
    
    // Verify values match
    assert_eq!(deserialized.value_at_quantile(0.5), original_p50);
    assert_eq!(deserialized.value_at_quantile(0.95), original_p95);
    assert_eq!(deserialized.value_at_quantile(0.99), original_p99);
    assert_eq!(deserialized.mean(), original_mean);
    assert_eq!(deserialized.len(), hist.len());
}

#[test]
fn test_histogram_merging_preserves_percentiles() {
    // Create two histograms with different distributions
    let mut hist1 = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    let mut hist2 = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    
    // Hist1: Fast operations (100-200µs)
    for _ in 0..1000 {
        hist1.record(100).unwrap();
        hist1.record(150).unwrap();
        hist1.record(200).unwrap();
    }
    
    // Hist2: Slow operations (500-1000µs)
    for _ in 0..1000 {
        hist2.record(500).unwrap();
        hist2.record(750).unwrap();
        hist2.record(1000).unwrap();
    }
    
    // Merge hist2 into hist1
    hist1.add(&hist2).unwrap();
    
    // Verify merged histogram has correct statistics
    assert_eq!(hist1.len(), 6000); // 3000 from each
    
    // p50 should be between the two distributions (~300-400µs)
    let p50 = hist1.value_at_quantile(0.5);
    assert!(p50 >= 200 && p50 <= 500, "p50={} should be between distributions", p50);
    
    // p95 should be in the upper distribution (>700µs)
    let p95 = hist1.value_at_quantile(0.95);
    assert!(p95 >= 700, "p95={} should be in upper distribution", p95);
}

#[test]
fn test_ipc_summary_serialization() {
    // Create a minimal IpcSummary manually for testing serialization
    let ipc = IpcSummary {
        wall_seconds: 1.5,
        total_bytes: 10240,
        total_ops: 100,
        p50_us: 100,
        p95_us: 500,
        p99_us: 1000,
        get: OpAgg { ops: 50, bytes: 5120, mean_us: 120, p50_us: 100, p95_us: 400, p99_us: 800 },
        put: OpAgg { ops: 50, bytes: 5120, mean_us: 180, p50_us: 150, p95_us: 600, p99_us: 1200 },
        meta: OpAgg::default(),
        get_bins: SizeBins { by_bucket: HashMap::new() },
        put_bins: SizeBins { by_bucket: HashMap::new() },
        meta_bins: SizeBins { by_bucket: HashMap::new() },
        get_hists_serialized: vec![],
        total_errors: 0,
        error_rate: 0.0,
        put_hists_serialized: vec![],
        meta_hists_serialized: vec![],
    };
    
    // Serialize to JSON
    let json = serde_json::to_string(&ipc).unwrap();
    assert!(json.contains("\"wall_seconds\":1.5"));
    assert!(json.contains("\"total_ops\":100"));
    
    // Deserialize back
    let deserialized: IpcSummary = serde_json::from_str(&json).unwrap();
    
    // Verify fields match
    assert_eq!(deserialized.wall_seconds, 1.5);
    assert_eq!(deserialized.total_ops, 100);
    assert_eq!(deserialized.total_bytes, 10240);
    assert_eq!(deserialized.get.ops, 50);
    assert_eq!(deserialized.put.ops, 50);
}

#[test]
fn test_store_cache_key_generation() {
    // This tests the cache key format used in get_cached_store()
    // Key format: "{base_uri}_range:{enabled}_cache:{mode}"
    
    let base_uri = "s3://my-bucket/prefix";
    
    // Test with range enabled, cache enabled
    let key1 = format!("{}_range:{}_cache:{}", base_uri, true, "Normal");
    assert_eq!(key1, "s3://my-bucket/prefix_range:true_cache:Normal");
    
    // Test with range disabled, cache disabled
    let key2 = format!("{}_range:{}_cache:{}", base_uri, false, "Disabled");
    assert_eq!(key2, "s3://my-bucket/prefix_range:false_cache:Disabled");
    
    // Different URIs should have different keys
    let key3 = format!("{}_range:{}_cache:{}", "s3://other-bucket", true, "Normal");
    assert_ne!(key1, key3);
    
    // Different configs should have different keys
    let key4 = format!("{}_range:{}_cache:{}", base_uri, false, "Normal");
    assert_ne!(key1, key4);
}

// Note: WorkloadConfig integration testing removed - tested via end-to-end tests instead
// Configuration parsing is tested in unit tests within config.rs

#[test]
fn test_size_bins_merge() {
    let mut bins1 = SizeBins { by_bucket: HashMap::new() };
    let mut bins2 = SizeBins { by_bucket: HashMap::new() };
    
    // Bins1: Add some data
    bins1.add(0);
    bins1.add(100);
    bins1.add(10_000);
    
    // Bins2: Add different data
    bins2.add(0);
    bins2.add(500_000);
    
    // Merge bins2 into bins1
    bins1.merge_from(&bins2);
    
    // Verify total operations in each bucket
    // Bucket 0: 0 bytes -> should have 2 ops
    let bucket_0 = bins1.by_bucket.get(&0).unwrap();
    assert_eq!(bucket_0.0, 2); // Two 0-byte operations
    
    // Should have entries for different size buckets
    assert!(bins1.by_bucket.len() >= 3); // At least 3 different buckets
}

#[test]
fn test_op_agg_accumulation() {
    let mut agg = OpAgg::default();
    
    // No operations initially
    assert_eq!(agg.ops, 0);
    assert_eq!(agg.bytes, 0);
    
    // Simulate adding operations
    agg.ops = 100;
    agg.bytes = 1024 * 100;
    agg.p50_us = 100;
    agg.p95_us = 500;
    agg.p99_us = 1000;
    
    // Verify values
    assert_eq!(agg.ops, 100);
    assert_eq!(agg.bytes, 102400);
    assert_eq!(agg.p50_us, 100);
    assert_eq!(agg.p95_us, 500);
    assert_eq!(agg.p99_us, 1000);
}

#[cfg(test)]
mod store_cache_tests {
    use super::*;
    use std::sync::Arc;
    
    // Note: We can't easily test the actual ObjectStore caching without
    // creating real stores, but we can test the cache structure itself
    
    #[test]
    fn test_store_cache_concurrent_access() {
        // Simulate the StoreCache structure
        type MockCache = Arc<Mutex<HashMap<String, String>>>;
        
        let cache: MockCache = Arc::new(Mutex::new(HashMap::new()));
        
        // Insert a value
        {
            let mut map = cache.lock().unwrap();
            map.insert("key1".to_string(), "value1".to_string());
        }
        
        // Read from multiple "threads" (simulated)
        let cache2 = cache.clone();
        let value = {
            let map = cache2.lock().unwrap();
            map.get("key1").cloned()
        };
        
        assert_eq!(value, Some("value1".to_string()));
        
        // Verify cache size
        let map = cache.lock().unwrap();
        assert_eq!(map.len(), 1);
    }
}

/// Integration test: Verify process scaling display format
#[test]
fn test_process_scaling_display() {
    let single = ProcessScaling::Single;
    let auto = ProcessScaling::Auto;
    let manual = ProcessScaling::Manual(16);
    
    // These are displayed in dry-run output
    assert_eq!(single.resolve(), 1);
    assert!(auto.resolve() >= 1); // System-dependent
    assert_eq!(manual.resolve(), 16);
}

/// Test that histogram buckets are created with correct bounds
#[test]
fn test_histogram_bucket_creation() {
    // Match the bounds used in workload.rs
    let hist = Histogram::<u64>::new_with_bounds(1, 60_000_000, 3).unwrap();
    
    // Should be able to record very low and very high values
    let mut test_hist = hist.clone();
    test_hist.record(1).unwrap();           // 1µs
    test_hist.record(100).unwrap();         // 100µs
    test_hist.record(1_000).unwrap();       // 1ms
    test_hist.record(10_000).unwrap();      // 10ms
    test_hist.record(1_000_000).unwrap();   // 1s
    test_hist.record(60_000_000).unwrap();  // 60s (max)
    
    assert_eq!(test_hist.len(), 6);
}
